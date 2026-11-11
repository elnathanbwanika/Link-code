//! Ollama's native API.
//!
//! Unlike the OpenAI-compatible adapters, Ollama streams **newline-delimited
//! JSON**, not SSE, and it supports extras the compatibility layer drops:
//! `keep_alive`, `num_ctx`, and a separate `thinking` field.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::{Stream, StreamExt};
use serde_json::{json, Map, Value};

use crate::error::{LinkModelError, ModelResult};
use crate::openai::HttpClient;
use crate::provider::{ConnectionReport, ModelProvider, ModelStream};
use crate::registry::{ModelConfig, ProviderKind};
use crate::sse::{finish_reason, truncate, ThinkSplitter};
use crate::types::{Capabilities, ChatMessage, ChatRequest, ModelInfo, StreamEvent, Usage};

/// Split a response body into NDJSON values, streaming.
pub(crate) fn ndjson_events(
    response: reqwest::Response,
) -> impl Stream<Item = ModelResult<Value>> {
    futures_util::stream::unfold(
        (response.bytes_stream(), String::new(), false),
        |(mut bytes, mut buffer, done)| async move {
            if done {
                return None;
            }
            loop {
                if let Some(position) = buffer.find('\n') {
                    let line = buffer[..position].trim().to_string();
                    buffer.drain(..=position);
                    if line.is_empty() {
                        continue;
                    }
                    let item = serde_json::from_str::<Value>(&line).map_err(|e| {
                        LinkModelError::Malformed(format!(
                            "Ollama sent a line that is not JSON ({e}): {}",
                            truncate(&line, 200)
                        ))
                    });
                    return Some((item, (bytes, buffer, false)));
                }
                match bytes.next().await {
                    Some(Ok(chunk)) => {
                        buffer.push_str(&String::from_utf8_lossy(&chunk));
                    }
                    Some(Err(error)) => {
                        return Some((
                            Err(LinkModelError::Stream(error.to_string())),
                            (bytes, buffer, true),
                        ))
                    }
                    None => {
                        let rest = std::mem::take(&mut buffer);
                        if rest.trim().is_empty() {
                            return None;
                        }
                        let item = serde_json::from_str::<Value>(&rest).map_err(|e| {
                            LinkModelError::Malformed(format!("trailing Ollama payload: {e}"))
                        });
                        return Some((item, (bytes, String::new(), true)));
                    }
                }
            }
        },
    )
}

/// An Ollama server.
pub struct OllamaProvider {
    config: ModelConfig,
    http: HttpClient,
}

impl OllamaProvider {
    pub fn new(config: ModelConfig, api_key: Option<String>) -> Self {
        let http = HttpClient::new(&config.extra_headers, api_key);
        Self { config, http }
    }

    fn wire_message(message: &ChatMessage) -> Value {
        let mut object = Map::new();
        object.insert("role".to_string(), Value::String(message.role.as_str().to_string()));
        object.insert("content".to_string(), Value::String(message.content.clone()));
        if !message.images.is_empty() {
            let images: Vec<Value> = message
                .images
                .iter()
                .map(|image| Value::String(image.data_base64.clone()))
                .collect();
            object.insert("images".to_string(), Value::Array(images));
        }
        if !message.tool_calls.is_empty() {
            let calls: Vec<Value> = message
                .tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "function": {
                            "name": call.name,
                            "arguments": call
                                .parsed_arguments()
                                .unwrap_or_else(|| json!({}))
                        }
                    })
                })
                .collect();
            object.insert("tool_calls".to_string(), Value::Array(calls));
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
        messages.extend(request.messages.iter().map(Self::wire_message));

        let mut options = Map::new();
        options.insert(
            "num_ctx".to_string(),
            json!(self.config.context_window.min(1_000_000)),
        );
        if let Some(max_tokens) = request.max_tokens.or(Some(self.config.max_output_tokens)) {
            options.insert("num_predict".to_string(), json!(max_tokens));
        }
        if let Some(temperature) = request.temperature.or(self.config.temperature) {
            options.insert("temperature".to_string(), json!(temperature));
        }
        if let Some(top_p) = request.top_p.or(self.config.top_p) {
            options.insert("top_p".to_string(), json!(top_p));
        }

        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(request.model.clone()));
        body.insert("messages".to_string(), Value::Array(messages));
        body.insert("stream".to_string(), Value::Bool(stream));
        body.insert("options".to_string(), Value::Object(options));
        if let Some(keep_alive) = &self.config.keep_alive {
            body.insert("keep_alive".to_string(), Value::String(keep_alive.clone()));
        }
        if !request.tools.is_empty() {
            body.insert("tools".to_string(), Value::Array(request.tools.clone()));
        }
        if let Some(extra) = &request.extra_body {
            if let Some(object) = extra.as_object() {
                for (key, value) in object {
                    if key == "options" {
                        if let (Some(base), Some(overrides)) = (
                            body.get_mut("options").and_then(Value::as_object_mut),
                            value.as_object(),
                        ) {
                            for (key, value) in overrides {
                                base.insert(key.clone(), value.clone());
                            }
                        }
                    } else {
                        body.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        Value::Object(body)
    }
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn label(&self) -> &str {
        &self.config.display_name
    }

    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::Ollama
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
        let mut models = Vec::new();
        for entry in value
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let Some(name) = entry.get("name").and_then(Value::as_str) else {
                continue;
            };
            let mut info = ModelInfo::new(name);
            info.size_bytes = entry.get("size").and_then(Value::as_u64);
            info.modified_at = entry.get("modified_at").and_then(Value::as_str).map(str::to_string);
            info.parameter_count = entry
                .get("details")
                .and_then(|d| d.get("parameter_size"))
                .and_then(Value::as_str)
                .map(str::to_string);
            models.push(info);
        }
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    async fn stream_chat(&self, request: ChatRequest) -> ModelResult<ModelStream> {
        let url = self.config.chat_url();
        let body = self.build_body(&request, true);
        let builder = self.http.apply(self.http.client.post(&url)).json(&body);
        let builder = match &self.http.api_key {
            Some(key) => builder.bearer_auth(key),
            None => builder,
        };
        let response = builder.send().await.map_err(|e| LinkModelError::Connection {
            url: url.clone(),
            source: Box::new(e),
        })?;
        let response = self.http.check(&url, response).await?;

        let events = ndjson_events(response);
        let mut splitter = ThinkSplitter::new();
        let stream = async_stream::stream! {
            futures_util::pin_mut!(events);
            let mut finish = finish_reason(None);
            while let Some(item) = events.next().await {
                match item {
                    Err(error) => yield Err(error),
                    Ok(value) => {
                        let done = value.get("done").and_then(Value::as_bool).unwrap_or(false);
                        if let Some(message) = value.get("message") {
                            if let Some(thinking) = message.get("thinking").and_then(Value::as_str) {
                                if !thinking.is_empty() {
                                    yield Ok(StreamEvent::ThinkingDelta(thinking.to_string()));
                                }
                            }
                            if let Some(text) = message.get("content").and_then(Value::as_str) {
                                for event in splitter.push(text) {
                                    yield Ok(event);
                                }
                            }
                            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                                for (index, call) in calls.iter().enumerate() {
                                    let function = call.get("function");
                                    let name = function
                                        .and_then(|f| f.get("name"))
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string();
                                    let arguments = function
                                        .and_then(|f| f.get("arguments"))
                                        .map(|value| {
                                            if let Some(text) = value.as_str() {
                                                text.to_string()
                                            } else {
                                                value.to_string()
                                            }
                                        })
                                        .unwrap_or_else(|| "{}".to_string());
                                    yield Ok(StreamEvent::ToolCallDelta {
                                        index,
                                        id: Some(format!("call_{index}")),
                                        name: Some(name),
                                        arguments_delta: arguments,
                                    });
                                    finish = crate::types::FinishReason::ToolCalls;
                                }
                            }
                        }
                        let prompt = value
                            .get("prompt_eval_count")
                            .and_then(Value::as_u64)
                            .unwrap_or(0) as usize;
                        let completion = value.get("eval_count").and_then(Value::as_u64).unwrap_or(0) as usize;
                        if prompt > 0 || completion > 0 {
                            yield Ok(StreamEvent::Usage(Usage {
                                prompt_tokens: prompt,
                                completion_tokens: completion,
                            }));
                        }
                        if done {
                            yield Ok(StreamEvent::Done { finish_reason: finish.clone() });
                        }
                    }
                }
            }
            for event in splitter.flush() {
                yield Ok(event);
            }
        };
        Ok(Box::pin(stream))
    }

    async fn complete(&self, request: ChatRequest) -> ModelResult<(String, Usage)> {
        let url = self.config.chat_url();
        let body = self.build_body(&request, false);
        let builder = self.http.apply(self.http.client.post(&url)).json(&body);
        let builder = match &self.http.api_key {
            Some(key) => builder.bearer_auth(key),
            None => builder,
        };
        let response = builder.send().await.map_err(|e| LinkModelError::Connection {
            url: url.clone(),
            source: Box::new(e),
        })?;
        let value = self
            .http
            .check(&url, response)
            .await?
            .json::<Value>()
            .await
            .map_err(|e| LinkModelError::Malformed(format!("expected JSON from {url}: {e}")))?;
        let text = value
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let usage = Usage {
            prompt_tokens: value.get("prompt_eval_count").and_then(Value::as_u64).unwrap_or(0)
                as usize,
            completion_tokens: value.get("eval_count").and_then(Value::as_u64).unwrap_or(0) as usize,
        };
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

/// Exposed so the registry can construct this adapter uniformly.
pub fn make(config: ModelConfig, api_key: Option<String>) -> Arc<dyn ModelProvider> {
    Arc::new(OllamaProvider::new(config, api_key))
}
