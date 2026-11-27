//! Session storage.
//!
//! Every conversation is saved per workspace under
//! `~/.local/share/link-code/sessions/<workspace>/<session-id>.json`, so it can
//! be resumed, renamed, forked, exported to Markdown or deleted.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{data_dir, write_json_atomic};
use crate::error::{LinkError, Result};
use crate::event::{TodoItem, TodoStatus};
use crate::permission::PermissionMode;
use link_models::types::{ChatMessage, Role, ToolCallRequest};

use crate::DATA_FORMAT_VERSION;

/// Who produced a stored message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredRole {
    System,
    User,
    Assistant,
    Tool,
}

impl StoredRole {
    pub fn label(self) -> &'static str {
        match self {
            StoredRole::System => "System",
            StoredRole::User => "You",
            StoredRole::Assistant => "Assistant",
            StoredRole::Tool => "Tool",
        }
    }
}

/// One persisted message. Kept deliberately close to the wire format so a
/// resumed session reconstructs exactly what the model saw.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: Uuid,
    pub role: StoredRole,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool calls requested by an assistant turn, including Gemini's signature.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallRequest>,
    /// Workspace-relative paths attached with `@` or pinned with `#`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    pub created_at: DateTime<Utc>,
}

impl StoredMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            role: StoredRole::User,
            content: content.into(),
            thinking: None,
            model: None,
            tool_name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            attachments: Vec::new(),
            created_at: Utc::now(),
        }
    }

    pub fn assistant(content: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            role: StoredRole::Assistant,
            content: content.into(),
            thinking: None,
            model: Some(model.into()),
            tool_name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            attachments: Vec::new(),
            created_at: Utc::now(),
        }
    }

    /// A tool-only assistant step. Tool results are stored separately so the
    /// next model sees the same call/result sequence the previous one saw.
    pub fn tool_call(call: ToolCallRequest, model: impl Into<String>) -> Self {
        let mut message = Self::assistant("", model);
        message.tool_calls.push(call);
        message
    }

    pub fn tool_result(call_id: impl Into<String>, tool: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            role: StoredRole::Tool,
            content: content.into(),
            thinking: None,
            model: None,
            tool_name: Some(tool.into()),
            tool_call_id: Some(call_id.into()),
            tool_calls: Vec::new(),
            attachments: Vec::new(),
            created_at: Utc::now(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            role: StoredRole::System,
            content: content.into(),
            thinking: None,
            model: None,
            tool_name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            attachments: Vec::new(),
            created_at: Utc::now(),
        }
    }
}

/// One agent turn, with its cost accounting and checkpoint link.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: Uuid,
    pub model: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub cost_usd: f64,
    /// Checkpoint created before this turn, for **Undo turn**.
    pub checkpoint_id: Option<String>,
    pub mode: PermissionMode,
}

/// Durable description of the work inside one conversation.
///
/// Messages alone tell a resumed model what was said, but not what was
/// *finished*. Keeping the live checklist, the files already changed and a
/// short note about the stopping point lets the next turn continue instead of
/// restarting from scratch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkState {
    /// Latest `update_tasks` checklist.
    #[serde(default)]
    pub todos: Vec<TodoItem>,
    /// Workspace-relative files the agent wrote or edited, in first-seen order.
    #[serde(default)]
    pub files: Vec<String>,
    /// Short, human-readable description of where the last turn stopped.
    #[serde(default)]
    pub note: String,
    /// When the last turn finished.
    #[serde(default)]
    pub last_turn_at: Option<DateTime<Utc>>,
}

/// A saved conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub id: String,
    pub workspace: PathBuf,
    pub title: String,
    /// Model selected when the session was created (each message keeps its own).
    pub model: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub messages: Vec<StoredMessage>,
    #[serde(default)]
    pub turns: Vec<Turn>,
    /// Checklist and edited-file log used to resume work without repeating it.
    #[serde(default)]
    pub work: WorkState,
    /// True while the title is still machine-generated (first user message).
    #[serde(default)]
    pub auto_title: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Lightweight record for the Sessions list, so the UI never parses every
/// message just to draw the sidebar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub model: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub message_count: usize,
    pub workspace: PathBuf,
}

impl Session {
    pub fn new(workspace: &Path, model: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            version: DATA_FORMAT_VERSION,
            id: Uuid::new_v4().to_string(),
            workspace: workspace.to_path_buf(),
            title: "New session".to_string(),
            model: model.into(),
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            turns: Vec::new(),
            work: WorkState::default(),
            auto_title: true,
            tags: Vec::new(),
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    pub fn push(&mut self, message: StoredMessage) {
        if self.auto_title && message.role == StoredRole::User {
            self.title = derive_title(&message.content);
            self.auto_title = false;
        }
        self.messages.push(message);
        self.touch();
    }

    pub fn push_user(&mut self, content: impl Into<String>) -> &StoredMessage {
        self.push(StoredMessage::user(content));
        self.messages.last().expect("just pushed")
    }

    pub fn start_turn(&mut self, model: impl Into<String>, mode: PermissionMode) -> Uuid {
        let turn = Turn {
            id: Uuid::new_v4(),
            model: model.into(),
            started_at: Utc::now(),
            finished_at: None,
            prompt_tokens: 0,
            completion_tokens: 0,
            cost_usd: 0.0,
            checkpoint_id: None,
            mode,
        };
        let id = turn.id;
        self.turns.push(turn);
        self.touch();
        id
    }

    pub fn finish_turn(&mut self, turn_id: Uuid, prompt_tokens: usize, completion_tokens: usize, cost_usd: f64) {
        if let Some(turn) = self.turns.iter_mut().find(|t| t.id == turn_id) {
            turn.finished_at = Some(Utc::now());
            turn.prompt_tokens = prompt_tokens;
            turn.completion_tokens = completion_tokens;
            turn.cost_usd = cost_usd;
        }
        self.touch();
    }

    pub fn set_turn_checkpoint(&mut self, turn_id: Uuid, checkpoint_id: impl Into<String>) {
        if let Some(turn) = self.turns.iter_mut().find(|t| t.id == turn_id) {
            turn.checkpoint_id = Some(checkpoint_id.into());
        }
    }

    /// Remove every message and turn from the last turn onwards (Undo turn).
    pub fn truncate_last_turn(&mut self) -> bool {
        let Some(turn) = self.turns.pop() else {
            return false;
        };
        // Messages created at or after the turn start belong to that turn.
        while let Some(last) = self.messages.last() {
            if last.created_at >= turn.started_at {
                self.messages.pop();
            } else {
                break;
            }
        }
        self.touch();
        true
    }

    pub fn rename(&mut self, title: impl Into<String>) {
        self.title = title.into();
        self.auto_title = false;
        self.touch();
    }

    pub fn total_tokens(&self) -> (usize, usize) {
        self.turns.iter().fold((0, 0), |(prompt, completion), turn| {
            (prompt + turn.prompt_tokens, completion + turn.completion_tokens)
        })
    }

    pub fn total_cost(&self) -> f64 {
        self.turns.iter().map(|turn| turn.cost_usd).sum()
    }

    // ---- durable work state ---------------------------------------------

    /// Replace the live checklist and persist it with the session.
    pub fn set_todos(&mut self, items: Vec<TodoItem>) {
        self.work.todos = items;
        self.touch();
    }

    /// Remember a file the agent changed, once. Paths are workspace-relative
    /// when possible so the note stays readable across sessions.
    pub fn record_edited_file(&mut self, path: &str) {
        let path = path.trim();
        if path.is_empty() {
            return;
        }
        if self.work.files.iter().any(|existing| existing == path) {
            return;
        }
        self.work.files.push(path.to_string());
        // A runaway edit loop should not grow the session file forever.
        if self.work.files.len() > 200 {
            self.work.files.remove(0);
        }
        self.touch();
    }

    /// Describe where the turn stopped, so a reopened session can continue.
    pub fn set_resume_note(&mut self, note: impl Into<String>) {
        self.work.note = note.into();
        self.work.last_turn_at = Some(Utc::now());
        self.touch();
    }

    /// Number of checklist items that are not done yet.
    pub fn open_task_count(&self) -> usize {
        self.work
            .todos
            .iter()
            .filter(|item| item.status != TodoStatus::Done)
            .count()
    }

    /// Rebuild the exact provider history: user turns, assistant turns with
    /// their tool calls, and tool results in their original order. Old
    /// sessions without stored tool calls still load and produce text history.
    pub fn chat_history(&self) -> Vec<ChatMessage> {
        let mut history: Vec<ChatMessage> = Vec::new();
        for stored in &self.messages {
            match stored.role {
                // The system prompt is rebuilt for every turn from config and
                // project memory; sending it twice makes some providers reject
                // the request.
                StoredRole::System => continue,
                StoredRole::User => history.push(ChatMessage::user(stored.content.clone())),
                StoredRole::Assistant => {
                    if stored.tool_calls.is_empty() && stored.content.trim().is_empty() {
                        continue;
                    }
                    let mut message = ChatMessage::assistant(stored.content.clone());
                    message.tool_calls = stored.tool_calls.clone();
                    // Several tool-only steps in one round are stored as
                    // separate messages; merge them so providers see one
                    // assistant turn with all its calls before the results.
                    if let Some(last) = history.last_mut() {
                        if last.role == Role::Assistant && !stored.tool_calls.is_empty() {
                            last.tool_calls.extend(message.tool_calls);
                            if !message.content.is_empty() {
                                if !last.content.is_empty() {
                                    last.content.push('\n');
                                }
                                last.content.push_str(&message.content);
                            }
                            continue;
                        }
                    }
                    history.push(message);
                }
                StoredRole::Tool => history.push(ChatMessage::tool_result(
                    stored.tool_call_id.clone().unwrap_or_default(),
                    stored.tool_name.clone().unwrap_or_default(),
                    stored.content.clone(),
                )),
            }
        }
        history
    }

    /// True when the session carries enough state to resume meaningfully.
    pub fn has_work_state(&self) -> bool {
        !self.work.todos.is_empty()
            || !self.work.files.is_empty()
            || !self.work.note.trim().is_empty()
    }

    /// System-prompt section that turns a reopened conversation into a
    /// continuation rather than a fresh attempt.
    pub fn resume_prompt(&self) -> Option<String> {
        if !self.has_work_state() {
            return None;
        }
        let open = self.open_task_count();
        let mut out = String::from("## Continuing this session\n\n");
        out.push_str(
            "This conversation already contains earlier work on the task. \
             Do not start over, re-plan from scratch, or repeat steps that are \
             already finished.\n\n",
        );
        if !self.work.todos.is_empty() {
            out.push_str(&format!("Task checklist ({open} open):\n"));
            for item in &self.work.todos {
                out.push_str(&format!("- {} {}\n", item.status.marker(), item.text));
            }
            out.push('\n');
        }
        if !self.work.files.is_empty() {
            out.push_str("Files already changed in this session:\n");
            for file in &self.work.files {
                out.push_str(&format!("- {file}\n"));
            }
            out.push('\n');
        }
        if !self.work.note.trim().is_empty() {
            out.push_str("Where the previous turn stopped:\n");
            out.push_str(self.work.note.trim());
            out.push_str("\n\n");
        }
        out.push_str(
            "Continue from the first unfinished checklist item. Do not re-read or \
             rewrite a file listed as already changed unless the user asks for it. \
             If the user's new message starts unrelated work, follow the user instead.",
        );
        Some(out)
    }

    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            model: self.model.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            message_count: self.messages.len(),
            workspace: self.workspace.clone(),
        }
    }

    /// A copy with a fresh id, for "Fork session".
    pub fn fork(&self, title: Option<String>) -> Self {
        let now = Utc::now();
        Self {
            version: DATA_FORMAT_VERSION,
            id: Uuid::new_v4().to_string(),
            workspace: self.workspace.clone(),
            title: title.unwrap_or_else(|| format!("{} (fork)", self.title)),
            model: self.model.clone(),
            created_at: now,
            updated_at: now,
            messages: self.messages.clone(),
            turns: self.turns.clone(),
            work: self.work.clone(),
            auto_title: false,
            tags: self.tags.clone(),
        }
    }
}

/// Derive a short title from the first user message.
pub fn derive_title(text: &str) -> String {
    let first_line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("New session");
    let cleaned = first_line.trim_start_matches(['/', '!', '@', '#']);
    let mut title: String = cleaned.chars().take(60).collect();
    if cleaned.chars().count() > 60 {
        title.push('\u{2026}');
    }
    if title.trim().is_empty() {
        "New session".to_string()
    } else {
        title.trim().to_string()
    }
}

/// Reads and writes sessions on disk, one directory per workspace.
#[derive(Debug, Clone)]
pub struct SessionStore {
    root: PathBuf,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::default_location()
    }
}

impl SessionStore {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `~/.local/share/link-code/sessions`
    pub fn default_location() -> Self {
        Self::at(data_dir().join("sessions"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A stable, human-readable directory name for a workspace path.
    pub fn workspace_dir(&self, workspace: &Path) -> PathBuf {
        let canonical = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        let name = canonical
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "workspace".to_string());
        let slug: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' })
            .take(48)
            .collect();
        self.root.join(format!("{slug}-{}", short_hash(&canonical.to_string_lossy())))
    }

    fn session_path(&self, workspace: &Path, session_id: &str) -> PathBuf {
        self.workspace_dir(workspace).join(format!("{session_id}.json"))
    }

    pub fn save(&self, session: &Session) -> Result<()> {
        let dir = self.workspace_dir(&session.workspace);
        std::fs::create_dir_all(&dir).map_err(|e| LinkError::io(&dir, e))?;
        write_json_atomic(&self.session_path(&session.workspace, &session.id), session)
    }

    pub fn load(&self, workspace: &Path, session_id: &str) -> Result<Session> {
        let path = self.session_path(workspace, session_id);
        let text = std::fs::read_to_string(&path).map_err(|e| LinkError::io(&path, e))?;
        serde_json::from_str(&text).map_err(|e| LinkError::Session(format!("{}: {e}", path.display())))
    }

    /// Every session for a workspace, most recently updated first.
    pub fn list(&self, workspace: &Path) -> Vec<SessionSummary> {
        let dir = self.workspace_dir(workspace);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut summaries: Vec<SessionSummary> = entries
            .flatten()
            .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("json"))
            .filter_map(|entry| {
                let text = std::fs::read_to_string(entry.path()).ok()?;
                let session: Session = serde_json::from_str(&text).ok()?;
                Some(session.summary())
            })
            .collect();
        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        summaries
    }

    pub fn most_recent(&self, workspace: &Path) -> Option<Session> {
        let summary = self.list(workspace).into_iter().next()?;
        self.load(workspace, &summary.id).ok()
    }

    pub fn delete(&self, workspace: &Path, session_id: &str) -> Result<()> {
        let path = self.session_path(workspace, session_id);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| LinkError::io(&path, e))?;
        }
        Ok(())
    }

    /// Load, copy with a new id and title, save, and return the fork.
    pub fn fork(&self, workspace: &Path, session_id: &str, title: Option<String>) -> Result<Session> {
        let session = self.load(workspace, session_id)?;
        let fork = session.fork(title);
        self.save(&fork)?;
        Ok(fork)
    }

    /// Render a session as Markdown for `/export`.
    pub fn export_markdown(session: &Session) -> String {
        let mut out = String::new();
        out.push_str(&format!("# {}\n\n", session.title));
        out.push_str(&format!(
            "- Workspace: `{}`\n- Model: `{}`\n- Created: {}\n- Messages: {}\n\n---\n\n",
            session.workspace.display(),
            session.model,
            session.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
            session.messages.len()
        ));
        for message in &session.messages {
            match message.role {
                StoredRole::User => out.push_str("## You\n\n"),
                StoredRole::Assistant => {
                    let model = message.model.as_deref().unwrap_or(&session.model);
                    out.push_str(&format!("## Assistant (`{model}`)\n\n"));
                }
                StoredRole::Tool => {
                    let tool = message.tool_name.as_deref().unwrap_or("tool");
                    out.push_str(&format!("## Tool result (`{tool}`)\n\n"));
                }
                StoredRole::System => out.push_str("## System\n\n"),
            }
            if let Some(thinking) = &message.thinking {
                out.push_str("<details><summary>Thinking</summary>\n\n");
                out.push_str(thinking);
                out.push_str("\n\n</details>\n\n");
            }
            out.push_str(message.content.trim_end());
            out.push_str("\n\n");
        }
        out
    }
}

/// Six-byte FNV-1a hash rendered as hex: enough to disambiguate two folders
/// with the same name without inventing a dependency.
fn short_hash(value: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", (hash & 0xffff_ffff) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn store() -> (tempfile::TempDir, SessionStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        (dir, store)
    }

    #[test]
    fn session_round_trips_through_the_store() {
        let (dir, store) = store();
        let workspace = dir.path();
        let mut session = Session::new(workspace, "ollama:qwen2.5-coder:32b");
        session.push_user("add a login function");
        session.push(StoredMessage::assistant("Sure, here is a plan.", "ollama:qwen2.5-coder:32b"));
        store.save(&session).unwrap();

        let loaded = store.load(workspace, &session.id).unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.messages[1].model.as_deref(), Some("ollama:qwen2.5-coder:32b"));
    }

    #[test]
    fn chat_history_restores_tool_calls_results_and_signatures() {
        let dir = workspace();
        let mut session = Session::new(dir.path(), "gemini");
        session.push_user("fix the bug");

        let mut first = ToolCallRequest::new("c1", "list_dir", "{}");
        first.thought_signature = Some("sig-1".to_string());
        session.push(StoredMessage::tool_call(first, "gemini"));
        session.push(StoredMessage::tool_result("c1", "list_dir", "{\"files\":[\"a.rs\"]}"));

        let mut second = ToolCallRequest::new("c2", "read_file", "{\"path\":\"a.rs\"}");
        second.thought_signature = Some("sig-2".to_string());
        session.push(StoredMessage::tool_call(second, "gemini"));
        session.push(StoredMessage::tool_result("c2", "read_file", "bug on line 4"));
        session.push(StoredMessage::assistant("Found it: line 4.", "gemini"));

        let history = session.chat_history();
        assert_eq!(history.len(), 5);
        assert_eq!(history[0].content, "fix the bug");
        assert_eq!(history[1].tool_calls.len(), 1);
        assert_eq!(
            history[1].tool_calls[0].thought_signature.as_deref(),
            Some("sig-1")
        );
        assert_eq!(history[2].role, Role::Tool);
        assert_eq!(history[2].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(history[3].tool_calls[0].name, "read_file");
        assert_eq!(history[4].content, "Found it: line 4.");
    }

    #[test]
    fn title_is_derived_from_the_first_user_message() {
        let dir = workspace();
        let mut session = Session::new(dir.path(), "m");
        assert!(session.auto_title);
        session.push_user("/review the parser please");
        assert_eq!(session.title, "review the parser please");
        assert!(!session.auto_title);
        session.rename("Custom title");
        assert_eq!(session.title, "Custom title");
    }

    #[test]
    fn long_titles_are_truncated() {
        let text = "x".repeat(200);
        let title = derive_title(&text);
        assert!(title.chars().count() <= 61);
        assert!(title.ends_with('\u{2026}'));
        assert_eq!(derive_title("   \n  "), "New session");
    }

    #[test]
    fn listing_is_newest_first_and_scoped_to_the_workspace() {
        let (dir, store) = store();
        let workspace = dir.path();
        let mut first = Session::new(workspace, "m");
        first.push_user("first");
        store.save(&first).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut second = Session::new(workspace, "m");
        second.push_user("second");
        store.save(&second).unwrap();

        let listed = store.list(workspace);
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, second.id);
        assert_eq!(store.most_recent(workspace).unwrap().id, second.id);

        // A different workspace sees nothing.
        let other = tempfile::tempdir().unwrap();
        assert!(store.list(other.path()).is_empty());
    }

    #[test]
    fn fork_keeps_messages_but_changes_identity() {
        let (dir, store) = store();
        let workspace = dir.path();
        let mut session = Session::new(workspace, "m");
        session.push_user("hello");
        store.save(&session).unwrap();

        let fork = store.fork(workspace, &session.id, None).unwrap();
        assert_ne!(fork.id, session.id);
        assert_eq!(fork.messages.len(), 1);
        assert!(fork.title.ends_with("(fork)"));
        assert_eq!(store.list(workspace).len(), 2);
    }

    #[test]
    fn deleting_a_session_removes_it_from_the_list() {
        let (dir, store) = store();
        let workspace = dir.path();
        let mut session = Session::new(workspace, "m");
        session.push_user("bye");
        store.save(&session).unwrap();
        store.delete(workspace, &session.id).unwrap();
        assert!(store.list(workspace).is_empty());
        // Deleting twice is not an error.
        store.delete(workspace, &session.id).unwrap();
    }

    #[test]
    fn turn_accounting_and_undo() {
        let dir = workspace();
        let mut session = Session::new(dir.path(), "m");
        session.push_user("do the thing");
        let turn = session.start_turn("model-a", PermissionMode::Ask);
        session.push(StoredMessage::assistant("done", "model-a"));
        session.finish_turn(turn, 1200, 340, 0.0);
        assert_eq!(session.total_tokens(), (1200, 340));

        assert!(session.truncate_last_turn());
        assert!(session.messages.len() == 1, "assistant reply should be undone");
        assert!(session.turns.is_empty());
    }

    #[test]
    fn export_markdown_includes_models_and_messages() {
        let dir = workspace();
        let mut session = Session::new(dir.path(), "m");
        session.push_user("hi");
        session.push(StoredMessage::assistant("hello there", "local:qwen"));
        let markdown = SessionStore::export_markdown(&session);
        assert!(markdown.starts_with("# "));
        assert!(markdown.contains("## You"));
        assert!(markdown.contains("`local:qwen`"));
        assert!(markdown.contains("hello there"));
    }

    #[test]
    fn workspace_directories_are_stable_and_unique() {
        let (_dir, store) = store();
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_eq!(store.workspace_dir(a.path()), store.workspace_dir(a.path()));
        assert_ne!(store.workspace_dir(a.path()), store.workspace_dir(b.path()));
    }

    #[test]
    fn work_state_survives_a_round_trip_and_builds_a_resume_prompt() {
        let (dir, store) = store();
        let workspace = dir.path();
        let mut session = Session::new(workspace, "m");
        session.push_user("add a login page");
        session.set_todos(vec![
            TodoItem {
                text: "read the router".to_string(),
                status: TodoStatus::Done,
            },
            TodoItem {
                text: "create the login page".to_string(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                text: "add tests".to_string(),
                status: TodoStatus::Pending,
            },
        ]);
        session.record_edited_file("src/routes.rs");
        session.record_edited_file("src/routes.rs");
        session.record_edited_file("src/login.rs");
        session.set_resume_note("Stopped after scaffolding the form; tests are next.");
        store.save(&session).unwrap();

        let loaded = store.load(workspace, &session.id).unwrap();
        assert_eq!(loaded.work.todos.len(), 3);
        assert_eq!(loaded.work.files, vec!["src/routes.rs", "src/login.rs"]);
        assert_eq!(loaded.open_task_count(), 2);
        let prompt = loaded.resume_prompt().unwrap();
        assert!(prompt.contains("create the login page"));
        assert!(prompt.contains("Do not start over"));
        assert!(prompt.contains("2 open"));
        assert!(prompt.contains("src/login.rs"));
        assert!(prompt.contains("tests are next"));
    }

    #[test]
    fn an_empty_session_has_no_resume_prompt() {
        let dir = workspace();
        let session = Session::new(dir.path(), "m");
        assert!(!session.has_work_state());
        assert!(session.resume_prompt().is_none());
    }
}
