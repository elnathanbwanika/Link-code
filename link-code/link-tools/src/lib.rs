//! # link-tools
//!
//! The tools the agent can call. Every tool goes through
//! [`link_core::paths::Sandbox`] for path resolution and returns a
//! [`link_core::permission::PermissionRequest`] so the permission engine — not
//! the tool, and certainly not the model — decides whether it may run.
//!
//! Tools never panic on bad input: a missing argument, an unreadable file or a
//! failing command becomes a `ToolOutput` describing the problem, which the
//! model can act on.

pub mod fs_tools;
pub mod misc;
pub mod search;
pub mod shell;

use std::sync::Arc;

use link_core::tool::{Tool, ToolRegistry};

pub use fs_tools::{
    CreateFileTool, EditFileTool, ListDirTool, MultiEditTool, ReadFileTool, WriteFileTool,
};
pub use misc::{AskUserTool, UpdateTasksTool, WebFetchTool};
pub use search::{GlobTool, GrepTool};
pub use shell::BashTool;

/// Every built-in tool name, matching the registry contents.
pub const BUILTIN_TOOL_NAMES: &[&str] = &[
    "read_file",
    "write_file",
    "create_file",
    "edit_file",
    "multi_edit",
    "list_dir",
    "glob",
    "grep",
    "bash",
    "update_tasks",
    "web_fetch",
    "ask_user",
];

/// Build the standard registry.
///
/// `enable_web` is wired to Settings → Agent → "Enable web tools", which is off
/// by default: Link Code makes no network calls the user did not configure.
pub fn builtin_registry(enable_web: bool) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadFileTool));
    registry.register(Arc::new(WriteFileTool));
    registry.register(Arc::new(CreateFileTool));
    registry.register(Arc::new(EditFileTool));
    registry.register(Arc::new(MultiEditTool));
    registry.register(Arc::new(ListDirTool));
    registry.register(Arc::new(GlobTool));
    registry.register(Arc::new(GrepTool));
    registry.register(Arc::new(BashTool));
    registry.register(Arc::new(UpdateTasksTool));
    registry.register(Arc::new(AskUserTool));
    if enable_web {
        registry.register(Arc::new(WebFetchTool));
    }
    registry
}

/// Register every built-in, including the optional web tool.
pub fn registry_with_tools(tools: Vec<Arc<dyn Tool>>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for tool in tools {
        registry.register(tool);
    }
    registry
}
