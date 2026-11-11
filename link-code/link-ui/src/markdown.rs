//! A small Markdown renderer for streaming assistant text.
//!
//! Deliberately not a full CommonMark implementation: it recognises exactly the
//! constructs a coding agent's answers use — fenced code blocks, headings, list
//! items, block quotes, links and inline `code` / **bold** / *italic* — and it is
//! safe to run on a *partial* document, because it is called on every streamed
//! token batch. An unterminated code fence renders as a code block that is still
//! arriving, which is exactly what the user should see.

use std::sync::OnceLock;

use regex::Regex;

/// A renderable segment of an assistant message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Prose, already converted to Pango markup.
    Text(String),
    /// A fenced code block.
    Code { language: String, code: String },
}

/// Split text into prose and code segments.
pub fn segments(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut prose = String::new();
    let mut lines = text.lines().peekable();

    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if let Some(info) = trimmed.strip_prefix("```") {
            if !prose.trim().is_empty() {
                out.push(Segment::Text(prose_markup(&prose)));
            }
            prose.clear();
            let language = info.trim().to_string();
            let mut code = String::new();
            for inner in lines.by_ref() {
                if inner.trim_start().starts_with("```") {
                    break;
                }
                code.push_str(inner);
                code.push('\n');
            }
            let code = code.trim_end_matches('\n').to_string();
            out.push(Segment::Code { language, code });
            continue;
        }
        prose.push_str(line);
        prose.push('\n');
    }

    if !prose.trim().is_empty() {
        out.push(Segment::Text(prose_markup(&prose)));
    }
    out
}

/// Convert prose to Pango markup. Text is escaped *first*, so a model cannot
/// inject markup into the UI even by emitting HTML.
pub fn prose_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    let mut in_code_fence = false;
    for raw_line in text.lines() {
        let trimmed = raw_line.trim();
        if trimmed.starts_with("```") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if trimmed.is_empty() {
            out.push('\n');
            continue;
        }
        if let Some(level) = heading_level(trimmed) {
            let body = inline_markup(trimmed.trim_start_matches('#').trim());
            let size = match level {
                1 => "x-large",
                2 => "large",
                _ => "medium",
            };
            out.push_str(&format!(
                "<span weight=\"bold\" size=\"{size}\">{body}</span>\n"
            ));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            out.push_str(&format!("  \u{2022} {}\n", inline_markup(rest)));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("> ") {
            out.push_str(&format!(
                "<span foreground=\"#9a9a9a\">\u{2502} {}</span>\n",
                inline_markup(rest)
            ));
            continue;
        }
        if in_code_fence {
            out.push_str(&format!(
                "<span font_family=\"JetBrains Mono\" size=\"small\">{}</span>\n",
                escape(raw_line)
            ));
            continue;
        }
        out.push_str(&inline_markup(trimmed));
        out.push('\n');
    }
    out
}

/// Escape, then apply inline styling.<br>
/// Order matters: escape first so nothing a model emits can become markup.
pub fn inline_markup(text: &str) -> String {
    let escaped = escape(text);
    let mut out = link_regex()
        .replace_all(&escaped, |caps: &regex::Captures| {
            format!("<a href=\"{}\">{}</a>", &caps[2], &caps[1])
        })
        .into_owned();
    out = code_regex()
        .replace_all(&out, |caps: &regex::Captures| {
            format!(
                "<span font_family=\"JetBrains Mono\" background=\"#1f1f1f\"> {} </span>",
                &caps[1]
            )
        })
        .into_owned();
    out = bold_regex()
        .replace_all(&out, "<b>$1</b>")
        .into_owned();
    out = italic_regex()
        .replace_all(&out, "<i>$1</i>")
        .into_owned();
    strikethrough_regex()
        .replace_all(&out, "<s>$1</s>")
        .into_owned()
}

fn heading_level(line: &str) -> Option<usize> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && line.chars().nth(hashes) == Some(' ') {
        Some(hashes)
    } else {
        None
    }
}

/// Pango-safe escaping.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn link_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[([^\]]+)\]\((https?://[^)\s]+)\)").expect("static regex"))
}

fn code_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"`([^`]+)`").expect("static regex"))
}

fn bold_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\*\*([^*]+)\*\*").expect("static regex"))
}

fn italic_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|\s)\*([^*\n]+)\*(?:\s|$)").expect("static regex"))
}

fn strikethrough_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"~~([^~]+)~~").expect("static regex"))
}

/// Trim a Pango string to a sane length for a collapsed card.
pub fn summarise(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        flat
    } else {
        let mut short: String = flat.chars().take(max_chars).collect();
        short.push('\u{2026}');
        short
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_prose_and_code() {
        let text = "Here is a fix:\n\n```rust\nfn main() {}\n```\n\nThat should do it.";
        let segments = segments(text);
        assert_eq!(segments.len(), 3);
        match &segments[1] {
            Segment::Code { language, code } => {
                assert_eq!(language, "rust");
                assert_eq!(code, "fn main() {}");
            }
            other => panic!("expected a code segment, got {other:?}"),
        }
    }

    #[test]
    fn unterminated_fence_renders_as_code() {
        let segments = segments("partial:\n```python\nprint(1)\n");
        assert!(matches!(segments.last(), Some(Segment::Code { .. })));
    }

    #[test]
    fn escapes_html_from_the_model() {
        let markup = prose_markup("<script>alert(1)</script>");
        assert!(!markup.contains("<script>"));
        assert!(markup.contains("&lt;script&gt;"));
    }

    #[test]
    fn applies_inline_styles() {
        let markup = inline_markup("use `cargo test` and **beware** of *this* [link](https://x.dev)");
        assert!(markup.contains("<b>beware</b>"), "{markup}");
        assert!(markup.contains("<i>this</i>"), "{markup}");
        assert!(markup.contains("JetBrains Mono"), "{markup}");
        assert!(markup.contains("href=\"https://x.dev\""), "{markup}");
    }

    #[test]
    fn renders_headings_and_lists() {
        let markup = prose_markup("# Title\n- one\n- two\n> quoted");
        assert!(markup.contains("weight=\"bold\""));
        assert!(markup.contains("\u{2022} one"));
        assert!(markup.contains("quoted"));
    }

    #[test]
    fn summarise_flattens_whitespace() {
        assert_eq!(summarise("a\n\n  b", 20), "a b");
        assert!(summarise(&"x".repeat(50), 10).ends_with('\u{2026}'));
    }
}
