//! Bookkeeping, interaction and optional network tools.

use async_trait::async_trait;
use link_core::error::{LinkError, Result};
use link_core::event::{TodoItem, TodoStatus};
use link_core::permission::{ActionKind, PermissionRequest, RiskLevel};
use link_core::tool::{Tool, ToolCall, ToolContext, ToolDisplay, ToolOutput};
use serde_json::{json, Value};

use crate::fs_tools::object_schema;

// ---------------------------------------------------------------------------
// update_tasks
// ---------------------------------------------------------------------------

pub struct UpdateTasksTool;

impl UpdateTasksTool {
    fn parse(call: &ToolCall) -> Result<Vec<TodoItem>> {
        let raw = call
            .arguments
            .get("tasks")
            .or_else(|| call.arguments.get("todos"))
            .or_else(|| call.arguments.get("items"))
            .cloned()
            .ok_or_else(|| LinkError::InvalidArgs {
                tool: "update_tasks".to_string(),
                message: "`tasks` is required".to_string(),
            })?;
        let array = match raw {
            Value::Array(items) => items,
            Value::String(text) => serde_json::from_str::<Vec<Value>>(&text).map_err(|e| {
                LinkError::InvalidArgs {
                    tool: "update_tasks".to_string(),
                    message: format!("`tasks` is not a JSON array: {e}"),
                }
            })?,
            _ => {
                return Err(LinkError::InvalidArgs {
                    tool: "update_tasks".to_string(),
                    message: "`tasks` must be an array of {content, status}".to_string(),
                })
            }
        };
        let mut items = Vec::new();
        for entry in array {
            // Accept both `{"content": .., "status": ..}` and a bare string.
            let (text, status) = match entry {
                Value::String(text) => (text, TodoStatus::Pending),
                Value::Object(object) => {
                    let text = object
                        .get("content")
                        .or_else(|| object.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let status = match object.get("status").and_then(Value::as_str) {
                        Some("done" | "completed" | "complete") => TodoStatus::Done,
                        Some("in_progress" | "in-progress" | "active") => TodoStatus::InProgress,
                        _ => TodoStatus::Pending,
                    };
                    (text, status)
                }
                _ => continue,
            };
            if !text.trim().is_empty() {
                items.push(TodoItem { text, status });
            }
        }
        Self::normalise_statuses(&mut items);
        Ok(items)
    }

    /// Keep the checklist honest: at most one task is in progress. When the
    /// model sends several, the first wins and the rest go back to pending.
    /// When it marks nothing in progress, the next pending task is promoted so
    /// the checklist always shows what is being worked on right now.
    fn normalise_statuses(items: &mut [TodoItem]) {
        let mut seen = false;
        for item in items.iter_mut() {
            if item.status == TodoStatus::InProgress {
                if seen {
                    item.status = TodoStatus::Pending;
                } else {
                    seen = true;
                }
            }
        }
        if !seen {
            if let Some(next) = items
                .iter_mut()
                .find(|item| item.status == TodoStatus::Pending)
            {
                next.status = TodoStatus::InProgress;
            }
        }
    }

    fn render(items: &[TodoItem]) -> String {
        let mut out = String::from("<tasks>\n");
        for item in items {
            out.push_str(&format!(
                "{} {} ({})\n",
                item.status.marker(),
                item.text,
                item.status.label()
            ));
        }
        out.push_str("</tasks>");
        out
    }
}

#[async_trait]
impl Tool for UpdateTasksTool {
    fn name(&self) -> &'static str {
        "update_tasks"
    }

    fn description(&self) -> &'static str {
        "Plan and track multi-step work. Pass the complete ordered task list. Exactly one task is in_progress at a time; mark each task completed when it is done. The checklist is pinned in the agent panel."
    }

    fn parameters(&self) -> Value {
        object_schema(
            json!({
                "tasks": {
                    "type": "array",
                    "description": "The complete task list, in order",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "Short description of the task" },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"]
                            }
                        },
                        "required": ["content", "status"]
                    }
                }
            }),
            &["tasks"],
        )
    }

    fn kind(&self) -> ActionKind {
        // Pure bookkeeping: it never touches the user's files.
        ActionKind::Read
    }

    fn summarise(&self, call: &ToolCall) -> String {
        let count = call
            .arguments
            .get("tasks")
            .or_else(|| call.arguments.get("todos"))
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        format!("Update tasks ({count})")
    }

    async fn run(&self, ctx: &ToolContext, call: &ToolCall) -> Result<ToolOutput> {
        let items = Self::parse(call)?;
        ctx.replace_todos(items.clone());
        Ok(ToolOutput::text(Self::render(&items)).with_display(ToolDisplay::Todos(items)))
    }
}

// ---------------------------------------------------------------------------
// ask_user
// ---------------------------------------------------------------------------

pub struct AskUserTool;

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &'static str {
        "ask_user"
    }

    fn description(&self) -> &'static str {
        "Ask the user a question when a decision is genuinely required, offering up to four options. Prefer acting on a reasonable assumption when the answer is obvious."
    }

    fn parameters(&self) -> Value {
        object_schema(
            json!({
                "question": { "type": "string", "description": "The question to show" },
                "options": {
                    "type": "array",
                    "description": "Tappable answers; the last one is used as \"something else\" when provided",
                    "items": { "type": "string" }
                }
            }),
            &["question"],
        )
    }

    fn kind(&self) -> ActionKind {
        ActionKind::Read
    }

    fn summarise(&self, call: &ToolCall) -> String {
        let question = call.opt_str_arg("question").unwrap_or_default();
        let short: String = question.chars().take(60).collect();
        format!("Ask: {short}")
    }

    async fn run(&self, ctx: &ToolContext, call: &ToolCall) -> Result<ToolOutput> {
        let question = call.str_arg("question")?;
        let options: Vec<String> = call
            .arguments
            .get("options")
            .and_then(Value::as_array)
            .map(|array| {
                array
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let Some(asker) = ctx.asker.clone() else {
            // No UI attached (tests, headless): tell the model to proceed.
            return Ok(ToolOutput::text(
                "No interactive session is available, so the question could not be shown. \
                 Continue by choosing the most reasonable option and stating the assumption."
                    .to_string(),
            ));
        };

        match asker.ask(call.id.clone(), question.clone(), options.clone()).await {
            Some(answer) => Ok(
                ToolOutput::text(format!("The user answered: {answer}")).with_display(
                    ToolDisplay::Question {
                        question,
                        options,
                    },
                ),
            ),
            None => Ok(ToolOutput::text(
                "The user dismissed the question without answering. Continue with your best judgement."
                    .to_string(),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// web_fetch (opt-in)
// ---------------------------------------------------------------------------

pub struct WebFetchTool;

impl WebFetchTool {
    /// Remove a `<tag>...</tag>` block entirely (script and style content is
    /// never useful to a model and can be enormous).
    fn strip_blocks(html: &str, tag: &str) -> String {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        let mut out = String::new();
        let mut rest = html;
        loop {
            // `to_ascii_lowercase` preserves byte offsets, so the indices stay valid.
            let lowered = rest.to_ascii_lowercase();
            let Some(start) = lowered.find(&open) else {
                out.push_str(rest);
                break;
            };
            out.push_str(&rest[..start]);
            let Some(relative_end) = lowered[start..].find(&close) else {
                break;
            };
            rest = &rest[start + relative_end + close.len()..];
        }
        out
    }

    /// Strip tags and collapse whitespace so the model gets readable text.
    fn html_to_text(html: &str) -> String {
        let without_scripts = Self::strip_blocks(html, "script");
        let without_styles = Self::strip_blocks(&without_scripts, "style");
        let mut stripped = String::with_capacity(without_styles.len());
        let mut in_tag = false;
        for ch in without_styles.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => {
                    in_tag = false;
                    stripped.push(' ');
                }
                other if !in_tag => stripped.push(other),
                _ => {}
            }
        }
        let decoded = stripped
            .replace("&nbsp;", " ")
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'");
        decoded
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &'static str {
        "web_fetch"
    }

    fn description(&self) -> &'static str {
        "Fetch a URL and return its text content. Disabled unless the user turns on \"Enable web tools\" in Settings, because it sends the URL to a third party."
    }

    fn parameters(&self) -> Value {
        object_schema(
            json!({
                "url": { "type": "string", "description": "Absolute http(s) URL" }
            }),
            &["url"],
        )
    }

    fn kind(&self) -> ActionKind {
        ActionKind::Network
    }

    fn summarise(&self, call: &ToolCall) -> String {
        format!(
            "Fetch `{}`",
            call.opt_str_arg("url").unwrap_or_else(|| "?".to_string())
        )
    }

    fn permission_request(&self, _ctx: &ToolContext, call: &ToolCall) -> PermissionRequest {
        PermissionRequest::new("web_fetch", ActionKind::Network, self.summarise(call))
            .with_detail(call.opt_str_arg("url").unwrap_or_default())
            .with_pattern(call.opt_str_arg("url").unwrap_or_default())
            .with_risk(RiskLevel::Medium)
            .with_reason("fetches a URL over the network, which shares the request with a third party")
    }

    async fn run(&self, ctx: &ToolContext, call: &ToolCall) -> Result<ToolOutput> {
        let url = call.str_arg("url")?;
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(LinkError::InvalidArgs {
                tool: "web_fetch".to_string(),
                message: "only http and https URLs are supported".to_string(),
            });
        }
        let client = link_core::proxy::apply(reqwest::Client::builder())
            .user_agent(concat!("LinkCode/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| LinkError::other(format!("could not build an HTTP client: {e}")))?;
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| LinkError::other(format!("could not fetch {url}: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            return Ok(ToolOutput::text(format!(
                "Fetching {url} failed with HTTP {status}"
            )));
        }
        let body = response
            .text()
            .await
            .map_err(|e| LinkError::other(format!("could not read the response body: {e}")))?;
        let text = if body.trim_start().starts_with('<') {
            Self::html_to_text(&body)
        } else {
            body
        };
        let capped = ToolOutput::text(text).truncate_to(ctx.output_cap.min(64 * 1024));
        Ok(capped.with_display(ToolDisplay::Text(format!("Fetched {url}"))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use link_core::paths::Sandbox;

    fn context(root: &std::path::Path) -> ToolContext {
        ToolContext::new(Sandbox::new(root).unwrap())
    }

    #[tokio::test]
    async fn update_tasks_replaces_the_list_and_updates_context() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let call = ToolCall::new(
            "1",
            "update_tasks",
            json!({"tasks": [
                {"content": "read the code", "status": "completed"},
                {"content": "write the patch", "status": "in_progress"}
            ]}),
        );
        let output = UpdateTasksTool.run(&ctx, &call).await.unwrap();
        assert!(output.content.contains("write the patch"));
        assert_eq!(ctx.todo_snapshot().len(), 2);
        assert_eq!(ctx.todo_snapshot()[1].status, TodoStatus::InProgress);
    }

    #[tokio::test]
    async fn update_tasks_keeps_exactly_one_task_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let call = ToolCall::new(
            "1",
            "update_tasks",
            json!({"tasks": [
                {"content": "one", "status": "in_progress"},
                {"content": "two", "status": "in_progress"},
                {"content": "three", "status": "pending"}
            ]}),
        );
        UpdateTasksTool.run(&ctx, &call).await.unwrap();
        let items = ctx.todo_snapshot();
        assert_eq!(items[0].status, TodoStatus::InProgress);
        assert_eq!(items[1].status, TodoStatus::Pending);
        assert_eq!(items[2].status, TodoStatus::Pending);
    }

    #[tokio::test]
    async fn update_tasks_promotes_the_first_pending_task() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let call = ToolCall::new(
            "1",
            "update_tasks",
            json!({"tasks": [
                {"content": "first", "status": "pending"},
                {"content": "second", "status": "pending"}
            ]}),
        );
        UpdateTasksTool.run(&ctx, &call).await.unwrap();
        let items = ctx.todo_snapshot();
        assert_eq!(items[0].status, TodoStatus::InProgress);
        assert_eq!(items[1].status, TodoStatus::Pending);
    }

    #[tokio::test]
    async fn update_tasks_tolerates_string_arguments_and_bare_text() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let call = ToolCall::new(
            "1",
            "update_tasks",
            json!({ "tasks": "[\"write tests\"]" }),
        );
        let output = UpdateTasksTool.run(&ctx, &call).await.unwrap();
        assert!(output.content.contains("write tests"));
    }

    #[tokio::test]
    async fn ask_user_without_a_ui_tells_the_model_to_continue() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let call = ToolCall::new("1", "ask_user", json!({"question": "Which file?"}));
        let output = AskUserTool.run(&ctx, &call).await.unwrap();
        assert!(output.content.contains("Continue"));
    }

    #[test]
    fn html_is_reduced_to_readable_text() {
        let html = "<html><head><style>p{}</style><script>var x=1;</script></head>\
                    <body><h1>Title</h1><p>Hello&nbsp;world</p></body></html>";
        let text = WebFetchTool::html_to_text(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Hello world"));
        assert!(!text.contains("var x"));
        assert!(!text.contains("<p>"));
    }

    #[tokio::test]
    async fn web_fetch_rejects_non_http_urls() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let call = ToolCall::new("1", "web_fetch", json!({"url": "file:///etc/passwd"}));
        assert!(WebFetchTool.run(&ctx, &call).await.is_err());
    }
}
