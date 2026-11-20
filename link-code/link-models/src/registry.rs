//! The models manager's data model.
//!
//! A [`ModelConfig`] is everything Link Code needs to talk to one endpoint.
//! API keys are deliberately **not** part of it: they live in the OS keyring
//! (see `link_core::secrets`), and only a boolean says whether one is stored.
//! That is why exporting a registry is safe to share.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::Capabilities;
use crate::REGISTRY_FORMAT_VERSION;

/// Which wire protocol an endpoint speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// OpenAI `/chat/completions`. Covers OpenAI itself, Ollama's compat layer,
    /// llama.cpp server, LM Studio, vLLM, OpenRouter, Groq, Together, DeepSeek…
    OpenAiCompatible,
    /// Anthropic `/v1/messages`.
    Anthropic,
    /// Google `generativelanguage.googleapis.com`.
    Gemini,
    /// Ollama's native `/api/chat` (needed for `keep_alive`, `num_ctx`, …).
    Ollama,
}

/// Normalise a base URL pasted by the user.
///
/// People paste anything: a bare host (`https://api.openai.com`), the full
/// endpoint (`.../v1/chat/completions`) or a URL with a trailing slash. This
/// turns all of them into the root the adapters expect, and adds the `/v1`
/// segment when an OpenAI-compatible endpoint was given without a path.
pub fn normalize_base_url(kind: ProviderKind, raw: &str) -> String {
    let mut base = raw.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return base;
    }
    // Strip a pasted endpoint so the adapter can append its own path.
    for suffix in [
        "/chat/completions",
        "/completions",
        "/responses",
        "/models",
        "/api/chat",
        "/api/tags",
    ] {
        if let Some(stripped) = base.strip_suffix(suffix) {
            base = stripped.trim_end_matches('/').to_string();
        }
    }
    match kind {
        ProviderKind::OpenAiCompatible => {
            if let Ok(url) = url::Url::parse(&base) {
                if url.path().trim_matches('/').is_empty() {
                    base.push_str("/v1");
                }
            }
        }
        // These adapters append the version themselves.
        ProviderKind::Anthropic => {
            if let Some(stripped) = base.strip_suffix("/v1") {
                base = stripped.to_string();
            }
        }
        ProviderKind::Gemini => {
            if let Some(stripped) = base.strip_suffix("/v1beta") {
                base = stripped.to_string();
            }
        }
        ProviderKind::Ollama => {
            if let Some(stripped) = base.strip_suffix("/api") {
                base = stripped.to_string();
            }
        }
    }
    base
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 4] = [
        ProviderKind::OpenAiCompatible,
        ProviderKind::Anthropic,
        ProviderKind::Gemini,
        ProviderKind::Ollama,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::OpenAiCompatible => "OpenAI-compatible",
            ProviderKind::Anthropic => "Anthropic",
            ProviderKind::Gemini => "Google Gemini",
            ProviderKind::Ollama => "Ollama native",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ProviderKind::OpenAiCompatible => {
                "Ollama, llama.cpp server, LM Studio, vLLM, OpenRouter, Groq, DeepSeek, custom"
            }
            ProviderKind::Anthropic => "Anthropic Claude via /v1/messages",
            ProviderKind::Gemini => "Google Gemini via generateContent",
            ProviderKind::Ollama => "Ollama's native API, with keep_alive and num_ctx support",
        }
    }

    /// One-click preset applied when this provider is chosen.
    pub fn default_base_url(self) -> &'static str {
        match self {
            // There is no single "OpenAI-compatible" host; the user pastes
            // the endpoint for whichever platform they use.
            ProviderKind::OpenAiCompatible => "",
            ProviderKind::Anthropic => "https://api.anthropic.com",
            ProviderKind::Gemini => "https://generativelanguage.googleapis.com",
            ProviderKind::Ollama => "http://localhost:11434",
        }
    }

    /// Local endpoints are free and need no API key.
    pub fn is_cloud(self) -> bool {
        matches!(
            self,
            ProviderKind::Anthropic | ProviderKind::Gemini | ProviderKind::OpenAiCompatible
        )
    }

    /// Whether an API key is normally required.
    pub fn requires_api_key(self, base_url: &str) -> bool {
        match self {
            ProviderKind::Anthropic | ProviderKind::Gemini => true,
            ProviderKind::Ollama => false,
            ProviderKind::OpenAiCompatible => !is_localhost(base_url),
        }
    }

    pub fn icon_name(self) -> &'static str {
        match self {
            ProviderKind::OpenAiCompatible => "provider-openai-symbolic",
            ProviderKind::Anthropic => "provider-anthropic-symbolic",
            ProviderKind::Gemini => "provider-gemini-symbolic",
            ProviderKind::Ollama => "provider-ollama-symbolic",
        }
    }
}

pub fn is_localhost(base_url: &str) -> bool {
    let lowered = base_url.to_ascii_lowercase();
    lowered.contains("localhost")
        || lowered.contains("127.0.0.1")
        || lowered.contains("0.0.0.0")
        || lowered.contains("[::1]")
        || lowered.contains("host.docker.internal")
}

/// Role a model is assigned to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// The model the agent panel chats with.
    Chat,
    /// Cheap model for suggestions, summaries, titles, compaction.
    Fast,
    /// Fill-in-the-middle model for inline ghost text.
    Completion,
}

impl ModelRole {
    pub const ALL: [ModelRole; 3] = [ModelRole::Chat, ModelRole::Fast, ModelRole::Completion];

    pub fn key(self) -> &'static str {
        match self {
            ModelRole::Chat => "chat",
            ModelRole::Fast => "fast",
            ModelRole::Completion => "completion",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ModelRole::Chat => "Chat / Agent",
            ModelRole::Fast => "Fast tasks (suggestions, summaries)",
            ModelRole::Completion => "Inline completion",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "chat" => Some(ModelRole::Chat),
            "fast" => Some(ModelRole::Fast),
            "completion" => Some(ModelRole::Completion),
            _ => None,
        }
    }
}

/// One API key stored for a base URL.
///
/// The key material itself never appears here: it lives in the OS keyring (see
/// `link_core::secrets`). This record only keeps the non-secret metadata needed
/// to show the key list, pick an active key and skip keys that ran out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiKeyEntry {
    /// Stable id used to look the secret up for this key.
    pub id: String,
    /// Human label, e.g. `Key 1` or a note the user typed.
    pub label: String,
    /// True once the provider rejected or exhausted this key. It is skipped
    /// until another successful request proves it usable again.
    pub exhausted: bool,
    pub exhausted_at: Option<DateTime<Utc>>,
}

impl Default for ApiKeyEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            exhausted: false,
            exhausted_at: None,
        }
    }
}

/// Every API key that belongs to one base URL.
///
/// Keys are scoped to the URL rather than to a model configuration: two models
/// served by the same endpoint share one pool and use the same active key.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiKeyPool {
    pub keys: Vec<ApiKeyEntry>,
    /// Index of the key currently in use. Always clamped into `keys`.
    pub active: usize,
}

impl ApiKeyPool {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Index of the key requests should use: the active one when it is still
    /// usable, otherwise the first healthy key, otherwise the active/first key
    /// (all keys exhausted, so the next request probes them again).
    pub fn effective_active(&self) -> Option<usize> {
        if self.keys.is_empty() {
            return None;
        }
        let active = self.active.min(self.keys.len() - 1);
        if !self.keys[active].exhausted {
            Some(active)
        } else {
            self.keys
                .iter()
                .position(|key| !key.exhausted)
                .or(Some(active))
        }
    }

    pub fn active_entry(&self) -> Option<&ApiKeyEntry> {
        self.effective_active().map(|index| &self.keys[index])
    }

    /// Keep `active` inside the vector after a removal.
    pub fn clamp(&mut self) {
        if self.keys.is_empty() {
            self.active = 0;
        } else {
            self.active = self.active.min(self.keys.len() - 1);
        }
    }

    pub fn mark_exhausted(&mut self, key_id: &str, at: DateTime<Utc>) -> bool {
        let Some(index) = self.keys.iter().position(|key| key.id == key_id) else {
            return false;
        };
        let changed = !self.keys[index].exhausted;
        self.keys[index].exhausted = true;
        self.keys[index].exhausted_at = Some(at);
        self.active = self.next_available(index);
        changed
    }

    /// The next healthy key after `from`, wrapping around. When every key is
    /// exhausted, advance one position so a retry still probes another key.
    pub fn next_available(&self, from: usize) -> usize {
        if self.keys.is_empty() {
            return 0;
        }
        let len = self.keys.len();
        for step in 1..=len {
            let index = (from + step) % len;
            if !self.keys[index].exhausted {
                return index;
            }
        }
        (from + 1) % len
    }

    pub fn mark_usable(&mut self, key_id: &str) -> bool {
        let Some(index) = self.keys.iter().position(|key| key.id == key_id) else {
            return false;
        };
        let changed = self.keys[index].exhausted || self.keys[index].exhausted_at.is_some();
        self.keys[index].exhausted = false;
        self.keys[index].exhausted_at = None;
        changed
    }

    pub fn reset_exhausted(&mut self) -> bool {
        let mut changed = false;
        for key in &mut self.keys {
            if key.exhausted || key.exhausted_at.is_some() {
                key.exhausted = false;
                key.exhausted_at = None;
                changed = true;
            }
        }
        changed
    }
}

/// Cumulative usage for one model, shown on its card.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelStats {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Exponential moving average of decode speed, tokens/second.
    pub avg_tokens_per_second: f64,
}

impl ModelStats {
    /// Fold a new sample into the running average.
    pub fn record(&mut self, prompt_tokens: usize, completion_tokens: usize, tokens_per_second: f64) {
        self.requests += 1;
        self.prompt_tokens += prompt_tokens as u64;
        self.completion_tokens += completion_tokens as u64;
        if tokens_per_second > 0.0 {
            let weight = 0.2;
            self.avg_tokens_per_second = if self.avg_tokens_per_second <= 0.0 {
                tokens_per_second
            } else {
                self.avg_tokens_per_second * (1.0 - weight) + tokens_per_second * weight
            };
        }
    }
}

/// Everything needed to talk to one endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    /// Stable internal id, also the keyring key suffix.
    pub id: String,
    pub display_name: String,
    pub provider: ProviderKind,
    /// Root URL, without a trailing slash, e.g. `http://localhost:11434/v1`.
    pub base_url: String,
    /// Provider-specific model id, e.g. `qwen2.5-coder:32b`.
    pub model_id: String,
    pub context_window: usize,
    pub max_output_tokens: u32,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub extra_headers: Vec<(String, String)>,
    pub extra_body: Option<Value>,
    pub capabilities: Capabilities,
    pub system_prompt: Option<String>,
    /// True when a key is present in the OS keyring. The key itself is never
    /// stored here and never serialised.
    pub has_api_key: bool,
    pub stats: ModelStats,
    pub created_at: Option<DateTime<Utc>>,
    /// Free-form note shown on the model card.
    pub notes: Option<String>,
    /// Ollama `keep_alive` (e.g. `"30m"`), only meaningful for Ollama.
    pub keep_alive: Option<String>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            display_name: String::new(),
            provider: ProviderKind::OpenAiCompatible,
            base_url: String::new(),
            model_id: String::new(),
            context_window: 32_768,
            max_output_tokens: 4_096,
            temperature: Some(0.2),
            top_p: None,
            extra_headers: Vec::new(),
            extra_body: None,
            capabilities: Capabilities::default(),
            system_prompt: None,
            has_api_key: false,
            stats: ModelStats::default(),
            created_at: None,
            notes: None,
            keep_alive: None,
        }
    }
}

impl ModelConfig {
    /// A new config with a generated id.
    pub fn new(display_name: impl Into<String>, provider: ProviderKind, model_id: impl Into<String>) -> Self {
        let display_name = display_name.into();
        let model_id = model_id.into();
        Self {
            id: generate_id(&display_name, &model_id),
            base_url: provider.default_base_url().to_string(),
            display_name,
            provider,
            model_id,
            created_at: Some(Utc::now()),
            ..Self::default()
        }
    }

    /// `ollama · qwen2.5-coder:32b` — the exact string on the model badge.
    pub fn badge(&self) -> String {
        let short = match self.provider {
            ProviderKind::OpenAiCompatible => "openai",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Ollama => "ollama",
        };
        format!("{short} \u{00b7} {}", self.model_id)
    }

    /// Chat completions URL for this endpoint.
    pub fn chat_url(&self) -> String {
        let base = normalize_base_url(self.provider, &self.base_url);
        let base = base.trim_end_matches('/');
        match self.provider {
            ProviderKind::OpenAiCompatible => format!("{base}/chat/completions"),
            ProviderKind::Anthropic => format!("{base}/v1/messages"),
            ProviderKind::Gemini => {
                format!("{base}/v1beta/models/{}:streamGenerateContent?alt=sse", self.model_id)
            }
            ProviderKind::Ollama => format!("{base}/api/chat"),
        }
    }

    /// URL used by "Fetch models" / "Test connection".
    pub fn models_url(&self) -> String {
        let base = normalize_base_url(self.provider, &self.base_url);
        let base = base.trim_end_matches('/');
        match self.provider {
            ProviderKind::OpenAiCompatible => format!("{base}/models"),
            ProviderKind::Ollama => format!("{base}/api/tags"),
            ProviderKind::Gemini => format!("{base}/v1beta/models"),
            // Anthropic has no public model-listing endpoint.
            ProviderKind::Anthropic => format!("{base}/v1/models"),
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self.provider, ProviderKind::Ollama) || is_localhost(&self.base_url)
    }

    pub fn requires_api_key(&self) -> bool {
        self.provider.requires_api_key(&self.base_url)
    }

    /// Stable identifier for this endpoint's API-key pool.
    ///
    /// Two model configurations that point at the same normalised base URL
    /// share the pool, which is what makes one key apply to every model on an
    /// endpoint. An empty URL has no pool.
    pub fn key_pool_id(&self) -> String {
        normalize_base_url(self.provider, &self.base_url)
    }

    /// Reset a field to the provider's defaults. Used by the "presets" button.
    pub fn apply_provider_preset(&mut self) {
        self.base_url = self.provider.default_base_url().to_string();
        self.has_api_key = false;
    }

    /// Validation shared by the Add/Edit modal and by import.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.display_name.trim().is_empty() {
            problems.push("Display name is required".to_string());
        }
        if self.base_url.trim().is_empty() {
            problems.push("Base URL is required".to_string());
        } else if url::Url::parse(self.base_url.trim()).is_err() {
            problems.push(format!("`{}` is not a valid URL", self.base_url));
        }
        if self.model_id.trim().is_empty() {
            problems.push("Model ID is required".to_string());
        }
        if self.context_window < 512 {
            problems.push("Context window looks too small (minimum 512)".to_string());
        }
        if self.max_output_tokens as usize >= self.context_window {
            problems.push("Max output tokens must be smaller than the context window".to_string());
        }
        problems
    }
}

/// Deterministic, readable id: `ollama-qwen25-coder-32b-a1b2c3`.
pub fn generate_id(display_name: &str, model_id: &str) -> String {
    let mut slug: String = format!("{display_name}-{model_id}")
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(40).collect();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("{slug}-{}", &suffix[..6])
}

/// The configured models, plus which one fills each role.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelRegistry {
    pub version: u32,
    pub models: Vec<ModelConfig>,
    /// [`ModelRole::key`] -> [`ModelConfig::id`].
    pub roles: BTreeMap<String, String>,
    /// Normalised base URL -> its API-key pool metadata. Key material stays in
    /// the secret store; this only records ids, labels and active/exhausted
    /// state so rotation survives a restart.
    pub key_pools: BTreeMap<String, ApiKeyPool>,
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self {
            version: REGISTRY_FORMAT_VERSION,
            models: Vec::new(),
            roles: BTreeMap::new(),
            key_pools: BTreeMap::new(),
        }
    }
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry from a list of configs, assigning the first as `chat`.
    pub fn from_configs(models: Vec<ModelConfig>) -> Self {
        let mut registry = Self {
            version: REGISTRY_FORMAT_VERSION,
            models,
            roles: BTreeMap::new(),
            key_pools: BTreeMap::new(),
        };
            if let Some(first_id) = registry.models.first().map(|m| m.id.clone()) {
            registry.set_role(ModelRole::Chat, &first_id);
        }
        registry
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Number of configured models.
    pub fn len_models(&self) -> usize {
        self.models.len()
    }

    pub fn get(&self, id: &str) -> Option<&ModelConfig> {
        self.models.iter().find(|model| model.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut ModelConfig> {
        self.models.iter_mut().find(|model| model.id == id)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    /// Add a model. Returns the id of the stored config.
    pub fn add(&mut self, mut config: ModelConfig) -> String {
        if config.id.is_empty() {
            config.id = generate_id(&config.display_name, &config.model_id);
        }
        if config.created_at.is_none() {
            config.created_at = Some(Utc::now());
        }
        let id = config.id.clone();
        self.models.retain(|model| model.id != id);
        self.models.push(config);
        if self.roles.get(ModelRole::Chat.key()).is_none() {
            self.set_role(ModelRole::Chat, &id);
        }
        id
    }

    /// Insert or replace, matching on id.
    pub fn upsert(&mut self, config: ModelConfig) {
        let id = config.id.clone();
        match self.models.iter_mut().find(|model| model.id == id) {
            Some(existing) => *existing = config,
            None => {
                self.models.push(config);
            }
        }
    }

    /// Duplicate a model under a new name and id ("Duplicate" action).
    pub fn duplicate(&mut self, id: &str, new_name: impl Into<String>) -> Option<String> {
        let mut copy = self.get(id)?.clone();
        copy.display_name = new_name.into();
        copy.id = generate_id(&copy.display_name, &copy.model_id);
        copy.stats = ModelStats::default();
        let new_id = copy.id.clone();
        self.models.push(copy);
        Some(new_id)
    }

    /// Remove a model and clear any role that pointed at it.
    pub fn remove(&mut self, id: &str) -> Option<ModelConfig> {
        let index = self.models.iter().position(|model| model.id == id)?;
        let removed = self.models.remove(index);
        self.roles.retain(|_, value| value != id);
        Some(removed)
    }

    pub fn by_role(&self, role: ModelRole) -> Option<&ModelConfig> {
        let id = self.roles.get(role.key())?;
        self.get(id)
    }

    /// The chat model, or the first configured model as a fallback.
    pub fn chat_model(&self) -> Option<&ModelConfig> {
        self.by_role(ModelRole::Chat).or_else(|| self.models.first())
    }

    /// Fast-task model, falling back to the chat model.
    pub fn fast_model(&self) -> Option<&ModelConfig> {
        self.by_role(ModelRole::Fast).or_else(|| self.chat_model())
    }

    pub fn completion_model(&self) -> Option<&ModelConfig> {
        self.by_role(ModelRole::Completion)
    }

    pub fn set_role(&mut self, role: ModelRole, model_id: &str) {
        if self.contains(model_id) {
            self.roles.insert(role.key().to_string(), model_id.to_string());
        }
    }

    pub fn clear_role(&mut self, role: ModelRole) {
        self.roles.remove(role.key());
    }

    /// Record a completed request against the model's stats.
    pub fn record_usage(
        &mut self,
        id: &str,
        prompt_tokens: usize,
        completion_tokens: usize,
        tokens_per_second: f64,
    ) {
        if let Some(model) = self.get_mut(id) {
            model.stats.record(prompt_tokens, completion_tokens, tokens_per_second);
        }
    }

    /// Serialise without secrets. Keys were never here to begin with; the
    /// `has_api_key` flag is reset so an imported file does not claim a key
    /// exists on the importing machine.
    pub fn export_json(&self) -> String {
        let mut copy = self.clone();
        for model in &mut copy.models {
            model.has_api_key = false;
        }
        // Pools are local state (and would point at ids whose secrets are not
        // in the file). They are not part of a shareable registry.
        copy.key_pools.clear();
        serde_json::to_string_pretty(&copy).unwrap_or_else(|_| "{}".to_string())
    }

    /// Parse an exported registry. Unknown fields are ignored, so files from a
    /// newer build still import.
    pub fn import_json(text: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
        // Accept both a full registry and a bare array of models.
        let mut registry: ModelRegistry = if value.is_array() {
            let models: Vec<ModelConfig> =
                serde_json::from_value(value).map_err(|e| format!("not a model list: {e}"))?;
            ModelRegistry::from_configs(models)
        } else {
            serde_json::from_str(text).map_err(|e| format!("not a model registry: {e}"))?
        };
        // Re-key anything with a missing or duplicate id.
        let mut seen = std::collections::BTreeSet::new();
        for model in &mut registry.models {
            if model.id.is_empty() || !seen.insert(model.id.clone()) {
                model.id = generate_id(&model.display_name, &model.model_id);
                seen.insert(model.id.clone());
            }
            model.has_api_key = false;
        }
        // Imported pools refer to secrets on another machine.
        registry.key_pools.clear();
        registry.version = REGISTRY_FORMAT_VERSION;
        Ok(registry)
    }

    /// Merge an imported registry into this one, keeping existing ids.
    pub fn merge(&mut self, other: ModelRegistry) -> usize {
        let mut added = 0;
        for model in other.models {
            if !self.contains(&model.id) {
                self.models.push(model);
                added += 1;
            }
        }
        if self.roles.get(ModelRole::Chat.key()).is_none() {
            if let Some(first) = self.models.first() {
                let id = first.id.clone();
                self.set_role(ModelRole::Chat, &id);
            }
        }
        added
    }
}

/// Test fixtures. The app ships **no** built-in models: every endpoint is
/// entered by the user in the Models dialog.
#[cfg(test)]
fn sample_configs() -> Vec<ModelConfig> {
    let mut configs = Vec::new();

    let mut ollama = ModelConfig::new("Local Ollama", ProviderKind::Ollama, "qwen2.5-coder:7b");
    ollama.base_url = "http://localhost:11434".to_string();
    ollama.keep_alive = Some("30m".to_string());
    configs.push(ollama);

    let mut lm_studio = ModelConfig::new("LM Studio", ProviderKind::OpenAiCompatible, "local-model");
    lm_studio.base_url = "http://localhost:1234/v1".to_string();
    configs.push(lm_studio);

    let mut openrouter = ModelConfig::new("OpenRouter", ProviderKind::OpenAiCompatible, "anthropic/claude-3.5-sonnet");
    openrouter.base_url = "https://openrouter.ai/api/v1".to_string();
    openrouter.context_window = 200_000;
    configs.push(openrouter);

    let mut openai = ModelConfig::new("OpenAI", ProviderKind::OpenAiCompatible, "gpt-4o-mini");
    openai.base_url = "https://api.openai.com/v1".to_string();
    openai.context_window = 128_000;
    configs.push(openai);

    let mut anthropic = ModelConfig::new("Anthropic", ProviderKind::Anthropic, "claude-sonnet-4-20250514");
    anthropic.context_window = 200_000;
    configs.push(anthropic);

    let mut gemini = ModelConfig::new("Gemini", ProviderKind::Gemini, "gemini-2.0-flash");
    gemini.context_window = 1_000_000;
    configs.push(gemini);

    configs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_valid_and_has_a_unique_id() {
        let presets = sample_configs();
        assert!(presets.len() >= 6);
        let mut ids = std::collections::BTreeSet::new();
        for preset in &presets {
            assert!(preset.validate().is_empty(), "{} failed validation: {:?}", preset.display_name, preset.validate());
            assert!(ids.insert(preset.id.clone()), "duplicate id {}", preset.id);
        }
    }

    #[test]
    fn role_assignment_falls_back_sensibly() {
        let presets = sample_configs();
        let chat_id = presets[0].id.clone();
        let registry = ModelRegistry::from_configs(presets);
        assert_eq!(registry.by_role(ModelRole::Chat).unwrap().id, chat_id);
        // No fast model assigned yet -> falls back to the chat model.
        assert_eq!(registry.fast_model().unwrap().id, chat_id);
        assert!(registry.completion_model().is_none());
    }

    #[test]
    fn removing_a_model_clears_its_roles() {
        let mut registry = ModelRegistry::from_configs(sample_configs());
        let id = registry.models[1].id.clone();
        registry.set_role(ModelRole::Fast, &id);
        registry.remove(&id);
        assert!(registry.by_role(ModelRole::Fast).is_none());
        assert!(!registry.contains(&id));
    }

    #[test]
    fn duplication_resets_stats_and_rekeys() {
        let mut registry = ModelRegistry::from_configs(vec![sample_configs().remove(0)]);
        let original = registry.models[0].id.clone();
        registry.record_usage(&original, 100, 100, 20.0);
        let copy = registry.duplicate(&original, "Copy").unwrap();
        assert_ne!(copy, original);
        assert_eq!(registry.get(&copy).unwrap().stats.requests, 0);
        assert_eq!(registry.len_models(), 2);
    }

    #[test]
    fn export_never_contains_key_material() {
        let mut registry = ModelRegistry::from_configs(sample_configs());
        for model in &mut registry.models {
            model.has_api_key = true;
        }
        let exported = registry.export_json();
        assert!(!exported.contains("\"has_api_key\": true"));
        assert!(!exported.contains("sk-"));
    }

    #[test]
    fn import_round_trips_and_rekeys_duplicates() {
        let registry = ModelRegistry::from_configs(sample_configs());
        let exported = registry.export_json();
        let imported = ModelRegistry::import_json(&exported).unwrap();
        assert_eq!(imported.models.len(), registry.models.len());

        // A bare array is accepted too.
        let array = format!("[{}]", serde_json::to_string(&registry.models[0]).unwrap());
        let imported_array = ModelRegistry::import_json(&array).unwrap();
        assert_eq!(imported_array.models.len(), 1);

        // Duplicate ids inside one document get re-keyed rather than colliding.
        let duplicated = format!(
            "[{}]",
            vec![serde_json::to_string(&registry.models[0]).unwrap(); 2].join(",")
        );
        let rekeyed = ModelRegistry::import_json(&duplicated).unwrap();
        assert_ne!(rekeyed.models[0].id, rekeyed.models[1].id);
    }

    #[test]
    fn import_rejects_garbage_without_panicking() {
        assert!(ModelRegistry::import_json("not json").is_err());
        assert!(ModelRegistry::import_json("{\"models\": 5}").is_err());
    }

    #[test]
    fn merge_only_adds_new_models() {
        let models = sample_configs();
        let mut target = ModelRegistry::from_configs(vec![models[0].clone()]);
        let source = ModelRegistry::from_configs(models);
        let before = target.models.len();
        let added = target.merge(source.clone());
        assert_eq!(target.models.len(), before + added);
        assert_eq!(added, source.models.len() - 1);
        assert_eq!(target.merge(source), 0);
    }

    #[test]
    fn api_key_requirements_depend_on_the_endpoint() {
        let ollama = ModelConfig::new("o", ProviderKind::Ollama, "m");
        assert!(!ollama.requires_api_key());
        assert!(ollama.is_local());
        let mut remote = ModelConfig::new("r", ProviderKind::OpenAiCompatible, "m");
        remote.base_url = "https://api.example.com/v1".to_string();
        assert!(remote.requires_api_key());
        assert!(!remote.is_local());
    }

    #[test]
    fn validation_catches_common_mistakes() {
        let mut config = ModelConfig::new("", ProviderKind::Ollama, "");
        config.max_output_tokens = 100_000;
        let problems = config.validate();
        assert!(problems.iter().any(|p| p.contains("Display name")));
        assert!(problems.iter().any(|p| p.contains("Model ID")));
        assert!(problems.iter().any(|p| p.contains("Max output tokens")));
    }

    #[test]
    fn base_urls_are_normalised_to_what_the_adapters_expect() {
        // A bare host gets /v1 for OpenAI-compatible endpoints.
        assert_eq!(
            normalize_base_url(ProviderKind::OpenAiCompatible, "https://api.openai.com/"),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            normalize_base_url(ProviderKind::OpenAiCompatible, "http://localhost:11434"),
            "http://localhost:11434/v1"
        );
        // A pasted full endpoint is reduced to its root.
        assert_eq!(
            normalize_base_url(
                ProviderKind::OpenAiCompatible,
                "https://api.deepseek.com/v1/chat/completions"
            ),
            "https://api.deepseek.com/v1"
        );
        // Existing versioned paths are left alone.
        assert_eq!(
            normalize_base_url(ProviderKind::OpenAiCompatible, "https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1"
        );
        // Anthropic's adapter adds /v1 itself.
        assert_eq!(
            normalize_base_url(ProviderKind::Anthropic, "https://api.anthropic.com/v1"),
            "https://api.anthropic.com"
        );
        // And chat_url never doubles the version segment.
        let mut anthropic = ModelConfig::new("a", ProviderKind::Anthropic, "m");
        anthropic.base_url = "https://api.anthropic.com/v1".to_string();
        assert_eq!(anthropic.chat_url(), "https://api.anthropic.com/v1/messages");
    }
}
