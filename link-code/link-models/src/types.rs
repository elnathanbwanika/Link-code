//! The normalised wire format.
//!
//! Every provider adapter converts its native JSON into these types on the way
//! in and out. That is what lets the agent loop be oblivious to whether it is
//! talking to Ollama, an OpenAI-compatible server, Anthropic or Gemini.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Chat role, named as the OpenAI API names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// An image pasted or dropped into the agent input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    pub mime_type: String,
    /// Base64 without a data-URL prefix.
    pub data_base64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ImageAttachment {
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime_type, self.data_base64)
    }
}

/// A tool call as requested by the model.
///
/// `arguments` stays a *string* on purpose: providers stream it in fragments and
/// weaker models emit trailing commas or unclosed braces. Parsing is deferred
/// to [`ToolCallRequest::parsed_arguments`] and is deliberately forgiving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRequest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

impl ToolCallRequest {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    /// Parse the argument JSON, repairing the usual model mistakes.
    /// Returns `None` when the payload is hopeless, so the caller can ask the
    /// model to try again instead of crashing.
    pub fn parsed_arguments(&self) -> Option<Value> {
        let trimmed = self.arguments.trim();
        if trimmed.is_empty() {
            return Some(Value::Object(Default::default()));
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            return Some(value);
        }
        repair_json(trimmed).and_then(|repaired| serde_json::from_str::<Value>(&repaired).ok())
    }
}

/// Best-effort repairs for slightly-broken model JSON.
///
/// Handles the three failures seen most often in the wild:
/// * a trailing comma before `}` or `]`
/// * truncated output missing closing braces/brackets
/// * a JSON object wrapped in a Markdown code fence
pub fn repair_json(input: &str) -> Option<String> {
    let mut text = input.trim();
    // Strip a markdown fence if the model wrapped its arguments.
    if let Some(start) = text.find('{') {
        if text.starts_with("```") {
            text = &text[start..];
        }
    }
    let text = text.trim_end_matches("```").trim();
    let mut out = String::with_capacity(text.len() + 8);
    let bytes: Vec<char> = text.chars().collect();
    let mut in_string = false;
    let mut escaped = false;
    let mut depth: i32 = 0;
    let mut last_significant = ' ';
    for (index, ch) in bytes.iter().enumerate() {
        if in_string {
            out.push(*ch);
            if escaped {
                escaped = false;
            } else if *ch == '\\' {
                escaped = true;
            } else if *ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                out.push(*ch);
            }
            '{' | '[' => {
                depth += 1;
                out.push(*ch);
            }
            '}' | ']' => {
                // Drop an offending trailing comma.
                if last_significant == ',' {
                    out.pop();
                }
                depth -= 1;
                out.push(*ch);
            }
            ',' => out.push(*ch),
            c if c.is_whitespace() => out.push(*ch),
            _ => out.push(*ch),
        }
        if !ch.is_whitespace() {
            last_significant = *ch;
        }
        // Guard against pathological input.
        if index > 1_000_000 {
            return None;
        }
    }
    if in_string {
        out.push('"');
    }
    while depth > 0 {
        out.push('}');
        depth -= 1;
    }
    Some(out)
}

/// One message in the conversation sent to a provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageAttachment>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::plain(Role::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::plain(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::plain(Role::Assistant, content)
    }

    pub fn tool_result(call_id: impl Into<String>, name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            name: Some(name.into()),
            images: Vec::new(),
        }
    }

    fn plain(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            images: Vec::new(),
        }
    }

    pub fn with_images(mut self, images: Vec<ImageAttachment>) -> Self {
        self.images = images;
        self
    }

    /// True for an assistant turn that asked for tools and has no prose.
    pub fn is_tool_call_only(&self) -> bool {
        self.role == Role::Assistant && self.content.trim().is_empty() && !self.tool_calls.is_empty()
    }
}

/// What a model configuration says it can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    pub streaming: bool,
    pub tools: bool,
    pub vision: bool,
    pub reasoning: bool,
    /// Fill-in-the-middle completion support (inline ghost text).
    pub fim: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            streaming: true,
            tools: true,
            vision: false,
            reasoning: false,
            fim: false,
        }
    }
}

impl Capabilities {
    /// Labels for the chips on a model card.
    pub fn chips(&self) -> Vec<&'static str> {
        let mut chips = Vec::new();
        if self.tools {
            chips.push("Tools");
        }
        if self.streaming {
            chips.push("Streaming");
        }
        if self.vision {
            chips.push("Vision");
        }
        if self.reasoning {
            chips.push("Reasoning");
        }
        if self.fim {
            chips.push("FIM");
        }
        chips
    }
}

/// Token accounting reported by a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
}

impl Usage {
    pub fn total(&self) -> usize {
        self.prompt_tokens + self.completion_tokens
    }

    /// Decode speed, in tokens per second, over a wall-clock duration.
    pub fn tokens_per_second(&self, elapsed: std::time::Duration) -> f64 {
        let seconds = elapsed.as_secs_f64();
        if seconds <= 0.0 {
            0.0
        } else {
            self.completion_tokens as f64 / seconds
        }
    }
}

/// Why generation stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other(String),
}

impl FinishReason {
    pub fn from_wire(value: &str) -> Self {
        match value {
            "stop" | "end_turn" | "STOP" => FinishReason::Stop,
            "length" | "max_tokens" | "MAX_TOKENS" => FinishReason::Length,
            "tool_calls" | "tool_use" | "function_call" => FinishReason::ToolCalls,
            "content_filter" | "safety" => FinishReason::ContentFilter,
            other => FinishReason::Other(other.to_string()),
        }
    }
}

/// A normalised streaming event.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// Visible assistant text.
    TextDelta(String),
    /// Reasoning / `<think>` content, rendered in a thinking block.
    ThinkingDelta(String),
    /// A fragment of a tool call. Fragments sharing an `index` belong together.
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: String,
    },
    Usage(Usage),
    Done { finish_reason: FinishReason },
}

/// A request to generate a completion.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    /// Provider-specific model id.
    pub model: String,
    pub messages: Vec<ChatMessage>,
    /// System prompt, kept separate so adapters can place it correctly
    /// (Anthropic uses a top-level `system` field; Gemini uses
    /// `systemInstruction`).
    pub system: Option<String>,
    /// OpenAI-shaped tool schemas. Empty when tools are disabled or the model
    /// has no native tool calling (the text protocol is used instead).
    pub tools: Vec<Value>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
    /// Extra top-level JSON merged into the request body verbatim.
    pub extra_body: Option<Value>,
    /// Extra HTTP headers (a model's custom `X-` headers, for example).
    pub extra_headers: Vec<(String, String)>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: Vec::new(),
            system: None,
            tools: Vec::new(),
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop: Vec::new(),
            extra_body: None,
            extra_headers: Vec::new(),
        }
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_messages(mut self, messages: Vec<ChatMessage>) -> Self {
        self.messages = messages;
        self
    }

    pub fn with_tools(mut self, tools: Vec<Value>) -> Self {
        self.tools = tools;
        self
    }

    /// Rough estimate of the prompt size, used before the provider replies.
    pub fn estimated_prompt_tokens(&self) -> usize {
        let system = self.system.as_deref().unwrap_or("");
        let messages: usize = self
            .messages
            .iter()
            .map(|message| {
                let arguments: usize = message.tool_calls.iter().map(|c| c.arguments.len()).sum();
                message.content.chars().count() + arguments
            })
            .sum();
        ((system.chars().count() + messages) / 4) + self.messages.len() * 4
    }
}

/// A model discovered from an endpoint, shown by "Fetch models".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter_count: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
}

impl ModelInfo {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            context_window: None,
            parameter_count: None,
            size_bytes: None,
            modified_at: None,
        }
    }

    /// `qwen2.5-coder:32b · 19.9 GB`
    pub fn display(&self) -> String {
        match self.size_bytes {
            Some(bytes) => format!("{} · {}", self.id, human_bytes(bytes)),
            None => self.name.clone().unwrap_or_else(|| self.id.clone()),
        }
    }
}

/// `19_900_000_000` -> `19.9 GB`
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}
