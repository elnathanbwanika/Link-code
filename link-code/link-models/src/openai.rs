//! OpenAI-compatible adapter.
//!
//! Covers OpenAI itself and everything that imitates `/chat/completions`:
//! Ollama's compatibility layer, llama.cpp's server, LM Studio, vLLM,
//! OpenRouter, Groq, Together, DeepSeek and any custom gateway.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};

use crate::error::{classify_http, LinkModelError, ModelResult};
use crate::provider::{ConnectionReport, ModelProvider, ModelStream};
use crate::registry::{ModelConfig, ProviderKind};
use crate::sse::{finish_reason, json_events, truncate, ThinkSplitter};
use crate::types::{Capabilities, ChatMessage, ChatRequest, FinishReason, ImageAttachment, ModelInfo, Role, StreamEvent, Usage};

/// Strip decoration that comes along when a key is copied from a dashboard or
/// a shell: wrapping quotes, a `Bearer` prefix, a header name, a BOM and
/// zero-width characters. Sending any of those turns a valid key into HTTP 401.
pub fn sanitize_api_key(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    *c,
                    '\u{feff}' | '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}'
                )
        })
        .collect();

    let mut key = cleaned.trim();
    for _ in 0..2 {
        key = key
            .trim_matches(|c| c == '"' || c == '\'' || c == '`')
            .trim();
        match strip_key_prefix(key) {
            Some(stripped) => key = stripped,
            None => break,
        }
    }
    key.trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .trim()
        .to_string()
}

fn strip_key_prefix(text: &str) -> Option<&str> {
    for prefix in ["authorization:", "api-key:", "x-api-key:", "bearer "] {
        if let Some(head) = text.get(..prefix.len()) {
            if head.eq_ignore_ascii_case(prefix) {
                return Some(text[prefix.len()..].trim());
            }
        }
    }
    None
}

/// Shared HTTP plumbing for the text/JSON adapters.
#[derive(Clone)]
pub(crate) struct HttpClient {
    pub(crate) client: reqwest::Client,
    pub(crate) api_key: Option<String>,
    pub(crate) extra_headers: Vec<(String, String)>,
}

impl HttpClient {
    pub(crate) fn new(extra_headers: &[(String, String)], api_key: Option<String>) -> Self {
        let client = crate::proxy::apply(reqwest::Client::builder())
            .user_agent(concat!("LinkCode/", env!("CARGO_PKG_VERSION")))
            // Local models can be slow to accept a connection when loading.
            .connect_timeout(std::time::Duration::from_secs(20))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .build()
            .unwrap_or_default();
        // A key pasted from a terminal often carries a trailing newline or
        // space. Sending that verbatim makes otherwise valid keys fail with
        // 401, so normalise it once here.
        let api_key = api_key
            .map(|key| sanitize_api_key(&key))
            .filter(|key| !key.is_empty());
        Self {
            client,
            api_key,
            extra_headers: extra_headers.to_vec(),
        }
    }

    pub(crate) fn apply(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut builder = builder;
        for (name, value) in &self.extra_headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        builder
    }

    /// Attach the API key in every shape OpenAI-compatible servers use.
    ///
    /// `Authorization: Bearer` is the standard; a few gateways (notably Azure
    /// and some proxies) read `api-key` instead. Sending both is harmless for
    /// servers that only look at one of them.
    pub(crate) fn auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) => builder.bearer_auth(key).header("api-key", key.as_str()),
            None => builder,
        }
    }

    /// Turn a non-success response into a categorised error.
    pub(crate) async fn check(&self, url: &str, response: reqwest::Response) -> ModelResult<reqwest::Response> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.text().await.unwrap_or_default();
        // A 401 with no key attached is our configuration problem, not a
        // rejected credential; reporting it as "rejected" sends the user
        // hunting for a key that is actually missing.
        if self.api_key.is_none() && matches!(status.as_u16(), 401 | 403) {
            return Err(LinkModelError::MissingApiKey {
                url: url.to_string(),
            });
        }
        Err(classify_http(url, status, &headers, truncate(body.trim(), 600)))
    }

    pub(crate) async fn get_json(&self, url: &str) -> ModelResult<Value> {
        let builder = self.auth(self.apply(self.client.get(url)));
        let response = builder
            .send()
            .await
            .map_err(|e| LinkModelError::Connection {
                url: url.to_string(),
                source: Box::new(e),
            })?;
        let response = self.check(url, response).await?;
        response.json::<Value>().await.map_err(|e| LinkModelError::Malformed(format!("expected JSON from {url}: {e}")))
    }
}

/// An OpenAI-compatible endpoint.
pub struct OpenAiProvider {
    config: ModelConfig,
    http: HttpClient,
}

impl OpenAiProvider {
    pub fn new(config: ModelConfig, api_key: Option<String>) -> Self {
        let http = HttpClient::new(&config.extra_headers, api_key);
        Self { config, http }
    }

    /// Render one message in OpenAI's wire format.
    fn wire_message(message: &ChatMessage) -> Value {
        let content = if message.images.is_empty() {
            Value::String(message.content.clone())
        } else {
            let mut parts = vec![json!({ "type": "text", "text": message.content })];
            for image in &message.images {
                parts.push(image_part(image));
            }
            Value::Array(parts)
        };

        let mut object = Map::new();
        object.insert("role".to_string(), Value::String(message.role.as_str().to_string()));
        object.insert("content".to_string(), content);
        if !message.tool_calls.is_empty() {
            let calls: Vec<Value> = message
                .tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": { "name": call.name, "arguments": call.arguments }
                    })
                })
                .collect();
            object.insert("tool_calls".to_string(), Value::Array(calls));
        }
        if message.role == Role::Tool {
            if let Some(id) = &message.tool_call_id {
                object.insert("tool_call_id".to_string(), Value::String(id.clone()));
            }
        }
        if let Some(name) = &message.name {
            if message.role == Role::Tool {
                object.insert("name".to_string(), Value::String(name.clone()));
            }
        }
        Value::Object(object)
    }

    fn build_body(&self, request: &ChatRequest, stream: bool) -> Value {
        let mut messages = Vec::new();
        if let Some(system) = request.system.as_deref() {
            if !system.trim().is_empty() {
                messages.push(json!({ "role": "system", "content": system }));
            }
        }
        // A system message already inside the history is de-duplicated by role
        // order: providers accept several, and the agent loop sends at most one.
        messages.extend(request.messages.iter().map(Self::wire_message));

        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(request.model.clone()));
        body.insert("messages".to_string(), Value::Array(messages));
        body.insert("stream".to_string(), Value::Bool(stream));
        if stream {
            // Ask for a final usage frame; servers that do not support it ignore it.
            body.insert("stream_options".to_string(), json!({ "include_usage": true }));
        }
        if !request.tools.is_empty() {
            body.insert("tools".to_string(), Value::Array(request.tools.clone()));
            body.insert("tool_choice".to_string(), Value::String("auto".to_string()));
        }
        if let Some(temperature) = request.temperature {
            body.insert("temperature".to_string(), json!(temperature));
        }
        if let Some(top_p) = request.top_p {
            body.insert("top_p".to_string(), json!(top_p));
        }
        if let Some(max_tokens) = request.max_tokens {
            body.insert("max_tokens".to_string(), json!(max_tokens));
        }
        if !request.stop.is_empty() {
            body.insert("stop".to_string(), json!(request.stop));
        }
        if let Some(extra) = &request.extra_body {
            if let Some(object) = extra.as_object() {
                for (key, value) in object {
                    body.insert(key.clone(), value.clone());
                }
            }
        }
        Value::Object(body)
    }
}

fn image_part(image: &ImageAttachment) -> Value {
    json!({
        "type": "image_url",
        "image_url": { "url": image.data_url() }
    })
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn label(&self) -> &str {
        &self.config.display_name
    }

    fn provider_kind(&self) -> ProviderKind {
        self.config.provider
    }

    fn capabilities(&self) -> Capabilities {
        self.config.capabilities
    }

    fn context_window(&self) -> usize {
        self.config.context_window
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        let url = self.config.models_url();
        let value = self.http.get_json(&url).await?;
        // OpenAI returns `{ "data": [...] }`; Ollama's compat layer and some
        // gateways return a bare array or a `models` key. Accept all of them.
        let entries: Vec<&Value> = if let Some(array) = value.as_array() {
            array.iter().collect()
        } else if let Some(array) = value.get("data").and_then(Value::as_array) {
            array.iter().collect()
        } else if let Some(array) = value.get("models").and_then(Value::as_array) {
            array.iter().collect()
        } else if let Some(array) = value
            .get("data")
            .and_then(|data| data.get("models"))
            .and_then(Value::as_array)
        {
            array.iter().collect()
        } else {
            Vec::new()
        };
        let mut models = Vec::new();
        for entry in entries {
            let id = entry
                .get("id")
                .or_else(|| entry.get("name"))
                .or_else(|| entry.get("model"))
                .and_then(Value::as_str);
            if let Some(id) = id {
                if id.trim().is_empty() {
                    continue;
                }
                let mut info = ModelInfo::new(id);
                info.name = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .filter(|name| name != id);
                info.context_window = entry
                    .get("context_length")
                    .or_else(|| entry.get("context_window"))
                    .or_else(|| entry.get("max_context_length"))
                    .and_then(Value::as_u64)
                    .map(|value| value as usize);
                models.push(info);
            }
        }
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models.dedup_by(|a, b| a.id == b.id);
        Ok(models)
    }

    async fn stream_chat(&self, request: ChatRequest) -> ModelResult<ModelStream> {
        let url = self.config.chat_url();
        let body = self.build_body(&request, true);
        let builder = self.http.auth(self.http.apply(self.http.client.post(&url)).json(&body));
        let response = builder.send().await.map_err(|e| LinkModelError::Connection {
            url: url.clone(),
            source: Box::new(e),
        })?;
        let response = self.http.check(&url, response).await?;

        let events = json_events(response);
        let mut splitter = ThinkSplitter::new();
        let stream = async_stream::stream! {
            futures_util::pin_mut!(events);
            // Many gateways end the stream with `[DONE]` or a closed
            // connection and never send a `finish_reason` chunk. Track it so a
            // clean end can be reported as a normal finish instead of a
            // truncated reply.
            let mut finished = false;
            let mut terminal = FinishReason::Stop;
            while let Some(item) = events.next().await {
                match item {
                    Err(error) => {
                        // A malformed frame is reported and skipped; the loop
                        // keeps going so a long generation is not lost.
                        yield Err(error);
                    }
                    Ok(value) => {
                        if let Some(usage) = extract_usage(&value) {
                            yield Ok(StreamEvent::Usage(usage));
                        }
                        let Some(choice) = value.get("choices").and_then(|c| c.get(0)) else {
                            continue;
                        };
                        if let Some(delta) = choice.get("delta") {
                            if let Some(reasoning) = delta
                                .get("reasoning_content")
                                .or_else(|| delta.get("reasoning"))
                                .or_else(|| delta.get("thinking"))
                                .and_then(Value::as_str)
                            {
                                if !reasoning.is_empty() {
                                    yield Ok(StreamEvent::ThinkingDelta(reasoning.to_string()));
                                }
                            }
                            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                                for event in splitter.push(text) {
                                    yield Ok(event);
                                }
                            }
                            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                                for call in calls {
                                    let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                                    let id = call.get("id").and_then(Value::as_str).map(str::to_string);
                                    let function = call.get("function");
                                    let name = function
                                        .and_then(|f| f.get("name"))
                                        .and_then(Value::as_str)
                                        .map(str::to_string);
                                    let arguments_delta = function
                                        .and_then(|f| f.get("arguments"))
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string();
                                    let event = StreamEvent::ToolCallDelta {
                                        index,
                                        id,
                                        name,
                                        arguments_delta,
                                    };
                                    yield Ok(event);
                                }
                            }
                        }
                        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                            if reason != "" && reason != "null" {
                                terminal = finish_reason(Some(reason));
                                finished = true;
                                yield Ok(StreamEvent::Done {
                                    finish_reason: terminal.clone(),
                                });
                            }
                        }
                    }
                }
            }
            for event in splitter.flush() {
                yield Ok(event);
            }
            // The stream ended without an explicit finish reason: `[DONE]` was
            // seen or the server closed cleanly. That is a complete reply.
            if !finished {
                yield Ok(StreamEvent::Done { finish_reason: terminal });
            }
        };
        Ok(Box::pin(stream))
    }

    async fn complete(&self, request: ChatRequest) -> ModelResult<(String, Usage)> {
        let url = self.config.chat_url();
        let mut body = self.build_body(&request, false);
        // Streaming flags must not leak into a non-streaming call.
        if let Some(object) = body.as_object_mut() {
            object.remove("stream_options");
        }
        let builder = self.http.auth(self.http.apply(self.http.client.post(&url)).json(&body));
        let response = builder.send().await.map_err(|e| LinkModelError::Connection {
            url: url.clone(),
            source: Box::new(e),
        })?;
        let value = self.http.check(&url, response).await?.json::<Value>().await.map_err(|e| {
            LinkModelError::Malformed(format!("expected JSON from {url}: {e}"))
        })?;
        let text = value
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let usage = extract_usage(&value).unwrap_or_default();
        Ok((text, usage))
    }

    async fn test_connection(&self) -> ModelResult<ConnectionReport> {
        let started = std::time::Instant::now();
        let mut request = ChatRequest::new(&self.config.model_id);
        request.messages = vec![ChatMessage::user("Reply with the single word: pong")];
        request.max_tokens = Some(8);
        request.temperature = Some(0.0);
        let (text, usage) = self.complete(request).await?;
        let elapsed = started.elapsed();
        Ok(ConnectionReport {
            model: self.config.badge(),
            latency_ms: elapsed.as_millis() as u64,
            tokens_per_second: usage.tokens_per_second(elapsed),
            tool_calling: self.config.capabilities.tools,
            streaming: self.config.capabilities.streaming,
            message: format!("Replied: {}", truncate(text.trim(), 60)),
        })
    }
}

/// Read `usage` from either a streaming frame or a full response.
fn extract_usage(value: &Value) -> Option<Usage> {
    let usage = value.get("usage")?;
    let prompt = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let completion = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    if prompt == 0 && completion == 0 {
        None
    } else {
        Some(Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
        })
    }
}

/// Exposed for the registry so `provider_for` can be unit tested.
pub fn make(config: ModelConfig, api_key: Option<String>) -> Arc<dyn ModelProvider> {
    Arc::new(OpenAiProvider::new(config, api_key))
}

#[cfg(test)]
mod tests {
    use super::sanitize_api_key;

    #[test]
    fn sanitizes_pasted_keys() {
        assert_eq!(sanitize_api_key("  sk-abc\n"), "sk-abc");
        assert_eq!(sanitize_api_key("\"sk-abc\""), "sk-abc");
        assert_eq!(sanitize_api_key("'sk-abc'"), "sk-abc");
        assert_eq!(sanitize_api_key("Bearer sk-abc"), "sk-abc");
        assert_eq!(sanitize_api_key("bearer   sk-abc"), "sk-abc");
        assert_eq!(sanitize_api_key("Authorization: Bearer sk-abc"), "sk-abc");
        assert_eq!(sanitize_api_key("api-key: sk-abc"), "sk-abc");
        assert_eq!(sanitize_api_key("\u{feff}sk-\u{200b}abc"), "sk-abc");
        assert_eq!(sanitize_api_key("   "), "");
    }

    #[test]
    fn keeps_internal_characters() {
        assert_eq!(sanitize_api_key("sk-a_b-c.d"), "sk-a_b-c.d");
    }
}
