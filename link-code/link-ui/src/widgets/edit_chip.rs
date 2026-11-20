//! The compact one-line row that replaces file-edit arguments in the chat.
//!
//! The code itself is streamed into the editor, so the conversation only shows
//! a single row:
//!
//! ```text
//! [rs] main.rs  (Edited)  +5  -2   ...   39 s  ✓  ⌄
//! ```
//!
//! Clicking the file name focuses it in the editor; the chevron expands a
//! small summary with the full path. Nothing else about the payload lands in
//! the chat.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use gtk::prelude::*;

use link_core::event::ToolStatus;

use crate::state::UiState;

pub struct EditChip {
    pub root: gtk::Box,
    spinner: gtk::Spinner,
    glyph: gtk::Label,
    name: gtk::Label,
    op: gtk::Label,
    added_pill: gtk::Label,
    removed_pill: gtk::Label,
    duration: gtk::Label,
    check: gtk::Label,
    detail_text: gtk::Label,
    path: RefCell<String>,
    tool: RefCell<String>,
    status: Cell<ToolStatus>,
    added: Cell<usize>,
    removed: Cell<usize>,
    started: Instant,
}

impl EditChip {
    pub fn new(state: &Rc<UiState>, _call_id: &str, tool: &str, path: &str) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 2);
        root.add_css_class("lc-edit-row");
        root.set_halign(gtk::Align::Fill);
        root.set_margin_top(1);
        root.set_margin_bottom(1);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 7);
        header.set_valign(gtk::Align::Center);

        let glyph = gtk::Label::new(Some("·"));
        glyph.add_css_class("lc-ftype");
        glyph.add_css_class("lc-ftype-default");
        glyph.set_width_chars(2);
        glyph.set_xalign(0.5);
        header.append(&glyph);

        // The file name is its own flat button so a click focuses the editor
        // without the rest of the row acting like a button.
        let name = gtk::Label::new(None);
        name.add_css_class("lc-edit-file");
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        let name_button = gtk::Button::builder().child(&name).has_frame(false).build();
        name_button.add_css_class("lc-edit-file-btn");
        header.append(&name_button);

        let op = gtk::Label::new(Some("(Edited)"));
        op.add_css_class("lc-edit-op");
        op.set_visible(false);
        header.append(&op);

        let added_pill = gtk::Label::new(None);
        added_pill.add_css_class("lc-add-pill");
        added_pill.set_visible(false);
        header.append(&added_pill);

        let removed_pill = gtk::Label::new(None);
        removed_pill.add_css_class("lc-del-pill");
        removed_pill.set_visible(false);
        header.append(&removed_pill);

        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        header.append(&spacer);

        let duration = gtk::Label::new(None);
        duration.add_css_class("lc-edit-duration");
        duration.set_visible(false);
        header.append(&duration);

        let spinner = gtk::Spinner::new();
        spinner.add_css_class("lc-edit-spinner");
        spinner.set_valign(gtk::Align::Center);
        spinner.set_visible(false);
        header.append(&spinner);

        let check = gtk::Label::new(Some("\u{2713}"));
        check.add_css_class("lc-edit-check");
        check.set_visible(false);
        header.append(&check);

        let chevron = gtk::Button::with_label("\u{203a}");
        chevron.add_css_class("lc-edit-chevron");
        chevron.set_tooltip_text(Some("Show edit details"));
        chevron.set_has_frame(false);
        header.append(&chevron);

        root.append(&header);

        let detail = gtk::Box::new(gtk::Orientation::Vertical, 2);
        detail.add_css_class("lc-edit-detail");
        detail.set_visible(false);
        let detail_text = gtk::Label::new(None);
        detail_text.add_css_class("lc-mono");
        detail_text.add_css_class("lc-tiny");
        detail_text.add_css_class("lc-dim");
        detail_text.set_xalign(0.0);
        detail_text.set_wrap(true);
        detail_text.set_wrap_mode(gtk::pango::WrapMode::Char);
        detail_text.set_selectable(true);
        detail.append(&detail_text);
        root.append(&detail);
        let detail_for_toggle = detail.clone();

        let chip = Rc::new(Self {
            root: root.clone(),
            spinner,
            glyph,
            name,
            op,
            added_pill,
            removed_pill,
            duration,
            check,
            detail_text,
            path: RefCell::new(String::new()),
            tool: RefCell::new(tool.to_string()),
            status: Cell::new(ToolStatus::Pending),
            added: Cell::new(0),
            removed: Cell::new(0),
            started: Instant::now(),
        });

        chip.set_path(path);
        chip.set_status(ToolStatus::Running);

        let state = state.clone();
        let weak = Rc::downgrade(&chip);
        name_button.connect_clicked(move |_| {
            if let Some(chip) = weak.upgrade() {
                let path = chip.path.borrow().clone();
                if !path.is_empty() {
                    crate::panels::editor::focus_file(&state, Path::new(&path));
                }
            }
        });

        {
            let detail = detail_for_toggle.clone();
            let chevron = chevron.clone();
            chevron.clone().connect_clicked(move |_| {
                let showing = !detail.is_visible();
                detail.set_visible(showing);
                chevron.set_label(if showing { "\u{2304}" } else { "\u{203a}" });
            });
        }

        chip
    }

    pub fn path(&self) -> String {
        self.path.borrow().clone()
    }

    pub fn set_path(&self, path: &str) {
        if path.is_empty() {
            return;
        }
        *self.path.borrow_mut() = path.to_string();
        self.root.set_tooltip_text(Some(path));
        self.refresh();
    }

    pub fn set_tool(&self, tool: &str) {
        if tool.is_empty() {
            return;
        }
        *self.tool.borrow_mut() = tool.to_string();
        self.refresh();
    }

    /// True for tools that replace the whole file; their row reports lines
    /// added rather than a `+X −Y` delta.
    fn writes_whole_file(&self) -> bool {
        matches!(self.tool.borrow().as_str(), "write_file" | "create_file")
    }

    pub fn set_counts(&self, added: usize, removed: usize) {
        self.added.set(added);
        self.removed.set(removed);
        self.refresh();
    }

    pub fn set_status(&self, status: ToolStatus) {
        self.status.set(status);
        self.refresh();
    }

    fn file(&self) -> String {
        let path = self.path.borrow();
        Path::new(path.as_str())
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone())
    }

    fn refresh(&self) {
        let path = self.path.borrow().clone();
        let file = self.file();
        if file.is_empty() {
            return;
        }
        if !path.is_empty() {
            self.root.set_tooltip_text(Some(&path));
            let glyph = file_type(&path);
            self.glyph.set_text(glyph.0);
            self.glyph.remove_css_class("lc-ftype-default");
            self.glyph.add_css_class(glyph.1);
        }
        self.name.set_text(&file);

        let whole = self.writes_whole_file();
        match self.status.get() {
            ToolStatus::Pending | ToolStatus::Running => {
                self.spinner.set_visible(true);
                self.spinner.start();
                self.op.set_text(if whole { "(Writing)" } else { "(Editing)" });
                self.op.remove_css_class("lc-error");
                self.op.add_css_class("lc-edit-op");
                self.op.set_visible(true);
                self.added_pill.set_visible(false);
                self.removed_pill.set_visible(false);
                self.duration.set_visible(false);
                self.check.set_visible(false);
            }
            ToolStatus::Done => {
                self.spinner.stop();
                self.spinner.set_visible(false);
                self.op.set_text(if whole { "(Written)" } else { "(Edited)" });
                self.op.remove_css_class("lc-error");
                self.op.set_visible(true);
                let added = self.added.get();
                let removed = self.removed.get();
                self.added_pill.set_text(&format!("+{added}"));
                self.added_pill.set_visible(added > 0);
                self.removed_pill.set_text(&format!("\u{2212}{removed}"));
                self.removed_pill.set_visible(removed > 0);
                let elapsed = self.started.elapsed();
                self.duration.set_text(&if elapsed.as_secs() == 0 {
                    format!("{} ms", elapsed.as_millis())
                } else {
                    format!("{} s", elapsed.as_secs())
                });
                self.duration.set_visible(true);
                self.check.set_visible(true);
            }
            ToolStatus::Failed | ToolStatus::Denied => {
                self.spinner.stop();
                self.spinner.set_visible(false);
                let denied = self.status.get() == ToolStatus::Denied;
                self.op.set_text(if denied { "(Denied)" } else { "(Failed)" });
                self.op.add_css_class("lc-error");
                self.op.set_visible(true);
                self.added_pill.set_visible(false);
                self.removed_pill.set_visible(false);
                self.duration.set_visible(false);
                self.check.set_visible(false);
            }
        }

        let added = self.added.get();
        let removed = self.removed.get();
        let status = match self.status.get() {
            ToolStatus::Done => "done",
            ToolStatus::Failed => "failed",
            ToolStatus::Denied => "denied",
            _ => "running",
        };
        self.detail_text.set_text(&format!(
            "{path}\n{added} added \u{00b7} {removed} removed \u{00b7} {status}"
        ));
    }
}

/// Short, colour-coded glyph for the file type, matching the sidebar tree.
fn file_type(path: &str) -> (&'static str, &'static str) {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => ("RS", "lc-ftype-rs"),
        "md" | "markdown" => ("MD", "lc-ftype-md"),
        "json" => ("{}", "lc-ftype-json"),
        "toml" | "yaml" | "yml" => ("TM", "lc-ftype-toml"),
        "js" | "mjs" | "cjs" | "jsx" => ("JS", "lc-ftype-js"),
        "ts" | "tsx" => ("TS", "lc-ftype-js"),
        "html" | "htm" => ("<>", "lc-ftype-html"),
        "css" | "scss" => ("#", "lc-ftype-css"),
        "py" => ("PY", "lc-ftype-py"),
        "go" => ("GO", "lc-ftype-gradle"),
        "kt" => ("KT", "lc-ftype-kt"),
        "sh" | "bash" | "zsh" => ("SH", "lc-ftype-git"),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" => ("IM", "lc-ftype-img"),
        _ => ("·", "lc-ftype-default"),
    }
}