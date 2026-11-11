//! Server-sent-event plumbing shared by every streaming adapter.

use std::collections::BTreeMap;

use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use serde_json::Value;

use crate::error::{LinkModelError, ModelResult};
use crate::types::{FinishReason, StreamEvent, ToolCallRequest};

/// Turn a `reqwest` response body into a stream of decoded JSON payloads.
///
/// `[DONE]` sentinels and keep-alive comments are dropped. A payload that is
/// not JSON becomes a [`LinkModelError::Malformed`] item rather than a panic, so
/// one bad frame cannot kill a long generation.
pub fn json_events(response: reqwest::Response) -> impl Stream<Item = ModelResult<Value>> {
    response.bytes_stream().eventsource().filter_map(|item| async move {
        match item {
            Ok(event) => {
                let data = event.data;
                let trimmed = data.trim();
                if trimmed.is_empty() || trimmed == "[DONE]" {
                    return None;
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(value) => Some(Ok(value)),
                    Err(error) => Some(Err(LinkModelError::Malformed(format!(
                        "unparseable SSE payload ({error}): {}",
                        truncate(trimmed, 240)
                    )))),
                }
            }
            Err(error) => Some(Err(LinkModelError::Stream(error.to_string()))),
        }
    })
}

pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}\u{2026}")
}

/// A tool call being assembled from streamed fragments.
#[derive(Debug, Default, Clone)]
struct PartialCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

/// Assembles the `tool_calls` fragments every OpenAI-compatible server emits.
///
/// Fragments are grouped by `index`; ids are synthesised when a provider omits
/// them (Ollama and some local servers do).
#[derive(Debug, Default)]
pub struct ToolCallAccumulator {
    calls: BTreeMap<usize, PartialCall>,
}

impl ToolCallAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Feed one fragment.
    pub fn push(&mut self, event: &StreamEvent) {
        if let StreamEvent::ToolCallDelta {
            index,
            id,
            name,
            arguments_delta,
        } = event
        {
            let entry = self.calls.entry(*index).or_default();
            if let Some(id) = id {
                if !id.is_empty() {
                    entry.id = Some(id.clone());
                }
            }
            if let Some(name) = name {
                if !name.is_empty() {
                    entry.name = Some(name.clone());
                }
            }
            entry.arguments.push_str(arguments_delta);
        }
    }

    /// Finish assembling, in index order.
    pub fn finish(self) -> Vec<ToolCallRequest> {
        self.calls
            .into_iter()
            .map(|(index, partial)| ToolCallRequest {
                id: partial.id.unwrap_or_else(|| format!("call_{index}")),
                name: partial.name.unwrap_or_default(),
                arguments: partial.arguments,
            })
            .filter(|call| !call.name.is_empty())
            .collect()
    }
}

/// Splits inline reasoning out of a text stream.
///
/// Many local models (DeepSeek-R1 derivatives, QwQ, Qwen3 thinking builds) emit
/// `<think>...</think>` inside the ordinary content rather than in a dedicated
/// field. This state machine routes those spans to [`StreamEvent::ThinkingDelta`]
/// so the UI can show a thinking block, and copes with the tag being split
/// across several deltas.
#[derive(Debug, Default)]
pub struct ThinkSplitter {
    pending: String,
    in_think: bool,
}

const OPEN_TAGS: [&str; 2] = ["<think>", " thinking"];
const CLOSE_TAGS: [&str; 2] = ["</think>", " response"];

impl ThinkSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_inside(&self) -> bool {
        self.in_think
    }

    /// Feed a delta; returns zero or more normalised events.
    pub fn push(&mut self, delta: &str) -> Vec<StreamEvent> {
        self.pending.push_str(delta);
        let mut out = Vec::new();

        loop {
            let tags: &[&'static str] = if self.in_think { &CLOSE_TAGS } else { &OPEN_TAGS };
            match find_first_tag(&self.pending, tags) {
                Some((position, tag)) => {
                    let before: String = self.pending[..position].to_string();
                    if !before.is_empty() {
                        out.push(self.wrap(before));
                    }
                    self.pending = self.pending[position + tag.len()..].to_string();
                    self.in_think = !self.in_think;
                }
                None => {
                    // Hold back any trailing partial tag.
                    let hold = longest_tag_prefix_suffix(&self.pending, tags);
                    let emit_len = self.pending.len().saturating_sub(hold);
                    if emit_len > 0 {
                        let emit: String = self.pending[..emit_len].to_string();
                        out.push(self.wrap(emit));
                        self.pending = self.pending[emit_len..].to_string();
                    }
                    break;
                }
            }
        }
        out
    }

    /// Flush whatever is left when the stream ends.
    pub fn flush(&mut self) -> Vec<StreamEvent> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let pending = std::mem::take(&mut self.pending);
        vec![self.wrap(pending)]
    }

    fn wrap(&self, text: String) -> StreamEvent {
        if self.in_think {
            StreamEvent::ThinkingDelta(text)
        } else {
            StreamEvent::TextDelta(text)
        }
    }
}

fn find_first_tag(haystack: &str, tags: &[&'static str]) -> Option<(usize, &'static str)> {
    let mut best: Option<(usize, &'static str)> = None;
    for tag in tags {
        if let Some(position) = haystack.find(*tag) {
            if best.map_or(true, |(current, _)| position < current) {
                best = Some((position, *tag));
            }
        }
    }
    best
}

/// Length of the longest suffix of `text` that is a proper prefix of one of the
/// tags. Used to avoid emitting half a tag and then mis-parsing it.
fn longest_tag_prefix_suffix(text: &str, tags: &[&str]) -> usize {
    let max = tags.iter().map(|tag| tag.len()).max().unwrap_or(0);
    let bytes = text.as_bytes();
    let upper = max.saturating_sub(1).min(bytes.len());
    for length in (1..=upper).rev() {
        let start = bytes.len() - length;
        if !text.is_char_boundary(start) {
            continue;
        }
        let suffix = &text[start..];
        if tags.iter().any(|tag| tag.starts_with(suffix)) {
            return length;
        }
    }
    0
}

/// Map a wire finish reason to the internal enum, defaulting sensibly.
pub fn finish_reason(value: Option<&str>) -> FinishReason {
    match value {
        Some(text) => FinishReason::from_wire(text),
        None => FinishReason::Stop,
    }
}
