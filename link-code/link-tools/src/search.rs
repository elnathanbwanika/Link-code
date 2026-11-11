//! Search tools: `glob` and `grep`.
//!
//! Both walk the workspace with the `ignore` crate, so `.gitignore`, `.ignore`
//! and global git excludes are honoured exactly as they are by git itself. That
//! matters more than raw speed: a model that greps `target/` or `node_modules/`
//! wastes its whole context window.

use async_trait::async_trait;
use globset::{Glob, GlobMatcher};
use link_core::error::{LinkError, Result};
use link_core::permission::ActionKind;
use link_core::tool::{Tool, ToolCall, ToolContext, ToolDisplay, ToolOutput};
use regex::RegexBuilder;
use serde_json::{json, Value};
use std::path::Path;

use crate::fs_tools::object_schema;

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Compile a glob, returning a friendly error the model can act on.
fn compile_glob(pattern: &str) -> Result<GlobMatcher> {
    Glob::new(pattern)
        .map(|glob| glob.compile_matcher())
        .map_err(|error| LinkError::InvalidArgs {
            tool: "glob".to_string(),
            message: format!("`{pattern}` is not a valid glob: {error}"),
        })
}

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "glob"
    }

    fn description(&self) -> &'static str {
        "Find files by glob pattern, e.g. `**/*.rs` or `src/**/mod.rs`. Respects .gitignore, sorted, capped."
    }

    fn parameters(&self) -> Value {
        object_schema(
            json!({
                "pattern": { "type": "string", "description": "Glob pattern matched against workspace-relative paths" },
                "path": { "type": "string", "description": "Directory to search from (default `.`)" },
                "max_results": { "type": "integer", "description": "Maximum matches to return (default 200)" }
            }),
            &["pattern"],
        )
    }

    fn kind(&self) -> ActionKind {
        ActionKind::Read
    }

    fn summarise(&self, call: &ToolCall) -> String {
        format!(
            "Glob `{}`",
            call.opt_str_arg("pattern").unwrap_or_else(|| "?".to_string())
        )
    }

    async fn run(&self, ctx: &ToolContext, call: &ToolCall) -> Result<ToolOutput> {
        let pattern = call.str_arg("pattern")?;
        let matcher = compile_glob(&pattern)?;
        let root = ctx
            .sandbox
            .resolve_existing(call.opt_str_arg("path").unwrap_or_else(|| ".".to_string()))?;
        let limit = call.u64_arg("max_results").unwrap_or(200).clamp(1, 5_000) as usize;

        let mut matches = Vec::new();
        let mut truncated = false;
        let walker = ignore::WalkBuilder::new(&root).standard_filters(true).build();
        for entry in walker.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let relative = ctx.sandbox.relative(path);
            if matcher.is_match(&relative) {
                matches.push(relative);
                if matches.len() >= limit {
                    truncated = true;
                    break;
                }
            }
        }
        matches.sort();
        let mut body = format!("<glob pattern=\"{pattern}\" matches=\"{}\">\n", matches.len());
        body.push_str(&matches.join("\n"));
        body.push_str("\n</glob>");
        if truncated {
            body.push_str("\n[more matches omitted]");
        }
        Ok(
            ToolOutput::text(body).with_display(ToolDisplay::Text(format!(
                "{} match{}",
                matches.len(),
                if matches.len() == 1 { "" } else { "es" }
            ))),
        )
    }
}

pub struct GrepTool;

impl GrepTool {
    /// Search a single file, appending `path:line: text` rows.
    fn search_file(
        ctx: &ToolContext,
        path: &Path,
        regex: &regex::Regex,
        out: &mut Vec<String>,
        limit: usize,
    ) -> Result<bool> {
        let metadata = std::fs::metadata(path).map_err(|e| LinkError::io(path, e))?;
        if metadata.len() > MAX_FILE_BYTES {
            return Ok(false);
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            return Ok(false);
        };
        let relative = ctx.sandbox.relative(path);
        for (index, line) in content.lines().enumerate() {
            if regex.is_match(line) {
                out.push(format!("{}:{}: {}", relative, index + 1, line.trim_end()));
                if out.len() >= limit {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search file contents with a Rust regular expression. Returns `path:line: text` rows. Respects .gitignore."
    }

    fn parameters(&self) -> Value {
        object_schema(
            json!({
                "pattern": { "type": "string", "description": "Rust regex, e.g. `fn\\s+main`" },
                "path": { "type": "string", "description": "Directory to search (default `.`)" },
                "glob": { "type": "string", "description": "Only search files matching this glob, e.g. `**/*.rs`" },
                "case_sensitive": { "type": "boolean", "description": "Default true" },
                "max_results": { "type": "integer", "description": "Maximum rows to return (default 100)" }
            }),
            &["pattern"],
        )
    }

    fn kind(&self) -> ActionKind {
        ActionKind::Read
    }

    fn summarise(&self, call: &ToolCall) -> String {
        format!(
            "Grep `{}`",
            call.opt_str_arg("pattern").unwrap_or_else(|| "?".to_string())
        )
    }

    async fn run(&self, ctx: &ToolContext, call: &ToolCall) -> Result<ToolOutput> {
        let pattern = call.str_arg("pattern")?;
        let limit = call.u64_arg("max_results").unwrap_or(100).clamp(1, 2_000) as usize;
        let regex = RegexBuilder::new(&pattern)
            .case_insensitive(!call.bool_arg("case_sensitive").unwrap_or(true))
            .build()
            .map_err(|error| LinkError::InvalidArgs {
                tool: "grep".to_string(),
                message: format!("`{pattern}` is not a valid regex: {error}"),
            })?;
        let filter = match call.opt_str_arg("glob") {
            Some(glob) => Some(compile_glob(&glob)?),
            None => None,
        };
        let root = ctx
            .sandbox
            .resolve_existing(call.opt_str_arg("path").unwrap_or_else(|| ".".to_string()))?;

        let mut rows = Vec::new();
        let mut hit_limit = false;
        let walker = ignore::WalkBuilder::new(&root).standard_filters(true).build();
        for entry in walker.flatten() {
            let path = entry.path();
            if !path.is_file() || ctx.cancel.is_cancelled() {
                continue;
            }
            if let Some(filter) = &filter {
                if !filter.is_match(ctx.sandbox.relative(path)) {
                    continue;
                }
            }
            if Self::search_file(ctx, path, &regex, &mut rows, limit)? {
                hit_limit = true;
                break;
            }
        }

        let mut body = format!("<grep pattern=\"{pattern}\" matches=\"{}\">\n", rows.len());
        body.push_str(&rows.join("\n"));
        body.push_str("\n</grep>");
        if hit_limit {
            body.push_str("\n[more matches omitted]");
        }
        Ok(
            ToolOutput::text(body).with_display(ToolDisplay::Text(format!(
                "{} match{}",
                rows.len(),
                if rows.len() == 1 { "" } else { "es" }
            ))),
        )
    }
}
