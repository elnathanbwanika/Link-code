//! The text-protocol tool-call fallback.
//!
//! Plenty of capable local models have no native tool calling. Rather than
//! hiding that, Link Code teaches them a small, explicit protocol and parses it
//! **defensively**: a model may wrap arguments in a fence, use `parameters`
//! instead of `arguments`, emit an array of calls, or truncate mid-object. All
//! of those are recovered here; only genuinely unparseable output is reported
//! back for a corrective retry.

use serde_json::{json, Value};

use crate::types::{repair_json, ToolCallRequest};

/// Outcome of scanning a response for tool calls.
#[derive(Debug, Clone, Default)]
pub struct ParsedToolCalls {
    /// Complete calls, in the order they appeared.
    pub calls: Vec<ToolCallRequest>,
    /// Code fences that looked like tool calls but could not be parsed.
    pub malformed: Vec<String>,
    /// The response text with every recognised tool-call block removed.
    pub remaining_text: String,
}

impl ParsedToolCalls {
    pub fn has_calls(&self) -> bool {
        !self.calls.is_empty()
    }
}

/// The instruction block appended to the system prompt when a model has no
/// native tool calling.
pub fn fallback_protocol_prompt(tool_names: &[&str]) -> String {
    let names = if tool_names.is_empty() {
        "(none available)".to_string()
    } else {
        tool_names.join(", ")
    };
    format!(
        "\n\n## Tool calling\n\nThis model has no native tool calling, so use this protocol.\n\n\
To call a tool, emit a fenced block tagged `tool` containing **one JSON object per line**:\n\n\
```tool\n{{\"tool\": \"read_file\", \"arguments\": {{\"path\": \"src/main.rs\"}}}}\n```\n\n\
Rules:\n\
- Available tools: {names}\n\
- Exactly one JSON object per line; one line per call.\n\
- Wait for the tool results before writing prose that depends on them.\n\
- Never invent tool names or arguments outside the documented schemas.\n\
- If no tool is needed, answer normally with no `tool` block.\n"
    )
}

/// Extract tool calls from a model response.
pub fn parse(text: &str) -> ParsedToolCalls {
    let mut result = ParsedToolCalls::default();
    let mut remaining = String::with_capacity(text.len());

    for segment in split_fences(text) {
        if !segment.is_tool {
            remaining.push_str(&segment.body);
            continue;
        }
        let body = segment.body;
        let mut parsed_any = false;
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match parse_call_value(line) {
                Ok(calls) => {
                    result.calls.extend(calls);
                    parsed_any = true;
                }
                Err(problem) => result.malformed.push(problem),
            }
        }
        if !parsed_any {
            result.malformed.push(format!(
                "a `tool` block contained no parseable JSON object: {}",
                crate::sse::truncate(body.trim(), 120)
            ));
        }
    }

    // Also accept XML-ish blocks for models that prefer them.
    let (without_xml, xml_calls, xml_problems) = parse_xml_blocks(&remaining);
    if !xml_calls.is_empty() || !xml_problems.is_empty() {
        remaining = without_xml;
        result.calls.extend(xml_calls);
        result.malformed.extend(xml_problems);
    }

    // Assign stable ids so results can be matched back.
    for (index, call) in result.calls.iter_mut().enumerate() {
        if call.id.is_empty() {
            call.id = format!("call_{index}");
        }
    }

    result.remaining_text = remaining.trim().to_string();
    result
}

/// True when the response looks like it is trying to call a tool.
pub fn looks_like_tool_call(text: &str) -> bool {
    text.contains("```tool") || text.contains("<tool_use>") || text.contains("<tool_call>")
}

struct Fence {
    is_tool: bool,
    body: String,
}

/// Split on ``` fences, tagging the ones whose info string is `tool`.
fn split_fences(text: &str) -> Vec<Fence> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        let (before, from_fence) = rest.split_at(start);
        if !before.is_empty() {
            out.push(Fence {
                is_tool: false,
                body: before.to_string(),
            });
        }
        let after_ticks = &from_fence[3..];
        let Some(newline) = after_ticks.find('\n') else {
            out.push(Fence {
                is_tool: false,
                body: from_fence.to_string(),
            });
            rest = "";
            break;
        };
        let info = after_ticks[..newline].trim().to_string();
        let body_and_rest = &after_ticks[newline + 1..];
        let is_tool = info == "tool" || info.starts_with("tool");
        match body_and_rest.find("```") {
            Some(end) => {
                let body = &body_and_rest[..end];
                if is_tool {
                    out.push(Fence {
                        is_tool: true,
                        body: body.to_string(),
                    });
                } else {
                    // Keep non-tool fences verbatim, backticks included.
                    out.push(Fence {
                        is_tool: false,
                        body: format!("```{info}\n{body}```"),
                    });
                }
                rest = &body_and_rest[end + 3..];
            }
            None => {
                // An unterminated fence is almost always a truncated stream;
                // parse a `tool` block body anyway and keep everything else.
                out.push(Fence {
                    is_tool,
                    body: if is_tool {
                        body_and_rest.to_string()
                    } else {
                        from_fence.to_string()
                    },
                });
                rest = "";
            }
        }
    }
    if !rest.is_empty() {
        out.push(Fence {
            is_tool: false,
            body: rest.to_string(),
        });
    }
    out
}

/// Parse one JSON object (or array of objects) into tool calls.
fn parse_call_value(line: &str) -> Result<Vec<ToolCallRequest>, String> {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => match repair_json(line) {
            Some(repaired) => serde_json::from_str(&repaired).map_err(|_| {
                format!(
                    "could not parse tool call JSON ({error}): {}",
                    crate::sse::truncate(line, 160)
                )
            })?,
            None => {
                return Err(format!(
                    "could not parse tool call JSON ({error}): {}",
                    crate::sse::truncate(line, 160)
                ))
            }
        },
    };
    match value {
        Value::Array(items) => {
            let mut calls = Vec::new();
            for item in items {
                calls.push(single_call(item)?);
            }
            if calls.is_empty() {
                Err("tool block contained an empty array".to_string())
            } else {
                Ok(calls)
            }
        }
        Value::Object(_) => Ok(vec![single_call(value)?]),
        _ => Err(format!(
            "tool block must contain a JSON object, found: {}",
            crate::sse::truncate(line, 80)
        )),
    }
}

fn single_call(value: Value) -> Result<ToolCallRequest, String> {
    // Accept the several shapes models actually produce.
    let name = value
        .get("tool")
        .or_else(|| value.get("name"))
        .or_else(|| value.get("tool_name"))
        .or_else(|| value.get("function"))
        .and_then(|v| match v {
            Value::Object(object) => object.get("name"),
            other => Some(other),
        })
        .and_then(Value::as_str)
        .map(str::to_string);

    let Some(name) = name.filter(|name| !name.trim().is_empty()) else {
        return Err(format!(
            "tool call is missing a name: {}",
            crate::sse::truncate(&value.to_string(), 160)
        ));
    };

    let arguments = value
        .get("arguments")
        .or_else(|| value.get("parameters"))
        .or_else(|| value.get("args"))
        .or_else(|| value.get("input"))
        .or_else(|| value.get("function").and_then(|f| f.get("arguments")))
        .cloned()
        .unwrap_or_else(|| json!({}));

    let arguments = match arguments {
        Value::String(text) => match serde_json::from_str::<Value>(&text) {
            Ok(parsed) => parsed,
            Err(_) => repair_json(&text)
                .and_then(|repaired| serde_json::from_str::<Value>(&repaired).ok())
                .unwrap_or(Value::String(text)),
        },
        other => other,
    };

    let id = value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    Ok(ToolCallRequest::new(id, name, arguments.to_string()))
}

/// Parse `<tool_call>` / `<tool_use>` XML blocks, returning the text with those
/// blocks removed plus any complete calls and any problems.
fn parse_xml_blocks(text: &str) -> (String, Vec<ToolCallRequest>, Vec<String>) {
    let mut calls = Vec::new();
    let mut problems = Vec::new();
    let mut remaining = String::with_capacity(text.len());
    let mut cursor = 0usize;

    while let Some(open_start) = text[cursor..].find('<') {
        let open_start = cursor + open_start;
        let rest = &text[open_start..];
        let Some(open_end) = rest.find('>') else {
            break;
        };
        let open_tag = &rest[..=open_end];
        let Some(tag_name) = xml_tag_name(open_tag) else {
            remaining.push_str(&text[cursor..open_start + open_end + 1]);
            cursor = open_start + open_end + 1;
            continue;
        };
        if tag_name != "tool_call" && tag_name != "tool_use" && tag_name != "function_call" {
            remaining.push_str(&text[cursor..open_start + open_end + 1]);
            cursor = open_start + open_end + 1;
            continue;
        }
        let close_tag = format!("</{tag_name}>");
        let body_start = open_start + open_end + 1;
        let Some(close_offset) = text[body_start..].find(&close_tag) else {
            // Unterminated: keep the prose before it and report the problem.
            remaining.push_str(&text[cursor..open_start]);
            problems.push(format!(
                "unterminated <{tag_name}> block: {}",
                crate::sse::truncate(text[open_start..].trim(), 120)
            ));
            cursor = text.len();
            break;
        };
        remaining.push_str(&text[cursor..open_start]);
        let body = &text[body_start..body_start + close_offset];
        match parse_xml_body(open_tag, body) {
            Ok(call) => calls.push(call),
            Err(problem) => problems.push(problem),
        }
        cursor = body_start + close_offset + close_tag.len();
    }
    if cursor < text.len() {
        remaining.push_str(&text[cursor..]);
    }
    (remaining, calls, problems)
}

fn xml_tag_name(tag: &str) -> Option<String> {
    let inner = tag.trim_start_matches('<').trim_start_matches('/');
    let name: String = inner
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Pull the name and arguments out of an XML tool block.
fn parse_xml_body(open_tag: &str, body: &str) -> Result<ToolCallRequest, String> {
    let attribute = |name: &str| -> Option<String> {
        let needle = format!("{name}=\"");
        let start = open_tag.find(&needle)? + needle.len();
        let end = open_tag[start..].find('"')? + start;
        Some(open_tag[start..end].to_string())
    };

    let element = |name: &str| -> Option<String> {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let start = body.find(&open)? + open.len();
        let end = body[start..].find(&close)? + start;
        Some(body[start..end].trim().to_string())
    };

    let name = attribute("name")
        .or_else(|| element("name"))
        .or_else(|| element("tool"))
        .unwrap_or_default();
    if name.trim().is_empty() {
        return Err(format!(
            "tool block is missing a name: {}",
            crate::sse::truncate(body.trim(), 120)
        ));
    }

    let raw_arguments = element("arguments")
        .or_else(|| element("parameters"))
        .or_else(|| element("args"))
        .or_else(|| attribute("arguments"))
        .unwrap_or_else(|| "{}".to_string());

    let arguments = serde_json::from_str::<Value>(&raw_arguments)
        .or_else(|_| {
            repair_json(&raw_arguments)
                .ok_or(())
                .and_then(|repaired| serde_json::from_str::<Value>(&repaired).map_err(|_| ()))
        })
        .map_err(|_| {
            format!(
                "tool `{name}` has unparseable arguments: {}",
                crate::sse::truncate(&raw_arguments, 120)
            )
        })?;

    Ok(ToolCallRequest::new("", name, arguments.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_single_fenced_call() {
        let text = "Let me look.\n\n```tool\n{\"tool\": \"read_file\", \"arguments\": {\"path\": \"src/main.rs\"}}\n```\n";
        let parsed = parse(text);
        assert_eq!(parsed.calls.len(), 1);
        assert_eq!(parsed.calls[0].name, "read_file");
        assert_eq!(parsed.calls[0].id, "call_0");
        assert_eq!(
            parsed.calls[0].parsed_arguments().unwrap()["path"],
            serde_json::json!("src/main.rs")
        );
        assert!(parsed.remaining_text.contains("Let me look."));
        assert!(!parsed.remaining_text.contains("tool"));
        assert!(parsed.malformed.is_empty());
    }

    #[test]
    fn parses_several_calls_one_per_line() {
        let text = "```tool\n{\"tool\": \"list_dir\", \"arguments\": {\"path\": \".\"}}\n{\"name\": \"grep\", \"parameters\": {\"pattern\": \"TODO\"}}\n```";
        let parsed = parse(text);
        assert_eq!(parsed.calls.len(), 2);
        assert_eq!(parsed.calls[1].name, "grep");
        assert_eq!(parsed.calls[1].id, "call_1");
    }

    #[test]
    fn parses_an_array_of_calls() {
        let text = "```tool\n[{\"tool\": \"a\", \"arguments\": {}}, {\"tool\": \"b\", \"arguments\": {}}]\n```";
        let parsed = parse(text);
        assert_eq!(parsed.calls.len(), 2);
        assert_eq!(parsed.calls[0].name, "a");
    }

    #[test]
    fn repairs_trailing_commas() {
        let text = "```tool\n{\"tool\": \"read_file\", \"arguments\": {\"path\": \"a.rs\",},}\n```";
        let parsed = parse(text);
        assert_eq!(parsed.calls.len(), 1);
        assert_eq!(parsed.calls[0].name, "read_file");
    }

    #[test]
    fn reports_malformed_blocks_instead_of_panicking() {
        let text = "```tool\nthis is not json at all\n```";
        let parsed = parse(text);
        assert!(parsed.calls.is_empty());
        assert_eq!(parsed.malformed.len(), 1);
        assert!(parsed.malformed[0].contains("could not parse") || parsed.malformed[0].contains("no parseable"));
    }

    #[test]
    fn reports_calls_without_a_name() {
        let text = "```tool\n{\"arguments\": {\"path\": \"a\"}}\n```";
        let parsed = parse(text);
        assert!(parsed.calls.is_empty());
        assert_eq!(parsed.malformed.len(), 1);
    }

    #[test]
    fn keeps_ordinary_code_fences_visible() {
        let text = "Here:\n\n```rust\nfn main() {}\n```\n";
        let parsed = parse(text);
        assert!(parsed.calls.is_empty());
        assert!(parsed.remaining_text.contains("```rust"));
        assert!(parsed.remaining_text.contains("fn main()"));
    }

    #[test]
    fn handles_a_truncated_fence() {
        let text = "```tool\n{\"tool\": \"read_file\", \"arguments\": {\"path\": \"a.rs\"}}";
        let parsed = parse(text);
        assert_eq!(parsed.calls.len(), 1);
    }

    #[test]
    fn parses_xml_blocks() {
        let text = "<tool_use><name>read_file</name><arguments>{\"path\": \"src/lib.rs\"}</arguments></tool_use>";
        let parsed = parse(text);
        assert_eq!(parsed.calls.len(), 1);
        assert_eq!(parsed.calls[0].name, "read_file");
        assert!(parsed.remaining_text.is_empty());
    }

    #[test]
    fn parses_xml_attribute_form() {
        let text = "<tool_call name=\"grep\"><arguments>{\"pattern\": \"TODO\"}</arguments></tool_call>";
        let parsed = parse(text);
        assert_eq!(parsed.calls.len(), 1);
        assert_eq!(parsed.calls[0].name, "grep");
    }

    #[test]
    fn unterminated_xml_is_reported() {
        let text = "thinking... <tool_use><name>read_file</name>";
        let parsed = parse(text);
        assert!(parsed.calls.is_empty());
        assert!(!parsed.malformed.is_empty());
        assert!(parsed.remaining_text.contains("thinking..."));
    }

    #[test]
    fn detects_tool_markup() {
        assert!(looks_like_tool_call("```tool\n{}\n```"));
        assert!(looks_like_tool_call("<tool_use>x</tool_use>"));
        assert!(!looks_like_tool_call("just prose"));
    }

    #[test]
    fn protocol_prompt_lists_tools() {
        let prompt = fallback_protocol_prompt(&["read_file", "bash"]);
        assert!(prompt.contains("read_file, bash"));
        assert!(prompt.contains("```tool"));
    }
}
