//! One-line chips for tool calls in the conversation.
//!
//! A walked-through turn is mostly narration, so tools take as little room as
//! possible: a single pill such as `Read index.html ✓` or `Ran command ✓`.
//! Nothing a tool returns is pasted into the chat. `bash` is the one exception:
//! its header expands on click to reveal the command and the streamed output.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use link_core::event::ToolStatus;

use crate::markdown;
use crate::state::UiState;

pub struct ToolChip {
    pub root: gtk::Box,
    header: gtk::Button,
    spinner: gtk::Spinner,
    glyph: gtk::Label,
    label: gtk::Label,
    /// Expandable body, used by `bash`.
    body: gtk::Box,
    output: gtk::Label,
    notes: gtk::Box,
    buffer: Rc<RefCell<String>>,
    flush_pending: Rc<Cell<bool>>,
    tool: String,
    path: RefCell<String>,
    status: Cell<ToolStatus>,
    expanded: Cell<bool>,
}

impl ToolChip {
    pub fn new(
        state: &Rc<UiState>,
        call_id: &str,
        tool: &str,
        summary: &str,
        args: &serde_json::Value,
    ) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 3);
        root.add_css_class("lc-tool-chip-root");
        root.set_halign(gtk::Align::Start);
        root.set_margin_start(2);

        let header = gtk::Button::new();
        header.add_css_class("lc-tool-chip");
        header.set_has_frame(false);
        header.set_halign(gtk::Align::Start);
        header.set_tooltip_text(Some(call_id));

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let spinner = gtk::Spinner::new();
        spinner.add_css_class("lc-tool-spinner");
        spinner.set_valign(gtk::Align::Center);
        spinner.set_visible(false);
        let glyph = gtk::Label::new(Some("\u{2022}"));
        glyph.add_css_class("lc-tool-glyph");
        let label = gtk::Label::new(None);
        label.add_css_class("lc-tool-label");
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        label.set_max_width_chars(46);
        label.set_xalign(0.0);

        row.append(&spinner);
        row.append(&glyph);
        row.append(&label);
        header.set_child(Some(&row));
        root.append(&header);

        let notes = gtk::Box::new(gtk::Orientation::Vertical, 2);
        notes.set_margin_start(18);
        root.append(&notes);

        // Only `bash` exposes its command and output, and only after a click.
        let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
        body.add_css_class("lc-tool-chip-body");
        body.set_margin_start(18);
        body.set_visible(false);
        let output = gtk::Label::new(None);
        if tool == "bash" {
            let command = args
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if !command.is_empty() {
                let heading = gtk::Label::new(Some("COMMAND"));
                heading.add_css_class("lc-section");
                heading.set_xalign(0.0);
                body.append(&heading);
                let code = gtk::Label::new(Some(command));
                code.add_css_class("lc-mono");
                code.add_css_class("lc-tiny");
                code.add_css_class("lc-dim");
                code.set_xalign(0.0);
                code.set_selectable(true);
                code.set_wrap(true);
                code.set_wrap_mode(gtk::pango::WrapMode::Char);
                body.append(&code);
            }
            output.add_css_class("lc-mono");
            output.add_css_class("lc-tiny");
            output.set_xalign(0.0);
            output.set_selectable(true);
            output.set_wrap(true);
            output.set_wrap_mode(gtk::pango::WrapMode::Char);
            output.set_text("No output yet");
            let scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vscrollbar_policy(gtk::PolicyType::Automatic)
                .min_content_height(24)
                .max_content_height(200)
                .propagate_natural_height(true)
                .child(&output)
                .build();
            body.append(&scroll);
        }
        root.append(&body);

        let path = args
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        let clickable = tool == "bash" || !path.is_empty();

        let chip = Rc::new(Self {
            root,
            header,
            spinner,
            glyph,
            label,
            body,
            output,
            notes,
            buffer: Rc::new(RefCell::new(String::new())),
            flush_pending: Rc::new(Cell::new(false)),
            tool: tool.to_string(),
            path: RefCell::new(path),
            status: Cell::new(ToolStatus::Pending),
            expanded: Cell::new(false),
        });
        chip.set_summary(summary, args);
        chip.refresh();

        if clickable {
            chip.header.set_cursor_from_name(Some("pointer"));
            let state = state.clone();
            let weak = Rc::downgrade(&chip);
            chip.header.connect_clicked(move |_| {
                let Some(chip) = weak.upgrade() else {
                    return;
                };
                if chip.tool == "bash" {
                    let open = !chip.expanded.get();
                    chip.expanded.set(open);
                    chip.body.set_visible(open);
                } else {
                    let path = chip.path.borrow().clone();
                    if !path.is_empty() {
                        crate::panels::editor::focus_file(&state, Path::new(&path));
                    }
                }
            });
        }

        chip
    }

    pub fn path(&self) -> String {
        self.path.borrow().clone()
    }

    pub fn tool(&self) -> &str {
        &self.tool
    }

    pub fn set_status(&self, status: ToolStatus) {
        self.status.set(status);
        self.refresh();
    }

    /// Render the final tool result. Only `bash` keeps the text, because its
    /// header expands on demand; every other tool stays a one-line chip.
    pub fn set_output(&self, content: &str) {
        if self.tool != "bash" {
            return;
        }
        let mut buffer = self.buffer.borrow_mut();
        buffer.clear();
        buffer.push_str(content);
        self.output.set_text(&markdown::summarise(&buffer, 24_000));
    }

    pub fn append_output(&self, chunk: &str) {
        if self.tool != "bash" {
            return;
        }
        {
            let mut buffer = self.buffer.borrow_mut();
            buffer.push_str(chunk);
            if buffer.len() > 24_000 {
                let cut = buffer.len() - 24_000;
                buffer.drain(..cut);
            }
        }
        self.schedule_flush();
    }

    fn schedule_flush(&self) {
        if self.flush_pending.replace(true) {
            return;
        }
        let buffer = self.buffer.clone();
        let output = self.output.clone();
        let pending = self.flush_pending.clone();
        glib::timeout_add_local_once(Duration::from_millis(60), move || {
            pending.set(false);
            output.set_text(&markdown::summarise(&buffer.borrow(), 24_000));
        });
    }

    pub fn add_note(&self, text: &str, css: &str) {
        let label = gtk::Label::new(Some(text));
        label.add_css_class("lc-tool-note");
        label.add_css_class(css);
        label.add_css_class("lc-tiny");
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::Char);
        self.notes.append(&label);
    }

    pub fn clear_notes(&self) {
        while let Some(child) = self.notes.first_child() {
            self.notes.remove(&child);
        }
    }

    /// Tooltip used for failures, so the raw result never enters the chat log.
    pub fn set_detail_text(&self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.label.set_tooltip_text(Some(text));
    }

    fn set_summary(&self, summary: &str, args: &serde_json::Value) {
        let text = friendly_label(&self.tool, &self.path.borrow(), summary, args);
        self.label.set_text(&text);
        self.header.set_tooltip_text(Some(&text));
    }

    fn refresh(&self) {
        let running = matches!(self.status.get(), ToolStatus::Pending | ToolStatus::Running);
        self.spinner.set_visible(running);
        if running {
            self.spinner.start();
        } else {
            self.spinner.stop();
        }

        let current = self.label.text().to_string();
        let base = strip_status(&current);
        match self.status.get() {
            ToolStatus::Pending | ToolStatus::Running => {
                self.glyph.set_text("\u{270e}");
                self.glyph.remove_css_class("lc-error");
                self.label.set_text(&progressive_label(&self.tool, &base));
            }
            ToolStatus::Done => {
                self.glyph.set_text("\u{2713}");
                self.glyph.remove_css_class("lc-error");
                self.label.set_text(&base);
            }
            ToolStatus::Failed | ToolStatus::Denied => {
                self.glyph.set_text("\u{2717}");
                self.glyph.add_css_class("lc-error");
                let suffix = if self.status.get() == ToolStatus::Denied {
                    "denied"
                } else {
                    "failed"
                };
                self.label.set_text(&format!("{base} \u{2014} {suffix}"));
            }
        }
    }
}

/// A human sentence for the tool, with no arguments or code.
fn friendly_label(
    tool: &str,
    path: &str,
    summary: &str,
    args: &serde_json::Value,
) -> String {
    let file = if path.is_empty() {
        String::new()
    } else {
        Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string())
    };
    match tool {
        "read_file" => format!("Read {file}"),
        "list_dir" => {
            if file.is_empty() {
                "Listed files".to_string()
            } else {
                format!("Listed {file}")
            }
        }
        "glob" => {
            let pattern = args
                .get("pattern")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if pattern.is_empty() {
                "Searched files".to_string()
            } else {
                format!("Searched files for {pattern}")
            }
        }
        "grep" => {
            let pattern = args
                .get("pattern")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if pattern.is_empty() {
                "Searched the code".to_string()
            } else {
                format!("Searched the code for {pattern}")
            }
        }
        "bash" => "Ran command".to_string(),
        "web_fetch" => "Fetched page".to_string(),
        "ask_user" => "Asked a question".to_string(),
        "update_tasks" => "Updated tasks".to_string(),
        other => {
            let words = other.replace('_', " ");
            let mut chars = words.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => summary.to_string(),
            }
        }
    }
}

fn progressive_label(tool: &str, base: &str) -> String {
    match tool {
        "read_file" => base.replacen("Read ", "Reading ", 1) + "\u{2026}",
        "bash" => "Running command\u{2026}".to_string(),
        "grep" => "Searching the code\u{2026}".to_string(),
        "glob" => "Searching files\u{2026}".to_string(),
        "list_dir" => "Listing files\u{2026}".to_string(),
        _ => format!("{base}\u{2026}"),
    }
}

fn strip_status(text: &str) -> String {
    for suffix in [" \u{2014} failed", " \u{2014} denied"] {
        if let Some(stripped) = text.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    if let Some(stripped) = text.strip_suffix('\u{2026}') {
        return stripped
            .replacen("Reading ", "Read ", 1)
            .replacen("Searching the code", "Searched the code", 1)
            .replacen("Searching files", "Searched files", 1)
            .replacen("Listing files", "Listed files", 1)
            .replacen("Running command", "Ran command", 1);
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn labels_reads_with_the_file_name_only() {
        let args = json!({"path": "src/panels/agent.rs"});
        assert_eq!(
            friendly_label("read_file", "src/panels/agent.rs", "", &args),
            "Read agent.rs"
        );
    }

    #[test]
    fn labels_bash_and_search_without_arguments() {
        assert_eq!(friendly_label("bash", "", "ls -la", &json!({})), "Ran command");
        assert_eq!(
            friendly_label("grep", "", "", &json!({"pattern": "footer"})),
            "Searched the code for footer"
        );
    }

    #[test]
    fn progressive_then_final_labels_round_trip() {
        let running = progressive_label("read_file", "Read index.html");
        assert_eq!(running, "Reading index.html\u{2026}");
        assert_eq!(strip_status(&running), "Read index.html");

        assert_eq!(strip_status("Running command\u{2026}"), "Ran command");
        assert_eq!(
            strip_status("Reading index.html\u{2026} \u{2014} failed"),
            "Reading index.html\u{2026}"
        );
    }
}
