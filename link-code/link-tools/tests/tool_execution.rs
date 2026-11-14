//! Regression tests for tool execution: new-file writes used to resolve to a
//! path with a trailing separator (EISDIR) and shell commands used to hang
//! until the timeout because the read loop never noticed both pipes had closed.

use link_core::paths::Sandbox;
use link_core::permission::PermissionRules;
use link_core::tool::{Tool, ToolCall, ToolContext};
use link_tools::{BashTool, WriteFileTool};
use serde_json::json;
use std::time::{Duration, Instant};

fn context(dir: &tempfile::TempDir) -> ToolContext {
    let sandbox = Sandbox::new(dir.path()).unwrap();
    ToolContext::new(sandbox)
}

#[tokio::test]
async fn write_file_creates_a_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(&dir);
    let call = ToolCall::new(
        "call_1",
        "write_file",
        json!({ "path": "new.txt", "content": "hello\n" }),
    );

    let output = WriteFileTool.run(&ctx, &call).await.expect("write should succeed");

    assert!(output.content.contains("Wrote new.txt"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
        "hello\n"
    );
    let _ = PermissionRules::default();
}

#[tokio::test]
async fn write_file_creates_missing_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(&dir);
    let call = ToolCall::new(
        "call_2",
        "write_file",
        json!({ "path": "src/deep/new.rs", "content": "fn main() {}\n" }),
    );

    WriteFileTool.run(&ctx, &call).await.expect("write should succeed");

    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/deep/new.rs")).unwrap(),
        "fn main() {}\n"
    );
}

#[tokio::test]
async fn bash_finishes_as_soon_as_the_command_does() {
    let dir = tempfile::tempdir().unwrap();
    // A long timeout would hide the hang: the command itself is instant.
    let ctx = context(&dir).with_shell_timeout(Duration::from_secs(120));
    let call = ToolCall::new("call_3", "bash", json!({ "command": "echo shell-ok" }));

    let started = Instant::now();
    let output = BashTool.run(&ctx, &call).await.expect("bash should run");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "bash waited for the timeout instead of returning: {elapsed:?}"
    );
    assert!(output.content.contains("shell-ok"), "got: {}", output.content);
    assert!(!output.content.contains("timeout"), "got: {}", output.content);
}
