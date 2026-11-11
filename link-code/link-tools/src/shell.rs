//! The `bash` tool.
//!
//! Commands run in the workspace directory with a timeout, an output cap, and a
//! kill switch. Output is streamed to the UI as it arrives so the user watches
//! a build progress rather than staring at a spinner.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use link_core::permission::{request_for_bash, ActionKind, PermissionRequest};
use link_core::tool::{Tool, ToolCall, ToolContext, ToolDisplay, ToolOutput};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

use crate::fs_tools::object_schema;

pub struct BashTool;

impl BashTool {
    /// Pick the shell: `$SHELL` when set and executable, else `bash`, else `sh`.
    fn shell() -> String {
        if let Ok(shell) = std::env::var("SHELL") {
            if std::path::Path::new(&shell).is_file() {
                return shell;
            }
        }
        for candidate in ["/bin/bash", "/usr/bin/bash", "/bin/sh"] {
            if std::path::Path::new(candidate).is_file() {
                return candidate.to_string();
            }
        }
        "sh".to_string()
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Run a shell command in the workspace directory. Use it for builds, tests and git. Long output is capped and the command is killed after the configured timeout."
    }

    fn parameters(&self) -> Value {
        object_schema(
            json!({
                "command": { "type": "string", "description": "Shell command line" },
                "timeout_ms": { "type": "integer", "description": "Override the default timeout for this command" }
            }),
            &["command"],
        )
    }

    fn kind(&self) -> ActionKind {
        ActionKind::Execute
    }

    fn summarise(&self, call: &ToolCall) -> String {
        let command = call.opt_str_arg("command").unwrap_or_default();
        let first_line = command.lines().next().unwrap_or("").trim();
        let mut short: String = first_line.chars().take(90).collect();
        if first_line.chars().count() > 90 {
            short.push('\u{2026}');
        }
        format!("Run `{short}`")
    }

    fn permission_request(&self, ctx: &ToolContext, call: &ToolCall) -> PermissionRequest {
        let command = call.opt_str_arg("command").unwrap_or_default();
        request_for_bash(&command, ctx.sandbox.root())
    }

    async fn run(&self, ctx: &ToolContext, call: &ToolCall) -> Result<ToolOutput, link_core::error::LinkError> {
        let command = call.str_arg("command")?;
        let timeout = Duration::from_millis(
            call.u64_arg("timeout_ms")
                .unwrap_or(ctx.shell_timeout.as_millis() as u64)
                .clamp(1_000, 3_600_000),
        );
        let cap = ctx.output_cap;
        let started = Instant::now();

        let mut child = tokio::process::Command::new(Self::shell())
            .arg("-c")
            .arg(&command)
            .current_dir(ctx.sandbox.root())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| link_core::error::LinkError::other(format!("could not start the shell: {e}")))?;

        let mut stdout = child.stdout.take().expect("stdout was piped");
        let mut stderr = child.stderr.take().expect("stderr was piped");
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let mut buf_out = [0u8; 8192];
        let mut buf_err = [0u8; 8192];
        let mut out_closed = false;
        let mut err_closed = false;
        let mut timed_out = false;
        let mut cancelled = false;

        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let cancel = ctx.cancellation();

        loop {
            tokio::select! {
                _ = &mut deadline => {
                    timed_out = true;
                    let _ = child.start_kill();
                    break;
                }
                _ = cancel.cancelled() => {
                    cancelled = true;
                    let _ = child.start_kill();
                    break;
                }
                result = stdout.read(&mut buf_out), if !out_closed => match result {
                    Ok(0) => out_closed = true,
                    Ok(read) => {
                        let chunk = &buf_out[..read];
                        ctx.emit(&call.id, &String::from_utf8_lossy(chunk));
                        if out.len() < cap {
                            out.extend_from_slice(chunk);
                        }
                    }
                    Err(_) => out_closed = true,
                },
                result = stderr.read(&mut buf_err), if !err_closed => match result {
                    Ok(0) => err_closed = true,
                    Ok(read) => {
                        let chunk = &buf_err[..read];
                        ctx.emit(&call.id, &String::from_utf8_lossy(chunk));
                        if err.len() < cap {
                            err.extend_from_slice(chunk);
                        }
                    }
                    Err(_) => err_closed = true,
                },
                else => break,
            }
        }

        let status = child.wait().await.ok();
        let exit_code = status.as_ref().and_then(std::process::ExitStatus::code);
        let duration_ms = started.elapsed().as_millis() as u64;

        let stdout_text = String::from_utf8_lossy(&out).into_owned();
        let stderr_text = String::from_utf8_lossy(&err).into_owned();
        let mut combined = String::new();
        if !stdout_text.is_empty() {
            combined.push_str(&stdout_text);
        }
        if !stderr_text.is_empty() {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&stderr_text);
        }

        let mut notes = Vec::new();
        if timed_out {
            notes.push(format!(
                "killed after the {}s timeout",
                timeout.as_secs()
            ));
        }
        if cancelled {
            notes.push("cancelled by the user".to_string());
        }
        if out.len() >= cap || err.len() >= cap {
            notes.push(format!("output capped at {cap} bytes"));
        }

        let header = format!(
            "<command exit_code=\"{}\" duration_ms=\"{duration_ms}\"{}>\n",
            exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            if notes.is_empty() {
                String::new()
            } else {
                format!(" note=\"{}\"", notes.join("; "))
            }
        );
        let report = format!("{header}{combined}\n</command>");

        let display = ToolDisplay::Command {
            command: command.clone(),
            cwd: ctx.sandbox.root().display().to_string(),
            exit_code,
            duration_ms,
            output: combined.clone(),
        };

        Ok(ToolOutput::text(report)
            .truncate_to(cap)
            .with_display(display))
    }
}
