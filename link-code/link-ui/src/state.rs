//! Shared, GTK-thread-local application state.
//!
//! GTK is single threaded, so everything here uses `Rc` + `RefCell`/`Cell`
//! rather than `Arc<Mutex<_>>`. Long-running work (model streams, shell
//! commands) runs on the tokio runtime exposed by [`runtime`]; results come
//! back to this state through `glib::MainContext::spawn_local`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use gtk::prelude::*;
use link_core::agent::AgentHandle;
use link_core::config::{config_dir, Config, ConfigStore};
use link_core::event::TodoItem;
use link_core::permission::PermissionMode;
use link_core::secrets::{self, SecretStore};
use link_core::session::{Session, SessionStore};
use link_models::registry::{ApiKeyPool, ModelConfig, ModelRegistry, ModelRole, ProviderKind};
use link_models::{
    KeyPool, KeyPoolObserver, KeyPoolUpdate, KeySlot, ModelProvider, RotatingProvider,
};
use uuid::Uuid;

use crate::widgets::chat_message::BlockView;
use crate::widgets::tool_chip::ToolChip;

/// Keyring entry holding the proxy password. `config.json` never contains it.
pub const PROXY_SECRET_KEY: &str = "proxy-password";

/// The process-wide tokio runtime. Agent loops, provider calls and shell
/// commands run here; the GTK main loop is never blocked.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("link-code-async")
            .enable_all()
            .build()
            .expect("could not start the tokio runtime")
    })
}

/// `~/.config/link-code/models.json`
pub fn registry_path() -> PathBuf {
    config_dir().join("models.json")
}

/// Load the model registry. The app ships with no built-in models: an empty
/// registry is normal and the UI prompts the user to add their own endpoint.
pub fn load_registry() -> ModelRegistry {
    let path = registry_path();
    let mut registry = if let Ok(text) = std::fs::read_to_string(&path) {
        match serde_json::from_str::<ModelRegistry>(&text) {
            Ok(registry) => registry,
            Err(error) => {
                tracing::warn!(?error, path = %path.display(), "models.json unreadable; starting empty");
                ModelRegistry::new()
            }
        }
    } else {
        ModelRegistry::new()
    };

    // Older builds seeded a list of preset endpoints. Drop those so every
    // model in the list is one the user actually configured.
    if remove_legacy_presets(&mut registry) {
        if let Err(error) = save_registry(&registry) {
            tracing::warn!(%error, "could not rewrite models.json after removing presets");
        }
    }
    registry
}

/// The exact endpoints earlier versions inserted on first run.
fn is_legacy_preset(model: &ModelConfig) -> bool {
    let base = model.base_url.trim_end_matches('/');
    let known: &[(ProviderKind, &str, &str)] = &[
        (ProviderKind::Ollama, "http://localhost:11434", "qwen2.5-coder:7b"),
        (ProviderKind::OpenAiCompatible, "http://localhost:1234/v1", "local-model"),
        (ProviderKind::OpenAiCompatible, "http://localhost:8080/v1", "local-model"),
        (ProviderKind::OpenAiCompatible, "https://openrouter.ai/api/v1", "anthropic/claude-3.5-sonnet"),
        (ProviderKind::OpenAiCompatible, "https://api.openai.com/v1", "gpt-4o-mini"),
        (ProviderKind::Anthropic, "https://api.anthropic.com", "claude-sonnet-4-20250514"),
        (ProviderKind::Gemini, "https://generativelanguage.googleapis.com", "gemini-2.0-flash"),
        (ProviderKind::OpenAiCompatible, "https://api.deepseek.com/v1", "deepseek-chat"),
    ];
    // Only remove untouched presets: a key or recorded usage means the user
    // adopted that entry and it should not disappear.
    model.stats.requests == 0
        && !model.has_api_key
        && known.iter().any(|(provider, url, id)| {
            model.provider == *provider && base == *url && model.model_id == *id
        })
}

fn remove_legacy_presets(registry: &mut ModelRegistry) -> bool {
    let before = registry.models.len();
    let removed: Vec<String> = registry
        .models
        .iter()
        .filter(|model| is_legacy_preset(model))
        .map(|model| model.id.clone())
        .collect();
    if removed.is_empty() {
        return false;
    }
    for id in &removed {
        registry.remove(id);
    }
    if registry.roles.get(ModelRole::Chat.key()).is_none() {
        if let Some(first) = registry.models.first().map(|model| model.id.clone()) {
            registry.set_role(ModelRole::Chat, &first);
        }
    }
    tracing::info!(removed = before - registry.models.len(), "removed built-in model presets");
    true
}

pub fn save_registry(registry: &ModelRegistry) -> Result<(), String> {
    let path = registry_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(registry).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())
}

/// The widgets every event handler needs. Built once per window.
pub struct Widgets {
    pub window: adw::ApplicationWindow,
    pub h_paned: gtk::Paned,
    pub agent_paned: gtk::Paned,
    pub v_paned: gtk::Paned,
    pub sidebar_root: gtk::Box,
    pub editor_root: gtk::Box,
    pub bottom_root: gtk::Box,
    pub agent_root: gtk::Box,
    pub rail: gtk::Box,

    pub sidebar: Rc<crate::panels::sidebar::SidebarPanel>,
    pub editor: Rc<crate::panels::editor::EditorPane>,
    pub bottom: Rc<crate::panels::bottom::BottomPanel>,

    pub chat_list: gtk::Box,
    pub chat_scroll: gtk::ScrolledWindow,
    /// True while the conversation should follow new agent output. Cleared
    /// when the reader scrolls up, restored when they reach the bottom again.
    pub chat_following: Rc<Cell<bool>>,
    pub activity: gtk::Box,
    pub activity_spinner: gtk::Spinner,
    pub activity_label: gtk::Label,
    pub input: gtk::TextView,
    pub send_button: gtk::Button,
    pub stop_button: gtk::Button,
    pub mode_button: gtk::Button,
    pub composer_model_button: gtk::Button,
    pub context_bar: gtk::ProgressBar,
    pub context_label: gtk::Label,
    pub tasks: Rc<crate::panels::agent::TaskPanel>,
    pub suggestions_box: gtk::Box,
    pub attachments_box: gtk::Box,
    pub attachments_scroll: gtk::ScrolledWindow,

    pub status_workspace: gtk::Label,
    pub status_tokens: gtk::Label,
    pub status_cost: gtk::Label,
    pub status_cost_sep: gtk::Label,
    pub rail_explorer: gtk::Button,
    pub rail_history: gtk::Button,
    pub rail_agent: gtk::Button,
    pub rail_bottom: gtk::Button,
    pub toasts: adw::ToastOverlay,
}

/// Everything a window shares with its callbacks.
pub struct UiState {
    pub config_store: ConfigStore,
    pub config: RefCell<Config>,
    pub registry: RefCell<ModelRegistry>,
    pub workspace: RefCell<Option<PathBuf>>,
    pub session_store: SessionStore,
    pub session: RefCell<Option<Session>>,
    pub secrets: Arc<dyn SecretStore>,
    pub agent_handle: RefCell<Option<AgentHandle>>,
    pub busy: Cell<bool>,
    pub mode: Cell<PermissionMode>,
    pub context_used: Cell<usize>,
    pub context_max: Cell<usize>,
    pub cost: Cell<f64>,
    /// Width of the agent column when it was last hidden, restored on show.
    pub agent_width: Cell<i32>,

    pub widgets: RefCell<Option<Rc<Widgets>>>,
    pub blocks: RefCell<HashMap<Uuid, Rc<BlockView>>>,
    /// Creation order of assistant blocks in the current turn, so the saved
    /// message keeps multiple text blocks in the order they streamed.
    pub block_order: RefCell<Vec<Uuid>>,
    pub tools: RefCell<HashMap<String, Rc<ToolChip>>>,
    /// Compact chat chips for streamed file edits, keyed by tool call id.
    pub edits: RefCell<HashMap<String, Rc<crate::widgets::edit_chip::EditChip>>>,
    pub attachments: RefCell<Vec<String>>,

    /// Text accumulated during the current turn, saved when it finishes.
    pub turn_text: RefCell<String>,
    pub turn_thinking: RefCell<String>,
    pub turn_model: RefCell<String>,
}

impl UiState {
    pub fn new(config_store: ConfigStore, mut config: Config) -> Rc<Self> {
        let mode = config.agent.default_mode;
        let secrets = secrets::default_store();
        // The proxy password is a credential: it lives in the secret store, not
        // in plaintext `config.json`, and is folded back in for this session.
        if let Some(password) = secrets.get(PROXY_SECRET_KEY) {
            config.proxy.password = password;
        }
        // Every provider, discovery probe and web tool reads this global when
        // it builds its HTTP client, so install the saved proxy up front.
        link_models::proxy::set(config.proxy.clone());
        let mut registry = load_registry();
        // Fold any single keys written by older builds into per-base-URL pools.
        let mut registry_dirty =
            link_core::secrets::migrate_model_keys(&mut registry, secrets.as_ref());
        // Keep `has_api_key` in sync with the actual secret store and pools, not
        // just whatever was last written to models.json.
        {
            let keyed_pools: std::collections::HashSet<String> = registry
                .key_pools
                .iter()
                .filter(|(_, pool)| pool.has_usable_key())
                .map(|(id, _)| id.clone())
                .collect();
            for model in &mut registry.models {
                let pool_id = model.key_pool_id();
                let has = (!pool_id.is_empty() && keyed_pools.contains(&pool_id))
                    || secrets.get_model_key(&model.id).is_some();
                if model.has_api_key != has {
                    model.has_api_key = has;
                    registry_dirty = true;
                }
            }
        }
        let state = Rc::new(Self {
            config_store,
            config: RefCell::new(config),
            registry: RefCell::new(registry),
            workspace: RefCell::new(None),
            session_store: SessionStore::default_location(),
            session: RefCell::new(None),
            secrets,
            agent_handle: RefCell::new(None),
            busy: Cell::new(false),
            mode: Cell::new(mode),
            context_used: Cell::new(0),
            context_max: Cell::new(0),
            cost: Cell::new(0.0),
            agent_width: Cell::new(420),
            widgets: RefCell::new(None),
            blocks: RefCell::new(HashMap::new()),
            block_order: RefCell::new(Vec::new()),
            tools: RefCell::new(HashMap::new()),
            edits: RefCell::new(HashMap::new()),
            attachments: RefCell::new(Vec::new()),
            turn_text: RefCell::new(String::new()),
            turn_thinking: RefCell::new(String::new()),
            turn_model: RefCell::new(String::new()),
        });
        if registry_dirty {
            let registry = state.registry.borrow().clone();
            if let Err(error) = save_registry(&registry) {
                tracing::warn!(%error, "could not persist the migrated API-key pools");
            }
        }
        state
    }

    /// Convenience accessor; callbacks run after the widgets are installed.
    pub fn widgets(&self) -> Option<Rc<Widgets>> {
        self.widgets.borrow().clone()
    }

    pub fn toast(&self, message: &str) {
        if let Some(widgets) = self.widgets() {
            widgets.toasts.add_toast(adw::Toast::new(message));
        }
    }

    // ---- workspace ------------------------------------------------------

    pub fn workspace_path(&self) -> Option<PathBuf> {
        self.workspace.borrow().clone()
    }

    pub fn set_workspace(&self, path: &Path) -> Result<(), String> {
        let path = path
            .canonicalize()
            .map_err(|error| format!("could not open {}: {error}", path.display()))?;
        if !path.is_dir() {
            return Err(format!("{} is not a folder", path.display()));
        }

        *self.workspace.borrow_mut() = Some(path.clone());
        *self.session.borrow_mut() = None;

        {
            let mut config = self.config.borrow_mut();
            config.remember_workspace(&path);
        }
        let _ = self.save_config();
        Ok(())
    }

    pub fn save_config(&self) -> Result<(), String> {
        let mut config = self.config.borrow().clone();
        // Credentials never go to `config.json`; the keyring keeps them.
        if !config.proxy.password.is_empty() {
            if let Err(error) = self.secrets.set(PROXY_SECRET_KEY, &config.proxy.password) {
                tracing::warn!(%error, "could not save the proxy password");
            }
        } else if let Err(error) = self.secrets.delete(PROXY_SECRET_KEY) {
            // No keyring (or nothing stored) is normal; the password is simply
            // absent. Only surface it at debug level.
            tracing::debug!(%error, "no saved proxy password to clear");
        }
        config.proxy.password.clear();
        // Settings apply immediately: the next HTTP client picks this up.
        link_models::proxy::set(self.config.borrow().proxy.clone());
        self.config_store
            .save(&config)
            .map_err(|error| error.to_string())
    }

    // ---- models ---------------------------------------------------------

    /// The model configured for the `chat` role, with a sensible fallback.
    pub fn chat_model(&self) -> Option<ModelConfig> {
        let config = self.config.borrow();
        let registry = self.registry.borrow();
        let role_id = registry.roles.get(ModelRole::Chat.key()).cloned();
        let active = config
            .active_model("chat")
            .map(str::to_string)
            .or(role_id)
            .or_else(|| registry.models.first().map(|model| model.id.clone()))?;
        registry
            .get(&active)
            .cloned()
            .or_else(|| registry.models.first().cloned())
    }

    pub fn chat_model_label(&self) -> String {
        self.chat_model()
            .map(|model| model.badge())
            .unwrap_or_else(|| "No model configured".to_string())
    }

    /// Make a model the chat model and persist the choice in both stores.
    pub fn set_chat_model(&self, id: &str) {
        {
            let mut config = self.config.borrow_mut();
            config.set_active_model("chat", id);
        }
        self.registry
            .borrow_mut()
            .set_role(ModelRole::Chat, id);
        let _ = self.save_config();
        let registry = self.registry.borrow().clone();
        let _ = save_registry(&registry);
        self.refresh_model_badges();
    }

    // ---- API-key pools --------------------------------------------------
    //
    // A pool belongs to a normalised base URL. Every model pointed at the same
    // URL shares it, so one key applies to all of them. Key metadata (ids,
    // labels, which key is active or exhausted) lives in `models.json`; the key
    // material itself stays in the secret store.

    /// Pool metadata behind one base URL. Empty when nothing is stored yet.
    pub fn key_pool_snapshot(&self, pool_id: &str) -> ApiKeyPool {
        if pool_id.is_empty() {
            return ApiKeyPool::default();
        }
        self.registry
            .borrow()
            .key_pools
            .get(pool_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Pool metadata for the endpoint a model points at.
    pub fn model_key_pool(&self, model: &ModelConfig) -> ApiKeyPool {
        self.key_pool_snapshot(&model.key_pool_id())
    }

    /// The live pool (metadata + secret material) used to rotate keys during a
    /// turn. Falls back to a legacy single key when the migration has not run.
    pub fn build_key_pool(&self, model: &ModelConfig) -> KeyPool {
        let pool_id = model.key_pool_id();
        if pool_id.is_empty() {
            return KeyPool::default();
        }
        let metadata = self.key_pool_snapshot(&pool_id);
        let mut slots: Vec<KeySlot> = metadata
            .keys
            .iter()
            .filter_map(|entry| {
                let raw = self.secrets.get_pool_key(&pool_id, &entry.id)?;
                let key = link_models::sanitize_api_key(&raw);
                if key.is_empty() {
                    return None;
                }
                Some(KeySlot {
                    id: entry.id.clone(),
                    label: entry.label.clone(),
                    key,
                    exhausted: entry.exhausted,
                    exhausted_at: entry.exhausted_at,
                })
            })
            .collect();
        if slots.is_empty() {
            if let Some(raw) = self.secrets.get_model_key(&model.id) {
                let key = link_models::sanitize_api_key(&raw);
                if !key.is_empty() {
                    slots.push(KeySlot {
                        id: "legacy".to_string(),
                        label: "Default".to_string(),
                        key,
                        exhausted: false,
                        exhausted_at: None,
                    });
                }
            }
        }
        KeyPool::new(pool_id, metadata.active, slots)
    }

    /// Store one key for a base URL, verifying it can be read back.
    pub fn store_pool_key(&self, pool_id: &str, key_id: &str, secret: &str) -> Result<(), String> {
        let secret = link_models::sanitize_api_key(secret);
        self.secrets
            .set_pool_key(pool_id, key_id, &secret)
            .map_err(|error| format!("could not store the API key: {error}"))?;
        match self.secrets.get_pool_key(pool_id, key_id) {
            Some(stored) if link_models::sanitize_api_key(&stored) == secret => Ok(()),
            _ => Err("the API key could not be read back after saving".to_string()),
        }
    }

    /// Forget one key's secret.
    pub fn delete_pool_key(&self, pool_id: &str, key_id: &str) {
        let _ = self.secrets.delete_pool_key(pool_id, key_id);
    }

    /// Replace the metadata for one base URL and persist the registry.
    pub fn set_key_pool(&self, pool_id: &str, pool: ApiKeyPool) {
        if pool_id.is_empty() {
            return;
        }
        {
            let mut registry = self.registry.borrow_mut();
            if pool.keys.is_empty() {
                registry.key_pools.remove(pool_id);
            } else {
                registry.key_pools.insert(pool_id.to_string(), pool);
            }
        }
        let registry = self.registry.borrow().clone();
        let _ = save_registry(&registry);
        self.refresh_has_api_key();
    }

    /// Drop a base URL's pool metadata and every one of its stored secrets.
    pub fn delete_key_pool(&self, pool_id: &str) {
        if pool_id.is_empty() {
            return;
        }
        let pool = self.key_pool_snapshot(pool_id);
        for entry in &pool.keys {
            let _ = self.secrets.delete_pool_key(pool_id, &entry.id);
        }
        {
            let mut registry = self.registry.borrow_mut();
            registry.key_pools.remove(pool_id);
        }
        let registry = self.registry.borrow().clone();
        let _ = save_registry(&registry);
        self.refresh_has_api_key();
    }

    /// Recompute `has_api_key` for every model from the pools and the legacy
    /// per-model secrets. Persists the registry when the flag changes.
    pub fn refresh_has_api_key(&self) {
        let keyed_pools: HashSet<String> = {
            let registry = self.registry.borrow();
            registry
                .key_pools
                .iter()
                .filter(|(_, pool)| pool.has_usable_key())
                .map(|(id, _)| id.clone())
                .collect()
        };
        let mut changed = false;
        {
            let mut registry = self.registry.borrow_mut();
            for model in &mut registry.models {
                let pool_id = model.key_pool_id();
                let has = (!pool_id.is_empty() && keyed_pools.contains(&pool_id))
                    || self.secrets.get_model_key(&model.id).is_some();
                if model.has_api_key != has {
                    model.has_api_key = has;
                    changed = true;
                }
            }
        }
        if changed {
            let registry = self.registry.borrow().clone();
            let _ = save_registry(&registry);
        }
    }

    /// Persist a rotation change coming back from a running turn and, when the
    /// active key moved, tell the user which one ran out.
    pub fn apply_key_pool_update(&self, update: KeyPoolUpdate) {
        let switched = update.switched;
        let recovered = update.recovered;
        let exhausted_label = update.exhausted_label.clone();
        {
            let mut registry = self.registry.borrow_mut();
            if update.pool.keys.is_empty() {
                registry.key_pools.remove(&update.pool_id);
            } else {
                registry.key_pools.insert(update.pool_id.clone(), update.pool);
            }
        }
        let registry = self.registry.borrow().clone();
        let _ = save_registry(&registry);
        self.refresh_has_api_key();
        if switched {
            match exhausted_label {
                Some(label) => {
                    self.toast(&format!("{label} ran out \u{2014} switched to the next API key"))
                }
                None => self.toast("Switched to the next API key"),
            }
        } else if recovered {
            self.toast("An API key is available again");
        }
    }

    /// A provider that rotates through every key stored for this endpoint and
    /// reports changes back to the UI thread. Used by the agent turn.
    pub fn rotating_provider(self: &Rc<Self>, model: &ModelConfig) -> Arc<dyn ModelProvider> {
        let pool = self.build_key_pool(model);
        let observer = self.key_pool_observer();
        Arc::new(RotatingProvider::new(model.clone(), pool, observer))
    }

    /// A one-shot rotating provider built from typed keys, for "Load models"
    /// and "Test" before the form is saved. Changes are not persisted.
    pub fn ephemeral_provider(
        &self,
        model: &ModelConfig,
        keys: Vec<(String, String)>,
    ) -> Arc<dyn ModelProvider> {
        let slots: Vec<KeySlot> = keys
            .into_iter()
            .filter_map(|(label, secret)| {
                let key = link_models::sanitize_api_key(&secret);
                if key.is_empty() {
                    None
                } else {
                    Some((label, key))
                }
            })
            .enumerate()
            .map(|(index, (label, key))| KeySlot {
                id: format!("ephemeral-{index}"),
                label,
                key,
                exhausted: false,
                exhausted_at: None,
            })
            .collect();
        let pool = KeyPool::new(model.key_pool_id(), 0, slots);
        Arc::new(RotatingProvider::new(model.clone(), pool, None))
    }

    /// Build an observer that forwards pool changes to the GTK thread. GTK
    /// state is not `Send`, so updates cross the thread boundary through a
    /// channel and are applied by a local task.
    fn key_pool_observer(self: &Rc<Self>) -> Option<KeyPoolObserver> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<KeyPoolUpdate>();
        let state = self.clone();
        gtk::glib::MainContext::default().spawn_local(async move {
            while let Some(update) = rx.recv().await {
                state.apply_key_pool_update(update);
            }
        });
        Some(Arc::new(move |update| {
            let _ = tx.send(update);
        }))
    }

    pub fn refresh_model_badges(&self) {
        if let Some(widgets) = self.widgets() {
            let label = self.chat_model_label();
            crate::widgets::set_button_text_for(
                &widgets.composer_model_button,
                &label,
                18,
                gtk::pango::EllipsizeMode::End,
            );
            let tooltip = self
                .chat_model()
                .map(|model| {
                    format!(
                        "{}\n{} · {}",
                        model.display_name,
                        model.provider.label(),
                        model.model_id
                    )
                })
                .unwrap_or_else(|| "Configure a model in Models (Ctrl+M)".to_string());
            widgets.composer_model_button.set_tooltip_text(Some(&tooltip));
        }
    }

    // ---- permission mode ------------------------------------------------

    pub fn mode(&self) -> PermissionMode {
        self.mode.get()
    }

    pub fn set_mode(&self, mode: PermissionMode) {
        self.mode.set(mode);
        {
            let mut config = self.config.borrow_mut();
            config.agent.default_mode = mode;
        }
        let _ = self.save_config();
        self.refresh_mode_chip();
    }

    pub fn cycle_mode(&self) {
        self.set_mode(self.mode.get().next());
    }

    pub fn refresh_mode_chip(&self) {
        if let Some(widgets) = self.widgets() {
            let mode = self.mode.get();
            widgets.mode_button.set_label(mode.label());
            widgets
                .mode_button
                .set_tooltip_text(Some(mode.description()));
            if mode == PermissionMode::FullAuto {
                widgets.mode_button.add_css_class("full-auto");
            } else {
                widgets.mode_button.remove_css_class("full-auto");
            }
        }
    }

    // ---- sessions -------------------------------------------------------

    pub fn ensure_session(&self) -> Option<String> {
        let workspace = self.workspace_path()?;
        let mut slot = self.session.borrow_mut();
        if slot.is_none() {
            // Start a fresh session. `resume_last` reopens the newest existing
            // conversation when a workspace opens, so this only runs when the
            // user explicitly starts a new one (or the folder has no history).
            *slot = Some(Session::new(&workspace, self.chat_model_label()));
        }
        slot.as_ref().map(|session| session.id.clone())
    }

    pub fn save_session(&self) {
        let workspace = match self.workspace_path() {
            Some(workspace) => workspace,
            None => return,
        };
        let session = self.session.borrow().clone();
        if let Some(session) = session {
            if let Err(error) = self.session_store.save(&session) {
                tracing::warn!(?error, "could not save the session");
            }
            let _ = workspace;
        }
    }

    /// Push a finished assistant message into the persisted session.
    pub fn commit_turn(&self) {
        let text = self.turn_text.borrow().trim().to_string();
        let thinking = self.turn_thinking.borrow().trim().to_string();
        let model = self.turn_model.borrow().clone();
        if text.is_empty() {
            // Keep the user's message even when the model produced no answer.
            self.save_session();
            return;
        }
        let mut slot = self.session.borrow_mut();
        let Some(session) = slot.as_mut() else {
            return;
        };
        let mut message = link_core::session::StoredMessage::assistant(text, model);
        if !thinking.is_empty() {
            message.thinking = Some(thinking);
        }
        session.push(message);
        // Remember what was asked and how far the work got, so reopening this
        // session continues the task instead of starting it again.
        let request = session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == link_core::session::StoredRole::User)
            .map(|message| one_line(&message.content, 220))
            .unwrap_or_default();
        let outcome = one_line(&session.messages.last().map(|m| m.content.as_str()).unwrap_or(""), 700);
        let note = if request.is_empty() {
            format!("Last reply: {outcome}")
        } else {
            format!("Request: {request}\nOutcome: {outcome}")
        };
        session.set_resume_note(note);
        drop(slot);
        self.save_session();
    }

    /// Persist the agent's live checklist with the conversation.
    pub fn set_session_todos(&self, items: Vec<TodoItem>) {
        let changed = {
            let mut slot = self.session.borrow_mut();
            match slot.as_mut() {
                Some(session) => {
                    session.set_todos(items);
                    true
                }
                None => false,
            }
        };
        if changed {
            self.save_session();
        }
    }

    /// Remember a file the agent changed while working on this session.
    pub fn record_session_edit(&self, path: &Path) {
        let relative = self.relative_to_workspace(path);
        let changed = {
            let mut slot = self.session.borrow_mut();
            match slot.as_mut() {
                Some(session) => {
                    session.record_edited_file(&relative);
                    true
                }
                None => false,
            }
        };
        if changed {
            self.save_session();
        }
    }

    /// The system-prompt section that resumes a reopened conversation.
    pub fn session_resume_prompt(&self) -> Option<String> {
        self.session
            .borrow()
            .as_ref()
            .and_then(|session| session.resume_prompt())
    }

    pub fn push_user_message(&self, text: &str) {
        let mut slot = self.session.borrow_mut();
        if let Some(session) = slot.as_mut() {
            session.push_user(text);
            session.touch();
        }
    }

    pub fn load_session(&self, id: &str) -> Result<Session, String> {
        let workspace = self
            .workspace_path()
            .ok_or_else(|| "no workspace open".to_string())?;
        self.session_store
            .load(&workspace, id)
            .map_err(|error| error.to_string())
    }

    // ---- attachment helpers --------------------------------------------

    pub fn relative_to_workspace(&self, path: &Path) -> String {
        match self.workspace_path() {
            Some(root) => path
                .strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string(),
            None => path.to_string_lossy().to_string(),
        }
    }

    pub fn push_attachment(&self, path: &Path) {
        let relative = self.relative_to_workspace(path);
        let mut attachments = self.attachments.borrow_mut();
        if !attachments.contains(&relative) {
            attachments.push(relative);
        }
    }

    pub fn take_attachments(&self) -> Vec<String> {
        std::mem::take(&mut *self.attachments.borrow_mut())
    }
}

/// Collapse whitespace and cut to `limit` characters, for resume notes.
fn one_line(text: &str, limit: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= limit {
        return compact;
    }
    let mut out: String = compact.chars().take(limit.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}
