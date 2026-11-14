//! The tool runtime.
//!
//! `link-core` owns the [`Tool`] trait and the [`ToolRegistry`]; the concrete
//! implementations live in `link-tools`. That direction keeps the dependency
//! graph acyclic: `link-tools` depends on `link-core`, never the other way
//! round, and the app wires the registry together at start-up.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::diff::FileDiff;
use crate::error::{LinkError, Result};
use crate::event::TodoItem;
use crate::paths::Sandbox;
use crate::permission::{ActionKind, PermissionRequest};

/// One tool invocation requested by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned id, echoed back with the result.
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    /// Convenience accessor that turns a missing/wrongly-typed argument into a
    /// clean, model-readable error.
    pub fn str_arg(&self, key: &str) -> Result<String> {
        self.arguments
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| LinkError::InvalidArgs {
                tool: self.name.clone(),
                message: format!("missing or non-string argument `{key}`"),
            })
    }

    pub fn opt_str_arg(&self, key: &str) -> Option<String> {
        self.arguments
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    pub fn u64_arg(&self, key: &str) -> Option<u64> {
        self.arguments.get(key).and_then(Value::as_u64)
    }

    pub fn bool_arg(&self, key: &str) -> Option<bool> {
        self.arguments.get(key).and_then(Value::as_bool)
    }

    /// Arguments that arrive as a JSON string (common with weaker models) are
    /// transparently re-parsed. Returns `self` unchanged otherwise.
    pub fn normalised(&self) -> Self {
        if let Value::String(raw) = &self.arguments {
            if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
                if parsed.is_object() {
                    return Self {
                        id: self.id.clone(),
                        name: self.name.clone(),
                        arguments: parsed,
                    };
                }
            }
        }
        self.clone()
    }
}

/// Extra structured data the UI wants in order to render a rich card.
#[derive(Debug, Clone)]
pub enum ToolDisplay {
    None,
    /// Plain text shown in the expanded card body.
    Text(String),
    /// A diff card, with Accept/Reject when `applied` is false.
    Diff {
        diff: FileDiff,
        apply_path: String,
        /// New file contents (already written to disk when `applied` is true).
        new_content: String,
        /// Contents before the edit; `None` means the file did not exist, so
        /// rejecting removes it.
        previous: Option<String>,
    },
    /// A bash card: command, cwd, output, exit code, duration.
    Command {
        command: String,
        cwd: String,
        exit_code: Option<i32>,
        duration_ms: u64,
        output: String,
    },
    /// A todo checklist (from `update_tasks`).
    Todos(Vec<TodoItem>),
    /// A question for the user, with tappable options.
    Question { question: String, options: Vec<String> },
}

/// What a tool returns.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// Text fed back to the model as the tool result.
    pub content: String,
    /// True when `content` was trimmed to fit `tool_output_cap`.
    pub truncated: bool,
    pub display: ToolDisplay,
}

impl ToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            truncated: false,
            display: ToolDisplay::None,
        }
    }

    pub fn with_display(mut self, display: ToolDisplay) -> Self {
        self.display = display;
        self
    }

    pub fn truncate_to(mut self, cap: usize) -> Self {
        if self.content.len() > cap {
            let mut end = cap;
            while end > 0 && !self.content.is_char_boundary(end) {
                end -= 1;
            }
            let dropped = self.content.len() - end;
            self.content.truncate(end);
            self.content.push_str(&format!(
                "\n\n[... {dropped} bytes of output truncated by Link Code ...]"
            ));
            self.truncated = true;
        }
        self
    }
}

/// Sink used by tools that produce incremental output (bash, large writes).
/// The UI implements this to append into the tool card live.
pub trait ToolStream: Send + Sync {
    fn chunk(&self, call_id: &str, text: &str);
}

/// Lets `ask_user` put a question to the human and wait for the answer.
pub trait UserAsker: Send + Sync {
    fn ask(
        &self,
        call_id: String,
        question: String,
        options: Vec<String>,
    ) -> BoxFuture<'static, Option<String>>;
}

/// Everything a tool is allowed to touch.
#[derive(Clone)]
pub struct ToolContext {
    /// The workspace sandbox. Every path a tool touches goes through this.
    pub sandbox: Sandbox,
    pub shell_timeout: Duration,
    pub output_cap: usize,
    /// Redact secret-looking values out of results before they reach the model.
    pub protect_secrets: bool,
    /// Shared todo list, so `update_tasks` and the UI tracker agree.
    pub todos: Arc<Mutex<Vec<TodoItem>>>,
    /// Cancelled by the Stop button.
    pub cancel: CancellationToken,
    pub stream: Option<Arc<dyn ToolStream>>,
    pub asker: Option<Arc<dyn UserAsker>>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("root", &self.sandbox.root())
            .field("shell_timeout", &self.shell_timeout)
            .field("output_cap", &self.output_cap)
            .field("protect_secrets", &self.protect_secrets)
            .finish_non_exhaustive()
    }
}

impl ToolContext {
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox,
            shell_timeout: Duration::from_secs(120),
            output_cap: 128 * 1024,
            protect_secrets: true,
            todos: Arc::new(Mutex::new(Vec::new())),
            cancel: CancellationToken::new(),
            stream: None,
            asker: None,
        }
    }

    pub fn with_shell_timeout(mut self, timeout: Duration) -> Self {
        self.shell_timeout = timeout;
        self
    }

    pub fn with_output_cap(mut self, cap: usize) -> Self {
        self.output_cap = cap;
        self
    }

    pub fn with_stream(mut self, stream: Arc<dyn ToolStream>) -> Self {
        self.stream = Some(stream);
        self
    }

    pub fn with_asker(mut self, asker: Arc<dyn UserAsker>) -> Self {
        self.asker = Some(asker);
        self
    }

    pub fn cancellation(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Emit incremental output if a stream is attached.
    pub fn emit(&self, call_id: &str, text: &str) {
        if let Some(stream) = &self.stream {
            stream.chunk(call_id, text);
        }
    }

    pub fn todo_snapshot(&self) -> Vec<TodoItem> {
        self.todos.lock().expect("todo mutex poisoned").clone()
    }

    pub fn replace_todos(&self, items: Vec<TodoItem>) {
        *self.todos.lock().expect("todo mutex poisoned") = items;
    }
}

/// A capability the model can invoke.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Wire name, e.g. `read_file`. Must be unique in the registry.
    fn name(&self) -> &'static str;

    /// Description shown to the model (and in the tools list in Settings).
    fn description(&self) -> &'static str;

    /// JSON Schema for the arguments object, in OpenAI `function.parameters`
    /// shape. Used verbatim for native tool calling and for the text fallback
    /// protocol.
    fn parameters(&self) -> Value;

    /// Side effect class, used by the permission engine.
    fn kind(&self) -> ActionKind;

    /// One-line summary for the collapsed tool card, before execution.
    fn summarise(&self, call: &ToolCall) -> String;

    /// The approval this call would require. Read-only tools still return a
    /// request; the engine auto-approves them.
    fn permission_request(&self, ctx: &ToolContext, call: &ToolCall) -> PermissionRequest {
        PermissionRequest::new(self.name(), self.kind(), self.summarise(call))
            .with_pattern(self.summarise(call))
            .with_cwd(ctx.sandbox.root())
    }

    /// Execute. Errors are fed back to the model as tool errors, not crashes.
    async fn run(&self, ctx: &ToolContext, call: &ToolCall) -> Result<ToolOutput>;
}

/// The set of tools available to the agent loop.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    /// Register a tool. Registering the same name twice replaces the first,
    /// which keeps hot-reload of user tools predictable.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.retain(|existing| existing.name() != tool.name());
        self.tools.push(tool);
        self.tools.sort_by_key(|tool| tool.name());
    }

    pub fn with(mut self, tool: Arc<dyn Tool>) -> Self {
        self.register(tool);
        self
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(Arc::clone)
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|tool| tool.name()).collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.iter()
    }

    /// Tools in OpenAI `tools` array format, for providers with native tool
    /// calling.
    pub fn openai_tool_schemas(&self) -> Vec<Value> {
        self.tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name(),
                        "description": tool.description(),
                        "parameters": tool.parameters(),
                    }
                })
            })
            .collect()
    }

    /// Human-readable catalogue for the text fallback protocol and for the
    /// system prompt (so models that ignore schemas still know what exists).
    pub fn describe_all(&self) -> String {
        let mut out = String::new();
        for tool in &self.tools {
            out.push_str(&format!("### {}\n{}\n", tool.name(), tool.description()));
            let params = tool.parameters();
            if let Some(properties) = params.get("properties").and_then(Value::as_object) {
                let required: Vec<&str> = params
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|array| array.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                for (name, schema) in properties {
                    let kind = schema
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("any");
                    let marker = if required.contains(&name.as_str()) { " (required)" } else { "" };
                    let description = schema
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    out.push_str(&format!("  - {name}: {kind}{marker} — {description}\n"));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn string_arguments_are_reparsed() {
        let call = ToolCall::new("1", "read_file", json!("{\"path\": \"src/main.rs\"}"));
        let normalised = call.normalised();
        assert_eq!(normalised.str_arg("path").unwrap(), "src/main.rs");

        let already_object = ToolCall::new("2", "read_file", json!({"path": "a"}));
        assert_eq!(already_object.normalised().arguments, json!({"path": "a"}));
    }

    #[test]
    fn missing_arguments_produce_readable_errors() {
        let call = ToolCall::new("1", "read_file", json!({}));
        let error = call.str_arg("path").unwrap_err();
        assert!(error.to_string().contains("read_file"));
        assert!(error.to_string().contains("path"));
    }
}
