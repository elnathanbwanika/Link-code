//! The agent loop.
//!
//! This is the heart of Link Code and it contains no UI code at all. It:
//!
//! 1. takes a user message and builds the context (system prompt, project
//!    memory, `@file` attachments, conversation history);
//! 2. streams a response, emitting [`AgentEvent`]s as tokens arrive;
//! 3. parses tool calls — native ones, or the text protocol for models without
//!    native tool calling;
//! 4. asks the [`PermissionEngine`] about each call, prompting the user through
//!    an [`Approver`] when the engine says "ask";
//! 5. executes the tools, snapshots files into a checkpoint first, and feeds the
//!    results back;
//! 6. repeats until the model stops calling tools or the iteration cap is hit.
//!
//! Everything the loop does is observable: every step emits a typed event, so the
//! agent panel can render exactly what happened, and tests can assert on it.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use link_models::error::{ErrorCategory as ModelErrorCategory, LinkModelError};
use link_models::provider::ModelProvider;
use link_models::sse::ToolCallAccumulator;
use link_models::toolcall;
use link_models::types::{
    ChatMessage, ChatRequest, FinishReason, StreamEvent, ToolCallRequest, Usage,
};

use crate::checkpoint::CheckpointStore;
use crate::context::{estimate_tokens, ContextMeter, ContextPart};
use crate::error::LinkError;
use crate::event::{AgentEvent, DoneReason, ErrorKind, EventSink, ToolStatus};
use crate::permission::{
    Decision, PermissionDecision, PermissionEngine, PermissionMode, PermissionRequest,
};
use crate::project::ProjectMemory;
use crate::tool::{ToolCall, ToolContext, ToolDisplay, ToolRegistry};

/// The built-in system prompt. Project memory is appended after it.
pub const BASE_SYSTEM_PROMPT: &str = r#"You are Link Code, an expert coding agent working directly in the user's local workspace through a graphical IDE.

Principles:
- Be concise. Skip preamble and apologies; go straight to the work.
- Read before you write. Use `read_file`, `grep` and `glob` to understand the code before editing it.
- Prefer small, surgical edits with `edit_file` over rewriting whole files with `write_file`.
- Never guess at file contents or APIs. Verify with a tool call.
- Every file you change must be inside the workspace. There is no way to escape it.
- After changing code, run the project's own checks (tests, linter, build) with `bash` when they exist.
- Explain what you changed and why, briefly. If you made an assumption, say so explicitly.
- When a task is multi-step, keep the task list up to date with `todo_write`.
- If a command or edit is refused by the user, adapt; do not repeat the same denied action.
- If you are blocked, say what you need instead of producing speculative code.
"#;

/// Everything the loop needs that is not a dependency.
#[derive(Debug, Clone)]
pub struct AgentOptions {
    /// Internal configuration id, echoed in the model badge.
    pub model_id: String,
    /// Badge label, e.g. `ollama \u{00b7} qwen2.5-coder:32b`.
    pub model_label: String,
    /// Extra system prompt from the model config, appended to the base prompt.
    pub system_prompt_override: Option<String>,
    /// Contents of `LINK.md` and `.link/rules/*.md`.
    pub project_memory: ProjectMemory,
    /// Hard cap on tool iterations in one turn.
    pub max_iterations: u32,
    pub max_output_tokens: u32,
    pub temperature: Option<f64>,
    /// Compaction trigger, 0.0 - 1.0.
    pub auto_compact_threshold: f64,
    /// Generate next-step chips after each answer.
    pub generate_suggestions: bool,
    /// Offer tools at all (false for pure chat models).
    pub tools_enabled: bool,
    /// Redact secret-looking values before they leave the machine.
    pub protect_secrets: bool,
    /// Model window, used by the meter when the provider does not say.
    pub context_window: usize,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            model_id: String::new(),
            model_label: "unconfigured".to_string(),
            system_prompt_override: None,
            project_memory: ProjectMemory::default(),
            max_iterations: 40,
            max_output_tokens: 8192,
            temperature: Some(0.2),
            auto_compact_threshold: 0.85,
            generate_suggestions: true,
            tools_enabled: true,
            protect_secrets: true,
            context_window: 32_768,
        }
    }
}

/// Answers the approval modal. The UI implements this; tests use the helpers
/// below.
pub trait Approver: Send + Sync {
    fn approve(&self, call_id: String, request: PermissionRequest) -> BoxFuture<'static, Decision>;
}

/// Approves everything once. Useful for tests and for `Full auto`.
pub struct AutoApprover;

impl Approver for AutoApprover {
    fn approve(&self, _call_id: String, _request: PermissionRequest) -> BoxFuture<'static, Decision> {
        Box::pin(async { Decision::AllowOnce })
    }
}

/// Refuses everything. Used to prove that nothing runs without approval.
pub struct DenyApprover;

impl Approver for DenyApprover {
    fn approve(&self, _call_id: String, _request: PermissionRequest) -> BoxFuture<'static, Decision> {
        Box::pin(async { Decision::Deny })
    }
}

/// Messages the UI can queue while the agent is busy.
pub enum AgentMessage {
    /// A user prompt, with `@`-attached workspace paths.
    Send {
        text: String,
        attachments: Vec<String>,
    },
    /// Swap the model mid-session; history is preserved.
    SetModel {
        provider: Arc<dyn ModelProvider>,
        options: Box<AgentOptions>,
    },
    /// Change the permission mode (`Shift+Tab`).
    SetMode(PermissionMode),
    /// Replace the project memory (after `/init`).
    SetProjectMemory(ProjectMemory),
    /// Compact the conversation on demand (`/compact`).
    Compact,
    /// Stop streaming and cancel running tools (`Esc`).
    Stop,
    /// Shut the loop down.
    Close,
}

/// A clonable control handle for a running loop.
///
/// The cancellation token lives behind an `Arc<Mutex<..>>` so a new turn can
/// install a fresh token while every clone the UI holds keeps working.
#[derive(Clone)]
pub struct AgentHandle {
    token: Arc<Mutex<CancellationToken>>,
    state: Arc<Mutex<AgentState>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Thinking,
    RunningTool,
    Stopped,
}

impl Default for AgentHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentHandle {
    pub fn new() -> Self {
        Self {
            token: Arc::new(Mutex::new(CancellationToken::new())),
            state: Arc::new(Mutex::new(AgentState::Idle)),
        }
    }

    /// The token for the *current* turn.
    pub fn token(&self) -> CancellationToken {
        self.token.lock().expect("token mutex poisoned").clone()
    }

    /// Install a fresh token for a new turn and return it.
    pub fn begin_turn(&self) -> CancellationToken {
        let token = CancellationToken::new();
        {
            let mut guard = self.token.lock().expect("token mutex poisoned");
            // Re-arming: the previous token is dropped once nothing holds it.
            *guard = token.clone();
        }
        *self.state.lock().expect("state mutex poisoned") = AgentState::Thinking;
        token
    }

    /// Cancel the current turn immediately. The UI calls this from `Esc`.
    pub fn stop(&self) {
        self.token.lock().expect("token mutex poisoned").cancel();
        *self.state.lock().expect("state mutex poisoned") = AgentState::Stopped;
    }

    pub fn state(&self) -> AgentState {
        *self.state.lock().expect("state mutex poisoned")
    }

    pub fn set_state(&self, state: AgentState) {
        *self.state.lock().expect("state mutex poisoned") = state;
    }
}

/// Record of one tool call, returned so callers can log it.
#[derive(Debug, Clone)]
pub struct ToolExecutionRecord {
    pub call_id: String,
    pub tool: String,
    pub summary: String,
    pub ok: bool,
    pub content: String,
    pub duration_ms: u64,
}

/// The loop itself.
pub struct AgentLoop {
    provider: Arc<dyn ModelProvider>,
    registry: ToolRegistry,
    sink: Arc<dyn EventSink>,
    approver: Arc<dyn Approver>,
    engine: PermissionEngine,
    tool_ctx: ToolContext,
    options: AgentOptions,
    history: Vec<ChatMessage>,
    meter: ContextMeter,
    checkpoints: Option<CheckpointStore>,
    session_id: String,
    handle: AgentHandle,
    /// Tool calls observed this turn, exposed for the Suggestions log.
    last_suggestions: Vec<String>,
}

impl AgentLoop {
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        registry: ToolRegistry,
        tool_ctx: ToolContext,
        sink: Arc<dyn EventSink>,
        approver: Arc<dyn Approver>,
        engine: PermissionEngine,
        options: AgentOptions,
    ) -> Self {
        let window = options.context_window.max(1);
        let mut loop_ = Self {
            provider,
            registry,
            sink,
            approver,
            engine,
            tool_ctx,
            options,
            history: Vec::new(),
            meter: ContextMeter::new(window),
            checkpoints: None,
            session_id: Uuid::new_v4().to_string(),
            handle: AgentHandle::new(),
            last_suggestions: Vec::new(),
        };
        loop_.refresh_static_context();
        loop_
    }

    // ---- wiring ---------------------------------------------------------

    pub fn handle(&self) -> AgentHandle {
        self.handle.clone()
    }

    pub fn with_handle(mut self, handle: AgentHandle) -> Self {
        self.handle = handle;
        self
    }

    pub fn with_checkpoints(mut self, store: CheckpointStore, session_id: impl Into<String>) -> Self {
        self.checkpoints = Some(store);
        self.session_id = session_id.into();
        self
    }

    pub fn with_history(mut self, history: Vec<ChatMessage>) -> Self {
        self.history = history;
        self.refresh_static_context();
        self
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    pub fn history(&self) -> &[ChatMessage] {
        &self.history
    }

    pub fn options(&self) -> &AgentOptions {
        &self.options
    }

    pub fn meter(&self) -> &ContextMeter {
        &self.meter
    }

    pub fn engine(&self) -> &PermissionEngine {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut PermissionEngine {
        &mut self.engine
    }

    pub fn sink(&self) -> &Arc<dyn EventSink> {
        &self.sink
    }

    pub fn suggestions(&self) -> &[String] {
        &self.last_suggestions
    }

    /// Swap the model without losing the conversation (header dropdown or
    /// `/model`).
    pub fn set_model(&mut self, provider: Arc<dyn ModelProvider>, options: AgentOptions) {
        self.provider = provider;
        let window = options.context_window.max(1);
        self.options = options;
        self.meter.set_max_tokens(window);
        // Force a fresh provider = fresh tokenisation cache downstream.
        self.refresh_static_context();
        self.sink.emit(AgentEvent::ModelChanged {
            model: self.options.model_label.clone(),
        });
    }

    pub fn set_project_memory(&mut self, memory: ProjectMemory) {
        self.options.project_memory = memory;
        self.refresh_static_context();
    }

    /// Stop the current turn. Safe to call from any thread.
    pub fn stop(&self) {
        self.handle.stop();
    }

    pub fn emit(&self, event: AgentEvent) {
        self.sink.emit(event);
    }

    // ---- context --------------------------------------------------------

    /// The system prompt: base + model override + project memory + tool
    /// catalogue and, when needed, the text tool protocol.
    pub fn system_prompt(&self) -> String {
        let mut prompt = String::from(BASE_SYSTEM_PROMPT);
        if let Some(override_) = &self.options.system_prompt_override {
            if !override_.trim().is_empty() {
                prompt.push_str("\n\n## Project-specific instructions\n\n");
                prompt.push_str(override_.trim());
                prompt.push('\n');
            }
        }
        prompt.push_str(&self.options.project_memory.system_section());
        if self.options.tools_enabled && !self.registry.is_empty() {
            prompt.push_str("\n\n## Available tools\n\n");
            prompt.push_str(&self.registry.describe_all());
            if !self.provider.capabilities().tools {
                let names = self.registry.names();
                prompt.push_str(&toolcall::fallback_protocol_prompt(&names));
            }
        }
        prompt
    }

    fn refresh_static_context(&mut self) {
        let system = self.system_prompt();
        self.meter.set_text(ContextPart::SystemPrompt, &system);
        self.meter.set_text(
            ContextPart::ProjectMemory,
            &self.options.project_memory.instructions,
        );
        let history: String = self
            .history
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        self.meter.set_text(ContextPart::History, &history);
    }

    fn emit_context(&self) {
        let breakdown = self.meter.breakdown();
        self.sink.emit(AgentEvent::ContextUpdated {
            used_tokens: breakdown.total,
            max_tokens: breakdown.max_tokens,
        });
    }

    // ---- the turn -------------------------------------------------------

    /// Build the user message: attachments first, then the prompt.
    fn build_user_message(&self, text: &str, attachments: &[String]) -> String {
        if attachments.is_empty() {
            return text.to_string();
        }
        let mut out = String::new();
        for attachment in attachments {
            match self.tool_ctx.sandbox.resolve_existing(attachment) {
                Ok(path) => {
                    let relative = self.tool_ctx.sandbox.relative(&path);
                    if crate::paths::is_secret_path(&path) && self.options.protect_secrets {
                        out.push_str(&format!(
                            "<file path=\"{relative}\" redacted=\"true\">\n[Link Code withheld this file because it matches a secret pattern. Ask the user to paste the part you need.]\n</file>\n"
                        ));
                        continue;
                    }
                    match std::fs::read_to_string(&path) {
                        Ok(content) => {
                            let content = if self.options.protect_secrets {
                                redact_secrets(&content)
                            } else {
                                content
                            };
                            out.push_str(&format!(
                                "<file path=\"{relative}\">\n{content}\n</file>\n"
                            ));
                        }
                        Err(error) => out.push_str(&format!(
                            "<file path=\"{relative}\" error=\"true\">\nCould not read: {error}\n</file>\n"
                        )),
                    }
                }
                Err(error) => out.push_str(&format!(
                    "<attachment path=\"{attachment}\" error=\"true\">\n{error}\n</attachment>\n"
                )),
            }
        }
        if !out.is_empty() {
            out.push_str("\n");
        }
        out.push_str(text);
        out
    }

    /// Run one complete turn: stream, act, repeat.
    pub async fn send(&mut self, text: String, attachments: Vec<String>) -> Result<(), LinkError> {
        let token = self.handle.begin_turn();
        self.tool_ctx.cancel = token.clone();
        let turn_id = Uuid::new_v4();
        self.sink.emit(AgentEvent::TurnStarted {
            turn_id,
            model: self.options.model_label.clone(),
        });

        let content = self.build_user_message(&text, &attachments);
        self.history.push(ChatMessage::user(content));
        self.refresh_static_context();
        self.emit_context();

        let mut malformed_retries = 0u32;
        let mut iteration = 0u32;
        let reason = loop {
            if token.is_cancelled() {
                break DoneReason::Cancelled;
            }
            if iteration >= self.options.max_iterations.max(1) {
                self.sink.emit(AgentEvent::Notice {
                    message: format!(
                        "Stopped after {} tool iterations; send another message to continue.",
                        self.options.max_iterations
                    ),
                });
                break DoneReason::IterationLimit;
            }
            iteration += 1;

            if self
                .meter
                .needs_compaction(self.options.auto_compact_threshold)
            {
                self.compact(token.clone()).await;
                self.sink.emit(AgentEvent::Notice {
                    message: "Context compacted automatically".to_string(),
                });
            }

            let streamed = match self.stream_response(&token, turn_id).await {
                Ok(value) => value,
                Err(StreamFailure::Cancelled) => break DoneReason::Cancelled,
                Err(StreamFailure::Provider(error)) => {
                    self.report_model_error(&error);
                    break DoneReason::Errored;
                }
            };

            let mut calls = streamed.calls;
            let mut visible = streamed.text;

            // Models without native tool calling speak the text protocol.
            if self.options.tools_enabled && !self.provider.capabilities().tools {
                let parsed = toolcall::parse(&visible);
                if !parsed.remaining_text.is_empty() || !parsed.calls.is_empty() {
                    self.sink.emit(AgentEvent::TextComplete {
                        block_id: streamed.block_id,
                        text: parsed.remaining_text.clone(),
                    });
                    visible = parsed.remaining_text;
                }
                if !parsed.malformed.is_empty() && parsed.calls.is_empty() {
                    malformed_retries += 1;
                    if malformed_retries > 2 {
                        self.sink.emit(AgentEvent::Error {
                            block_id: Some(streamed.block_id),
                            kind: ErrorKind::MalformedToolCall,
                            message: ErrorKind::MalformedToolCall.headline().to_string(),
                            detail: Some(parsed.malformed.join("\n")),
                        });
                        break DoneReason::Errored;
                    }
                    self.sink.emit(AgentEvent::Notice {
                        message: "The model's tool call did not parse; asking it to try again"
                            .to_string(),
                    });
                    self.history.push(ChatMessage::user(correction_prompt(&parsed.malformed)));
                    continue;
                }
                calls = parsed.calls;
            }

            let mut assistant = ChatMessage::assistant(visible.clone());
            assistant.tool_calls = calls.clone();
            self.history.push(assistant);

            if calls.is_empty() {
                break DoneReason::Completed;
            }

            for call in calls {
                if token.is_cancelled() {
                    break;
                }
                let record = self.execute_call(turn_id, call).await;
                self.history.push(ChatMessage::tool_result(
                    record.call_id,
                    record.tool,
                    record.content,
                ));
            }

            if token.is_cancelled() {
                break DoneReason::Cancelled;
            }
        };

        if reason == DoneReason::Completed && self.options.generate_suggestions {
            self.generate_suggestions(&token).await;
        }

        self.handle.set_state(AgentState::Idle);
        self.sink.emit(AgentEvent::TurnFinished { turn_id, reason });
        self.sink.emit(AgentEvent::Done { reason });
        self.refresh_static_context();
        self.emit_context();
        Ok(())
    }

    /// Stream one model response, emitting deltas as they arrive.
    async fn stream_response(
        &mut self,
        token: &CancellationToken,
        _turn_id: Uuid,
    ) -> Result<StreamedTurn, StreamFailure> {
        let provider = Arc::clone(&self.provider);
        let block_id = Uuid::new_v4();
        self.sink.emit(AgentEvent::BlockStarted { block_id });

        let mut request = ChatRequest::new(self.options.model_id.clone());
        request.system = Some(self.system_prompt());
        request.messages = self.history.clone();
        request.max_tokens = Some(self.options.max_output_tokens);
        request.temperature = self.options.temperature;
        if self.options.tools_enabled && provider.capabilities().tools && !self.registry.is_empty() {
            request.tools = self.registry.openai_tool_schemas();
        }
        self.meter
            .set(ContextPart::History, request.estimated_prompt_tokens());

        self.handle.set_state(AgentState::Thinking);
        let mut stream = provider
            .stream_chat(request)
            .await
            .map_err(StreamFailure::Provider)?;

        let mut accumulator = ToolCallAccumulator::new();
        let mut visible = String::new();
        let mut thinking = String::new();
        let mut usage = Usage::default();
        let mut finish = FinishReason::Stop;
        let started = Instant::now();
        let mut first_token: Option<Instant> = None;

        loop {
            tokio::select! {
                _ = token.cancelled() => return Err(StreamFailure::Cancelled),
                item = stream.next() => match item {
                    None => break,
                    Some(Err(error)) => return Err(StreamFailure::Provider(error)),
                    Some(Ok(event)) => match event {
                        StreamEvent::TextDelta(text) => {
                            if first_token.is_none() {
                                first_token = Some(Instant::now());
                            }
                            visible.push_str(&text);
                            self.sink.emit(AgentEvent::TokenDelta { block_id, text });
                        }
                        StreamEvent::ThinkingDelta(text) => {
                            thinking.push_str(&text);
                            self.sink.emit(AgentEvent::ThinkingDelta { block_id, text });
                        }
                        StreamEvent::ToolCallDelta { .. } => accumulator.push(&event),
                        StreamEvent::Usage(reported) => usage = reported,
                        StreamEvent::Done { finish_reason } => finish = finish_reason,
                    },
                },
            }
        }

        let calls = accumulator.finish();
        if usage.prompt_tokens == 0 && usage.completion_tokens == 0 {
            usage = Usage {
                prompt_tokens: self.meter.breakdown().total,
                completion_tokens: estimate_tokens(&visible),
            };
        }
        let generation = first_token.map(|start| start.elapsed()).unwrap_or_else(|| started.elapsed());
        let tokens_per_second = usage.tokens_per_second(generation);
        let cost = link_models::pricing::estimate_cost(
            provider.provider_kind(),
            &self.options.model_id,
            usage,
        );

        self.sink.emit(AgentEvent::Usage {
            block_id,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            tokens_per_second,
            latency_ms: started.elapsed().as_millis() as u64,
            cost_usd: cost,
        });
        self.meter
            .record_exact_total(usage.prompt_tokens, self.meter.breakdown().tool_results);
        self.emit_context();

        Ok(StreamedTurn {
            block_id,
            text: visible,
            calls,
            finish,
            usage,
        })
    }

    /// Execute one tool call, with approval, checkpointing and rich events.
    async fn execute_call(&mut self, turn_id: Uuid, request: ToolCallRequest) -> ToolExecutionRecord {
        let call_id = if request.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            request.id.clone()
        };
        let normalised = ToolCall::new(
            call_id.clone(),
            request.name.clone(),
            request
                .parsed_arguments()
                .unwrap_or(serde_json::Value::Null),
        )
        .normalised();
        let block_id = Uuid::new_v4();

        let Some(tool) = self.registry.get(&normalised.name) else {
            let message = format!(
                "No tool named `{}` exists. Available tools: {}",
                normalised.name,
                self.registry.names().join(", ")
            );
            self.sink.emit(AgentEvent::ToolRequested {
                block_id,
                call_id: call_id.clone(),
                tool: normalised.name.clone(),
                summary: format!("Unknown tool `{}`", normalised.name),
                args: normalised.arguments.clone(),
            });
            self.sink.emit(AgentEvent::ToolStatusChanged {
                call_id: call_id.clone(),
                status: ToolStatus::Failed,
            });
            return ToolExecutionRecord {
                call_id,
                tool: normalised.name,
                summary: "unknown tool".to_string(),
                ok: false,
                content: message,
                duration_ms: 0,
            };
        };

        let summary = tool.summarise(&normalised);
        self.sink.emit(AgentEvent::ToolRequested {
            block_id,
            call_id: call_id.clone(),
            tool: normalised.name.clone(),
            summary: summary.clone(),
            args: normalised.arguments.clone(),
        });

        // ---- permission -------------------------------------------------
        let permission = tool.permission_request(&self.tool_ctx, &normalised);
        let mut allowed = true;
        let mut denial_reason: Option<String> = None;
        match self.engine.evaluate(permission.clone()) {
            PermissionDecision::Allow(_) => {}
            PermissionDecision::Deny(reason) => {
                allowed = false;
                denial_reason = Some(reason);
                self.sink.emit(AgentEvent::ApprovalResolved {
                    call_id: call_id.clone(),
                    allowed: false,
                    scope: None,
                    feedback: denial_reason.clone(),
                });
            }
            PermissionDecision::Ask(request) => {
                self.sink.emit(AgentEvent::ApprovalRequested {
                    block_id,
                    call_id: call_id.clone(),
                    request: request.clone(),
                });
                let approver = Arc::clone(&self.approver);
                let decision = approver.approve(call_id.clone(), (*request).clone()).await;
                self.engine.record(&request, &decision);
                if decision.is_allowed() {
                    self.sink.emit(AgentEvent::ApprovalResolved {
                        call_id: call_id.clone(),
                        allowed: true,
                        scope: decision.scope(),
                        feedback: None,
                    });
                } else {
                    allowed = false;
                    denial_reason = match &decision {
                        Decision::DenyWithFeedback { reason } => Some(reason.clone()),
                        _ => None,
                    };
                    self.sink.emit(AgentEvent::ApprovalResolved {
                        call_id: call_id.clone(),
                        allowed: false,
                        scope: None,
                        feedback: denial_reason.clone(),
                    });
                }
            }
        }

        if !allowed {
            self.sink.emit(AgentEvent::ToolStatusChanged {
                call_id: call_id.clone(),
                status: ToolStatus::Denied,
            });
            let mut content = "The user denied this tool call.".to_string();
            if let Some(reason) = &denial_reason {
                content.push(' ');
                content.push_str(reason);
                content.push_str(" Do not repeat it; choose another approach or ask the user.");
            }
            return ToolExecutionRecord {
                call_id,
                tool: normalised.name,
                summary,
                ok: false,
                content,
                duration_ms: 0,
            };
        }

        self.checkpoint_before_write(turn_id, &permission);
        self.handle.set_state(AgentState::RunningTool);
        self.sink.emit(AgentEvent::ToolStatusChanged {
            call_id: call_id.clone(),
            status: ToolStatus::Running,
        });

        let started = Instant::now();
        let result = {
            let ctx = &self.tool_ctx;
            tool.run(ctx, &normalised).await
        };
        let duration_ms = started.elapsed().as_millis() as u64;
        self.handle.set_state(AgentState::Thinking);

        match result {
            Ok(output) => {
                match &output.display {
                    ToolDisplay::Diff { diff, apply_path, new_content, previous } => {
                        self.sink.emit(AgentEvent::DiffProposed {
                            block_id,
                            call_id: call_id.clone(),
                            diff: diff.clone(),
                            applied: true,
                        });
                        // Keep the editor honest about what changed on disk.
                        let _ = (apply_path, new_content, previous);
                    }
                    ToolDisplay::Todos(items) => {
                        self.sink.emit(AgentEvent::TodosUpdated { items: items.clone() });
                    }
                    _ => {}
                }
                let content = output.content.clone();
                let truncated = output.truncated;
                self.sink.emit(AgentEvent::ToolResult {
                    call_id: call_id.clone(),
                    tool: normalised.name.clone(),
                    ok: true,
                    content: content.clone(),
                    duration_ms,
                    truncated,
                });
                self.sink.emit(AgentEvent::ToolStatusChanged {
                    call_id: call_id.clone(),
                    status: ToolStatus::Done,
                });
                ToolExecutionRecord {
                    call_id,
                    tool: normalised.name,
                    summary,
                    ok: true,
                    content,
                    duration_ms,
                }
            }
            Err(error) => {
                let content = format!("Tool error: {error}");
                self.sink.emit(AgentEvent::ToolResult {
                    call_id: call_id.clone(),
                    tool: normalised.name.clone(),
                    ok: false,
                    content: content.clone(),
                    duration_ms,
                    truncated: false,
                });
                self.sink.emit(AgentEvent::ToolStatusChanged {
                    call_id: call_id.clone(),
                    status: ToolStatus::Failed,
                });
                ToolExecutionRecord {
                    call_id,
                    tool: normalised.name,
                    summary,
                    ok: false,
                    content,
                    duration_ms,
                }
            }
        }
    }

    /// Snapshot the target of a write before it happens, so "Undo turn" works.
    fn checkpoint_before_write(&mut self, turn_id: Uuid, request: &PermissionRequest) {
        if request.kind != crate::permission::ActionKind::Write {
            return;
        }
        let (Some(store), Some(detail)) = (&self.checkpoints, &request.detail) else {
            return;
        };
        let relative = detail.trim().trim_matches('`').to_string();
        let Ok(path) = self.tool_ctx.sandbox.resolve(&relative) else {
            return;
        };
        match store.snapshot(
            &self.session_id,
            turn_id,
            self.tool_ctx.sandbox.root(),
            "agent turn",
            std::slice::from_ref(&path),
        ) {
            Ok(checkpoint) => self.sink.emit(AgentEvent::Notice {
                message: format!("Checkpoint created ({})", checkpoint.id),
            }),
            Err(error) => tracing::warn!(?error, "could not create a checkpoint"),
        }
    }

    /// Replace the older half of the conversation with a model-written summary.
    /// Falls back to a mechanical truncation, so `/compact` always succeeds.
    pub async fn compact(&mut self, token: CancellationToken) {
        const KEEP_TAIL: usize = 6;
        if self.history.len() <= KEEP_TAIL {
            return;
        }
        let split = self.history.len() - KEEP_TAIL;
        let transcript = self.history[..split]
            .iter()
            .map(|message| format!("{}: {}", message.role.as_str(), message.content))
            .collect::<Vec<_>>()
            .join("\n");

        let mut request = ChatRequest::new(self.options.model_id.clone());
        request.system = Some(
            "You summarise coding sessions so an assistant can continue with less context. \
             Preserve the user's goal, decisions already made, files touched and why, \
             unresolved problems, and constraints. Be terse and factual; bullet points only."
                .to_string(),
        );
        request.messages = vec![ChatMessage::user(link_models::sse::truncate(&transcript, 24_000))];
        request.max_tokens = Some(1024);
        request.temperature = Some(0.0);

        let provider = Arc::clone(&self.provider);
        let summary = tokio::select! {
            _ = token.cancelled() => None,
            result = provider.complete(request) => result.ok().map(|(text, _)| text),
        };
        let summary = summary
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| {
                "Earlier turns were dropped to free context. The full transcript is still in the \
                 session file and can be exported with /export."
                    .to_string()
            });

        let mut rebuilt = Vec::with_capacity(KEEP_TAIL + 1);
        rebuilt.push(ChatMessage::system(format!(
            "<previous-conversation-summary>\n{summary}\n</previous-conversation-summary>"
        )));
        rebuilt.extend(self.history[split..].iter().cloned());
        self.history = rebuilt;
        self.refresh_static_context();
        self.emit_context();
    }

    /// Ask the active model for 2-4 follow-up suggestions for the next-step chips.
    async fn generate_suggestions(&mut self, token: &CancellationToken) {
        use link_models::types::Role;
        let last = self
            .history
            .iter()
            .rev()
            .find(|message| message.role == Role::Assistant)
            .map(|message| message.content.clone())
            .unwrap_or_default();
        if last.trim().is_empty() {
            return;
        }

        let mut request = ChatRequest::new(self.options.model_id.clone());
        request.system = Some(
            "Suggest next steps for an engineer using an AI coding IDE. Reply with exactly 3 \
             short imperative suggestions, one per line, no numbering, no quotes, each under \
             60 characters."
                .to_string(),
        );
        request.messages = vec![ChatMessage::user(link_models::sse::truncate(&last, 4_000))];
        request.max_tokens = Some(160);
        request.temperature = Some(0.3);

        let provider = Arc::clone(&self.provider);
        let result = tokio::select! {
            _ = token.cancelled() => return,
            result = tokio::time::timeout(Duration::from_secs(20), provider.complete(request)) => result,
        };
        let Ok(Ok((text, _))) = result else {
            return;
        };

        let items: Vec<String> = text
            .lines()
            .map(|line| {
                line.trim()
                    .trim_start_matches(|c: char| {
                        c.is_ascii_digit() || c == '-' || c == '*' || c == '.' || c == ' '
                    })
                    .trim()
                    .trim_matches('"')
                    .to_string()
            })
            .filter(|line| !line.is_empty() && line.chars().count() <= 80)
            .take(4)
            .collect();
        if items.is_empty() {
            return;
        }

        let block_id = Uuid::new_v4();
        self.last_suggestions = items.clone();
        for item in &items {
            self.sink.emit(AgentEvent::SuggestionShown {
                block_id,
                source: crate::event::SuggestionSource::NextStepChip,
                text: item.clone(),
            });
        }
        self.sink.emit(AgentEvent::SuggestionsReady { block_id, items });
    }

    /// Turn a provider failure into a plain-language error block.
    fn report_model_error(&self, error: &LinkModelError) {
        let kind = match error.category() {
            ModelErrorCategory::Connection => ErrorKind::Connection,
            ModelErrorCategory::Auth => ErrorKind::Auth,
            ModelErrorCategory::RateLimit => ErrorKind::RateLimit,
            ModelErrorCategory::ContextOverflow => ErrorKind::ContextOverflow,
            ModelErrorCategory::Malformed => ErrorKind::MalformedToolCall,
            ModelErrorCategory::Config => ErrorKind::NoModel,
            _ => ErrorKind::Other,
        };
        self.sink.emit(AgentEvent::Error {
            block_id: None,
            kind,
            message: kind.headline().to_string(),
            detail: Some(error.to_string()),
        });
    }
}

/// A turn's streamed result.
#[derive(Debug, Clone)]
struct StreamedTurn {
    block_id: Uuid,
    text: String,
    calls: Vec<ToolCallRequest>,
    #[allow(dead_code)]
    finish: FinishReason,
    #[allow(dead_code)]
    usage: Usage,
}

/// Why streaming stopped early.
enum StreamFailure {
    Cancelled,
    Provider(LinkModelError),
}

/// Redact anything that looks like a credential before it leaves the machine.
pub fn redact_secrets(text: &str) -> String {
    use std::sync::OnceLock;
    static PATTERNS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            r"(?i)(api[_-]?key|secret|token|passwd|password|passphrase)\s*[:=]\s*[\x22']?([A-Za-z0-9_\-]{12,})",
            r"(?i)bearer\s+[A-Za-z0-9._\-]{16,}",
            r"sk-[A-Za-z0-9]{16,}",
            r"ghp_[A-Za-z0-9]{20,}",
            r"AKIA[0-9A-Z]{12,}",
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----([\s\S]*?)-----END [A-Z ]*PRIVATE KEY-----",
        ]
        .iter()
        .filter_map(|pattern| regex::Regex::new(pattern).ok())
        .collect()
    });
    let mut out = text.to_string();
    for pattern in patterns {
        out = match pattern.captures_len() {
            3 => pattern
                .replace_all(&out, "$1=\"***REDACTED BY LINK CODE***\"")
                .into_owned(),
            _ => pattern
                .replace_all(&out, "***REDACTED BY LINK CODE***")
                .into_owned(),
        };
    }
    out
}

/// The message sent back when a fallback-protocol tool call did not parse.
pub fn correction_prompt(problems: &[String]) -> String {
    format!(
        "Your last tool call could not be parsed and nothing was executed. Problems:\n{}\n\n\
Re-emit the tool call as a single fenced block tagged `tool`, one JSON object per line, \
using exactly the documented tool names and argument schemas.",
        problems
            .iter()
            .map(|problem| format!("- {problem}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Drives a loop from a queue of [`AgentMessage`]s (queued prompts, stop,
/// model switches). The UI spawns this on the tokio runtime.
pub async fn run_agent(mut agent: AgentLoop, mut queue: tokio::sync::mpsc::UnboundedReceiver<AgentMessage>) {
    while let Some(message) = queue.recv().await {
        match message {
            AgentMessage::Send { text, attachments } => {
                if let Err(error) = agent.send(text, attachments).await {
                    agent.emit(AgentEvent::Error {
                        block_id: None,
                        kind: ErrorKind::Other,
                        message: "The turn could not be completed".to_string(),
                        detail: Some(error.to_string()),
                    });
                }
            }
            AgentMessage::SetModel { provider, options } => agent.set_model(provider, *options),
            AgentMessage::SetMode(mode) => agent.engine_mut().set_mode(mode),
            AgentMessage::SetProjectMemory(memory) => agent.set_project_memory(memory),
            AgentMessage::Compact => {
                let token = agent.handle.token();
                agent.compact(token).await;
                agent.emit(AgentEvent::Notice {
                    message: "Context compacted".to_string(),
                });
            }
            AgentMessage::Stop => agent.stop(),
            AgentMessage::Close => break,
        }
    }
}
