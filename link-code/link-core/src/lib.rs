//! # link-core
//!
//! The UI-independent heart of **Link Code**.
//!
//! `link-core` owns everything that is not GTK: the sandbox, the permission
//! engine, unified-diff handling, checkpoints, the session store, the context
//! meter, the tool runtime and — above all — the [`AgentLoop`] that drives a
//! conversation with a model and executes tool calls against the user's
//! workspace.
//!
//! The crate is deliberately free of any UI dependency, so the whole agent can
//! be unit tested on a headless machine and embedded in other front-ends.

pub mod agent;
pub mod checkpoint;
pub mod config;
pub mod context;
pub mod diff;
pub mod error;
pub mod event;
pub mod paths;
pub mod permission;
pub mod project;
pub mod secrets;
pub mod session;
pub mod tool;

pub use agent::{AgentHandle, AgentLoop, AgentMessage, AgentOptions, Approver};
pub use link_models::proxy;
pub use link_models::{ProxyProtocol, ProxySettings};
pub use checkpoint::{Checkpoint, CheckpointEntry, CheckpointStore};
pub use config::{Config, ConfigStore, ThinkingDisplay};
pub use context::{human_tokens, ContextBreakdown, ContextMeter, ContextPart};
pub use diff::{apply_edit, DiffHunk, DiffLine, DiffLineKind, FileDiff};
pub use error::{LinkError, Result};
pub use event::{
    AgentEvent, ChannelSink, DoneReason, ErrorKind, EventSink, NoopSink, RecordingSink,
    SuggestionSource, TodoItem, TodoStatus, ToolStatus,
};
pub use paths::{is_secret_path, Sandbox};
pub use permission::{
    analyze_command, bash_pattern, request_for_bash, request_for_write, ActionKind, AllowScope,
    CommandAnalysis, Decision, PermissionDecision, PermissionEngine, PermissionMode,
    PermissionRequest, PermissionRules, RiskLevel,
};
pub use project::{CustomCommand, ProjectMemory, WorkspaceSettings};
pub use secrets::{default_store, MemoryStore, SecretStore};
pub use session::{Session, SessionStore, SessionSummary, StoredMessage, StoredRole, Turn};
pub use tool::{
    Tool, ToolCall, ToolContext, ToolDisplay, ToolOutput, ToolRegistry, ToolStream, UserAsker,
};

/// Version of the on-disk data formats produced by this build.
pub const DATA_FORMAT_VERSION: u32 = 1;

/// Application identity, used for `~/.config` and `~/.local/share` paths.
pub const APP_ID: &str = "link-code";
