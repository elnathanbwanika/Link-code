# Link Code — Full UI Build Prompt

## Mission

You are taking over a Rust/GTK4 project called **Link Code** — a native AI coding IDE for Ubuntu Linux. The backend is **100% complete** (agent loop, model providers, tools, session storage, config). The UI is a **placeholder** — just a window with a title label. Your job is to **build the entire UI** from scratch inside `link-ui`, wiring it up to the existing backend.

**Do not touch any crate except `link-ui` unless you need to add a dependency to its `Cargo.toml`.**

---

## Repository Layout

```
link-code/
├── link-app/        # Binary crate — main.rs only (already correct, do not change)
├── link-core/       # Agent loop, config, session, permissions, events (DO NOT EDIT)
├── link-models/     # Model providers: OpenAI, Anthropic, Gemini, Ollama (DO NOT EDIT)
├── link-tools/      # Tool implementations: bash, read/write/edit file, grep… (DO NOT EDIT)
└── link-ui/         # ← YOUR WORK. Currently a placeholder. Build everything here.
    ├── Cargo.toml
    ├── resources/
    │   ├── fonts/   # Syncopate, JetBrains Mono, Inter (already embedded)
    │   └── style.css
    └── src/
        ├── lib.rs
        ├── app.rs       # Application bootstrap (keep but extend)
        ├── window.rs    # Placeholder window (replace with full IDE layout)
        ├── style.rs     # Font/CSS install (keep)
        └── markdown.rs  # Markdown renderer helper (keep)
```

---

## Tech Stack & Versions

```toml
gtk = { package = "gtk4", version = "0.11.5", features = ["v4_10"] }
adw = { package = "libadwaita", version = "0.9.2", features = ["v1_4"] }
tokio = { version = "1", features = ["full"] }   # already in workspace
```

**Critical rule:** `adw::ApplicationWindow` **must never** have `set_titlebar()` called on it — this crashes with an `Adwaita-ERROR`. Always use `adw::ToolbarView` with `add_top_bar(&header_bar)` and `set_content(...)` instead.

**GTK main thread rule:** All widget manipulation must happen on the GTK main thread. Use `glib::MainContext::default().spawn_local(...)` to bring async results back to the UI thread from Tokio tasks.

---

## Design System

The CSS design tokens are already in `resources/style.css`. Use them:

```css
/* Colors */
@lc_bg            /* #000000 — window background */
@lc_panel         /* #0d0d0d — sidebar / agent panel */
@lc_card          /* #141414 — card backgrounds */
@lc_border        /* #232323 — dividers */
@lc_hover         /* #1c1c1c — hover states */
@lc_accent        /* #ff6600 — orange primary */
@lc_accent_dim    /* #b34700 — pressed/muted accent */
@lc_text          /* #f2f2f2 — primary text */
@lc_text_dim      /* #9a9a9a — secondary text */
@lc_text_disabled /* #5c5c5c — disabled text */
@lc_green         /* #2ecc71 */
@lc_red           /* #ff4d4d */
@lc_amber         /* #ffb020 */
@lc_blue          /* #4da3ff */
```

CSS classes already defined: `.lc-title`, `.lc-heading`, `.lc-mono`, `.lc-code`, `.lc-dim`, `.lc-accent`, `.lc-ok`, `.lc-warn`, `.lc-error`, `.lc-info`, `.lc-card`, `.lc-rail`, `.lc-header`, `.lc-sidebar`, `.lc-center`, `.lc-agent`, `.lc-status`.

Fonts available: `"Syncopate"` (headings), `"JetBrains Mono"` (code), `"Inter"` (body).

---

## Application ID

```rust
pub const APP_ID: &str = "org.linkecosystem.LinkCode";
```

---

## Full IDE Layout

```
┌─────────────────────────────────────────────────────────────────────┐
│  HeaderBar: [☰ menu] [LINK CODE logo] [⌘K command palette] [⋯]    │
├────┬────────────────────────────────────────┬────────────────────────┤
│    │                                        │                        │
│Rail│       File Browser Sidebar             │    Agent Chat Panel    │
│    │  (tree view of workspace files)        │  (messages + input)   │
│    │                                        │                        │
│    ├────────────────────────────────────────┤                        │
│    │                                        │                        │
│    │        Editor Area (center)            │                        │
│    │   (file tabs + source view or          │                        │
│    │    welcome screen if no file open)     │                        │
│    │                                        │                        │
│    ├────────────────────────────────────────┤                        │
│    │   Bottom Panel (terminal / output)     │                        │
├────┴────────────────────────────────────────┴────────────────────────┤
│  Status bar: [workspace path] [model badge] [mode chip] [token count]│
└─────────────────────────────────────────────────────────────────────┘
```

Use `gtk::Paned` (horizontal) for the sidebar ↔ center ↔ agent split. Use `gtk::Paned` (vertical) for center ↔ bottom. The rail is a vertical `gtk::Box` on the far left.

---

## Backend APIs You Must Wire Up

### 1. Config — `link_core::config`

```rust
use link_core::config::{Config, ConfigStore};

// Load config
let config_store = ConfigStore::default_location();
let config: Config = config_store.load_or_default();

// Save config
config_store.save(&config).unwrap();

// Key config fields:
config.last_workspace          // Option<PathBuf>
config.general.recent_workspaces  // Vec<PathBuf>
config.agent.default_mode      // PermissionMode
config.agent.max_tool_iterations  // u32
config.agent.shell_timeout_secs   // u64
config.agent.protect_secrets      // bool
config.agent.enable_web_tools     // bool
config.appearance.theme           // String ("link-code-dark")
config.editor.tab_width           // u32
config.editor.show_line_numbers   // bool
config.keybindings                // BTreeMap<String, String>

// Active model per role
config.active_model("chat")       // Option<&str>  — "ollama:qwen2.5-coder:32b"
config.set_active_model("chat", "ollama:qwen2.5-coder:32b");
// Roles: "chat", "fast", "completion"
```

### 2. Model Registry — `link_models::registry`

```rust
use link_models::registry::{ModelRegistry, ModelConfig, ProviderKind, ModelRole};

// Load from disk
let registry = ModelRegistry::load_or_default();
// or
let registry = ModelRegistry::default_location().load_or_default();

// Save
registry.save().unwrap();

// All configured models
let models: &[ModelConfig] = registry.models();

// A single ModelConfig has:
model.id            // String  — "ollama:qwen2.5-coder:32b"
model.label         // String  — "Qwen 2.5 Coder 32B"
model.provider      // ProviderKind  (OpenAiCompatible, Anthropic, Gemini, Ollama)
model.base_url      // String
model.model_name    // String  — the actual model name on the endpoint
model.roles         // Vec<ModelRole>  — Chat, Fast, Completion
model.context_window // usize
model.has_api_key   // bool  — key is in OS keyring, not here

// ProviderKind variants: OpenAiCompatible, Anthropic, Gemini, Ollama
// ProviderKind methods:
ProviderKind::label()         // "Ollama native"
ProviderKind::description()   // human description
ProviderKind::default_base_url() // preset URL
ProviderKind::is_cloud()      // bool
ProviderKind::requires_api_key(base_url) // bool
```

### 3. Provider Factory — building a live provider

```rust
use std::sync::Arc;
use link_models::registry::{ModelConfig, ProviderKind};
use link_models::provider::{ModelProvider, ProviderFactory};

// Build a provider from a ModelConfig:
let provider: Arc<dyn ModelProvider> = ProviderFactory::build(&model_config, api_key_opt)?;

// ModelProvider methods:
provider.id()              // &str
provider.label()           // &str  — "ollama · qwen2.5-coder:32b"
provider.capabilities()    // Capabilities { streaming, tool_calling, vision, … }
provider.context_window()  // usize
provider.list_models()     // async → ModelResult<Vec<ModelInfo>>
provider.stream_chat(req)  // async → ModelResult<ModelStream>

// ModelStream is: Pin<Box<dyn Stream<Item = ModelResult<StreamEvent>> + Send>>
// StreamEvent variants:
StreamEvent::TokenDelta { text: String }
StreamEvent::ThinkingDelta { text: String }
StreamEvent::ToolCallDelta { index, id, name, arguments }
StreamEvent::FinishReason(FinishReason)
StreamEvent::Usage(Usage { prompt_tokens, completion_tokens })
```

### 4. Secrets / API Keys — `link_core::secrets`

```rust
use link_core::secrets;

// Store an API key in the OS keyring
secrets::set_api_key(&model_config.id, &api_key)?;

// Retrieve
let key: Option<String> = secrets::get_api_key(&model_config.id)?;

// Delete
secrets::delete_api_key(&model_config.id)?;
```

### 5. Agent Loop — `link_core::agent`

```rust
use std::sync::Arc;
use link_core::agent::{AgentRunner, AgentOptions, AgentControl, BASE_SYSTEM_PROMPT};
use link_core::event::{ChannelSink, AgentEvent};
use link_core::permission::{PermissionEngine, PermissionMode};
use link_core::tool::ToolContext;
use link_core::project::ProjectMemory;
use link_tools::builtin_registry;

// 1. Build the event channel (send events to UI)
let (sink, mut rx) = ChannelSink::new();

// 2. Build tool registry
let registry = builtin_registry(config.agent.enable_web_tools);

// 3. Build tool context (sandbox to workspace)
let tool_ctx = ToolContext::new(workspace_path.clone());

// 4. Load project memory (LINK.md, .link/rules/*.md)
let project_memory = ProjectMemory::load(&workspace_path);

// 5. Build permission engine
let engine = PermissionEngine::new(config.agent.default_mode, &config.permissions);

// 6. Build agent options
let options = AgentOptions {
    model_id: model_config.id.clone(),
    model_label: provider.label().to_string(),
    system_prompt_override: None,
    project_memory,
    max_iterations: config.agent.max_tool_iterations,
    max_output_tokens: config.agent.max_output_tokens,
    temperature: None,
    auto_compact_threshold: config.agent.auto_compact_threshold,
    generate_suggestions: config.agent.generate_suggestions,
    use_tools: provider.capabilities().tool_calling,
    context_window: provider.context_window(),
};

// 7. Build AgentControl (for cancellation)
let control = AgentControl::new();
let cancel_token = control.begin_turn();

// 8. Build runner
let mut runner = AgentRunner::new(
    provider,
    registry,
    tool_ctx,
    Arc::new(sink),
    Arc::new(approver),   // see Approver below
    engine,
    options,
);

// 9. Send user message
runner.send_user_message(&user_text, attachments);

// 10. Run one turn (spawn on tokio)
tokio::spawn(async move {
    runner.run_turn(cancel_token).await;
});

// 11. Drain events on GTK main thread
glib::MainContext::default().spawn_local(async move {
    while let Some(event) = rx.recv().await {
        handle_agent_event(event, &widgets);
    }
});

// Cancel a running turn:
control.cancel();
```

### 6. Approver trait — permission dialogs

```rust
use link_core::agent::Approver;
use link_core::permission::{PermissionRequest, PermissionDecision};

// You must implement this trait for the UI:
#[async_trait::async_trait]
impl Approver for MyApprover {
    async fn approve(&self, request: PermissionRequest) -> PermissionDecision {
        // Show a modal dialog asking the user to Allow/Deny/Allow-always/Deny-always
        // PermissionRequest has: .tool_name, .action_kind, .path (Option), .command (Option)
        // PermissionDecision variants: Allow, Deny, AllowAlways, DenyAlways
        // For FullAuto mode: return PermissionDecision::Allow immediately
        todo!()
    }
}
```

The approver MUST bridge from an async context back to GTK. Use a `tokio::sync::oneshot` channel: the Approver sends a oneshot sender into a `glib::MainContext::spawn_local` closure that shows the dialog, and awaits the reply.

### 7. AgentEvent — render these in the chat panel

```rust
// Every event the agent loop emits (in order during a turn):
AgentEvent::TurnStarted { turn_id: Uuid, model: String }

AgentEvent::BlockStarted { block_id: Uuid }
AgentEvent::TokenDelta  { block_id: Uuid, text: String }  // append to current assistant bubble
AgentEvent::ThinkingDelta { block_id: Uuid, text: String } // reasoning/thinking tokens (collapsible)
AgentEvent::TextComplete  { block_id: Uuid, text: String } // full final text of the block

AgentEvent::ToolRequested { call_id: Uuid, tool_name: String, arguments: serde_json::Value }
AgentEvent::ToolStatusChanged { call_id: Uuid, status: ToolStatus } // Pending→Running→Done/Failed/Denied
AgentEvent::ToolOutputDelta { call_id: Uuid, text: String } // streaming tool output
AgentEvent::ToolResult { call_id: Uuid, tool_name: String, output: String, status: ToolStatus }

AgentEvent::DiffProposed { call_id: Uuid, diff: FileDiff }  // show a diff view
AgentEvent::ApprovalRequested { request_id: Uuid, request: PermissionRequest } // show modal
AgentEvent::ApprovalResolved { request_id: Uuid, decision: PermissionDecision }

AgentEvent::PlanProposed { steps: Vec<String> }
AgentEvent::TodosUpdated { items: Vec<TodoItem> } // TodoItem { text, status: Pending/InProgress/Done }

AgentEvent::SuggestionsReady { suggestions: Vec<String> } // next-step chips

AgentEvent::ContextUpdated { used_tokens: usize, total_tokens: usize, percent: f64 }
AgentEvent::Usage { prompt_tokens: usize, completion_tokens: usize, tokens_per_second: f64, latency_ms: u64, cost_usd: f64 }

AgentEvent::Notice  { message: String }         // dim system note
AgentEvent::ModelChanged { model: String }
AgentEvent::Error   { block_id: Option<Uuid>, kind: ErrorKind, message: String, detail: Option<String> }
AgentEvent::Done    { reason: DoneReason }       // Completed / IterationLimit / Cancelled / Errored

AgentEvent::TurnFinished { turn_id: Uuid, prompt_tokens: usize, completion_tokens: usize,
                           tokens_per_second: f64, latency_ms: u64, cost_usd: f64 }
```

### 8. Session Storage — `link_core::session`

```rust
use link_core::session::{Session, SessionStore, StoredMessage, StoredRole};

// Load all sessions for a workspace
let store = SessionStore::for_workspace(&workspace_path);
let sessions: Vec<Session> = store.list()?;

// Load one session
let session: Session = store.load(&session_id)?;

// Save
store.save(&session)?;

// Delete
store.delete(&session_id)?;

// A Session has:
session.id           // String (UUID)
session.title        // String
session.model        // String
session.created_at   // DateTime<Utc>
session.updated_at   // DateTime<Utc>
session.messages     // Vec<StoredMessage>
session.turns        // Vec<Turn>
session.auto_title   // bool — true means title was auto-generated

// StoredMessage fields:
msg.id           // Uuid
msg.role         // StoredRole: System, User, Assistant, Tool
msg.content      // String
msg.thinking     // Option<String>  — reasoning tokens
msg.model        // Option<String>  — which model produced this
msg.tool_name    // Option<String>
msg.attachments  // Vec<String>  — workspace-relative paths
msg.created_at   // DateTime<Utc>
```

### 9. Project Memory — `link_core::project`

```rust
use link_core::project::ProjectMemory;

let memory = ProjectMemory::load(&workspace_path);
// Reads LINK.md and .link/rules/*.md from the workspace root
// memory.to_string() gives the concatenated rules text
```

### 10. Permission Mode

```rust
use link_core::permission::PermissionMode;

// Variants (cycle with Shift+Tab):
PermissionMode::Ask               // default: prompt before every write/bash
PermissionMode::AutoAcceptEdits   // apply file edits automatically; bash still prompts
PermissionMode::Plan              // read-only; agent only proposes plans
PermissionMode::FullAuto          // no prompts (red status bar warning)

PermissionMode::next()   // cycles Ask → AutoAcceptEdits → Plan → FullAuto → Ask
PermissionMode::label()  // "Ask" / "Auto-accept edits" / "Plan" / "Full auto"
PermissionMode::description()  // tooltip text
PermissionMode::is_read_only()      // bool
PermissionMode::auto_accepts_edits() // bool
```

---

## Features to Build (Priority Order)

### P0 — Must have (app is unusable without these)

#### 1. IDE Shell Layout
Replace the placeholder `window.rs` with the full layout described above. Use `adw::NavigationSplitView` or `gtk::Paned` for the three-column layout.

**Structure in Rust:**
```rust
// Top level inside ToolbarView:
let root = gtk::Box::new(Orientation::Vertical, 0);

// Main content area (horizontal paned: sidebar | center+bottom | agent)
let h_paned = gtk::Paned::new(Orientation::Horizontal);

// Left: rail (56px fixed) + sidebar (240px default)
let left = gtk::Box::new(Orientation::Horizontal, 0);
left.append(&build_rail());
left.append(&build_sidebar(&workspace));

// Center + bottom (vertical paned)
let v_paned = gtk::Paned::new(Orientation::Vertical);
v_paned.set_start_child(Some(&build_editor_area()));
v_paned.set_end_child(Some(&build_bottom_panel()));
v_paned.set_position(500);

h_paned.set_start_child(Some(&left));
h_paned.set_end_child(Some(&build_agent_panel()));
h_paned.set_position(300);

root.append(&h_paned);
root.append(&build_status_bar());

let toolbar_view = adw::ToolbarView::new();
toolbar_view.add_top_bar(&build_header_bar());
toolbar_view.set_content(Some(&root));
window.set_content(Some(&toolbar_view));
```

#### 2. Agent Chat Panel (RIGHT COLUMN)
The most important feature. This panel has three parts:

**Message list (scrollable, top):**
- Each turn is a visual block:
  - User bubble (right-aligned, `@lc_accent` left border, dark background)
  - Assistant bubble (left-aligned) — streams token-by-token from `TokenDelta` events
  - Thinking block (collapsible `<details>`-style, dim text, shows `ThinkingDelta` text)
  - Tool call cards: tool name + status chip (Pending/Running/Done/Failed/Denied) + output (collapsible)
  - Diff view on `DiffProposed` events (unified diff, syntax highlighted)
  - Error blocks (red border, `ErrorKind::headline()` as title, detail collapsible)
  - Todo list on `TodosUpdated` (checklist with ○ ◑ ● markers from `TodoStatus::marker()`)
  - Next-step suggestion chips on `SuggestionsReady` (clickable → prefill input)

**Implementation pattern for streaming:**
```rust
// On TokenDelta: find or create the assistant label for block_id, append text
// On TextComplete: render final text through markdown.rs
// Use glib::MainContext::spawn_local to process events on GTK thread
```

**Input bar (bottom, fixed):**
```
┌──────────────────────────────────────────────────────┐
│  [📎 @file]  [# pin]  textarea (auto-grows)          │
│  [Mode: Ask ▼]                           [Stop/Send] │
└──────────────────────────────────────────────────────┘
```
- `gtk::TextView` with `set_wrap_mode(gtk::WrapMode::Word)` for multi-line input
- `Enter` sends, `Shift+Enter` adds newline
- **@file attachment**: type `@` to open a file picker, attach workspace-relative path
- **Mode chip** (`[Mode: Ask ▼]`): clicking cycles `PermissionMode` with `Shift+Tab` shortcut
- **Send button**: `gtk::Button` with accent color; becomes **Stop** (cancels via `AgentControl::cancel()`) while agent is running
- **Context bar**: thin progress bar above input showing `ContextUpdated` token usage

#### 3. File Browser Sidebar (LEFT PANEL)
- `gtk::TreeView` or `gtk::ListView` showing the workspace directory tree
- Expandable folders, file icons by extension using `gtk::Image` from MIME type
- Clicking a file opens it in the editor area
- Right-click context menu: Open, Reveal in Files, Copy Path
- Respect `.gitignore` (use `link_tools::GlobTool` or walk the FS manually)
- Keyboard: arrows to navigate, Enter to open

#### 4. Model Picker (Header bar button)
A `adw::Dialog` (or popover) with:
- Dropdown showing all models from `ModelRegistry` where `role == ModelRole::Chat`
- "Add model" button → opens the Model Config dialog
- Shows active model label in the header bar button

#### 5. Status Bar
Fixed 26px bar at the very bottom:
```
[~/projects/my-app]  [ollama · qwen2.5 ▼]  [Ask]  [4,231 / 32,768 tokens]  [⚡ $0.0012]
```
- Workspace path (clickable → open folder picker)
- Model badge (clickable → model picker)
- Mode chip (clickable → cycle mode)
- Token count (from `ContextUpdated` events)
- Cost (from `Usage` events, only shown if > $0)

---

### P1 — Should have

#### 6. Settings Dialog (`Ctrl+,`)
`adw::PreferencesDialog` with pages:

**General page:**
- Reopen last workspace on launch (bool toggle)
- Recent workspaces list

**Agent page:**
- Default permission mode (dropdown: Ask / Auto-accept edits / Plan / Full auto)
- Max tool iterations (spinbutton, 1–100)
- Shell timeout in seconds (spinbutton, 5–3600)
- Enable web tools (toggle)
- Protect secrets (toggle)
- Generate next-step suggestions (toggle)

**Appearance page:**
- Editor font size (spinbutton)
- Show line numbers (toggle)
- Editor ligatures (toggle)

**Keybindings page:**
- Table showing action → keybinding (read-only for now)

#### 7. Models Management Dialog (`Ctrl+M`)
`adw::Dialog` with:

**Model list**: each row shows:
- Provider icon, model label, ID, roles (Chat / Fast / Completion chips)
- Edit and Delete buttons

**Add/Edit model form:**
```
Provider:    [Ollama native ▼]
Name:        [My Qwen Model]
Base URL:    [http://localhost:11434] [Fetch models ▼]
Model:       [qwen2.5-coder:32b]
Roles:       [✓ Chat] [ Fast] [ Completion]
Context:     [32768] tokens
API Key:     [•••••••••••] [Show] [Clear]
             [Test connection]
```
- "Fetch models" hits `provider.list_models()` and populates the model dropdown
- "Test connection" calls `provider.stream_chat()` with a minimal ping request and shows `ConnectionReport::summary()`
- API key stored via `link_core::secrets::set_api_key()`

#### 8. Command Palette (`Ctrl+K` / `Ctrl+Shift+P`)
`adw::Dialog` with a search entry and filtered list:
- Open folder
- New session
- Switch model
- Settings
- Toggle sidebar / agent panel / bottom panel
- Change permission mode
Filter with fuzzy matching as user types.

#### 9. Session History (Sidebar tab)
Add a "History" tab to the sidebar:
- List of past sessions for the current workspace (from `SessionStore::list()`)
- Each row: session title, relative date, model used
- Click to load session (restores `messages` into the chat panel)
- Right-click: Rename, Export as Markdown, Delete

---

### P2 — Nice to have (build if time allows)

#### 10. Editor Area (CENTER COLUMN)
- File tabs: `gtk::Notebook` or a custom tab bar
- For viewing/editing files: use `gtk::TextView` with monospace font (`JetBrains Mono`)
- Syntax highlighting: use the `sourceview5` crate (`sourceview5 = "0.8"`) with `gtk::SourceView`
- Show line numbers, current-line highlight
- Save with `Ctrl+S`

#### 11. Bottom Panel
`gtk::Notebook` with tabs:
- **Terminal**: embed a `vte4` terminal widget (`vte4 = "0.8"`) running the user's shell, cwd = workspace
- **Output**: scrollable log of agent tool outputs (read-only `gtk::TextView`)
- **Problems**: list of errors/warnings (populated by bash tool exit codes)

#### 12. Approval Modal
When `AgentEvent::ApprovalRequested` fires, show an `adw::AlertDialog`:
```
Link Code wants to run a shell command

  $ rm -rf ./build

[Deny]  [Deny always]  [Allow]  [Allow always]
```
The `Approver` implementation sends the user's choice back via `tokio::sync::oneshot`.

---

## File Structure to Create

Create these files inside `link-ui/src/`:

```
src/
├── lib.rs              # pub mod declarations
├── app.rs              # Keep existing, extend with Tokio runtime setup
├── window.rs           # Full IDE window (replace placeholder)
├── style.rs            # Keep existing
├── markdown.rs         # Keep existing
├── panels/
│   ├── mod.rs
│   ├── agent.rs        # Agent chat panel (P0)
│   ├── sidebar.rs      # File browser + session history (P0 + P1)
│   ├── editor.rs       # Editor area with tabs (P2)
│   └── bottom.rs       # Terminal + output panel (P2)
├── dialogs/
│   ├── mod.rs
│   ├── models.rs       # Model management dialog (P1)
│   ├── settings.rs     # Settings dialog (P1)
│   ├── approval.rs     # Permission approval modal (P2)
│   └── command_palette.rs  # Command palette (P1)
├── widgets/
│   ├── mod.rs
│   ├── chat_message.rs # Individual message bubble widget
│   ├── tool_card.rs    # Tool call card widget
│   ├── status_bar.rs   # Bottom status bar
│   └── model_badge.rs  # Model selector button
└── state.rs            # Shared app state (Arc<Mutex<AppState>>)
```

---

## Shared State

```rust
// state.rs
use std::sync::{Arc, Mutex};
use std::path::PathBuf;

pub struct AppState {
    pub workspace: PathBuf,
    pub config_store: link_core::config::ConfigStore,
    pub config: link_core::config::Config,
    pub registry: link_models::registry::ModelRegistry,
    pub session: Option<link_core::session::Session>,
    pub agent_control: link_core::agent::AgentControl,
    pub mode: link_core::permission::PermissionMode,
    pub context_used: usize,
    pub context_total: usize,
}

pub type SharedState = Arc<Mutex<AppState>>;
```

---

## Tokio Runtime

The GTK main loop and Tokio must coexist. Set up in `app.rs`:

```rust
// In run_with_paths(), before application.run():
let rt = tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("tokio runtime");
let _guard = rt.enter();

// Store the runtime handle somewhere accessible (e.g. thread_local! or pass it around)
// Use glib::MainContext::default().spawn_local(...) for GTK-thread async
// Use rt.spawn(...) for background work (agent loop runs here)
```

---

## Keyboard Shortcuts (register in `app.rs`)

```
Ctrl+K / Ctrl+Shift+P  → Command palette
Ctrl+O                  → Open folder
Ctrl+P                  → Quick open file
Ctrl+S                  → Save current file
Ctrl+L                  → Focus agent input
Ctrl+B                  → Toggle sidebar
Ctrl+J                  → Toggle bottom panel
Ctrl+\                  → Toggle agent panel
Ctrl+,                  → Settings
Ctrl+M                  → Models dialog
Shift+Tab               → Cycle permission mode
Escape                  → Cancel running agent turn
```

---

## Important Implementation Notes

1. **Never block the GTK thread.** All async work (agent loop, provider calls, file I/O) goes on Tokio. Use `glib::MainContext::default().spawn_local(async { ... })` to update widgets from async callbacks.

2. **Agent loop runs on Tokio, events come back via channel.** The pattern is:
   ```rust
   let (sink, mut rx) = ChannelSink::new();
   rt.spawn(async move { runner.run_turn(cancel_token).await; });
   glib::MainContext::default().spawn_local(async move {
       while let Some(event) = rx.recv().await {
           // update GTK widgets here — we're on the main thread
           process_event(&widgets, event);
       }
   });
   ```

3. **Markdown rendering** — `markdown.rs` already exists. It converts markdown text to GTK widget trees (labels with Pango markup). Use it for assistant message bubbles.

4. **Streaming text** — For `TokenDelta` events, **append text directly to a `gtk::Label`** using `label.set_text(&accumulated_text)`. Do not rebuild the widget on each delta.

5. **`adw::ToolbarView`** is the only correct way to add a header bar to `adw::ApplicationWindow`. Never use `set_titlebar()`.

6. **Permission approval** requires careful async bridging:
   ```rust
   // In Approver::approve() — called from Tokio thread:
   let (tx, rx) = tokio::sync::oneshot::channel();
   glib::MainContext::default().spawn_local(async move {
       let dialog = build_approval_dialog(&request);
       dialog.connect_response(move |_, response| {
           let _ = tx.send(response_to_decision(response));
       });
       dialog.present();
   });
   rx.await.unwrap_or(PermissionDecision::Deny)
   ```

7. **CSS classes** — Apply existing `.lc-*` classes to your widgets with `widget.add_css_class("lc-card")`.

8. **File watching** — When the agent edits files, refresh the editor view. Use `std::fs::read_to_string` on `ToolResult` events that mention file paths.

---

## Build & Test

```bash
# In the link-code/ workspace directory:
export PATH="$HOME/.cargo/bin:$PATH"

# Check everything compiles:
cargo check --all-targets

# Build release binary:
cargo build --release

# Run the binary:
./target/release/link-code /path/to/some/project

# The binary is at link-app/src/main.rs — it calls link_ui::app::run_with_paths(paths)
```

**Required system packages on Ubuntu:**
```bash
sudo apt-get install libgtk-4-dev libadwaita-1-dev libsecret-1-dev pkg-config
# For sourceview (P2 editor):
sudo apt-get install libgtksourceview-5-dev
# For VTE terminal (P2 terminal):
sudo apt-get install libvte-2.91-gtk4-dev
```

---

## Definition of Done

The app is complete when a user can:

1. Launch `link-code ~/my-project` and see the full IDE layout (not a placeholder)
2. See the file tree of `~/my-project` in the left sidebar
3. Configure at least one AI model (Ollama localhost or API key for Anthropic/Gemini/OpenAI) via the Models dialog
4. Type a message in the agent panel and get a streaming AI response
5. Watch the agent read and edit files in the workspace (tool cards with status)
6. Approve or deny shell commands via the approval modal
7. See the current model, permission mode, and token count in the status bar
8. Open Settings (`Ctrl+,`) and change agent options

That's the bar. Everything else (editor, terminal, command palette, session history) is bonus.
