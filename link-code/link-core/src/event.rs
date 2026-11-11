//! Typed events emitted by the agent loop and rendered by the UI.
//!
//! The agent loop is UI-independent: it never touches a widget. It pushes
//! [`AgentEvent`]s into an [`EventSink`], and `link-ui` translates them into
//! blocks in the agent panel. This is what keeps the GTK main thread free and
//! what makes the whole loop unit-testable with a recording sink.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::diff::FileDiff;
use crate::permission::PermissionRequest;

/// Status chip shown on a tool-call card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    Running,
    Done,
    Failed,
    Denied,
}

impl ToolStatus {
    pub fn label(self) -> &'static str {
        match self {
            ToolStatus::Pending => "Pending",
            ToolStatus::Running => "Running",
            ToolStatus::Done => "Done",
            ToolStatus::Failed => "Failed",
            ToolStatus::Denied => "Denied",
        }
    }

    pub fn css_class(self) -> &'static str {
        match self {
            ToolStatus::Pending => "chip-pending",
            ToolStatus::Running => "chip-running",
            ToolStatus::Done => "chip-done",
            ToolStatus::Failed => "chip-failed",
            ToolStatus::Denied => "chip-denied",
        }
    }
}

/// One line of the agent's live todo checklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub text: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

impl TodoStatus {
    pub fn marker(self) -> &'static str {
        match self {
            TodoStatus::Pending => "\u{25cb}",
            TodoStatus::InProgress => "\u{25d0}",
            TodoStatus::Done => "\u{25cf}",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in progress",
            TodoStatus::Done => "done",
        }
    }
}

/// Why a turn ended. Drives the footer text and whether we auto-continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoneReason {
    /// The model produced a final answer with no tool calls.
    Completed,
    /// The tool-iteration cap was reached (see `AgentOptions::max_iterations`).
    IterationLimit,
    /// The user pressed Stop / `Esc`.
    Cancelled,
    /// An error ended the turn.
    Errored,
}

/// Plain-language error categories shown on red error blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Endpoint unreachable (connection refused, DNS failure, TLS error).
    Connection,
    /// HTTP 401/403 — bad or missing API key.
    Auth,
    /// HTTP 429 — rate limited.
    RateLimit,
    /// The prompt no longer fits in the model's context window.
    ContextOverflow,
    /// The model returned something the parser could not make sense of.
    MalformedToolCall,
    /// The model isn't configured, or no model was selected for the role.
    NoModel,
    /// Anything else.
    Other,
}

impl ErrorKind {
    /// One-line plain-language cause, shown above the raw detail.
    pub fn headline(self) -> &'static str {
        match self {
            ErrorKind::Connection => "Could not reach the model endpoint",
            ErrorKind::Auth => "The endpoint rejected the API key",
            ErrorKind::RateLimit => "Rate limited by the provider",
            ErrorKind::ContextOverflow => "The conversation no longer fits in the context window",
            ErrorKind::MalformedToolCall => "The model produced a malformed tool call",
            ErrorKind::NoModel => "No model is configured for this role",
            ErrorKind::Other => "Something went wrong",
        }
    }
}

/// Where a suggestion came from — logged in the Suggestions tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionSource {
    NextStepChip,
    InlineCompletion,
    Autocomplete,
    SmartStrip,
    SlashCommand,
}

impl SuggestionSource {
    pub fn label(self) -> &'static str {
        match self {
            SuggestionSource::NextStepChip => "Next-step chip",
            SuggestionSource::InlineCompletion => "Inline completion",
            SuggestionSource::Autocomplete => "Autocomplete",
            SuggestionSource::SmartStrip => "Smart strip",
            SuggestionSource::SlashCommand => "Slash command",
        }
    }
}

/// Every observable step of a turn.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    // ---- turn lifecycle -------------------------------------------------
    TurnStarted { turn_id: Uuid, model: String },
    TurnFinished { turn_id: Uuid, reason: DoneReason },

    // ---- streaming text -------------------------------------------------
    /// Emitted before the first delta so the UI can create the block.
    BlockStarted { block_id: Uuid },
    TokenDelta { block_id: Uuid, text: String },
    ThinkingDelta { block_id: Uuid, text: String },
    /// Used by adapters that cannot stream, and to reconcile the final text.
    TextComplete { block_id: Uuid, text: String },

    // ---- tool lifecycle -------------------------------------------------
    ToolRequested {
        block_id: Uuid,
        call_id: String,
        tool: String,
        /// One-line, human-readable summary, e.g. ``read `src/main.rs` (lines 1-120)``.
        summary: String,
        args: serde_json::Value,
    },
    ToolStatusChanged {
        call_id: String,
        status: ToolStatus,
    },
    /// Live stdout/stderr for `bash`, and streamed content for large writes.
    ToolOutputDelta { call_id: String, chunk: String },
    ToolResult {
        call_id: String,
        tool: String,
        ok: bool,
        /// Result text fed back to the model (already truncated/capped).
        content: String,
        duration_ms: u64,
        truncated: bool,
    },

    // ---- edits ----------------------------------------------------------
    DiffProposed {
        block_id: Uuid,
        call_id: String,
        diff: FileDiff,
        /// `false` while waiting for Accept/Reject.
        applied: bool,
    },

    // ---- permissions ----------------------------------------------------
    ApprovalRequested {
        block_id: Uuid,
        call_id: String,
        request: Box<PermissionRequest>,
    },
    ApprovalResolved {
        call_id: String,
        allowed: bool,
        scope: Option<crate::permission::AllowScope>,
        feedback: Option<String>,
    },

    // ---- plan / todos ---------------------------------------------------
    PlanProposed { block_id: Uuid, markdown: String },
    TodosUpdated { items: Vec<TodoItem> },

    // ---- suggestions ----------------------------------------------------
    SuggestionShown {
        block_id: Uuid,
        source: SuggestionSource,
        text: String,
    },
    SuggestionsReady { block_id: Uuid, items: Vec<String> },
    SuggestionAccepted { text: String },

    // ---- context / usage ------------------------------------------------
    ContextUpdated {
        used_tokens: usize,
        max_tokens: usize,
    },
    Usage {
        block_id: Uuid,
        prompt_tokens: usize,
        completion_tokens: usize,
        tokens_per_second: f64,
        latency_ms: u64,
        cost_usd: f64,
    },

    // ---- misc -----------------------------------------------------------
    /// Dim one-line note: "Context compacted", "Checkpoint created", …
    Notice { message: String },
    ModelChanged { model: String },
    Error {
        block_id: Option<Uuid>,
        kind: ErrorKind,
        message: String,
        detail: Option<String>,
    },
    Done { reason: DoneReason },
}

impl AgentEvent {
    /// True when this event ends the turn.
    pub fn is_terminal(&self) -> bool {
        matches!(self, AgentEvent::Done { .. })
    }

    /// Short label used in logs and the Debug output panel.
    pub fn kind(&self) -> &'static str {
        match self {
            AgentEvent::TurnStarted { .. } => "TurnStarted",
            AgentEvent::TurnFinished { .. } => "TurnFinished",
            AgentEvent::BlockStarted { .. } => "BlockStarted",
            AgentEvent::TokenDelta { .. } => "TokenDelta",
            AgentEvent::ThinkingDelta { .. } => "ThinkingDelta",
            AgentEvent::TextComplete { .. } => "TextComplete",
            AgentEvent::ToolRequested { .. } => "ToolRequested",
            AgentEvent::ToolStatusChanged { .. } => "ToolStatusChanged",
            AgentEvent::ToolOutputDelta { .. } => "ToolOutputDelta",
            AgentEvent::ToolResult { .. } => "ToolResult",
            AgentEvent::DiffProposed { .. } => "DiffProposed",
            AgentEvent::ApprovalRequested { .. } => "ApprovalRequested",
            AgentEvent::ApprovalResolved { .. } => "ApprovalResolved",
            AgentEvent::PlanProposed { .. } => "PlanProposed",
            AgentEvent::TodosUpdated { .. } => "TodosUpdated",
            AgentEvent::SuggestionShown { .. } => "SuggestionShown",
            AgentEvent::SuggestionsReady { .. } => "SuggestionsReady",
            AgentEvent::SuggestionAccepted { .. } => "SuggestionAccepted",
            AgentEvent::ContextUpdated { .. } => "ContextUpdated",
            AgentEvent::Usage { .. } => "Usage",
            AgentEvent::Notice { .. } => "Notice",
            AgentEvent::ModelChanged { .. } => "ModelChanged",
            AgentEvent::Error { .. } => "Error",
            AgentEvent::Done { .. } => "Done",
        }
    }
}

/// The one-way channel the agent loop talks through.
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: AgentEvent);
}

/// Drops every event. Useful in tests and for headless runs.
pub struct NoopSink;

impl EventSink for NoopSink {
    fn emit(&self, _event: AgentEvent) {}
}

/// Forwards events onto a tokio channel. The GTK side owns the receiver and
/// drains it on the main loop, so widget updates never happen on a worker
/// thread.
#[derive(Clone)]
pub struct ChannelSink {
    tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
}

impl ChannelSink {
    pub fn new() -> (Self, tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    /// Wrap an existing sender (e.g. to fan out to several consumers).
    pub fn from_sender(tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>) -> Self {
        Self { tx }
    }
}

impl EventSink for ChannelSink {
    fn emit(&self, event: AgentEvent) {
        // A closed receiver means the UI went away; dropping is correct.
        let _ = self.tx.send(event);
    }
}

/// Records every event in memory. Used by the core test-suite.
#[derive(Default)]
pub struct RecordingSink {
    events: std::sync::Mutex<Vec<AgentEvent>>,
}

impl RecordingSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.events.lock().expect("sink mutex poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Snapshot of everything recorded so far.
    pub fn events(&self) -> Vec<AgentEvent> {
        self.events.lock().expect("sink mutex poisoned").clone()
    }

    /// All events of a given variant, by [`AgentEvent::kind`] label.
    pub fn of_kind(&self, kind: &str) -> Vec<AgentEvent> {
        self.events()
            .into_iter()
            .filter(|event| event.kind() == kind)
            .collect()
    }

    /// Concatenate every streamed text delta — the "what the user saw" text.
    pub fn streamed_text(&self) -> String {
        let mut out = String::new();
        for event in self.events() {
            if let AgentEvent::TokenDelta { text, .. } = event {
                out.push_str(&text);
            }
        }
        out
    }
}

impl EventSink for RecordingSink {
    fn emit(&self, event: AgentEvent) {
        self.events.lock().expect("sink mutex poisoned").push(event);
    }
}
