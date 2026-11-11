//! Endpoint discovery: "Fetch models", "Test connection", and the first-launch
//! probe that offers to import a local Ollama install in one click.
//!
//! Every probe here is short and cancellable, runs on the tokio runtime (never
//! the GTK main thread), and returns `None` rather than an error when nothing is
//! listening — a machine without Ollama is the normal case, not a failure.

use std::time::Duration;

use crate::error::ModelResult;
use crate::provider::{provider_for, ConnectionReport};
use crate::registry::{ModelConfig, ProviderKind};
use crate::types::ModelInfo;

/// Ports probed on first launch, in priority order.
const LOCAL_PROBES: &[(&str, u16, ProviderKind, &str)] = &[
    ("Ollama", 11434, ProviderKind::Ollama, "http://localhost:11434"),
    ("LM Studio", 1234, ProviderKind::OpenAiCompatible, "http://localhost:1234/v1"),
    ("llama.cpp server", 8080, ProviderKind::OpenAiCompatible, "http://localhost:8080/v1"),
];

/// A listening endpoint found on this machine.
#[derive(Debug, Clone)]
pub struct DetectedEndpoint {
    pub name: String,
    pub base_url: String,
    pub provider: ProviderKind,
    pub models: Vec<ModelInfo>,
    /// Version string when the endpoint reports one (Ollama does).
    pub version: Option<String>,
}

/// List the models a configured endpoint advertises.
pub async fn fetch_models(config: &ModelConfig, api_key: Option<String>) -> ModelResult<Vec<ModelInfo>> {
    let provider = provider_for(config, api_key);
    provider.list_models().await
}

/// Send a tiny request and report latency, speed and tool support.
pub async fn test_connection(
    config: &ModelConfig,
    api_key: Option<String>,
) -> ModelResult<ConnectionReport> {
    let provider = provider_for(config, api_key);
    provider.test_connection().await
}

/// Probe the well-known local ports. Returns every endpoint that answered.
pub async fn detect_local_endpoints() -> Vec<DetectedEndpoint> {
    let mut found = Vec::new();
    for (name, port, provider, base_url) in LOCAL_PROBES {
        if !port_is_open(*port).await {
            continue;
        }
        let mut config = ModelConfig::default();
        config.provider = *provider;
        config.base_url = (*base_url).to_string();
        config.model_id = String::new();
        config.display_name = (*name).to_string();
        let models = match fetch_models(&config, None).await {
            Ok(models) => models,
            Err(error) => {
                tracing::debug!(?error, endpoint = name, "endpoint answered but model listing failed");
                Vec::new()
            }
        };
        let version = if *provider == ProviderKind::Ollama {
            detect_ollama_version(base_url).await
        } else {
            None
        };
        found.push(DetectedEndpoint {
            name: (*name).to_string(),
            base_url: (*base_url).to_string(),
            provider: *provider,
            models,
            version,
        });
    }
    found
}

/// Build ready-to-use configs from everything discovered, one per model.
pub fn configs_from_detection(endpoint: &DetectedEndpoint) -> Vec<ModelConfig> {
    if endpoint.models.is_empty() {
        let mut config = ModelConfig::default();
        config.provider = endpoint.provider;
        config.base_url = endpoint.base_url.clone();
        config.display_name = endpoint.name.clone();
        config.model_id = "local-model".to_string();
        return vec![config];
    }
    endpoint
        .models
        .iter()
        .map(|info| {
            let mut config = ModelConfig::new(
                format!("{} ({})", info.id, endpoint.name),
                endpoint.provider,
                info.id.clone(),
            );
            config.base_url = endpoint.base_url.clone();
            if let Some(window) = info.context_window {
                config.context_window = window;
            }
            config.max_output_tokens = config.max_output_tokens.min(config.context_window as u32 / 2);
            config.capabilities.fim = true;
            config
        })
        .collect()
}

/// TCP connect check with a short timeout.
async fn port_is_open(port: u16) -> bool {
    let address = format!("127.0.0.1:{port}");
    matches!(
        tokio::time::timeout(
            Duration::from_millis(350),
            tokio::net::TcpStream::connect(address)
        )
        .await,
        Ok(Ok(_))
    )
}

/// Ollama exposes `GET /api/version`.
async fn detect_ollama_version(base_url: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(800))
        .build()
        .ok()?;
    let url = format!("{}/api/version", base_url.trim_end_matches('/'));
    let value = client.get(url).send().await.ok()?.json::<serde_json::Value>().await.ok()?;
    value.get("version").and_then(|v| v.as_str()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_produces_one_config_per_model() {
        let endpoint = DetectedEndpoint {
            name: "Ollama".to_string(),
            base_url: "http://localhost:11434".to_string(),
            provider: ProviderKind::Ollama,
            models: vec![
                ModelInfo::new("qwen2.5-coder:7b"),
                ModelInfo::new("llama3.2:3b"),
            ],
            version: Some("0.5.7".to_string()),
        };
        let configs = configs_from_detection(&endpoint);
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].base_url, "http://localhost:11434");
        assert_eq!(configs[0].model_id, "qwen2.5-coder:7b");
        assert!(configs[0].validate().is_empty());
        assert!(configs[0].is_local());
    }

    #[test]
    fn detection_without_models_still_offers_the_endpoint() {
        let endpoint = DetectedEndpoint {
            name: "Llama.cpp".to_string(),
            base_url: "http://localhost:8080/v1".to_string(),
            provider: ProviderKind::OpenAiCompatible,
            models: Vec::new(),
            version: None,
        };
        let configs = configs_from_detection(&endpoint);
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].model_id, "local-model");
    }
}
