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

    /// Name of the call being assembled at `index`, once the provider sent it.
    pub fn name_at(&self, index: usize) -> Option<&str> {
        self.calls.get(&index).and_then(|call| call.name.as_deref())
    }

    /// Provider id for the call at `index`, if it supplied one.
    pub fn id_at(&self, index: usize) -> Option<&str> {
        self.calls.get(&index).and_then(|call| call.id.as_deref())
    }

    /// Raw (still partial) JSON arguments accumulated for `index`.
    pub fn arguments_at(&self, index: usize) -> Option<&str> {
        self.calls.get(&index).map(|call| call.arguments.as_str())
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

/// Extract the current value of a top-level string field from a *partial* JSON
/// object.
///
/// Tool arguments are streamed as fragments, so the accumulated buffer is
/// usually not valid JSON yet. This walks the raw text, follows the string
/// escapes and returns as much of the requested field as can be decoded. It
/// never fails: an incomplete escape at the end is simply left out until the
/// next fragment completes it.
pub fn partial_json_string(raw: &str, field: &str) -> Option<String> {
    json_string_field(raw, field).map(|(value, _)| value)
}

/// Like [`partial_json_string`], but also says whether the closing quote has
/// arrived (the field is complete rather than still streaming).
pub fn json_string_field(raw: &str, field: &str) -> Option<(String, bool)> {
    let bytes = raw.as_bytes();
    let mut index = 0usize;
    let mut depth = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let start = index + 1;
                let (end, complete) = scan_string_end(raw, start);
                if depth == 1 && complete {
                    let key = &raw[start..end.saturating_sub(1)];
                    let mut cursor = end;
                    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                        cursor += 1;
                    }
                    if cursor < bytes.len() && bytes[cursor] == b':' && key == field {
                        cursor += 1;
                        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                            cursor += 1;
                        }
                        if cursor < bytes.len() && bytes[cursor] == b'"' {
                            let (_, closed) = scan_string_end(raw, cursor + 1);
                            return Some((                                decode_json_string_prefix(&raw[cursor + 1..]),
                                closed,
                            ));
                        }
                        return None;
                    }
                }
                index = end.max(index + 1);
            }
            b'{' | b'[' => {
                depth += 1;
                index += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                index += 1;
            }
            _ => index += 1,
        }
    }
    None
}

/// Return the index just past the closing quote of the string starting at
/// `start`, and whether a closing quote was found.
fn scan_string_end(raw: &str, start: usize) -> (usize, bool) {
    let bytes = raw.as_bytes();
    let mut index = start;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return (index + 1, true),
            _ => index += 1,
        }
    }
    (bytes.len(), false)
}

/// Decode a JSON string body, stopping at the closing quote or at an escape
/// sequence that is still incomplete.
fn decode_json_string_prefix(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    let mut high_surrogate: Option<u32> = None;
    while let Some(ch) = chars.next() {
        if ch == '"' {
            break;
        }
        if ch != '\\' {
            high_surrogate = None;
            out.push(ch);
            continue;
        }
        let Some(escape) = chars.next() else { break };
        match escape {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'u' => {
                let mut code = 0u32;
                let mut complete = true;
                for _ in 0..4 {
                    match chars.next().and_then(|c| c.to_digit(16)) {
                        Some(digit) => code = code * 16 + digit,
                        None => {
                            complete = false;
                            break;
                        }
                    }
                }
                if !complete {
                    break;
                }
                if (0xD800..=0xDBFF).contains(&code) {
                    high_surrogate = Some(code);
                } else if (0xDC00..=0xDFFF).contains(&code) {
                    if let Some(high) = high_surrogate.take() {
                        let scalar = 0x10000 + ((high - 0xD800) << 10) + (code - 0xDC00);
                        if let Some(decoded) = char::from_u32(scalar) {
                            out.push(decoded);
                        }
                    }
                } else {
                    high_surrogate = None;
                    if let Some(decoded) = char::from_u32(code) {
                        out.push(decoded);
                    }
                }
            }
            other => {
                high_surrogate = None;
                out.push(other);
            }
        }
    }
    out
}

/// One increment of a streamed JSON string field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldChunk {
    /// The key this text belongs to (`path`, `content`, `old_string`, …).
    pub field: String,
    /// Newly decoded characters, ready to append.
    pub text: String,
    /// True when the closing quote for the value has arrived.
    pub complete: bool,
}

/// Incrementally decodes top-level string fields out of streamed JSON
/// fragments.
///
/// A tool call's `arguments` arrive as many small JSON fragments. Re-parsing the
/// whole accumulated buffer for every fragment is O(n²) and visibly stalls long
/// file writes, so this keeps a tiny state machine instead: it tracks nesting,
/// string/key state and the escape sequence currently being decoded, and only
/// emits the newly appended characters.
#[derive(Debug, Default)]
pub struct JsonFieldStream {
    depth: usize,
    in_string: bool,
    is_key: bool,
    key: String,
    key_buf: String,
    expecting_value: bool,
    value_field: Option<String>,
    escape: bool,
    unicode: Option<(u32, u8)>,
    high_surrogate: Option<u32>,
}

impl JsonFieldStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one raw arguments fragment. Returns whatever fields grew.
    pub fn push(&mut self, fragment: &str) -> Vec<FieldChunk> {
        let mut chunks = Vec::new();
        let mut pending = String::new();
        let mut pending_field: Option<String> = None;

        for ch in fragment.chars() {
            if let Some((code, digits)) = self.unicode {
                match ch.to_digit(16) {
                    Some(digit) => {
                        let code = code * 16 + digit;
                        if digits + 1 == 4 {
                            self.unicode = None;
                            self.emit_scalar(code, &mut pending_field, &mut pending, &mut chunks);
                        } else {
                            self.unicode = Some((code, digits + 1));
                        }
                    }
                    None => self.unicode = None,
                }
                continue;
            }

            if self.escape {
                self.escape = false;
                match ch {
                    '"' => self.value_char('"', &mut pending_field, &mut pending, &mut chunks),
                    '\\' => self.value_char('\\', &mut pending_field, &mut pending, &mut chunks),
                    '/' => self.value_char('/', &mut pending_field, &mut pending, &mut chunks),
                    'b' => self.value_char('\u{8}', &mut pending_field, &mut pending, &mut chunks),
                    'f' => self.value_char('\u{c}', &mut pending_field, &mut pending, &mut chunks),
                    'n' => self.value_char('\n', &mut pending_field, &mut pending, &mut chunks),
                    'r' => self.value_char('\r', &mut pending_field, &mut pending, &mut chunks),
                    't' => self.value_char('\t', &mut pending_field, &mut pending, &mut chunks),
                    'u' => self.unicode = Some((0, 0)),
                    other => self.value_char(other, &mut pending_field, &mut pending, &mut chunks),
                }
                continue;
            }

            if !self.in_string {
                match ch {
                    '"' => {
                        self.in_string = true;
                        self.key_buf.clear();
                        self.is_key = self.depth == 1 && !self.expecting_value;
                        self.value_field = if self.depth == 1 && self.expecting_value {
                            Some(self.key.clone())
                        } else {
                            None
                        };
                    }
                    '{' | '[' => {
                        self.depth += 1;
                        self.expecting_value = false;
                        self.value_field = None;
                    }
                    '}' | ']' => {
                        self.depth = self.depth.saturating_sub(1);
                        self.expecting_value = false;
                        self.value_field = None;
                    }
                    ':' => self.expecting_value = true,
                    ',' => {
                        self.expecting_value = false;
                        self.value_field = None;
                    }
                    _ => {}
                }
                continue;
            }

            match ch {
                '\\' => self.escape = true,
                '"' => {
                    self.in_string = false;
                    if self.is_key {
                        if self.depth == 1 {
                            self.key = std::mem::take(&mut self.key_buf);
                        }
                    } else if let Some(field) = self.value_field.take() {
                        if matches!(
                            field.as_str(),
                            "path" | "content" | "old_string" | "new_string"
                        ) {
                            if pending_field.as_deref() == Some(field.as_str()) {
                                pending_field = None;
                            } else if pending_field.is_some() {
                                flush_chunk(&mut pending_field, &mut pending, &mut chunks, false);
                            }
                            chunks.push(FieldChunk {
                                field,
                                text: std::mem::take(&mut pending),
                                complete: true,
                            });
                        }
                    }
                    self.is_key = false;
                }
                other => {
                    if self.is_key {
                        self.key_buf.push(other);
                    } else {
                        self.value_char(other, &mut pending_field, &mut pending, &mut chunks);
                    }
                }
            }
        }

        if pending_field.is_some() {
            flush_chunk(&mut pending_field, &mut pending, &mut chunks, false);
        }
        chunks
    }

    fn value_char(
        &self,
        ch: char,
        pending_field: &mut Option<String>,
        pending: &mut String,
        chunks: &mut Vec<FieldChunk>,
    ) {
        let Some(field) = self.value_field.clone() else {
            return;
        };
        if !matches!(
            field.as_str(),
            "path" | "content" | "old_string" | "new_string"
        ) {
            return;
        }
        if pending_field.as_deref() != Some(field.as_str()) {
            flush_chunk(pending_field, pending, chunks, false);
            *pending_field = Some(field);
        }
        pending.push(ch);
    }

    /// Handle a completed `\uXXXX` escape, pairing surrogates when needed.
    fn emit_scalar(
        &mut self,
        code: u32,
        pending_field: &mut Option<String>,
        pending: &mut String,
        chunks: &mut Vec<FieldChunk>,
    ) {
        if (0xD800..=0xDBFF).contains(&code) {
            self.high_surrogate = Some(code);
            return;
        }
        if (0xDC00..=0xDFFF).contains(&code) {
            if let Some(high) = self.high_surrogate.take() {
                let scalar = 0x10000 + ((high - 0xD800) << 10) + (code - 0xDC00);
                if let Some(decoded) = char::from_u32(scalar) {
                    self.value_char(decoded, pending_field, pending, chunks);
                }
            }
            return;
        }
        self.high_surrogate = None;
        if let Some(decoded) = char::from_u32(code) {
            self.value_char(decoded, pending_field, pending, chunks);
        }
    }
}

fn flush_chunk(
    pending_field: &mut Option<String>,
    pending: &mut String,
    chunks: &mut Vec<FieldChunk>,
    complete: bool,
) {
    if let Some(field) = pending_field.take() {
        chunks.push(FieldChunk {
            field,
            text: std::mem::take(pending),
            complete,
        });
    } else {
        pending.clear();
    }
}

/// Map a wire finish reason to the internal enum, defaulting sensibly.
pub fn finish_reason(value: Option<&str>) -> FinishReason {
    match value {
        Some(text) => FinishReason::from_wire(text),
        None => FinishReason::Stop,
    }
}

#[cfg(test)]
mod tests {
    use super::partial_json_string;

    #[test]
    fn extracts_a_field_from_a_partial_object() {
        let raw = "{\"path\":\"index.html\",\"content\":\"<footer>Hel";
        assert_eq!(
            partial_json_string(raw, "path").as_deref(),
            Some("index.html")
        );
        assert_eq!(
            partial_json_string(raw, "content").as_deref(),
            Some("<footer>Hel")
        );
    }

    #[test]
    fn decodes_escapes_even_when_the_stream_is_mid_escape() {
        let raw = "{\"content\":\"line one\\nline \\\"two\\\"\\u00";
        assert_eq!(
            partial_json_string(raw, "content").as_deref(),
            Some("line one\nline \"two\"")
        );
        let complete = "{\"content\":\"line one\\nline \\\"two\\\"\\u00e9\"}";
        assert_eq!(
            partial_json_string(complete, "content").as_deref(),
            Some("line one\nline \"two\"\u{00e9}")
        );
    }

    #[test]
    fn ignores_lookalike_keys_inside_values() {
        let raw = "{\"content\":\"he said \\\"path\\\": nope\",\"path\":\"a.rs\"}";
        assert_eq!(partial_json_string(raw, "path").as_deref(), Some("a.rs"));
    }

    #[test]
    fn returns_none_until_the_field_appears() {
        assert_eq!(partial_json_string("{\"pa", "path"), None);
    }
}

#[cfg(test)]
mod field_stream_tests {
    use super::{FieldChunk, JsonFieldStream};

    fn feed(stream: &mut JsonFieldStream, fragments: &[&str]) -> Vec<FieldChunk> {
        fragments
            .iter()
            .flat_map(|fragment| stream.push(fragment))
            .collect()
    }

    #[test]
    fn reassembles_fields_split_across_fragments() {
        let mut stream = JsonFieldStream::new();
        let chunks = feed(
            &mut stream,
            &[
                "{\"pa",
                "th\":\"ind",
                "ex.html\",\"content\":\"<footer>",
                "Hi\\n</footer>",
                "\"}",
            ],
        );
        let path: String = chunks
            .iter()
            .filter(|chunk| chunk.field == "path")
            .map(|chunk| chunk.text.as_str())
            .collect();
        let content: String = chunks
            .iter()
            .filter(|chunk| chunk.field == "content")
            .map(|chunk| chunk.text.as_str())
            .collect();
        assert_eq!(path, "index.html");
        assert_eq!(content, "<footer>Hi\n</footer>");
        assert!(chunks
            .iter()
            .any(|chunk| chunk.field == "path" && chunk.complete));
    }

    #[test]
    fn decodes_an_escape_split_between_fragments() {
        let mut stream = JsonFieldStream::new();
        let chunks = feed(
            &mut stream,
            &[
                "{\"content\":\"a\\",
                "nb\\u00",
                "e9\\\"c\"}",
            ],
        );
        let text: String = chunks
            .iter()
            .filter(|chunk| chunk.field == "content")
            .map(|chunk| chunk.text.as_str())
            .collect();
        assert_eq!(text, "a\nb\u{00e9}\"c");
    }

    #[test]
    fn ignores_nested_and_non_target_fields() {
        let mut stream = JsonFieldStream::new();
        let chunks = feed(
            &mut stream,
            &["{\"path\":\"a.rs\",\"edits\":[{\"path\":\"inner\"}],\"content\":\"x\"}"],
        );
        let paths: Vec<_> = chunks.iter().filter(|c| c.field == "path").collect();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].text, "a.rs");
        let content: String = chunks
            .iter()
            .filter(|chunk| chunk.field == "content")
            .map(|chunk| chunk.text.as_str())
            .collect();
        assert_eq!(content, "x");
    }
}

#[cfg(test)]
mod field_completion_tests {
    use super::JsonFieldStream;

    #[test]
    fn marks_a_field_complete_even_when_the_closing_quote_is_its_own_fragment() {
        let mut stream = JsonFieldStream::new();
        let mut complete = false;
        let mut path = String::new();
        for fragment in ["{\"path\":\"src/main.rs", "\"", ",\"content\":\"x\"}"] {
            for chunk in stream.push(fragment) {
                if chunk.field == "path" {
                    path.push_str(&chunk.text);
                    complete |= chunk.complete;
                }
            }
        }
        assert_eq!(path, "src/main.rs");
        assert!(complete);
    }

    #[test]
    fn emits_the_full_value_in_deltas_across_fragments() {
        let mut stream = JsonFieldStream::new();
        let mut content = String::new();
        for fragment in ["{\"path\":\"a\",\"content\":\"he", "llo", " world\"}"] {
            for chunk in stream.push(fragment) {
                if chunk.field == "content" {
                    content.push_str(&chunk.text);
                }
            }
        }
        assert_eq!(content, "hello world");
    }
}
