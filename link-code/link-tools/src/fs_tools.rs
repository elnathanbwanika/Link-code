//! File tools: read, write, edit, multi-edit and directory listing.
//!
//! Every path is resolved through the workspace sandbox, so a model cannot
//! reach outside the folder the user opened even if it tries.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use link_core::diff::{apply_edit, FileDiff};
use link_core::error::{LinkError, Result};
use link_core::permission::{request_for_write, ActionKind, PermissionRequest, RiskLevel};
use link_core::tool::{Tool, ToolCall, ToolContext, ToolDisplay, ToolOutput};
use serde_json::{json, Value};

/// Build a JSON-Schema object for a tool's parameters.
pub(crate) fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// Read a file to string, refusing binary content.
pub(crate) fn read_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| LinkError::io(path, e))?;
    if bytes.contains(&0) {
        return Err(LinkError::other(format!(
            "`{}` looks like a binary file ({} bytes); refusing to read it as text",
            path.display(),
            bytes.len()
        )));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Slice of a file plus the line range that was actually returned.
fn line_range(content: &str, start: Option<u64>, end: Option<u64>) -> (String, usize, usize, usize) {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let first = start.unwrap_or(1).max(1) as usize;
    let last = end.map(|e| e as usize).unwrap_or(total).min(total).max(first);
    if first > total {
        return (String::new(), total, first, first.saturating_sub(1));
    }
    let slice = lines[first - 1..last.min(total)].join("\n");
    (slice, total, first, last.min(total))
}

// ---------------------------------------------------------------------------
// read_file
// ---------------------------------------------------------------------------

pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "Read a UTF-8 text file from the workspace. Optionally restrict to a line range. Always prefer this over `bash cat` so the output is capped and labelled."
    }

    fn parameters(&self) -> Value {
        object_schema(
            json!({
                "path": { "type": "string", "description": "Workspace-relative path, e.g. `src/main.rs`" },
                "start_line": { "type": "integer", "description": "First line to return (1-based, inclusive)" },
                "end_line": { "type": "integer", "description": "Last line to return (inclusive)" }
            }),
            &["path"],
        )
    }

    fn kind(&self) -> ActionKind {
        ActionKind::Read
    }

    fn summarise(&self, call: &ToolCall) -> String {
        let path = call.opt_str_arg("path").unwrap_or_else(|| "?".to_string());
        match (call.u64_arg("start_line"), call.u64_arg("end_line")) {
            (Some(start), Some(end)) => format!("Read `{path}` (lines {start}-{end})"),
            (Some(start), None) => format!("Read `{path}` (from line {start})"),
            _ => format!("Read `{path}`"),
        }
    }

    async fn run(&self, ctx: &ToolContext, call: &ToolCall) -> Result<ToolOutput> {
        let relative = call.str_arg("path")?;
        let path = ctx.sandbox.resolve_existing(&relative)?;
        let content = read_text(&path)?;
        let (slice, total, first, last) = line_range(
            &content,
            call.u64_arg("start_line"),
            call.u64_arg("end_line"),
        );
        let display_path = ctx.sandbox.relative(&path);
        let header = format!(
            "<file path=\"{display_path}\" total_lines=\"{total}\" shown=\"{first}-{last}\">\n"
        );
        let body = format!("{header}{slice}\n</file>");
        let output = ToolOutput::text(body).truncate_to(ctx.output_cap);
        Ok(output.with_display(ToolDisplay::Text(format!(
            "{display_path}: lines {first}-{last} of {total}"
        ))))
    }
}

// ---------------------------------------------------------------------------
// write_file
// ---------------------------------------------------------------------------

/// Resolve a write target and compute the diff against what is on disk now.
fn plan_write(
    ctx: &ToolContext,
    call: &ToolCall,
) -> Result<(PathBuf, String, Option<String>, FileDiff)> {
    let relative = call.str_arg("path")?;
    let content = call.str_arg("content")?;
    let path = ctx.sandbox.resolve(&relative)?;
    let previous = if path.is_file() {
        Some(read_text(&path).unwrap_or_default())
    } else {
        None
    };
    let display = ctx.sandbox.relative(&path);
    let diff = FileDiff::compute(display, previous.as_deref().unwrap_or(""), &content);
    Ok((path, content, previous, diff))
}

pub struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Create a file or replace its entire contents. Prefer `edit_file` for changes to an existing file. Parent directories are created automatically."
    }

    fn parameters(&self) -> Value {
        object_schema(
            json!({
                "path": { "type": "string", "description": "Workspace-relative path" },
                "content": { "type": "string", "description": "Full contents of the file" }
            }),
            &["path", "content"],
        )
    }

    fn kind(&self) -> ActionKind {
        ActionKind::Write
    }

    fn summarise(&self, call: &ToolCall) -> String {
        let path = call.opt_str_arg("path").unwrap_or_else(|| "?".to_string());
        let lines = call
            .opt_str_arg("content")
            .map(|content| content.lines().count())
            .unwrap_or(0);
        format!("Write `{path}` ({lines} lines)")
    }

    fn permission_request(&self, ctx: &ToolContext, call: &ToolCall) -> PermissionRequest {
        match plan_write(ctx, call) {
            Ok((path, _, _, diff)) => request_for_write(
                "write_file",
                &path,
                ctx.sandbox.root(),
                Some(diff.unified.clone()),
            ),
            Err(error) => PermissionRequest::new(
                "write_file",
                ActionKind::Write,
                self.summarise(call),
            )
            .with_reason(error.to_string())
            .with_risk(RiskLevel::High),
        }
    }

    async fn run(&self, ctx: &ToolContext, call: &ToolCall) -> Result<ToolOutput> {
        let (path, content, previous, diff) = plan_write(ctx, call)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LinkError::io(parent, e))?;
        }
        // Show a live "writing…" state for large new files.
        if previous.is_none() && content.len() > 8 * 1024 {
            for chunk in content.as_bytes().chunks(8 * 1024) {
                ctx.emit(&call.id, &String::from_utf8_lossy(chunk));
            }
        }
        std::fs::write(&path, content.as_bytes()).map_err(|e| LinkError::io(&path, e))?;
        let display = ctx.sandbox.relative(&path);
        let summary = if previous.is_none() {
            format!("Wrote {display} ({} lines, new file)", content.lines().count())
        } else {
            format!("Wrote {display} {}", diff.summary())
        };
        Ok(ToolOutput::text(summary).with_display(ToolDisplay::Diff {
            diff,
            apply_path: display,
            new_content: content,
            previous,
        }))
    }
}

// ---------------------------------------------------------------------------
// edit_file
// ---------------------------------------------------------------------------

/// Apply `old_string` -> `new_string` to a file on disk, returning the plan.
fn plan_edit(
    ctx: &ToolContext,
    path_arg: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<(PathBuf, String, Option<String>, FileDiff)> {
    let path = ctx.sandbox.resolve_existing(path_arg)?;
    let previous = read_text(&path)?;
    let updated = apply_edit(&previous, old_string, new_string, replace_all)?;
    let display = ctx.sandbox.relative(&path);
    let diff = FileDiff::compute(display, &previous, &updated);
    Ok((path, updated, Some(previous), diff))
}

pub struct EditFileTool;

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn description(&self) -> &'static str {
        "Replace an exact string in an existing file. `old_string` must appear exactly once unless `replace_all` is true; include surrounding lines to make it unique."
    }

    fn parameters(&self) -> Value {
        object_schema(
            json!({
                "path": { "type": "string", "description": "Workspace-relative path" },
                "old_string": { "type": "string", "description": "Exact text to replace, including indentation" },
                "new_string": { "type": "string", "description": "Replacement text" },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence (default false)" }
            }),
            &["path", "old_string", "new_string"],
        )
    }

    fn kind(&self) -> ActionKind {
        ActionKind::Write
    }

    fn summarise(&self, call: &ToolCall) -> String {
        format!(
            "Edit `{}`",
            call.opt_str_arg("path").unwrap_or_else(|| "?".to_string())
        )
    }

    fn permission_request(&self, ctx: &ToolContext, call: &ToolCall) -> PermissionRequest {
        let plan = (|| -> Result<_> {
            plan_edit(
                ctx,
                &call.str_arg("path")?,
                &call.str_arg("old_string")?,
                &call.str_arg("new_string")?,
                call.bool_arg("replace_all").unwrap_or(false),
            )
        })();
        match plan {
            Ok((path, _, _, diff)) => request_for_write(
                "edit_file",
                &path,
                ctx.sandbox.root(),
                Some(diff.unified.clone()),
            ),
            Err(error) => PermissionRequest::new("edit_file", ActionKind::Write, self.summarise(call))
                .with_reason(error.to_string())
                .with_risk(RiskLevel::High),
        }
    }

    async fn run(&self, ctx: &ToolContext, call: &ToolCall) -> Result<ToolOutput> {
        let (path, updated, previous, diff) = plan_edit(
            ctx,
            &call.str_arg("path")?,
            &call.str_arg("old_string")?,
            &call.str_arg("new_string")?,
            call.bool_arg("replace_all").unwrap_or(false),
        )?;
        std::fs::write(&path, updated.as_bytes()).map_err(|e| LinkError::io(&path, e))?;
        let display = ctx.sandbox.relative(&path);
        Ok(
            ToolOutput::text(format!("Edited {display} {}", diff.summary())).with_display(
                ToolDisplay::Diff {
                    diff,
                    apply_path: display,
                    new_content: updated,
                    previous,
                },
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// multi_edit
// ---------------------------------------------------------------------------

pub struct MultiEditTool;

impl MultiEditTool {
    /// Apply every edit in order, failing atomically on the first problem.
    fn plan(ctx: &ToolContext, call: &ToolCall) -> Result<(PathBuf, String, Option<String>, FileDiff)> {
        let path_arg = call.str_arg("path")?;
        let edits = call
            .arguments
            .get("edits")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| LinkError::InvalidArgs {
                tool: "multi_edit".to_string(),
                message: "`edits` must be an array of {old_string, new_string, replace_all?}".to_string(),
            })?;
        if edits.is_empty() {
            return Err(LinkError::InvalidArgs {
                tool: "multi_edit".to_string(),
                message: "`edits` must not be empty".to_string(),
            });
        }
        let path = ctx.sandbox.resolve_existing(&path_arg)?;
        let previous = read_text(&path)?;
        let mut working = previous.clone();
        for (index, edit) in edits.iter().enumerate() {
            let old_string = edit
                .get("old_string")
                .and_then(Value::as_str)
                .ok_or_else(|| LinkError::InvalidArgs {
                    tool: "multi_edit".to_string(),
                    message: format!("edit {index} is missing `old_string`"),
                })?;
            let new_string = edit
                .get("new_string")
                .and_then(Value::as_str)
                .ok_or_else(|| LinkError::InvalidArgs {
                    tool: "multi_edit".to_string(),
                    message: format!("edit {index} is missing `new_string`"),
                })?;
            let replace_all = edit
                .get("replace_all")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            working = apply_edit(&working, old_string, new_string, replace_all).map_err(|error| {
                LinkError::EditFailed(format!("edit {index} failed: {error}"))
            })?;
        }
        let display = ctx.sandbox.relative(&path);
        let diff = FileDiff::compute(display, &previous, &working);
        Ok((path, working, Some(previous), diff))
    }
}

#[async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &'static str {
        "multi_edit"
    }

    fn description(&self) -> &'static str {
        "Apply several exact-string edits to one file in a single call. Edits run in order and the whole call fails if any edit does not match, so the file is never left half-changed."
    }

    fn parameters(&self) -> Value {
        object_schema(
            json!({
                "path": { "type": "string", "description": "Workspace-relative path" },
                "edits": {
                    "type": "array",
                    "description": "Edits applied in order",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": { "type": "string" },
                            "new_string": { "type": "string" },
                            "replace_all": { "type": "boolean" }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            }),
            &["path", "edits"],
        )
    }

    fn kind(&self) -> ActionKind {
        ActionKind::Write
    }

    fn summarise(&self, call: &ToolCall) -> String {
        let path = call.opt_str_arg("path").unwrap_or_else(|| "?".to_string());
        let count = call
            .arguments
            .get("edits")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        format!("Edit `{path}` ({count} changes)")
    }

    fn permission_request(&self, ctx: &ToolContext, call: &ToolCall) -> PermissionRequest {
        match Self::plan(ctx, call) {
            Ok((path, _, _, diff)) => request_for_write(
                "multi_edit",
                &path,
                ctx.sandbox.root(),
                Some(diff.unified.clone()),
            ),
            Err(error) => PermissionRequest::new("multi_edit", ActionKind::Write, self.summarise(call))
                .with_reason(error.to_string())
                .with_risk(RiskLevel::High),
        }
    }

    async fn run(&self, ctx: &ToolContext, call: &ToolCall) -> Result<ToolOutput> {
        let (path, updated, previous, diff) = Self::plan(ctx, call)?;
        std::fs::write(&path, updated.as_bytes()).map_err(|e| LinkError::io(&path, e))?;
        let display = ctx.sandbox.relative(&path);
        Ok(
            ToolOutput::text(format!("Edited {display} {}", diff.summary())).with_display(
                ToolDisplay::Diff {
                    diff,
                    apply_path: display,
                    new_content: updated,
                    previous,
                },
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// list_dir
// ---------------------------------------------------------------------------

pub struct ListDirTool;

impl ListDirTool {
    /// Walk a directory, respecting `.gitignore`, into a sorted tree listing.
    pub(crate) fn walk(root: &Path, depth: usize, limit: usize) -> (String, usize, bool) {
        let mut entries: Vec<(usize, String, bool)> = Vec::new();
        let mut truncated = false;
        let walker = ignore::WalkBuilder::new(root)
            .max_depth(Some(depth.max(1)))
            .standard_filters(true)
            .hidden(false)
            .build();
        for result in walker {
            let Ok(entry) = result else { continue };
            if entry.path() == root {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .into_owned();
            let depth = entry.path().components().count() - root.components().count();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            entries.push((depth, relative, is_dir));
            if entries.len() >= limit {
                truncated = true;
                break;
            }
        }
        entries.sort();
        let total = entries.len();
        let mut out = String::new();
        for (depth, relative, is_dir) in &entries {
            let indent = "  ".repeat(depth.saturating_sub(1));
            let name = relative.rsplit('/').next().unwrap_or(relative);
            let suffix = if *is_dir { "/" } else { "" };
            out.push_str(&indent);
            out.push_str(name);
            out.push_str(suffix);
            out.push('\n');
        }
        (out, total, truncated)
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &'static str {
        "list_dir"
    }

    fn description(&self) -> &'static str {
        "List a directory tree. Respects `.gitignore`, so build output and vendored code stay out of the way."
    }

    fn parameters(&self) -> Value {
        object_schema(
            json!({
                "path": { "type": "string", "description": "Directory to list, relative to the workspace (default `.`)" },
                "depth": { "type": "integer", "description": "Maximum depth (default 2)" }
            }),
            &[],
        )
    }

    fn kind(&self) -> ActionKind {
        ActionKind::Read
    }

    fn summarise(&self, call: &ToolCall) -> String {
        format!(
            "List `{}`",
            call.opt_str_arg("path").unwrap_or_else(|| ".".to_string())
        )
    }

    async fn run(&self, ctx: &ToolContext, call: &ToolCall) -> Result<ToolOutput> {
        let relative = call.opt_str_arg("path").unwrap_or_else(|| ".".to_string());
        let path = ctx.sandbox.resolve_existing(&relative)?;
        if !path.is_dir() {
            return Err(LinkError::other(format!(
                "`{}` is not a directory",
                ctx.sandbox.relative(&path)
            )));
        }
        let depth = call.u64_arg("depth").unwrap_or(2).clamp(1, 8) as usize;
        let (listing, count, truncated) = Self::walk(&path, depth, 1500);
        let display = ctx.sandbox.relative(&path);
        let mut body = format!("<dir path=\"{display}\" entries=\"{count}\">\n{listing}</dir>");
        if truncated {
            body.push_str("\n[listing capped by Link Code]");
        }
        Ok(
            ToolOutput::text(body).with_display(ToolDisplay::Text(format!(
                "{display}: {count} entr{}",
                if count == 1 { "y" } else { "ies" }
            ))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use link_core::paths::Sandbox;

    fn context(root: &Path) -> ToolContext {
        ToolContext::new(Sandbox::new(root).unwrap())
    }

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\nfn helper() {}\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "# demo\n").unwrap();
        dir
    }

    #[tokio::test]
    async fn read_file_returns_numbered_selection() {
        let dir = workspace();
        let ctx = context(dir.path());
        let call = ToolCall::new("1", "read_file", json!({"path": "src/main.rs", "start_line": 2}));
        let output = ReadFileTool.run(&ctx, &call).await.unwrap();
        assert!(output.content.contains("fn helper()"));
        assert!(!output.content.contains("fn main()"));
        assert!(output.content.contains("shown=\"2-2\""));
    }

    #[tokio::test]
    async fn read_file_refuses_sandbox_escapes() {
        let dir = workspace();
        let ctx = context(dir.path());
        let call = ToolCall::new("1", "read_file", json!({"path": "../../etc/passwd"}));
        assert!(ReadFileTool.run(&ctx, &call).await.is_err());
    }

    #[tokio::test]
    async fn read_file_reports_missing_files() {
        let dir = workspace();
        let ctx = context(dir.path());
        let call = ToolCall::new("1", "read_file", json!({"path": "nope.rs"}));
        assert!(ReadFileTool.run(&ctx, &call).await.is_err());
    }

    #[tokio::test]
    async fn write_file_creates_parent_directories_and_reports_a_diff() {
        let dir = workspace();
        let ctx = context(dir.path());
        let call = ToolCall::new(
            "1",
            "write_file",
            json!({"path": "src/new/module.rs", "content": "pub fn x() {}\n"}),
        );
        let output = WriteFileTool.run(&ctx, &call).await.unwrap();
        assert!(dir.path().join("src/new/module.rs").exists());
        match output.display {
            ToolDisplay::Diff { diff, previous, .. } => {
                assert!(previous.is_none());
                assert!(diff.is_new);
                assert_eq!(diff.added, 1);
            }
            other => panic!("expected a diff display, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn edit_file_applies_and_records_the_previous_content() {
        let dir = workspace();
        let ctx = context(dir.path());
        let call = ToolCall::new(
            "1",
            "edit_file",
            json!({"path": "src/main.rs", "old_string": "fn helper() {}", "new_string": "fn helper() -> u8 { 1 }"}),
        );
        let output = EditFileTool.run(&ctx, &call).await.unwrap();
        let written = std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
        assert!(written.contains("fn helper() -> u8 { 1 }"));
        match output.display {
            ToolDisplay::Diff { previous, .. } => {
                assert!(previous.unwrap().contains("fn helper() {}"));
            }
            other => panic!("expected a diff display, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn edit_file_rejects_ambiguous_matches() {
        let dir = workspace();
        let ctx = context(dir.path());
        let call = ToolCall::new(
            "1",
            "edit_file",
            json!({"path": "src/main.rs", "old_string": "fn ", "new_string": "async fn "}),
        );
        assert!(EditFileTool.run(&ctx, &call).await.is_err());
    }

    #[tokio::test]
    async fn multi_edit_is_atomic_on_failure() {
        let dir = workspace();
        let ctx = context(dir.path());
        let before = std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
        let call = ToolCall::new(
            "1",
            "multi_edit",
            json!({
                "path": "src/main.rs",
                "edits": [
                    {"old_string": "fn main()", "new_string": "fn main2()"},
                    {"old_string": "this does not exist", "new_string": "x"}
                ]
            }),
        );
        assert!(MultiEditTool.run(&ctx, &call).await.is_err());
        assert_eq!(std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap(), before);
    }

    #[tokio::test]
    async fn multi_edit_applies_every_change_in_order() {
        let dir = workspace();
        let ctx = context(dir.path());
        let call = ToolCall::new(
            "1",
            "multi_edit",
            json!({
                "path": "src/main.rs",
                "edits": [
                    {"old_string": "fn main()", "new_string": "pub fn main()"},
                    {"old_string": "fn helper()", "new_string": "pub fn helper()"}
                ]
            }),
        );
        MultiEditTool.run(&ctx, &call).await.unwrap();
        let written = std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
        assert!(written.contains("pub fn main()"));
        assert!(written.contains("pub fn helper()"));
    }

    #[tokio::test]
    async fn list_dir_walks_respecting_gitignore() {
        let dir = workspace();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/junk"), "x").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        let ctx = context(dir.path());
        let call = ToolCall::new("1", "list_dir", json!({}));
        let output = ListDirTool.run(&ctx, &call).await.unwrap();
        assert!(output.content.contains("main.rs"));
        assert!(output.content.contains("src/"));
        assert!(!output.content.contains("junk"), "{} ", output.content);
    }

    #[tokio::test]
    async fn write_tool_marks_secret_files_as_mandatory() {
        let dir = workspace();
        let ctx = context(dir.path());
        let call = ToolCall::new("1", "write_file", json!({"path": ".env", "content": "A=1"}));
        let request = WriteFileTool.permission_request(&ctx, &call);
        assert!(request.mandatory);
        assert_eq!(request.risk, RiskLevel::High);
    }
}
