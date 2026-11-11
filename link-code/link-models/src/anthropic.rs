//! Anthropic adapter (`POST /v1/messages`).
//!
//! Anthropic differs from OpenAI in three ways that matter here:
//! * the system prompt is a top-level field, not a message;
//! * content is a list of typed blocks;
//! * tool results must be returned inside a **user** message, and consecutive
//!   results must be merged into a single message or the API rejects the turn.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};

use crate::error::{LinkModelError, ModelResult};
use crate::openai::HttpClient;
use crate::provider::{ConnectionReport, ModelProvider, ModelStream};
use crate::registry::{ModelConfig, ProviderKind};
use crate::sse::{finish_reason, json_events, truncate};
use crate::types::{Capabilities, ChatRequest, ModelInfo, Role, StreamEvent, Usage};

const API_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    config: ModelConfig,
    http: HttpClient,
}

impl AnthropicProvider {
    pub fn new(config: ModelConfig, api_key: Option<String>) -> Self {
        let http = HttpClient::new(&config.extra_headers, api_key);
        Self { config, http }
    }

    fn headers(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let builder = self
            .http
            .apply(builder)
            .header("anthropic-version", API_VERSION);
        match &self.http.api_key {
            Some(key) => builder.header("x-api-key", key),
            None => builder,
        }
    }

    fn build_body(&self, request: &ChatRequest) -> Value {
        let mut messages: Vec<Value> = Vec::new();
        for message in &request.messages {
            match message.role {
                Role::System => continue,
                Role::Tool => {
                    let block = json!({
                        "type": "tool_result",
                        "tool_use_id": message.tool_call_id.clone().unwrap_or_default(),
                        "content": message.content,
                    });
                    // Merge consecutive tool results into one user message.
                    if let Some(last) = messages.last_mut() {
                        if last.get("role").and_then(Value::as_str) == Some("user") {
                            if let Some(content) = last.get_mut("content").and_then(Value::as_array_mut) {
                                content.push(block);
                                continue;
                            }
                        }
                    }
                    messages.push(json!({ "role": "user", "content": [block] }));
                }
                Role::User => {
                    let mut content = Vec::new();
                    if !message.content.is_empty() {
                        content.push(json!({ "type": "text", "text": message.content }));
                    }
                    for image in &message.images {
                        content.push(json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": image.mime_type,
                                "data": image.data_base64,
                            }
                        }));
                    }
                    if content.is_empty() {
                        content.push(json!({ "type": "text", "text": "" }));
                    }
                    messages.push(json!({ "role": "user", "content": content }));
                }
                Role::Assistant => {
                    let mut content = Vec::new();
                    if !message.content.trim().is_empty() {
                        content.push(json!({ "type": "text", "text": message.content }));
                    }
                    for call in &message.tool_calls {
                        content.push(json!({
                            "type": "tool_use",
                            "id": call.id,
                            "name": call.name,
                            "input": call.parsed_arguments().unwrap_or_else(|| json!({})),
                        }));
                    }
                    if content.is_empty() {
                        content.push(json!({ "type": "text", "text": "" }));
                    }
                    messages.push(json!({ "role": "assistant", "content": content }));
                }
            }
        }

        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(request.model.clone()));
        body.insert(
            "max_tokens".to_string(),
            json!(request.max_tokens.unwrap_or(self.config.max_output_tokens)),
        );
        body.insert("messages".to_string(), Value::Array(messages));
        if let Some(system) = request.system.as_deref() {
            if !system.trim().is_empty() {
                body.insert("system".to_string(), Value::String(system.to_string()));
            }
        }
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .filter_map(|tool| {
                    let function = tool.get("function")?;
                    Some(json!({
                        "name": function.get("name")?,
                        "description": function.get("description").cloned().unwrap_or(json!("")),
                        "input_schema": function.get("parameters").cloned().unwrap_or(json!({
                            "type": "object", "properties": {}
                        })),
                    }))
                })
                .collect();
            body.insert("tools".to_string(), Value::Array(tools));
        }
        if let Some(temperature) = request.temperature {
            body.insert("temperature".to_string(), json!(temperature));
        }
        if let Some(top_p) = request.top_p {
            body.insert("top_p".to_string(), json!(top_p));
        }
        if !request.stop.is_empty() {
            body.insert("stop_sequences".to_string(), json!(request.stop));
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

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn label(&self) -> &str {
        &self.config.display_name
    }

    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
    }

    fn capabilities(&self) -> Capabilities {
        self.config.capabilities
    }

    fn context_window(&self) -> usize {
        self.config.context_window
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        let url = format!("{}/v1/models", self.config.base_url.trim_end_matches('/'));
        let value = self.http.get_json(&url).await?;
        let mut models = Vec::new();
        for entry in value.get("data").and_then(Value::as_array).cloned().unwrap_or_default() {
            if let Some(id) = entry.get("id").and_then(Value::as_str) {
                models.push(ModelInfo::new(id));
            }
        }
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    async fn stream_chat(&self, request: ChatRequest) -> ModelResult<ModelStream> {
        let url = self.config.chat_url();
        let mut body = self.build_body(&request);
        if let Some(object) = body.as_object_mut() {
            object.insert("stream".to_string(), Value::Bool(true));
        }
        let builder = self.headers(self.http.client.post(&url)).json(&body);
        let response = builder.send().await.map_err(|e| LinkModelError::Connection {
            url: url.clone(),
            source: Box::new(e),
        })?;
        let response = self.http.check(&url, response).await?;

        let events = json_events(response);
        let mut usage = Usage::default();
        let stream = async_stream::stream! {
            futures_util::pin_mut!(events);
            // Maps Anthropic's content-block index to the tool call index the
            // agent loop sees, so interleaved text and tool use line up.
            let mut tool_indices: std::collections::BTreeMap<u64, usize> = std::collections::BTreeMap::new();
            let mut next_tool = 0usize;
            let mut current_tool: Option<usize> = None;

            while let Some(item) = events.next().await {
                let value = match item {
                    Ok(value) => value,
                    Err(error) => {
                        yield Err(error);
                        continue;
                    }
                };
                match value.get("type").and_then(Value::as_str).unwrap_or("") {
                    "message_start" => {
                        if let Some(prompt) = value
                            .get("message")
                            .and_then(|m| m.get("usage"))
                            .and_then(|u| u.get("input_tokens"))
                            .and_then(Value::as_u64)
                        {
                            usage.prompt_tokens = prompt as usize;
                        }
                    }
                    "content_block_start" => {
                        let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                        let block = value.get("content_block");
                        let kind = block.and_then(|b| b.get("type")).and_then(Value::as_str).unwrap_or("");
                        if kind == "tool_use" {
                            let slot = next_tool;
                            next_tool += 1;
                            tool_indices.insert(index, slot);
                            current_tool = Some(slot);
                            let id = block
                                .and_then(|b| b.get("id"))
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            let name = block
                                .and_then(|b| b.get("name"))
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            yield Ok(StreamEvent::ToolCallDelta {
                                index: slot,
                                id,
                                name,
                                arguments_delta: String::new(),
                            });
                        }
                    }
                    "content_block_delta" => {
                        let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                        let Some(delta) = value.get("delta") else { continue };
                        match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                            "text_delta" => {
                                if let Some(text) = delta.get("text").and_then(Value::as_str) {
                                    yield Ok(StreamEvent::TextDelta(text.to_string()));
                                }
                            }
                            "thinking_delta" => {
                                if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                                    yield Ok(StreamEvent::ThinkingDelta(text.to_string()));
                                }
                            }
                            "input_json_delta" => {
                                let slot = tool_indices.get(&index).copied().or(current_tool).unwrap_or(0);
                                let partial = delta
                                    .get("partial_json")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                yield Ok(StreamEvent::ToolCallDelta {
                                    index: slot,
                                    id: None,
                                    name: None,
                                    arguments_delta: partial,
                                });
                            }
                            _ => {}
                        }
                    }
                    "content_block_stop" => {
                        current_tool = None;
                    }
                    "message_delta" => {
                        if let Some(output) = value
                            .get("usage")
                            .and_then(|u| u.get("output_tokens"))
                            .and_then(Value::as_u64)
                        {
                            usage.completion_tokens = output as usize;
                        }
                        yield Ok(StreamEvent::Usage(usage));
                        if let Some(reason) = value
                            .get("delta")
                            .and_then(|d| d.get("stop_reason"))
                            .and_then(Value::as_str)
                        {
                            yield Ok(StreamEvent::Done {
                                finish_reason: finish_reason(Some(reason)),
                            });
                        }
                    }
                    "message_stop" => {
                        yield Ok(StreamEvent::Done {
                            finish_reason: crate::types::FinishReason::Stop,
                        });
                    }
                    "error" => {
                        let message = value
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or("unknown Anthropic error")
                            .to_string();
                        yield Err(LinkModelError::Other(message));
                    }
                    _ => {}
                }
            }
        };
        Ok(Box::pin(stream))
    }

    async fn complete(&self, request: ChatRequest) -> ModelResult<(String, Usage)> {
        let url = self.config.chat_url();
        let body = self.build_body(&request);
        let builder = self.headers(self.http.client.post(&url)).json(&body);
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
        let mut text = String::new();
        for block in value.get("content").and_then(Value::as_array).cloned().unwrap_or_default() {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(piece) = block.get("text").and_then(Value::as_str) {
                    text.push_str(piece);
                }
            }
        }
        let usage = value
            .get("usage")
            .map(|u| Usage {
                prompt_tokens: u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as usize,
                completion_tokens: u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as usize,
            })
            .unwrap_or_default();
        Ok((text, usage))
    }

    async fn test_connection(&self) -> ModelResult<ConnectionReport> {
        let started = std::time::Instant::now();
        let mut request = ChatRequest::new(&self.config.model_id);
        request.messages = vec![crate::types::ChatMessage::user("Reply with the single word: pong")];
        request.max_tokens = Some(16);
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

pub fn make(config: ModelConfig, api_key: Option<String>) -> Arc<dyn ModelProvider> {
    Arc::new(AnthropicProvider::new(config, api_key))
}
