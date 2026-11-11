//! The single trait every endpoint implements.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::Stream;

use crate::error::{LinkModelError, ModelResult};
use crate::registry::{ModelConfig, ProviderKind};
use crate::types::{Capabilities, ChatRequest, ModelInfo, StreamEvent, Usage};

/// A boxed, `Send` stream of normalised events.
///
/// Cancellation is implemented by *dropping* the stream: the agent loop wraps
/// generation in a `tokio::select!` against the cancellation token, so pressing
/// Stop tears the HTTP response down immediately.
pub type ModelStream = Pin<Box<dyn Stream<Item = ModelResult<StreamEvent>> + Send>>;

/// The result of "Test connection".
#[derive(Debug, Clone)]
pub struct ConnectionReport {
    pub model: String,
    pub latency_ms: u64,
    pub tokens_per_second: f64,
    pub tool_calling: bool,
    pub streaming: bool,
    pub message: String,
}

impl ConnectionReport {
    /// `124 ms · 41.2 tok/s · tools ok`
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{} ms", self.latency_ms)];
        if self.tokens_per_second > 0.0 {
            parts.push(format!("{:.1} tok/s", self.tokens_per_second));
        }
        parts.push(if self.tool_calling {
            "tool calling ok".to_string()
        } else {
            "no native tool calling".to_string()
        });
        parts.join(" \u{00b7} ")
    }
}

/// One endpoint.
#[async_trait]
pub trait ModelProvider: Send + Sync + 'static {
    /// Configuration id this provider was built from.
    fn id(&self) -> &str;

    /// Human label used on the model badge, e.g. `ollama \u{00b7} qwen2.5-coder:32b`.
    fn label(&self) -> &str;

    fn provider_kind(&self) -> ProviderKind;

    fn capabilities(&self) -> Capabilities;

    /// Effective context window in tokens.
    fn context_window(&self) -> usize;

    /// Rough token count for a piece of text. Adapters that can count exactly
    /// override this; the default is the same 4-characters-per-token estimate
    /// the context meter uses.
    fn count_tokens(&self, text: &str) -> usize {
        if text.is_empty() {
            0
        } else {
            text.chars().count() / 4 + 1
        }
    }

    /// Models the endpoint advertises. Used by the modal's "Fetch models".
    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>>;

    /// Stream a chat completion.
    async fn stream_chat(&self, request: ChatRequest) -> ModelResult<ModelStream>;

    /// Non-streaming completion. Used for titles, suggestions and compaction.
    async fn complete(&self, request: ChatRequest) -> ModelResult<(String, Usage)>;

    /// Cheap round-trip used by "Test connection".
    async fn test_connection(&self) -> ModelResult<ConnectionReport> {
        let started = std::time::Instant::now();
        let mut request = ChatRequest::new(self.label());
        request.messages = vec![crate::types::ChatMessage::user("ping")];
        request.max_tokens = Some(4);
        request.temperature = Some(0.0);
        let (text, usage) = self.complete(request).await?;
        let elapsed = started.elapsed();
        Ok(ConnectionReport {
            model: self.label().to_string(),
            latency_ms: elapsed.as_millis() as u64,
            tokens_per_second: usage.tokens_per_second(elapsed),
            tool_calling: self.capabilities().tools,
            streaming: self.capabilities().streaming,
            message: format!("Replied with {}", crate::sse::truncate(text.trim(), 60)),
        })
    }
}

/// Builds the provider for a given configuration.
pub trait ProviderFactory: Send + Sync {
    fn create(&self, config: &ModelConfig, api_key: Option<String>) -> Arc<dyn ModelProvider>;
}

/// Dispatch to the adapter for a configuration's provider kind.
///
/// This is the only place that knows the mapping from [`ProviderKind`] to an
/// implementation, which keeps `link-ui` free of adapter details.
pub fn provider_for(config: &ModelConfig, api_key: Option<String>) -> Arc<dyn ModelProvider> {
    match config.provider {
        ProviderKind::OpenAiCompatible => {
            Arc::new(crate::openai::OpenAiProvider::new(config.clone(), api_key))
        }
        ProviderKind::Anthropic => Arc::new(crate::anthropic::AnthropicProvider::new(config.clone(), api_key)),
        ProviderKind::Gemini => Arc::new(crate::gemini::GeminiProvider::new(config.clone(), api_key)),
        ProviderKind::Ollama => Arc::new(crate::ollama::OllamaProvider::new(config.clone(), api_key)),
    }
}

/// Retry a provider operation with exponential backoff, honouring `Retry-After`.
///
/// Only used for *establishing* a request: once tokens are streaming, retrying
/// would duplicate output, so the agent loop deliberately does not retry there.
pub async fn with_retry<F, Fut, T>(attempts: u32, mut operation: F) -> ModelResult<T>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = ModelResult<T>>,
{
    let attempts = attempts.max(1);
    let mut last_error = None;
    for attempt in 0..attempts {
        match operation(attempt).await {
            Ok(value) => return Ok(value),
            Err(error) => {
                if !error.is_retryable() || attempt + 1 == attempts {
                    return Err(error);
                }
                let delay = error.retry_after().unwrap_or_else(|| {
                    Duration::from_millis(250u64.saturating_mul(1 << attempt.min(5)))
                });
                tracing::warn!(?error, attempt, delay_ms = delay.as_millis(), "retrying provider call");
                last_error = Some(error);
                tokio::time::sleep(delay.min(Duration::from_secs(30))).await;
            }
        }
    }
    Err(last_error.unwrap_or_else(|| LinkModelError::Other("retry loop exhausted".to_string())))
}
