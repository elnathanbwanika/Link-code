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

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use link_models::error::{ErrorCategory as ModelErrorCategory, LinkModelError};
use link_models::provider::ModelProvider;
use link_models::sse::{JsonFieldStream, ToolCallAccumulator};
use link_models::toolcall;
use link_models::types::{
    ChatMessage, ChatRequest, FinishReason, Role, StreamEvent, ToolCallRequest, Usage,
};

use crate::checkpoint::CheckpointStore;
use crate::context::{estimate_tokens, ContextMeter, ContextPart};
use crate::error::LinkError;
use crate::event::{AgentEvent, DoneReason, EditorStreamField, ErrorKind, EventSink, ToolStatus};
use crate::permission::{
    Decision, PermissionDecision, PermissionEngine, PermissionMode, PermissionRequest,
};
use crate::project::ProjectMemory;
use crate::tool::{ToolCall, ToolContext, ToolDisplay, ToolRegistry};

/// The built-in system prompt. Project memory is appended after it.
pub const BASE_SYSTEM_PROMPT: &str = r#"You are Link Code, an expert coding agent working inside a graphical IDE on the user's local workspace. The user sees your chat messages in a side panel and your file changes live in the editor.

Communication rules:
- Before each action, write one short sentence saying what you are about to do and why. Example: "I'll check how the nav is built before adding the button."
- After a group of actions, write one short sentence on what you found or did.
- Keep it to plain English. Never paste code, file contents, or diffs into your chat text. The editor shows them automatically.
- To create or change files, use ONLY write_file, edit_file or create_file. Never use bash to write or modify files. Heredocs, `sed -i`, `echo >`, `tee`, `dd` and node/python write scripts are rejected.
- At the start of multi-step work, call update_tasks with your plan. Update it as you go: one task in_progress at a time, mark each completed when done.
- Finish with a 2-3 sentence summary: what changed, which files, and anything the user should check.

Working rules:
- Read before you write. Use `read_file`, `grep` and `glob` to understand the code before editing it.
- Prefer small, surgical edits with `edit_file` over rewriting whole files with `write_file`.
- Never guess at file contents or APIs. Verify with a tool call.
- Every file you change must be inside the workspace. There is no way to escape it.
- After changing code, run the project's own checks (tests, linter, build) with `bash` when they exist.
- Do not stop early. A turn only ends when the task is genuinely finished: all edits are made, checks pass and the user has a short summary. Never stop after a plan, a first step or a promise to continue.
- If a command or edit is refused by the user, adapt; do not repeat the same denied action.
- If you are blocked, say what you need instead of producing speculative code.
"#;

/// Models sometimes end a turn after a few words — a cut-off stream, an empty
/// reply, or a promise ("Let me start by…") with no tool call. Rather than
/// handing control back to the user, the loop nudges the model to continue.
/// This caps how many nudges a single turn may spend.
const MAX_AUTO_CONTINUATIONS: u32 = 4;

/// Sent when a reply was empty, truncated or visibly unfinished.
const CONTINUATION_PROMPT: &str = "Your previous reply ended before the task was \
finished. Continue exactly where you stopped: do not repeat work that is already \
done, do not restate the plan, and do not ask whether to continue. Use tools as \
needed and keep going until the task is complete, verified and summarised.";

/// True when the text reads like a plan or promise with the work still ahead:
/// it trails off after a colon or ends on a lead-in such as "Let me" or
/// "Now I'll".
pub fn looks_unfinished(text: &str) -> bool {
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.ends_with(':') || trimmed.ends_with("...") || trimmed.ends_with('\u{2026}') {
        return true;
    }
    let tail = trimmed
        .rsplit(['\n', '.', '!', '?'])
        .find(|part| !part.trim().is_empty())
        .unwrap_or(trimmed)
        .trim()
        .to_ascii_lowercase();
    // "Let me know if you need anything else" is a sign-off, not a plan.
    if tail.starts_with("let me know") {
        return false;
    }
    const LEAD_INS: [&str; 14] = [
        "let me",
        "i'll",
        "i will",
        "i'm going to",
        "i am going to",
        "now i",
        "next, i",
        "next i",
        "first, i",
        "first i",
        "starting with",
        "i'm about to",
        "i am about to",
        "here's the plan",
    ];
    LEAD_INS.iter().any(|lead| tail.starts_with(lead))
}

/// Everything the loop needs that is not a dependency.
#[derive(Debug, Clone)]
pub struct AgentOptions {
    /// Model name sent to the provider, for example `gpt-4o-mini`. This is
    /// the endpoint's identifier (`ModelConfig::model_id`), never the local
    /// registry id.
    pub model_name: String,
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
            model_name: String::new(),
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
        let mut continuations = 0u32;
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
            let block_id = streamed.block_id;
            let finish = streamed.finish;

            // Models without native tool calling speak the text protocol.
            if self.options.tools_enabled && !self.provider.capabilities().tools {
                let parsed = toolcall::parse(&visible);
                if !parsed.remaining_text.is_empty() || !parsed.calls.is_empty() {
                    self.sink.emit(AgentEvent::TextComplete {
                        block_id,
                        text: parsed.remaining_text.clone(),
                    });
                    visible = parsed.remaining_text;
                }
                if !parsed.malformed.is_empty() && parsed.calls.is_empty() {
                    malformed_retries += 1;
                    if malformed_retries > 2 {
                        self.sink.emit(AgentEvent::Error {
                            block_id: Some(block_id),
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
            } else if self.options.tools_enabled
                && calls.is_empty()
                && toolcall::looks_like_tool_call(&visible)
            {
                // Some gateways strip the `tools` field or the model ignores it
                // and answers with the text protocol anyway. Recover those
                // calls instead of showing raw tool JSON to the user.
                let parsed = toolcall::parse(&visible);
                if !parsed.calls.is_empty() {
                    if !parsed.remaining_text.is_empty() {
                        self.sink.emit(AgentEvent::TextComplete {
                            block_id,
                            text: parsed.remaining_text.clone(),
                        });
                        visible = parsed.remaining_text;
                    }
                    calls = parsed.calls;
                }
            }

            // Keep the assistant turn in history so the next request can see
            // what it already produced. A content-free reply is dropped: some
            // providers reject an empty assistant message.
            if !visible.trim().is_empty() || !calls.is_empty() {
                let mut assistant = ChatMessage::assistant(visible.clone());
                assistant.tool_calls = calls.clone();
                self.history.push(assistant);
            }

            if calls.is_empty() {
                // A reply can end with no tool calls yet still be incomplete:
                // the stream was cut off, the model returned nothing, or it
                // stopped on a promise ("Let me start by…"). Nudge it to keep
                // going instead of handing an unfinished task back to the user.
                let empty = visible.trim().is_empty();
                // Only an explicit `length` finish means the model ran out of
                // output budget. A stream that simply ends is a normal reply:
                // most providers signal completion with `[DONE]` or by closing
                // the connection, not always with a finish_reason chunk.
                let truncated = finish == FinishReason::Length;
                let unfinished = !empty && looks_unfinished(&visible);
                if (truncated || empty || unfinished) && continuations < MAX_AUTO_CONTINUATIONS {
                    continuations += 1;
                    // Continuations are silent on purpose: the reply continues
                    // in place and a notice per nudge is just noise.
                    // A content-free assistant reply is not in the history, so
                    // the previous entry is already a user turn. Merge to keep
                    // Anthropic's strict role alternation.
                    if self.history.last().map(|message| message.role) == Some(Role::User) {
                        if let Some(last) = self.history.last_mut() {
                            last.content.push_str("\n\n");
                            last.content.push_str(CONTINUATION_PROMPT);
                        }
                    } else {
                        self.history.push(ChatMessage::user(CONTINUATION_PROMPT));
                    }
                    continue;
                }
                if (truncated || empty || unfinished) && !empty {
                    self.sink.emit(AgentEvent::Notice {
                        message: format!(
                            "The model paused after {MAX_AUTO_CONTINUATIONS} automatic continuations. Send a message to keep going."
                        ),
                    });
                }
                break DoneReason::Completed;
            }

            let mut pending = calls.into_iter();
            while let Some(call) = pending.next() {
                if token.is_cancelled() {
                    // These calls never run, so the live-preview text in the
                    // editor must be discarded rather than left as a phantom
                    // edit.
                    self.cancel_streamed_edit(&call);
                    for rest in pending {
                        self.cancel_streamed_edit(&rest);
                    }
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

        self.handle.set_state(AgentState::Idle);
        self.sink.emit(AgentEvent::TurnFinished { turn_id, reason });
        self.sink.emit(AgentEvent::Done { reason });
        self.refresh_static_context();
        self.emit_context();
        // Chips are a nicety; never hold the turn open for them.
        if reason == DoneReason::Completed && self.options.generate_suggestions {
            self.spawn_suggestions(token.clone());
        }
        Ok(())
    }

    /// Forward growing file-edit arguments to the editor instead of the chat.
    ///
    /// This runs for every tool-call fragment, before the call is complete, so
    /// the user watches the file being written rather than a block of code
    /// appearing in the conversation.
    fn track_file_edit_stream(
        &self,
        accumulator: &ToolCallAccumulator,
        live: &mut BTreeMap<usize, LiveEdit>,
        index: usize,
        fragment: &str,
    ) {
        let Some(tool) = accumulator.name_at(index) else {
            return;
        };
        if !matches!(tool, "write_file" | "create_file" | "edit_file") {
            return;
        }
        let call_id = accumulator
            .id_at(index)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("call_{index}"));
        let entry = live.entry(index).or_insert_with(|| LiveEdit {
            call_id,
            tool: tool.to_string(),
            path: String::new(),
            path_buf: String::new(),
            pending_content: String::new(),
            started: false,
            fields: JsonFieldStream::new(),
        });
        if let Some(id) = accumulator.id_at(index) {
            if !id.is_empty() {
                entry.call_id = id.to_string();
            }
        }
        for chunk in entry.fields.push(fragment) {
            match chunk.field.as_str() {
                "path" => {
                    entry.path_buf.push_str(&chunk.text);
                    if chunk.complete && !entry.path_buf.is_empty() && !entry.started {
                        entry.path = std::mem::take(&mut entry.path_buf);
                        let absolute = self.tool_ctx.sandbox.root().join(&entry.path);
                        self.sink.emit(AgentEvent::EditorStreamStarted {
                            call_id: entry.call_id.clone(),
                            tool: entry.tool.clone(),
                            path: entry.path.clone(),
                            new_file: !absolute.is_file(),
                        });
                        entry.started = true;
                        if !entry.pending_content.is_empty() {
                            self.sink.emit(AgentEvent::EditorStreamDelta {
                                call_id: entry.call_id.clone(),
                                field: EditorStreamField::Content,
                                text: std::mem::take(&mut entry.pending_content),
                            });
                        }
                    }
                }
                "content" if entry.started => {
                    self.sink.emit(AgentEvent::EditorStreamDelta {
                        call_id: entry.call_id.clone(),
                        field: EditorStreamField::Content,
                        text: chunk.text,
                    });
                }
                "content" => {
                    entry.pending_content.push_str(&chunk.text);
                }
                "old_string" if entry.started => {
                    self.sink.emit(AgentEvent::EditorStreamDelta {
                        call_id: entry.call_id.clone(),
                        field: EditorStreamField::OldString,
                        text: chunk.text,
                    });
                }
                "new_string" if entry.started => {
                    self.sink.emit(AgentEvent::EditorStreamDelta {
                        call_id: entry.call_id.clone(),
                        field: EditorStreamField::NewString,
                        text: chunk.text,
                    });
                }
                _ => {}
            }
        }
    }

    /// Tell the UI that every live edit stream is over.
    ///
    /// `completed` is false when the stream was cancelled or the provider
    /// failed: the editor then discards the half-typed buffer and reloads the
    /// real file from disk.
    fn emit_editor_stream_ends(&self, live: &BTreeMap<usize, LiveEdit>, completed: bool) {
        for edit in live.values().filter(|edit| edit.started) {
            self.sink.emit(AgentEvent::EditorStreamEnded {
                call_id: edit.call_id.clone(),
                path: edit.path.clone(),
                completed,
            });
        }
    }

    /// A streamed file-edit call was abandoned before it could run (the user
    /// cancelled the turn). Ask the UI to throw away the preview text.
    fn cancel_streamed_edit(&self, call: &ToolCallRequest) {
        if !matches!(
            call.name.as_str(),
            "write_file" | "create_file" | "edit_file" | "multi_edit"
        ) {
            return;
        }
        let Ok(arguments) = serde_json::from_str::<serde_json::Value>(&call.arguments) else {
            return;
        };
        if let Some(path) = arguments.get("path").and_then(|value| value.as_str()) {
            self.sink.emit(AgentEvent::EditorStreamEnded {
                call_id: call.id.clone(),
                path: path.to_string(),
                completed: false,
            });
        }
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

        let mut request = ChatRequest::new(self.options.model_name.clone());
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
        let mut live_edits: BTreeMap<usize, LiveEdit> = BTreeMap::new();
        let mut visible = String::new();
        let mut thinking = String::new();
        let mut usage = Usage::default();
        let mut finish = FinishReason::Stop;
        let started = Instant::now();
        let mut first_token: Option<Instant> = None;

        loop {
            tokio::select! {
                _ = token.cancelled() => {
                    self.emit_editor_stream_ends(&live_edits, false);
                    return Err(StreamFailure::Cancelled);
                },
                item = stream.next() => match item {
                    None => break,
                    Some(Err(error)) => {
                        self.emit_editor_stream_ends(&live_edits, false);
                        return Err(StreamFailure::Provider(error));
                    }
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
                        StreamEvent::ToolCallDelta { .. } => {
                            accumulator.push(&event);
                            if let StreamEvent::ToolCallDelta {
                                index,
                                arguments_delta,
                                ..
                            } = &event
                            {
                                self.track_file_edit_stream(
                                    &accumulator,
                                    &mut live_edits,
                                    *index,
                                    arguments_delta,
                                );
                            }
                        }
                        StreamEvent::Usage(reported) => usage = reported,
                        StreamEvent::Done { finish_reason } => {
                            finish = finish_reason;
                        }
                    },
                },
            }
        }

        self.emit_editor_stream_ends(&live_edits, true);

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
            &self.options.model_name,
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

        let mut request = ChatRequest::new(self.options.model_name.clone());
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

    /// Ask the active model for 2-4 follow-up suggestions for the next-step
    /// chips. Runs *after* the turn is reported done and on its own task, so
    /// the composer never waits on this extra model call.
    fn spawn_suggestions(&self, token: CancellationToken) {
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

        let mut request = ChatRequest::new(self.options.model_name.clone());
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
        let sink = Arc::clone(&self.sink);
        tokio::spawn(async move {
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
            for item in &items {
                sink.emit(AgentEvent::SuggestionShown {
                    block_id,
                    source: crate::event::SuggestionSource::NextStepChip,
                    text: item.clone(),
                });
            }
            sink.emit(AgentEvent::SuggestionsReady { block_id, items });
        });
    }

    /// Turn a provider failure into a plain-language error block.
    fn report_model_error(&self, error: &LinkModelError) {
        let kind = match error.category() {
            ModelErrorCategory::Connection => ErrorKind::Connection,
            ModelErrorCategory::Auth => match error {
                LinkModelError::MissingApiKey { .. } => ErrorKind::MissingApiKey,
                _ => ErrorKind::Auth,
            },
            ModelErrorCategory::RateLimit => ErrorKind::RateLimit,
            ModelErrorCategory::ContextOverflow => ErrorKind::ContextOverflow,
            ModelErrorCategory::Malformed => ErrorKind::MalformedToolCall,
            ModelErrorCategory::Config => ErrorKind::NoModel,
            _ => ErrorKind::Other,
        };
        let message = match error {
            LinkModelError::MissingApiKey { .. } => {
                "No API key is configured \u{2014} add one in Models (Ctrl+M)".to_string()
            }
            _ => kind.headline().to_string(),
        };
        self.sink.emit(AgentEvent::Error {
            block_id: None,
            kind,
            message,
            detail: Some(error.to_string()),
        });
    }
}

/// A file-edit call whose arguments are still streaming.
struct LiveEdit {
    call_id: String,
    tool: String,
    path: String,
    path_buf: String,
    /// Content that arrived before the path field was complete.
    pending_content: String,
    started: bool,
    fields: JsonFieldStream,
}

/// A turn's streamed result.
#[derive(Debug, Clone)]
struct StreamedTurn {
    block_id: Uuid,
    text: String,
    calls: Vec<ToolCallRequest>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;

    use link_models::provider::{ConnectionReport, ModelStream};
    use link_models::types::{Capabilities, ModelInfo};
    use link_models::{ModelResult, ProviderKind};

    use crate::event::RecordingSink;
    use crate::paths::Sandbox;
    use crate::permission::{PermissionEngine, PermissionMode, PermissionRules};
    use crate::tool::ToolRegistry;

    /// Replays a fixed script of streamed turns, recording how many requests the
    /// loop made.
    struct ScriptedProvider {
        turns: StdMutex<VecDeque<Vec<StreamEvent>>>,
        calls: StdMutex<usize>,
    }

    impl ScriptedProvider {
        fn new(turns: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                turns: StdMutex::new(turns.into()),
                calls: StdMutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for ScriptedProvider {
        fn id(&self) -> &str {
            "scripted"
        }

        fn label(&self) -> &str {
            "scripted"
        }

        fn provider_kind(&self) -> ProviderKind {
            ProviderKind::OpenAiCompatible
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                tools: true,
                ..Default::default()
            }
        }

        fn context_window(&self) -> usize {
            8_192
        }

        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(Vec::new())
        }

        async fn stream_chat(&self, _request: ChatRequest) -> ModelResult<ModelStream> {
            *self.calls.lock().unwrap() += 1;
            let events = self
                .turns
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(Box::pin(futures_util::stream::iter(
                events.into_iter().map(Ok),
            )))
        }

        async fn complete(&self, _request: ChatRequest) -> ModelResult<(String, Usage)> {
            Ok((String::new(), Usage::default()))
        }

        async fn test_connection(&self) -> ModelResult<ConnectionReport> {
            unreachable!("the continuation tests never probe the connection")
        }
    }

    fn agent(
        script: Vec<Vec<StreamEvent>>,
    ) -> (AgentLoop, Arc<RecordingSink>, Arc<ScriptedProvider>) {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let provider = Arc::new(ScriptedProvider::new(script));
        let sink = Arc::new(RecordingSink::new());
        let mut options = AgentOptions::default();
        options.model_name = "scripted".to_string();
        options.model_label = "scripted".to_string();
        options.tools_enabled = false;
        options.generate_suggestions = false;
        let agent = AgentLoop::new(
            provider.clone(),
            ToolRegistry::new(),
            ToolContext::new(sandbox),
            sink.clone(),
            Arc::new(DenyApprover),
            PermissionEngine::new(PermissionMode::Ask, PermissionRules::default()),
            options,
        );
        (agent, sink, provider)
    }

    fn say(text: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::TextDelta(text.to_string()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            },
        ]
    }

    /// Cancels the turn the moment the last edit stream finishes, to exercise
    /// the window between "editor unlocked" and "tool runs".
    struct CancelOnStreamEnd {
        inner: Arc<RecordingSink>,
        handle: AgentHandle,
    }

    impl EventSink for CancelOnStreamEnd {
        fn emit(&self, event: AgentEvent) {
            let cancel = matches!(
                event,
                AgentEvent::EditorStreamEnded {
                    completed: true,
                    ..
                }
            );
            self.inner.emit(event);
            if cancel {
                self.handle.stop();
            }
        }
    }

    struct FakeWriteTool;
    #[async_trait::async_trait]
    impl crate::tool::Tool for FakeWriteTool {
        fn name(&self) -> &'static str {
            "write_file"
        }

        fn description(&self) -> &'static str {
            "fake write tool"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                }
            })
        }

        fn kind(&self) -> crate::permission::ActionKind {
            crate::permission::ActionKind::Write
        }

        fn summarise(&self, call: &ToolCall) -> String {
            call.opt_str_arg("path").unwrap_or_default()
        }

        async fn run(
            &self,
            ctx: &ToolContext,
            call: &ToolCall,
        ) -> crate::error::Result<crate::tool::ToolOutput> {
            let path = call.str_arg("path")?;
            let content = call.str_arg("content")?;
            std::fs::write(ctx.sandbox.root().join(&path), &content)?;
            let diff = crate::diff::FileDiff::compute(path.clone(), "", &content);
            Ok(crate::tool::ToolOutput::text("wrote").with_display(ToolDisplay::Diff {
                diff,
                apply_path: path,
                new_content: content,
                previous: None,
            }))
        }
    }

    #[tokio::test]
    async fn cancelling_before_a_tool_runs_discards_the_live_preview() {
        let dir = tempfile::tempdir().unwrap();
        let original = "<html></html>\n";
        std::fs::write(dir.path().join("index.html"), original).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let provider = Arc::new(ScriptedProvider::new(vec![vec![
            StreamEvent::ToolCallDelta {
                index: 0,
                id: Some("call_1".to_string()),
                name: Some("write_file".to_string()),
                arguments_delta:
                    "{\"path\":\"index.html\",\"content\":\"<html><footer>hi</footer></html>\"}"
                        .to_string(),
            },
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
            },
        ]]));
        let handle = AgentHandle::new();
        let inner = Arc::new(RecordingSink::new());
        let sink = Arc::new(CancelOnStreamEnd {
            inner: inner.clone(),
            handle: handle.clone(),
        });
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(FakeWriteTool));
        let mut options = AgentOptions::default();
        options.model_name = "scripted".to_string();
        options.model_label = "scripted".to_string();
        options.tools_enabled = true;
        options.generate_suggestions = false;
        let mut agent = AgentLoop::new(
            provider,
            registry,
            ToolContext::new(sandbox),
            sink,
            Arc::new(AutoApprover),
            PermissionEngine::new(PermissionMode::FullAuto, PermissionRules::default()),
            options,
        )
        .with_handle(handle);
        agent
            .send("add a footer to index.html".to_string(), Vec::new())
            .await
            .unwrap();

        // The tool never ran, so the file on disk is untouched.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("index.html")).unwrap(),
            original
        );
        assert!(inner.of_kind("DiffProposed").is_empty());
        let ended = inner.of_kind("EditorStreamEnded");
        assert_eq!(ended.len(), 2, "{ended:?}");
        assert!(matches!(
            &ended[0],
            AgentEvent::EditorStreamEnded { completed: true, .. }
        ));
        assert!(matches!(
            &ended[1],
            AgentEvent::EditorStreamEnded {
                completed: false,
                ..
            }
        ));
        assert!(inner.of_kind("Done").iter().any(|event| matches!(
            event,
            AgentEvent::Done {
                reason: DoneReason::Cancelled
            }
        )));
    }

    #[tokio::test]
    async fn streams_file_edit_arguments_into_editor_events() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<html></html>\n").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let provider = Arc::new(ScriptedProvider::new(vec![
            vec![
                StreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("call_1".to_string()),
                    name: Some("write_file".to_string()),
                    arguments_delta: "{\"path\":\"index.html\",\"content\":\"<html>".to_string(),
                },
                StreamEvent::ToolCallDelta {
                    index: 0,
                    id: None,
                    name: None,
                    arguments_delta: "<footer>hi</footer></html>\\n\"}".to_string(),
                },
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                },
            ],
            say("Done."),
        ]));
        let sink = Arc::new(RecordingSink::new());
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(FakeWriteTool));
        let mut options = AgentOptions::default();
        options.model_name = "scripted".to_string();
        options.model_label = "scripted".to_string();
        options.tools_enabled = true;
        options.generate_suggestions = false;
        let mut agent = AgentLoop::new(
            provider,
            registry,
            ToolContext::new(sandbox),
            sink.clone(),
            Arc::new(AutoApprover),
            PermissionEngine::new(PermissionMode::FullAuto, PermissionRules::default()),
            options,
        );
        agent
            .send("add a footer to index.html".to_string(), Vec::new())
            .await
            .unwrap();

        let started = sink.of_kind("EditorStreamStarted");
        assert_eq!(started.len(), 1, "{started:?}");
        match &started[0] {
            AgentEvent::EditorStreamStarted { path, new_file, .. } => {
                assert_eq!(path, "index.html");
                assert!(!new_file);
            }
            other => panic!("expected a start event, got {other:?}"),
        }

        let mut content = String::new();
        for event in sink.of_kind("EditorStreamDelta") {
            if let AgentEvent::EditorStreamDelta { field, text, .. } = event {
                if field == EditorStreamField::Content {
                    content.push_str(&text);
                }
            }
        }
        assert!(content.contains("<footer>hi</footer>"), "{content}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("index.html")).unwrap(),
            "<html><footer>hi</footer></html>\n"
        );
        let ended = sink.of_kind("EditorStreamEnded");
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert!(matches!(
            &ended[0],
            AgentEvent::EditorStreamEnded { completed: true, .. }
        ));
        assert!(sink.streamed_text().contains("Done."));
        assert!(
            !sink.streamed_text().contains("<footer>"),
            "the file contents must never appear in the chat stream"
        );
    }

    #[test]
    fn detects_promises_but_not_sign_offs() {
        assert!(looks_unfinished("Let me start by reading the file"));
        assert!(looks_unfinished("I will update the config."));
        assert!(looks_unfinished("Now I'll check the tests:"));
        assert!(!looks_unfinished("Let me know if you need anything else."));
        assert!(!looks_unfinished("The fix is in place and the tests pass."));
        assert!(!looks_unfinished(""));
    }

    #[tokio::test]
    async fn continues_after_a_promise_instead_of_stopping() {
        let (mut agent, sink, provider) = agent(vec![
            say("Let me check the failing tests."),
            say("The failing tests now pass."),
        ]);
        agent.send("fix the tests".to_string(), Vec::new()).await.unwrap();

        assert_eq!(provider.calls(), 2, "the loop should have sent a nudge");
        let nudges = agent
            .history()
            .iter()
            .filter(|message| message.content == CONTINUATION_PROMPT)
            .count();
        assert_eq!(nudges, 1);
        assert!(
            sink.of_kind("Notice").is_empty(),
            "a successful continuation should be silent"
        );
        let done = sink.of_kind("Done");
        assert!(matches!(
            done.last(),
            Some(AgentEvent::Done {
                reason: DoneReason::Completed
            })
        ));
    }

    #[tokio::test]
    async fn a_clean_stream_end_is_not_truncation() {
        let (mut agent, _sink, provider) = agent(vec![
            vec![StreamEvent::TextDelta("I fixed the bug.".to_string())],
            say("This second turn should never be needed."),
        ]);
        agent.send("fix the bug".to_string(), Vec::new()).await.unwrap();
        assert_eq!(
            provider.calls(),
            1,
            "providers that end the stream with [DONE] are not truncated"
        );
    }

    #[tokio::test]
    async fn continues_when_the_model_hits_its_output_limit() {
        let (mut agent, _sink, provider) = agent(vec![
            vec![
                StreamEvent::TextDelta("fn main() {\n    // half a function".to_string()),
                StreamEvent::Done {
                    finish_reason: FinishReason::Length,
                },
            ],
            say("```\n}\n```\nThe fix is complete."),
        ]);
        agent.send("write the function".to_string(), Vec::new()).await.unwrap();
        assert_eq!(
            provider.calls(),
            2,
            "a `length` finish must continue the reply"
        );
    }

    #[tokio::test]
    async fn continues_after_an_empty_reply() {
        let (mut agent, _sink, provider) = agent(vec![vec![], say("Here is the fix.")]);
        agent.send("fix it".to_string(), Vec::new()).await.unwrap();
        assert_eq!(provider.calls(), 2);
        assert_eq!(
            agent
                .history()
                .iter()
                .filter(|message| message.content.contains(CONTINUATION_PROMPT))
                .count(),
            1,
            "an empty reply is merged into the pending user turn"
        );
    }

    #[tokio::test]
    async fn stops_after_the_continuation_budget() {
        let script: Vec<Vec<StreamEvent>> = (0..(MAX_AUTO_CONTINUATIONS + 1))
            .map(|_| say("Let me check one more file."))
            .collect();
        let (mut agent, sink, provider) = agent(script);
        agent.send("finish the task".to_string(), Vec::new()).await.unwrap();

        assert_eq!(
            provider.calls(),
            (MAX_AUTO_CONTINUATIONS + 1) as usize,
            "the loop must not nudge forever"
        );
        assert_eq!(
            agent
                .history()
                .iter()
                .filter(|message| message.content == CONTINUATION_PROMPT)
                .count(),
            MAX_AUTO_CONTINUATIONS as usize
        );
        let notices: Vec<String> = sink
            .of_kind("Notice")
            .into_iter()
            .filter_map(|event| match event {
                AgentEvent::Notice { message } => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            notices
                .iter()
                .any(|message| message.contains("automatic continuations")),
            "the user should get a clear stopping notice: {notices:?}"
        );
    }
}
