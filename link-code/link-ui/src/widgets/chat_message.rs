//! Chat bubbles: streaming assistant blocks, user bubbles, notices and errors.

use std::cell::{Cell, RefCell};

use gtk::glib;
use gtk::prelude::*;

use link_core::event::ErrorKind;

use crate::markdown;

/// A single assistant answer. Text streams into a plain label first and is
/// re-rendered as Markdown once the turn's text block is complete.
pub struct BlockView {
    pub root: gtk::Box,
    header: gtk::Box,
    body: gtk::Box,
    raw: gtk::Label,
    text: RefCell<String>,
    thinking: RefCell<String>,
    thinking_expander: gtk::Expander,
    thinking_label: gtk::Label,
    done: RefCell<bool>,
    /// True once a tool card, plan card or error card was attached.
    has_widgets: Cell<bool>,
    copy_btn: gtk::Button,
}

impl BlockView {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 3);
        root.add_css_class("lc-block-assistant");
        root.set_visible(false);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let dot = gtk::Label::new(Some("\u{25cf}"));
        dot.add_css_class("lc-working-dot");
        let who = gtk::Label::new(Some("LINK CODE"));
        who.add_css_class("lc-section");
        header.append(&dot);
        header.append(&who);

        let hspacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        hspacer.set_hexpand(true);
        header.append(&hspacer);
        let copy_btn = gtk::Button::from_icon_name("edit-copy-symbolic");
        copy_btn.add_css_class("lc-copy-btn");
        copy_btn.set_tooltip_text(Some("Copy reply"));
        copy_btn.set_visible(false);
        header.append(&copy_btn);

        root.append(&header);

        let raw = gtk::Label::new(None);
        raw.set_xalign(0.0);
        raw.set_wrap(true);
        raw.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        raw.set_selectable(true);
        raw.add_css_class("lc-body");
        root.append(&raw);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
        body.set_visible(false);
        root.append(&body);

        let thinking_label = gtk::Label::new(None);
        thinking_label.set_xalign(0.0);
        thinking_label.set_wrap(true);
        thinking_label.set_wrap_mode(gtk::pango::WrapMode::Char);
        thinking_label.set_selectable(true);
        thinking_label.add_css_class("lc-mono");
        thinking_label.add_css_class("lc-tiny");
        thinking_label.add_css_class("lc-dim");
        let thinking_expander = gtk::Expander::new(Some("Thinking"));
        thinking_expander.add_css_class("lc-tiny");
        thinking_expander.set_expanded(false);
        thinking_expander.set_child(Some(&thinking_label));
        thinking_expander.set_visible(false);
        root.append(&thinking_expander);

        Self {
            root,
            header,
            body,
            raw,
            text: RefCell::new(String::new()),
            thinking: RefCell::new(String::new()),
            thinking_expander,
            thinking_label,
            done: RefCell::new(false),
            has_widgets: Cell::new(false),
            copy_btn,
        }
    }

    /// Wire the copy button — must be called once after Self is wrapped in Rc.
    pub fn wire_copy_btn(self_rc: &std::rc::Rc<Self>) {
        let weak = std::rc::Rc::downgrade(self_rc);
        self_rc.copy_btn.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                let text = this.text.borrow().clone();
                if !text.is_empty() {
                    if let Some(display) = gtk::gdk::Display::default() {
                        display.clipboard().set_text(&text);
                    }
                }
            }
        });
    }

    /// Show the "LINK CODE" header. Hidden on every block but the first of a
    /// turn so a walkthrough reads as one continuous reply.
    pub fn set_header_visible(&self, visible: bool) {
        self.header.set_visible(visible);
    }

    pub fn append_text(&self, chunk: &str) {
        self.root.set_visible(true);
        let mut text = self.text.borrow_mut();
        text.push_str(chunk);
        // During streaming, show prose. Code blocks appear in the editor
        // once the full response lands (see agent.rs TextComplete handler).
        let prose = prose_without_code(&text);
        self.raw.set_text(if prose.trim().is_empty() {
            "\u{2756} Writing code\u{2026}"
        } else {
            &prose
        });
        self.raw.set_visible(true);
        self.body.set_visible(false);
        self.copy_btn.set_visible(true);
    }

    pub fn append_thinking(&self, chunk: &str) {
        self.root.set_visible(true);
        let mut thinking = self.thinking.borrow_mut();
        thinking.push_str(chunk);
        self.thinking_label.set_text(&thinking);
        self.thinking_expander.set_visible(true);
    }

    /// Replace the streamed text with the final, complete text.
    pub fn complete(&self, final_text: &str) {
        if !final_text.is_empty() {
            *self.text.borrow_mut() = final_text.to_string();
        }
        *self.done.borrow_mut() = true;
        if !self.text.borrow().is_empty() {
            self.copy_btn.set_visible(true);
        }
        self.render();
    }

    /// Re-render the prose parts only. Code blocks are shown in the editor
    /// by the caller (agent.rs) — not here.
    pub fn render(&self) {
        let text = self.text.borrow().clone();
        if text.trim().is_empty() {
            return;
        }
        self.root.set_visible(true);
        clear(&self.body);
        render_markdown_prose_only(&self.body, &text);
        let has_content = self.body.first_child().is_some();
        self.body.set_visible(has_content);
        self.raw.set_visible(!has_content);
        if !has_content {
            let prose = prose_without_code(&text);
            if !prose.trim().is_empty() {
                self.raw.set_text(&prose);
                self.raw.set_visible(true);
            }
        }
    }

    pub fn text(&self) -> String {
        self.text.borrow().clone()
    }

    pub fn is_done(&self) -> bool {
        *self.done.borrow()
    }

    pub fn add_widget(&self, widget: &impl IsA<gtk::Widget>) {
        self.has_widgets.set(true);
        self.root.set_visible(true);
        self.root.append(widget);
    }

    /// An assistant block that received neither text nor cards. The UI drops
    /// it from the chat so there is no blank space between tool calls.
    pub fn is_empty(&self) -> bool {
        self.text.borrow().trim().is_empty()
            && self.thinking.borrow().trim().is_empty()
            && !self.has_widgets.get()
    }

    pub fn detach(&self) {
        self.root.unparent();
    }

    pub fn finish(&self) {
        *self.done.borrow_mut() = true;
        if !self.text.borrow().trim().is_empty() {
            self.render();
        }
    }
}

/// Full markdown renderer — prose AND code blocks inline. Used for plan cards.
pub fn render_markdown_into(container: &gtk::Box, text: &str) {
    for segment in markdown::segments(text) {
        match segment {
            markdown::Segment::Text(markup) => {
                let label = gtk::Label::new(None);
                label.set_markup(&markup);
                label.set_xalign(0.0);
                label.set_wrap(true);
                label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                label.set_selectable(true);
                label.add_css_class("lc-body");
                container.append(&label);
            }
            markdown::Segment::Code { language, code } => {
                let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
                frame.add_css_class("lc-card");
                frame.add_css_class("lc-code-block");
                if !language.is_empty() {
                    let tag = gtk::Label::new(Some(&language));
                    tag.add_css_class("lc-section");
                    tag.set_xalign(0.0);
                    tag.set_margin_start(6);
                    tag.set_margin_top(4);
                    frame.append(&tag);
                }
                let label = gtk::Label::new(Some(&code));
                label.add_css_class("lc-mono");
                label.add_css_class("lc-tiny");
                label.set_xalign(0.0);
                label.set_selectable(true);
                label.set_wrap(false);
                label.set_margin_start(6);
                label.set_margin_end(6);
                label.set_margin_top(4);
                label.set_margin_bottom(6);
                let scroll = gtk::ScrolledWindow::builder()
                    .hscrollbar_policy(gtk::PolicyType::Automatic)
                    .vscrollbar_policy(gtk::PolicyType::Never)
                    .propagate_natural_height(true)
                    .child(&label)
                    .build();
                frame.append(&scroll);
                container.append(&frame);
            }
        }
    }
}

/// Render only the prose parts of `text`. Code blocks are skipped — they are
/// displayed in the editor. A small notice is shown in their place.
pub fn render_markdown_prose_only(container: &gtk::Box, text: &str) {
    for segment in markdown::segments(text) {
        match segment {
            markdown::Segment::Text(markup) => {
                let label = gtk::Label::new(None);
                label.set_markup(&markup);
                label.set_xalign(0.0);
                label.set_wrap(true);
                label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                label.set_selectable(true);
                label.add_css_class("lc-body");
                container.append(&label);
            }
            markdown::Segment::Code { language, .. } => {
                let name = if language.is_empty() { "code" } else { &language };
                let lbl = gtk::Label::new(None);
                let escaped = glib::markup_escape_text(name);
                lbl.set_markup(&format!(
                    "<span foreground=\"#777\" size=\"small\">\u{21b3} {escaped} \u{2014} shown in editor</span>"
                ));
                lbl.set_xalign(0.0);
                lbl.set_margin_top(2);
                lbl.set_margin_bottom(2);
                container.append(&lbl);
            }
        }
    }
}

/// Strip code fences from text, returning only the prose lines.
fn prose_without_code(text: &str) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

/// A right-aligned user message.
pub fn user_bubble(text: &str) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    outer.set_halign(gtk::Align::End);
    let bubble = gtk::Box::new(gtk::Orientation::Vertical, 2);
    bubble.add_css_class("lc-block-user");
    bubble.set_halign(gtk::Align::End);

    let hdr = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let who = gtk::Label::new(Some("YOU"));
    who.add_css_class("lc-section");
    who.add_css_class("lc-accent");
    who.set_xalign(1.0);
    who.set_hexpand(true);
    hdr.append(&who);

    let copy_btn = gtk::Button::from_icon_name("edit-copy-symbolic");
    copy_btn.add_css_class("lc-copy-btn");
    copy_btn.set_tooltip_text(Some("Copy message"));
    let text_owned = text.to_string();
    copy_btn.connect_clicked(move |_| {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&text_owned);
        }
    });
    hdr.append(&copy_btn);
    bubble.append(&hdr);

    let label = gtk::Label::new(None);
    label.set_markup(&crate::markdown::prose_markup(text));
    label.set_xalign(1.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_selectable(true);
    label.set_max_width_chars(52);
    bubble.append(&label);

    outer.append(&bubble);
    outer
}

/// A dim one-line system note.
pub fn notice(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("lc-block-system");
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_margin_start(2);
    label
}

/// A red error card with a collapsible raw detail.
pub fn error_card(kind: ErrorKind, message: &str, detail: Option<&str>) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    card.add_css_class("lc-block-error");
    let title = gtk::Label::new(Some(kind.headline()));
    title.add_css_class("lc-error");
    title.add_css_class("lc-tiny");
    title.set_xalign(0.0);
    card.append(&title);

    let body = gtk::Label::new(Some(message));
    body.set_xalign(0.0);
    body.set_wrap(true);
    body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    body.set_selectable(true);
    card.append(&body);

    if let Some(detail) = detail.filter(|detail| !detail.trim().is_empty()) {
        let label = gtk::Label::new(Some(detail));
        label.add_css_class("lc-mono");
        label.add_css_class("lc-tiny");
        label.add_css_class("lc-dim");
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::Char);
        label.set_selectable(true);
        let expander = gtk::Expander::new(Some("Detail"));
        expander.add_css_class("lc-tiny");
        expander.set_expanded(false);
        expander.set_child(Some(&label));
        card.append(&expander);
    }
    card
}

/// A plan card, rendered as Markdown inside a dashed frame.
pub fn plan_card(text: &str) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("lc-block-plan");
    let heading = gtk::Label::new(Some("PLAN"));
    heading.add_css_class("lc-section");
    heading.set_xalign(0.0);
    card.append(&heading);
    render_markdown_into(&card, text);
    card
}

pub fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}
