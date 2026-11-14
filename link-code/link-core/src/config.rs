//! Application configuration, stored as JSON under `~/.config/link-code/`.
//!
//! Everything is local. There is no account, and no telemetry flag to disable —
//! telemetry simply does not exist in this codebase.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{LinkError, Result};
use crate::permission::{PermissionMode, PermissionRules};
use crate::{APP_ID, DATA_FORMAT_VERSION};
use link_models::ProxySettings;

/// `~/.config/link-code`
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_ID)
}

/// `~/.local/share/link-code`
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_ID)
}

/// `~/.local/state/link-code` (logs, crash reports).
pub fn state_dir() -> PathBuf {
    dirs::state_dir()
        .map(|dir| dir.join(APP_ID))
        .unwrap_or_else(|| data_dir().join("state"))
}

/// How thinking/reasoning blocks are rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingDisplay {
    Expanded,
    #[default]
    Collapsed,
    Hidden,
}

impl ThinkingDisplay {
    pub const ALL: [ThinkingDisplay; 3] = [
        ThinkingDisplay::Expanded,
        ThinkingDisplay::Collapsed,
        ThinkingDisplay::Hidden,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ThinkingDisplay::Expanded => "Show expanded",
            ThinkingDisplay::Collapsed => "Show collapsed",
            ThinkingDisplay::Hidden => "Hide entirely",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralSettings {
    /// BCP-47-ish language tag used for UI copy.
    pub language: String,
    /// Reopen the last workspace on launch when no path was passed on the CLI.
    pub reopen_last_workspace: bool,
    /// Folders offered on the welcome screen, most recent first.
    pub recent_workspaces: Vec<PathBuf>,
    /// Maximum number of recent workspaces to remember.
    pub recent_limit: usize,
    /// Show "Clone Repository…" on the welcome screen.
    pub show_clone_action: bool,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            language: "en".to_string(),
            reopen_last_workspace: false,
            recent_workspaces: Vec::new(),
            recent_limit: 12,
            show_clone_action: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    /// Reserved: themes are locked to Link Code Dark (see README).
    pub theme: String,
    pub high_contrast: bool,
    /// Global UI scale multiplier (0.8 - 1.6).
    pub ui_scale: f64,
    pub editor_font: String,
    pub editor_font_size: f64,
    pub editor_ligatures: bool,
    pub show_minimap: bool,
    pub respect_reduced_motion: bool,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: "link-code-dark".to_string(),
            high_contrast: false,
            ui_scale: 1.0,
            editor_font: "JetBrains Mono".to_string(),
            editor_font_size: 13.0,
            editor_ligatures: true,
            show_minimap: false,
            respect_reduced_motion: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSettings {
    pub default_mode: PermissionMode,
    /// Compact automatically once this share of the window is used (0.0 - 1.0).
    pub auto_compact_threshold: f64,
    pub thinking_display: ThinkingDisplay,
    /// Hard cap on tool-use iterations per turn; stops runaway loops.
    pub max_tool_iterations: u32,
    /// Shell timeout in seconds.
    pub shell_timeout_secs: u64,
    /// Maximum bytes of shell output kept and sent back to the model.
    pub shell_output_cap: usize,
    /// Maximum bytes returned by any single tool result.
    pub tool_output_cap: usize,
    /// Default max output tokens requested from the model.
    pub max_output_tokens: u32,
    /// Redact secrets from anything sent to a model.
    pub protect_secrets: bool,
    /// Show the "Agent Output" tab in the bottom panel.
    pub show_agent_output_tab: bool,
    /// Enable the (optional, off by default) web tools.
    pub enable_web_tools: bool,
    /// Generate next-step suggestion chips after each response.
    pub generate_suggestions: bool,
    /// Enable inline ghost-text completion in the editor.
    pub inline_completion: bool,
    /// Model id used for inline completion; empty means "use the FIM role".
    pub inline_completion_model: String,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            default_mode: PermissionMode::Ask,
            auto_compact_threshold: 0.85,
            thinking_display: ThinkingDisplay::Collapsed,
            max_tool_iterations: 40,
            shell_timeout_secs: 120,
            shell_output_cap: 64 * 1024,
            tool_output_cap: 128 * 1024,
            max_output_tokens: 8192,
            protect_secrets: true,
            show_agent_output_tab: true,
            enable_web_tools: false,
            generate_suggestions: true,
            inline_completion: false,
            inline_completion_model: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub tab_width: u32,
    pub insert_spaces: bool,
    pub word_wrap: bool,
    pub show_line_numbers: bool,
    pub highlight_current_line: bool,
    pub bracket_matching: bool,
    pub code_folding: bool,
    pub auto_indent: bool,
    pub format_on_save: bool,
    pub trim_trailing_whitespace: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            tab_width: 2,
            insert_spaces: true,
            word_wrap: true,
            show_line_numbers: true,
            highlight_current_line: true,
            bracket_matching: true,
            code_folding: true,
            auto_indent: true,
            format_on_save: false,
            trim_trailing_whitespace: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    pub shell: String,
    pub font_size: f64,
    pub scrollback_lines: u32,
    pub cursor_blink: bool,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string()),
            font_size: 12.5,
            scrollback_lines: 10_000,
            cursor_blink: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GitSettings {
    pub auto_fetch: bool,
    /// Generate commit messages with the "fast" model role.
    pub ai_commit_messages: bool,
    /// Show inline blame annotations (experimental).
    pub show_blame: bool,
}

impl Default for GitSettings {
    fn default() -> Self {
        Self {
            auto_fetch: true,
            ai_commit_messages: true,
            show_blame: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacySettings {
    /// Redact anything matching a secret pattern before it leaves the machine.
    pub protect_secrets: bool,
    /// Warn when the user attaches a file that looks like a secret.
    pub warn_on_secret_attachment: bool,
    /// Telemetry is not implemented; this flag exists only to document that.
    pub telemetry: bool,
    /// Keep shell history out of the model context.
    pub exclude_shell_history: bool,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            protect_secrets: true,
            warn_on_secret_attachment: true,
            telemetry: false,
            exclude_shell_history: true,
        }
    }
}

/// The complete configuration document.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub general: GeneralSettings,
    pub appearance: AppearanceSettings,
    pub agent: AgentSettings,
    pub editor: EditorSettings,
    pub terminal: TerminalSettings,
    pub git: GitSettings,
    pub privacy: PrivacySettings,
    /// Outbound proxy used by every HTTP client the app builds.
    pub proxy: ProxySettings,
    pub permissions: PermissionRules,
    /// Command id -> keybinding, e.g. `"agent.focus": "<Control>l"`.
    pub keybindings: BTreeMap<String, String>,
    /// Remembered panel sizes. Keys: `sidebar`, `agent`, `bottom`.
    pub layout: BTreeMap<String, i32>,
    /// Selected model id per role: `chat`, `fast`, `completion`.
    pub active_models: BTreeMap<String, String>,
    /// Agent panel placement: `right` or `focus`.
    pub agent_panel_layout: String,
    /// Last workspace opened, used for `--continue` style relaunches.
    pub last_workspace: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: DATA_FORMAT_VERSION,
            general: GeneralSettings::default(),
            appearance: AppearanceSettings::default(),
            agent: AgentSettings::default(),
            editor: EditorSettings::default(),
            terminal: TerminalSettings::default(),
            git: GitSettings::default(),
            privacy: PrivacySettings::default(),
            proxy: ProxySettings::default(),
            permissions: PermissionRules::default(),
            keybindings: default_keybindings(),
            layout: BTreeMap::new(),
            active_models: BTreeMap::new(),
            agent_panel_layout: "right".to_string(),
            last_workspace: None,
        }
    }
}

impl Config {
    /// Remember a workspace folder, most recent first, deduplicated.
    pub fn remember_workspace(&mut self, path: &Path) {
        let path = path.to_path_buf();
        self.general.recent_workspaces.retain(|p| p != &path);
        self.general.recent_workspaces.insert(0, path.clone());
        let limit = self.general.recent_limit;
        self.general.recent_workspaces.truncate(limit);
        self.last_workspace = Some(path);
    }

    pub fn active_model(&self, role: &str) -> Option<&str> {
        self.active_models.get(role).map(String::as_str)
    }

    pub fn set_active_model(&mut self, role: &str, model_id: &str) {
        self.active_models
            .insert(role.to_string(), model_id.to_string());
    }

    /// Effective shell timeout, clamped to something sane.
    pub fn shell_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.agent.shell_timeout_secs.clamp(5, 3600))
    }
}

/// The default, fully remappable keybinding table.
pub fn default_keybindings() -> BTreeMap<String, String> {
    let pairs: [(&str, &str); 19] = [
        ("agent.focus", "<Control>l"),
        ("agent.send", "<Control>Return"),
        ("agent.stop", "Escape"),
        ("agent.cycle_mode", "<Shift>Tab"),
        ("agent.undo_turn", "<Control>z"),
        ("agent.new_session", "<Control><Shift>n"),
        ("view.toggle_sidebar", "<Control>b"),
        ("view.toggle_bottom", "<Control>j"),
        ("view.toggle_agent", "<Control>backslash"),
        ("view.toggle_wrap", "<Alt>z"),
        ("view.agent_focus_layout", "<Control><Shift>l"),
        ("view.split_horizontal", "<Control><Shift>5"),
        ("view.split_vertical", "<Control><Shift>6"),
        ("file.open_folder", "<Control>o"),
        ("file.quick_open", "<Control>p"),
        ("file.save", "<Control>s"),
        ("app.command_palette", "<Control><Shift>p"),
        ("app.settings", "<Control>comma"),
        ("app.models", "<Control>m"),
    ];
    pairs
        .into_iter()
        .map(|(command, keys)| (command.to_string(), keys.to_string()))
        .collect()
}

/// Reads and writes [`Config`] as JSON, atomically.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `~/.config/link-code/config.json`
    pub fn default_location() -> Self {
        Self::at(config_dir().join("config.json"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the config, falling back to defaults when the file is missing.
    /// A corrupt file is *never* fatal: it is preserved as `.bak` and defaults
    /// are returned, so the app always starts.
    pub fn load_or_default(&self) -> Config {
        match self.load() {
            Ok(config) => config,
            Err(error) => {
                if self.path.exists() {
                    let backup = self.path.with_extension("json.bak");
                    let _ = std::fs::rename(&self.path, &backup);
                    tracing::warn!(?error, backup = %backup.display(), "config was unreadable; using defaults");
                }
                Config::default()
            }
        }
    }

    pub fn load(&self) -> Result<Config> {
        if !self.path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(&self.path).map_err(|e| LinkError::io(&self.path, e))?;
        if text.trim().is_empty() {
            return Ok(Config::default());
        }
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| LinkError::Settings(format!("{}: {e}", self.path.display())))?;
        Ok(Self::migrate(value))
    }

    /// Apply forward migrations. Missing fields are filled by `serde(default)`,
    /// so migrations only need to handle renamed/reshaped fields.
    pub fn migrate(value: serde_json::Value) -> Config {
        let version = value
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32;
        let mut working = value.clone();
        if version < 1 {
            // v0 -> v1: `agent.default_permission_mode` became `agent.default_mode`.
            if let Some(agent) = working.get_mut("agent") {
                if let Some(legacy) = agent.get("default_permission_mode").cloned() {
                    agent["default_mode"] = legacy;
                }
            }
        }
        let mut config: Config = serde_json::from_value(working).unwrap_or_default();
        config.version = DATA_FORMAT_VERSION;
        config
    }

    pub fn save(&self, config: &Config) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LinkError::io(parent, e))?;
        }
        write_json_atomic(&self.path, config)
    }
}

/// Write JSON via a temporary file + rename, so a crash cannot leave a
/// half-written config behind.
pub fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| LinkError::Settings(format!("could not serialise settings: {e}")))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| LinkError::io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| LinkError::io(path, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use link_models::ProxyProtocol;

    #[test]
    fn default_round_trips_through_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::at(dir.path().join("config.json"));
        let mut config = Config::default();
        assert!(config.active_model("chat").is_none());
        config.set_active_model("chat", "ollama:qwen2.5-coder:32b");
        config.remember_workspace(Path::new("/tmp/demo"));
        store.save(&config).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.active_model("chat"), Some("ollama:qwen2.5-coder:32b"));
        assert_eq!(loaded.last_workspace.as_deref(), Some(Path::new("/tmp/demo")));
        assert_eq!(loaded.version, DATA_FORMAT_VERSION);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::at(dir.path().join("nope.json"));
        let config = store.load_or_default();
        assert_eq!(config.agent.default_mode, PermissionMode::Ask);
        assert!(!config.privacy.telemetry);
    }

    #[test]
    fn corrupt_file_is_backed_up_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{ not json at all").unwrap();
        let store = ConfigStore::at(&path);
        let config = store.load_or_default();
        assert_eq!(config.version, DATA_FORMAT_VERSION);
        assert!(path.with_extension("json.bak").exists());
    }

    #[test]
    fn unknown_fields_are_ignored_and_missing_fields_defaulted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{ "version": 1, "future_field": 42, "editor": { "tab_width": 8 } }"#,
        )
        .unwrap();
        let config = ConfigStore::at(&path).load().unwrap();
        assert_eq!(config.editor.tab_width, 8);
        assert_eq!(config.editor.show_line_numbers, EditorSettings::default().show_line_numbers);
        assert_eq!(config.agent.shell_timeout_secs, 120);
    }

    #[test]
    fn migrates_legacy_permission_mode_key() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{ "version": 0, "agent": { "default_permission_mode": "full_auto" } }"#,
        )
        .unwrap();
        let config = ConfigStore::migrate(value);
        assert_eq!(config.agent.default_mode, PermissionMode::FullAuto);
    }

    #[test]
    fn recent_workspaces_are_deduplicated_and_capped() {
        let mut config = Config::default();
        config.general.recent_limit = 2;
        config.remember_workspace(Path::new("/a"));
        config.remember_workspace(Path::new("/b"));
        config.remember_workspace(Path::new("/a"));
        assert_eq!(config.general.recent_workspaces.len(), 2);
        assert_eq!(config.general.recent_workspaces[0], PathBuf::from("/a"));
        assert_eq!(config.general.recent_workspaces[1], PathBuf::from("/b"));
    }

    #[test]
    fn shell_timeout_is_clamped() {
        let mut config = Config::default();
        config.agent.shell_timeout_secs = 0;
        assert_eq!(config.shell_timeout().as_secs(), 5);
        config.agent.shell_timeout_secs = 100_000;
        assert_eq!(config.shell_timeout().as_secs(), 3600);
    }

    #[test]
    fn proxy_settings_default_to_direct_and_survive_a_round_trip() {
        let config = Config::default();
        assert!(!config.proxy.is_enabled());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let store = ConfigStore::at(&path);
        let mut config = Config::default();
        config.proxy.protocol = ProxyProtocol::Socks5;
        config.proxy.host = "127.0.0.1".to_string();
        config.proxy.port = 1080;
        config.proxy.username = "u".to_string();
        store.save(&config).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.proxy.protocol, ProxyProtocol::Socks5);
        let url = loaded.proxy.url().unwrap();
        assert!(url.starts_with("socks5h://u@127.0.0.1:1080"), "{url}");
    }
}
