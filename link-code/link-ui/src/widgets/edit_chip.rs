//! The compact chip that replaces file-edit arguments in the chat.
//!
//! The code itself is streamed into the editor, so the conversation only shows
//! one line: `Writing index.html…`, later `Wrote index.html ✓ +42 lines` for a
//! whole-file write, or `Editing index.html…` → `Edited index.html ✓ +12 −2`
//! for a surgical edit. Clicking it focuses the file.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use gtk::prelude::*;

use link_core::event::ToolStatus;

use crate::state::UiState;

pub struct EditChip {
    pub root: gtk::Button,
    spinner: gtk::Spinner,
    glyph: gtk::Label,
    name: gtk::Label,
    counts: gtk::Label,
    path: RefCell<String>,
    tool: RefCell<String>,
    status: Cell<ToolStatus>,
    added: Cell<usize>,
    removed: Cell<usize>,
}

impl EditChip {
    pub fn new(state: &Rc<UiState>, _call_id: &str, tool: &str, path: &str) -> Rc<Self> {
        let root = gtk::Button::new();
        root.add_css_class("lc-edit-chip");
        root.set_halign(gtk::Align::Start);
        root.set_margin_start(4);
        root.set_margin_top(2);
        root.set_margin_bottom(2);
        root.set_has_frame(false);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let spinner = gtk::Spinner::new();
        spinner.add_css_class("lc-edit-spinner");
        let glyph = gtk::Label::new(Some("\u{270e}"));
        glyph.add_css_class("lc-edit-glyph");
        let name = gtk::Label::new(None);
        name.add_css_class("lc-edit-name");
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name.set_max_width_chars(42);
        let counts = gtk::Label::new(None);
        counts.add_css_class("lc-edit-counts");
        counts.set_visible(false);

        row.append(&spinner);
        row.append(&glyph);
        row.append(&name);
        row.append(&counts);
        root.set_child(Some(&row));

        let chip = Rc::new(Self {
            root: root.clone(),
            spinner,
            glyph,
            name,
            counts,
            path: RefCell::new(String::new()),
            tool: RefCell::new(tool.to_string()),
            status: Cell::new(ToolStatus::Pending),
            added: Cell::new(0),
            removed: Cell::new(0),
        });
        chip.set_path(path);
        chip.set_status(ToolStatus::Running);

        let state = state.clone();
        let weak = Rc::downgrade(&chip);
        root.connect_clicked(move |_| {
            if let Some(chip) = weak.upgrade() {
                let path = chip.path.borrow().clone();
                if !path.is_empty() {
                    crate::panels::editor::focus_file(&state, Path::new(&path));
                }
            }
        });
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

    /// True for tools that replace the whole file; their chip reports lines
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

    fn refresh(&self) {
        let path = self.path.borrow();
        let file = Path::new(path.as_str())
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        if file.is_empty() {
            return;
        }
        let whole = self.writes_whole_file();
        match self.status.get() {
            ToolStatus::Pending | ToolStatus::Running => {
                self.spinner.set_visible(true);
                self.spinner.start();
                self.glyph.set_text("\u{270e}");
                let verb = if whole { "Writing" } else { "Editing" };
                self.name.set_text(&format!("{verb} {file}\u{2026}"));
                self.counts.set_visible(false);
            }
            ToolStatus::Done => {
                self.spinner.stop();
                self.spinner.set_visible(false);
                self.glyph.set_text("\u{2713}");
                if whole {
                    self.name.set_text(&format!("Wrote {file}"));
                    self.counts.set_markup(&format!(
                        "<span foreground=\"#2ecc71\">+{} lines</span>",
                        self.added.get()
                    ));
                } else {
                    self.name.set_text(&format!("Edited {file}"));
                    self.counts.set_markup(&format!(
                        "<span foreground=\"#2ecc71\">+{}</span>  <span foreground=\"#ff6b6b\">\u{2212}{}</span>",
                        self.added.get(),
                        self.removed.get()
                    ));
                }
                self.counts.set_visible(true);
            }
            ToolStatus::Failed | ToolStatus::Denied => {
                self.spinner.stop();
                self.spinner.set_visible(false);
                self.glyph.set_text("\u{2717}");
                let suffix = if self.status.get() == ToolStatus::Denied {
                    "denied"
                } else {
                    "failed"
                };
                self.name.set_text(&format!("{file} \u{2014} {suffix}"));
                self.counts.set_visible(false);
            }
        }
    }
}
