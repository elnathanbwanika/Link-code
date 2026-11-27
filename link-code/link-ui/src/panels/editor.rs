//! The centre column: a small tabbed text editor.
//!
//! The backend does not require a full IDE editor, so this is deliberately
//! lightweight: `gtk::TextView` pages with a tab strip, dirty tracking and
//! `Ctrl+S`. Files edited by the agent are reloaded when a diff is proposed.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use gtk::glib;
use gtk::prelude::*;

use crate::state::UiState;

/// Green/red classification for one line in the fixed gutter.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DiffKind {
    Added,
    Removed,
}

/// A line start plus its diff colour. Marks move with the buffer, so the
/// gutter stays correct while the agent types or the user edits around it.
struct DiffMark {
    mark: gtk::TextMark,
    kind: DiffKind,
}

/// Which form the review buffer currently takes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReviewPhase {
    /// Inline diff: removed lines are still visible in red above the additions.
    Diff,
    /// The real file content; only the Accept/Revert bar remains.
    Clean,
}

struct ReviewState {
    old: String,
    new: String,
    phase: ReviewPhase,
}

pub struct OpenFile {
    pub key: String,
    pub path: PathBuf,
    pub text: gtk::TextView,
    pub page: gtk::Box,
    pub tab: gtk::ToggleButton,
    pub dirty: Cell<bool>,
    /// Shown in the page header while the agent writes into this buffer.
    pub ai_badge: gtk::Label,
    pub ai_spinner: gtk::Spinner,
    /// True while the agent owns the buffer, so manual dirty tracking pauses.
    pub ai_owned: Cell<bool>,
    /// Fixed-width number column that scrolls vertically with the text but
    /// never horizontally. It also draws the red/green diff borders/markers.
    gutter: gtk::DrawingArea,
    wrap_button: gtk::ToggleButton,
    /// "+X −Y" review bar shown under the header after an AI edit.
    diff_bar: gtk::Box,
    diff_summary: gtk::Label,
    accept_button: gtk::Button,
    revert_button: gtk::Button,
    /// Content before the current AI edit, kept until Accept or Revert.
    previous: RefCell<Option<String>>,
    /// Incremented for every new diff so stale fade timers can bail out.
    diff_gen: Cell<u64>,
    /// Weak back-reference for button callbacks; never creates a cycle.
    state: Weak<UiState>,
    /// Lines currently painted as added/removed in the gutter.
    diff_marks: RefCell<Vec<DiffMark>>,
    /// Active Accept/Revert review, if any.
    review: RefCell<Option<ReviewState>>,
    /// A diff that arrived while the typing animation was still running.
    pending_review: RefCell<Option<(String, String, usize, usize)>>,
}

/// State for one streamed file edit, keyed by tool call id.
pub struct AiStream {
    file: Rc<OpenFile>,
    old_text: String,
    /// Bounds of the old text while it is still visible (red, struck through).
    old_start: Option<gtk::TextMark>,
    old_end: Option<gtk::TextMark>,
    /// Where the new text is typed.
    replacement: Option<gtk::TextMark>,
    skip_typing: bool,
    /// True once the old text is kept on screen above the new text.
    kept_old: bool,
    /// Text waiting to be typed in when a whole tool call arrived at once.
    anim: Rc<AnimState>,
}

/// Shared state between a stream and its drain timer.
///
/// A provider that does not stream argument fragments delivers the entire file
/// body in one delta. Instead of pasting it, the text is queued here and typed
/// in over a second or two so the user still watches it appear.
struct AnimState {
    queue: RefCell<String>,
    draining: Cell<bool>,
    /// Set when the tool stream ended before the animation finished; the
    /// buffer is unlocked and marked dirty as soon as the last character lands.
    ended: Cell<bool>,
}

impl AnimState {
    fn new() -> Self {
        Self {
            queue: RefCell::new(String::new()),
            draining: Cell::new(false),
            ended: Cell::new(false),
        }
    }
}

pub struct EditorPane {
    pub root: gtk::Box,
    pub stack: gtk::Stack,
    pub tabs: gtk::Box,
    pub welcome: gtk::Widget,
    pub open: RefCell<Vec<Rc<OpenFile>>>,
    /// Live edits currently streaming in, keyed by tool call id.
    pub streams: RefCell<HashMap<String, AiStream>>,
    /// Number of drain timers currently typing into a buffer. While this is
    /// non-zero, disk reloads wait so they cannot clobber an animation.
    pub animating: Cell<usize>,
    /// Reloads postponed until the last animation finishes.
    pub deferred_reloads: RefCell<Vec<PathBuf>>,
    next_key: Cell<u32>,
}

pub fn build(state: &Rc<UiState>) -> Rc<EditorPane> {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("lc-center");

    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.set_hexpand(true);

    let welcome = welcome_page(state);
    stack.add_named(&welcome, Some("welcome"));

    let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tabs.add_css_class("lc-tabstrip");
    let tab_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&tabs)
        .build();
    root.append(&tab_scroll);
    root.append(&stack);

    Rc::new(EditorPane {
        root,
        stack,
        tabs,
        welcome,
        open: RefCell::new(Vec::new()),
        streams: RefCell::new(HashMap::new()),
        animating: Cell::new(0),
        deferred_reloads: RefCell::new(Vec::new()),
        next_key: Cell::new(0),
    })
}

fn welcome_page(state: &Rc<UiState>) -> gtk::Widget {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
    page.add_css_class("lc-welcome");
    page.set_valign(gtk::Align::Center);
    page.set_halign(gtk::Align::Center);
    page.set_margin_start(18);
    page.set_margin_end(18);
    page.set_margin_top(10);
    page.set_margin_bottom(10);

    let logo = gtk::Label::new(Some("LINK CODE"));
    logo.add_css_class("lc-title");
    logo.add_css_class("lc-accent");
    page.append(&logo);

    let subtitle = gtk::Label::new(Some(
        "A native AI coding IDE \u{00b7} pick a file from the sidebar or ask the agent",
    ));
    subtitle.add_css_class("lc-dim");
    // Wrap instead of forcing the centre column (and the whole window) wider.
    subtitle.set_wrap(true);
    subtitle.set_justify(gtk::Justification::Center);
    subtitle.set_max_width_chars(44);
    page.append(&subtitle);

    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    separator.set_margin_top(8);
    separator.set_margin_bottom(8);
    page.append(&separator);

    for (keys, action) in [
        ("Ctrl+K", "Command palette"),
        ("Ctrl+O", "Open folder"),
        ("Ctrl+P", "Quick open a file"),
        ("Ctrl+L", "Focus the agent input"),
        ("Ctrl+M", "Manage models"),
        ("Ctrl+,", "Settings"),
        ("Shift+Tab", "Cycle permission mode"),
        ("Esc", "Stop the agent"),
    ] {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let key = gtk::Label::new(Some(keys));
        key.add_css_class("lc-mono");
        key.add_css_class("lc-tiny");
        key.add_css_class("lc-badge");
        key.set_xalign(0.0);
        key.set_width_chars(10);
        let text = gtk::Label::new(Some(action));
        text.add_css_class("lc-tiny");
        text.add_css_class("lc-dim");
        text.set_xalign(0.0);
        row.append(&key);
        row.append(&text);
        page.append(&row);
    }

    if let Some(workspace) = state.workspace_path() {
        let path = gtk::Label::new(Some(&workspace.display().to_string()));
        path.add_css_class("lc-path");
        path.set_margin_top(10);
        path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        path.set_max_width_chars(42);
        page.append(&path);
    }

    // The welcome page must not dictate the window height: in a short window
    // it scrolls instead of stretching the whole IDE.
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .child(&page)
        .build();
    scroll.set_vexpand(true);
    scroll.upcast()
}

fn wrap_toggle(state: &Rc<UiState>) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::with_label("Wrap");
    button.add_css_class("lc-ghost");
    button.add_css_class("lc-tiny");
    button.set_tooltip_text(Some("Toggle word wrap (Alt+Z)"));
    button.set_active(state.config.borrow().editor.word_wrap);
    button
}

fn build_diff_bar() -> (gtk::Box, gtk::Label, gtk::Button, gtk::Button) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.add_css_class("lc-diff-bar");
    bar.set_margin_start(8);
    bar.set_margin_end(6);
    bar.set_margin_top(2);
    bar.set_margin_bottom(2);
    bar.set_visible(false);

    let summary = gtk::Label::new(None);
    summary.add_css_class("lc-mono");
    summary.add_css_class("lc-tiny");
    summary.set_xalign(0.0);
    bar.append(&summary);

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);

    let revert = gtk::Button::with_label("Revert");
    revert.add_css_class("lc-ghost");
    revert.add_css_class("lc-tiny");
    revert.set_tooltip_text(Some("Restore the content from before this AI edit"));
    bar.append(&revert);

    let accept = gtk::Button::with_label("Accept");
    accept.add_css_class("lc-ghost");
    accept.add_css_class("lc-tiny");
    accept.set_tooltip_text(Some("Keep the AI edit and clear the highlights"));
    bar.append(&accept);

    (bar, summary, accept, revert)
}

/// The code area is a fixed gutter plus the scrolling text view. Keeping the
/// gutter outside the horizontal scroller is what stops line numbers from
/// sliding away when the user scrolls a long line to the right.
fn build_code_area(view: &gtk::TextView) -> (gtk::Box, gtk::DrawingArea) {
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.add_css_class("lc-editor-body");
    body.set_vexpand(true);
    body.set_hexpand(true);

    let gutter = gtk::DrawingArea::new();
    gutter.add_css_class("lc-editor-gutter");
    gutter.set_size_request(56, -1);
    gutter.set_vexpand(true);
    gutter.set_valign(gtk::Align::Fill);
    gutter.set_can_target(false);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .child(view)
        .build();
    scroll.add_css_class("lc-editor-scroll");
    scroll.set_has_frame(false);
    // Overlay scrollbars fade out when idle, which makes a clipped long line
    // look like a layout bug. Keep real scrollbars so it is obvious when the
    // text extends past the pane.
    scroll.set_overlay_scrolling(false);

    body.append(&gutter);
    body.append(&scroll);
    (body, gutter)
}

/// Tab stops for the editor. GTK measures Pango tab positions in 1/1024 px.
fn code_tabs(tab_width: u32) -> gtk::pango::TabArray {
    let step = (tab_width.max(1) as i32) * 8 * gtk::pango::SCALE;
    let mut tabs = gtk::pango::TabArray::new(64, false);
    for index in 0..64 {
        tabs.set_tab(index, gtk::pango::TabAlign::Left, step * (index + 1));
    }
    tabs
}

fn gutter_width(buffer: &gtk::TextBuffer) -> i32 {
    let digits = buffer.line_count().max(1).to_string().len() as i32;
    (20 + digits * 8).max(44)
}

fn wire_editor(file: &Rc<OpenFile>) {
    // Gutter repaints with the vertical adjustment; horizontal scrolling does
    // not affect it because the DrawingArea sits outside the scroller.
    {
        let weak = Rc::downgrade(file);
        let draw = file.gutter.clone();
        draw.set_draw_func(move |area, cr, width, height| {
            if let Some(file) = weak.upgrade() {
                draw_gutter(area, cr, width, height, &file);
            }
        });
    }
    {
        let weak = Rc::downgrade(file);
        if let Some(adjustment) = file.text.vadjustment() {
            adjustment.connect_value_changed(move |_| {
                if let Some(file) = weak.upgrade() {
                    file.gutter.queue_draw();
                }
            });
        }
    }
    {
        let weak = Rc::downgrade(file);
        file.text.connect_notify_local(Some("wrap-mode"), move |_, _| {
            if let Some(file) = weak.upgrade() {
                file.gutter.queue_draw();
            }
        });
    }
    {
        let weak = Rc::downgrade(file);
        file.text.buffer().connect_changed(move |buffer| {
            if let Some(file) = weak.upgrade() {
                file.gutter.set_size_request(gutter_width(buffer), -1);
                file.gutter.queue_draw();
            }
        });
    }
    {
        let weak = Rc::downgrade(file);
        file.wrap_button.connect_toggled(move |button| {
            let Some(file) = weak.upgrade() else {
                return;
            };
            let on = button.is_active();
            set_wrap_mode(&file, on);
            if let Some(state) = file.state.upgrade() {
                state.config.borrow_mut().editor.word_wrap = on;
                let _ = state.save_config();
            }
        });
    }
    {
        let weak = Rc::downgrade(file);
        file.accept_button.connect_clicked(move |_| {
            if let Some(file) = weak.upgrade() {
                accept_review(&file, true);
            }
        });
    }
    {
        let weak = Rc::downgrade(file);
        file.revert_button.connect_clicked(move |_| {
            if let Some(file) = weak.upgrade() {
                revert_review(&file);
            }
        });
    }
    file.gutter.set_size_request(gutter_width(&file.text.buffer()), -1);
}

fn set_wrap_mode(file: &OpenFile, on: bool) {
    file.text.set_wrap_mode(if on {
        gtk::WrapMode::WordChar
    } else {
        gtk::WrapMode::None
    });
    file.gutter.queue_draw();
}

fn draw_gutter(
    _area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    width: i32,
    height: i32,
    file: &OpenFile,
) {
    // Paint the column background first: DrawingArea does not draw a theme
    // background on its own, and a transparent gutter would show scroll noise.
    cr.set_source_rgba(42.0 / 255.0, 41.0 / 255.0, 40.0 / 255.0, 1.0);
    cr.rectangle(0.0, 0.0, width as f64, height as f64);
    let _ = cr.fill();
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.06);
    cr.rectangle((width - 1) as f64, 0.0, 1.0, height as f64);
    let _ = cr.fill();

    let buffer = file.text.buffer();
    let offset = file
        .text
        .vadjustment()
        .map(|adjustment| adjustment.value())
        .unwrap_or(0.0);

    let mut kinds: HashMap<i32, DiffKind> = HashMap::new();
    {
        let marks = file.diff_marks.borrow();
        for mark in marks.iter() {
            let iter = buffer.iter_at_mark(&mark.mark);
            // A removed line wins over an added line at the same position.
            match kinds.get(&iter.line()) {
                Some(DiffKind::Removed) => {}
                _ => {
                    kinds.insert(iter.line(), mark.kind);
                }
            }
        }
    }

    cr.select_font_face(
        "JetBrains Mono",
        gtk::cairo::FontSlant::Normal,
        gtk::cairo::FontWeight::Normal,
    );
    cr.set_font_size(10.0);

    let mut iter = buffer.start_iter();
    loop {
        let (y, line_height) = file.text.line_yrange(&iter);
        let top = y as f64 - offset;
        if top + line_height as f64 >= 0.0 && top <= height as f64 {
            let line = iter.line();
            let number = (line + 1).to_string();
            let x = if let Ok(extents) = cr.text_extents(&number) {
                (width as f64 - 22.0 - extents.width()).max(4.0)
            } else {
                4.0
            };
            cr.set_source_rgba(155.0 / 255.0, 148.0 / 255.0, 140.0 / 255.0, 1.0);
            cr.move_to(x, top + (line_height as f64 + 8.0) / 2.0);
            let _ = cr.show_text(&number);

            if let Some(kind) = kinds.get(&line).copied() {
                match kind {
                    DiffKind::Added => {
                        cr.set_source_rgba(111.0 / 255.0, 191.0 / 255.0, 115.0 / 255.0, 1.0)
                    }
                    DiffKind::Removed => {
                        cr.set_source_rgba(229.0 / 255.0, 83.0 / 255.0, 75.0 / 255.0, 1.0)
                    }
                }
                cr.rectangle(0.0, top, 3.0, line_height as f64);
                let _ = cr.fill();

                let marker = match kind {
                    DiffKind::Added => "+",
                    DiffKind::Removed => "\u{2212}",
                };
                cr.move_to(6.0, top + (line_height as f64 + 8.0) / 2.0);
                let _ = cr.show_text(marker);
            }
        }
        if top > height as f64 {
            break;
        }
        if !iter.forward_line() {
            break;
        }
    }
}

impl EditorPane {
    pub fn active(&self) -> Option<Rc<OpenFile>> {
        let key = self.stack.visible_child_name()?.to_string();
        self.open
            .borrow()
            .iter()
            .find(|file| file.key == key)
            .cloned()
    }

    fn attach_tab(&self, file: &Rc<OpenFile>, index: usize) {
        let first = self.open.borrow().first().map(|first| first.tab.clone());
        if let Some(first) = first.filter(|first| *first != file.tab) {
            file.tab.set_group(Some(&first));
        }
        file.tab.set_active(true);
        self.tabs.append(&file.tab);
        let _ = index;
    }
}

pub fn open_file(state: &Rc<UiState>, path: &Path) {
    let _ = open_path(state, path, false);
}

/// Toggle soft wrapping for every open editor and persist the setting. Bound
/// to Alt+Z through `win.toggle-wrap`; also driven by the header button.
pub fn toggle_word_wrap(state: &Rc<UiState>) {
    let on = !state.config.borrow().editor.word_wrap;
    state.config.borrow_mut().editor.word_wrap = on;
    let _ = state.save_config();

    if let Some(widgets) = state.widgets() {
        let files: Vec<Rc<OpenFile>> = widgets.editor.open.borrow().iter().cloned().collect();
        for file in files {
            set_wrap_mode(&file, on);
            if file.wrap_button.is_active() != on {
                // This fires the button handler, which writes the same value;
                // the config is already updated so it is a cheap no-op.
                file.wrap_button.set_active(on);
            }
        }
    }
    state.toast(if on { "Word wrap on" } else { "Word wrap off" });
}

/// Open a file, optionally creating an empty page for a path that does not
/// exist yet (used when the agent starts writing a new file).
fn open_path(state: &Rc<UiState>, path: &Path, allow_missing: bool) -> Option<Rc<OpenFile>> {
    let widgets = state.widgets()?;
    let editor = widgets.editor.clone();
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    if let Some(existing) = editor
        .open
        .borrow()
        .iter()
        .find(|file| file.path == canonical)
        .cloned()
    {
        existing.tab.set_active(true);
        editor.stack.set_visible_child_name(&existing.key);
        existing.text.grab_focus();
        return Some(existing);
    }

    if canonical.is_dir() {
        return None;
    }
    let text = match std::fs::read_to_string(&canonical) {
        Ok(text) => text,
        Err(error) => {
            if allow_missing && !canonical.exists() {
                String::new()
            } else {
                state.toast(&format!(
                    "Could not read {}: {error}",
                    canonical.display()
                ));
                return None;
            }
        }
    };
    if text.len() > 8 * 1024 * 1024 {
        state.toast("File is larger than 8 MB; refusing to open it");
        return None;
    }

    let key = format!("file-{}", editor.next_key.get());
    editor.next_key.set(editor.next_key.get() + 1);

    let name = canonical
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| canonical.display().to_string());

    // ---- page -----------------------------------------------------------
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let page_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    page_header.set_margin_start(8);
    page_header.set_margin_end(6);
    page_header.set_margin_top(3);
    page_header.set_margin_bottom(3);
    page_header.add_css_class("lc-tabstrip");
    let relative = state.relative_to_workspace(&canonical);
    let path_label = gtk::Label::new(Some(&relative));
    path_label.add_css_class("lc-mono");
    path_label.add_css_class("lc-tiny");
    path_label.add_css_class("lc-dim");
    path_label.set_xalign(0.0);
    path_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    path_label.set_width_chars(20);
    page_header.append(&path_label);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    page_header.append(&spacer);
    let ai_spinner = gtk::Spinner::new();
    ai_spinner.add_css_class("lc-ai-spinner");
    ai_spinner.set_visible(false);
    page_header.append(&ai_spinner);
    let ai_badge = gtk::Label::new(Some("AI editing"));
    ai_badge.add_css_class("lc-ai-badge");
    ai_badge.set_visible(false);
    page_header.append(&ai_badge);
    let wrap_button = wrap_toggle(state);
    page_header.append(&wrap_button);
    let save = gtk::Button::with_label("Save");
    save.add_css_class("lc-ghost");
    save.add_css_class("lc-tiny");
    save.set_tooltip_text(Some("Save (Ctrl+S)"));
    {
        let state = state.clone();
        save.connect_clicked(move |_| save_current(&state));
    }
    page_header.append(&save);
    page.append(&page_header);

    let (diff_bar, diff_summary, accept_button, revert_button) = build_diff_bar();
    page.append(&diff_bar);

    let buffer = gtk::TextBuffer::new(None);
    buffer.set_text(&text);
    buffer.set_modified(false);
    let view = gtk::TextView::with_buffer(&buffer);
    view.set_monospace(true);
    view.set_left_margin(10);
    view.set_right_margin(12);
    view.set_top_margin(8);
    view.set_bottom_margin(8);
    view.set_wrap_mode(if state.config.borrow().editor.word_wrap {
        gtk::WrapMode::WordChar
    } else {
        gtk::WrapMode::None
    });
    view.set_tabs(&code_tabs(state.config.borrow().editor.tab_width));
    view.add_css_class("lc-code");
    view.set_vexpand(true);

    let (body, gutter) = build_code_area(&view);
    page.append(&body);

    let tab = gtk::ToggleButton::with_label(&name);
    tab.add_css_class("lc-tab");
    tab.set_tooltip_text(Some(&relative));

    let show_numbers = state.config.borrow().editor.show_line_numbers;
    gutter.set_visible(show_numbers);

    let file = Rc::new(OpenFile {
        key: key.clone(),
        path: canonical.clone(),
        text: view,
        page: page.clone(),
        tab,
        dirty: Cell::new(false),
        ai_badge,
        ai_spinner,
        ai_owned: Cell::new(false),
        gutter,
        wrap_button,
        diff_bar,
        diff_summary,
        accept_button,
        revert_button,
        previous: RefCell::new(None),
        diff_gen: Cell::new(0),
        state: Rc::downgrade(state),
        diff_marks: RefCell::new(Vec::new()),
        review: RefCell::new(None),
        pending_review: RefCell::new(None),
    });
    wire_editor(&file);

    // Dirty tracking.
    {
        let weak: Weak<OpenFile> = Rc::downgrade(&file);
        buffer.connect_changed(move |_| {
            if let Some(file) = weak.upgrade() {
                if file.ai_owned.get() {
                    return;
                }
                if !file.dirty.get() {
                    file.dirty.set(true);
                    let name = file
                        .path
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_else(|| "file".to_string());
                    file.tab.set_label(&format!("\u{25cf} {name}"));
                }
            }
        });
    }

    // Tab switch.
    {
        let editor = editor.clone();
        let key = key.clone();
        file.tab.connect_toggled(move |button| {
            if button.is_active() {
                editor.stack.set_visible_child_name(&key);
            }
        });
    }

    // Close button.
    let tab_child = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let tab_label = gtk::Label::new(Some(&name));
    tab_label.add_css_class("lc-tiny");
    tab_child.append(&tab_label);
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.add_css_class("lc-header-button");
    close.set_has_frame(false);
    tab_child.append(&close);
    {
        let weak: Weak<OpenFile> = Rc::downgrade(&file);
        let state = state.clone();
        close.connect_clicked(move |_| {
            if let Some(file) = weak.upgrade() {
                close_file(&state, &file);
            }
        });
    }
    file.tab.set_child(Some(&tab_child));

    editor.stack.add_named(&page, Some(&key));
    editor.open.borrow_mut().push(file.clone());
    editor.attach_tab(&file, editor.open.borrow().len());
    editor.stack.set_visible_child_name(&key);
    file.text.grab_focus();
    Some(file)
}

/// Create the two live-diff tags on a buffer. The colours match the spec:
/// green `rgba(46,160,67,.22)` for additions and red `rgba(248,81,73,.22)`
/// with a strike-through for removals.
fn ensure_diff_tags(buffer: &gtk::TextBuffer) {
    let table = buffer.tag_table();
    if table.lookup("ai-new").is_none() {
        if let Some(tag) = buffer.create_tag(Some("ai-new"), &[]) {
            tag.set_background(Some("rgba(46,160,67,0.22)"));
            tag.set_background_full_height(true);
            tag.set_paragraph_background(Some("rgba(46,160,67,0.10)"));
        }
    }
    if table.lookup("ai-old").is_none() {
        if let Some(tag) = buffer.create_tag(Some("ai-old"), &[]) {
            tag.set_background(Some("rgba(248,81,73,0.22)"));
            tag.set_background_full_height(true);
            tag.set_paragraph_background(Some("rgba(248,81,73,0.10)"));
            tag.set_strikethrough(true);
        }
    }
}

/// Keep a mark at the start of every line in `[start, end]` so the gutter can
/// paint the correct line even after later insertions move the text around.
fn add_line_marks(file: &OpenFile, kind: DiffKind, start: &gtk::TextIter, end: &gtk::TextIter) {
    let buffer = file.text.buffer();
    let first = start.line();
    let mut last = end.line();
    // A range that ends exactly at the start of a line does not include it.
    if last > first && end.line_offset() == 0 {
        last -= 1;
    }
    let mut marks = file.diff_marks.borrow_mut();
    let existing: Vec<(i32, DiffKind)> = marks
        .iter()
        .map(|mark| (buffer.iter_at_mark(&mark.mark).line(), mark.kind))
        .collect();
    for line in first..=last {
        if existing.contains(&(line, kind)) {
            continue;
        }
        if let Some(iter) = buffer.iter_at_line(line) {
            marks.push(DiffMark {
                mark: buffer.create_mark(None, &iter, true),
                kind,
            });
        }
    }
    drop(marks);
    file.gutter.queue_draw();
}

fn clear_marks_of_kind(file: &OpenFile, kind: DiffKind) {
    file.diff_marks.borrow_mut().retain(|mark| mark.kind != kind);
    file.gutter.queue_draw();
}

fn clear_diff_marks(file: &OpenFile) {
    file.diff_marks.borrow_mut().clear();
    file.gutter.queue_draw();
}

fn clear_diff_tags(file: &OpenFile) {
    let buffer = file.text.buffer();
    let start = buffer.start_iter();
    let end = buffer.end_iter();
    for tag in ["ai-old", "ai-new"] {
        buffer.remove_tag_by_name(tag, &start, &end);
    }
}

fn find_text(buffer: &gtk::TextBuffer, needle: &str) -> Option<(gtk::TextIter, gtk::TextIter)> {
    if needle.is_empty() {
        return None;
    }
    let start = buffer.start_iter();
    start.forward_search(
        needle,
        gtk::TextSearchFlags::empty(),
        None::<&gtk::TextIter>,
    )
}

fn scroll_to_iter(file: &OpenFile, iter: &gtk::TextIter) {
    let mut iter = *iter;
    file.text.scroll_to_iter(&mut iter, 0.0, false, 0.0, 0.35);
}

/// Highlight a substring red. The tag is applied to every occurrence currently
/// visible in the buffer; `add_line_marks` records the gutter bars.
fn highlight_old(file: &OpenFile, start: &gtk::TextIter, end: &gtk::TextIter) {
    let buffer = file.text.buffer();
    ensure_diff_tags(&buffer);
    buffer.apply_tag_by_name("ai-old", start, end);
    clear_marks_of_kind(file, DiffKind::Removed);
    add_line_marks(file, DiffKind::Removed, start, end);
}

/// Normalise a page that is still showing the inline review before a new AI
/// edit starts. The buffer is put back to the real file content so `previous`
/// captures the right baseline.
fn normalise_review(file: &OpenFile) {
    let phase = file.review.borrow().as_ref().map(|review| review.phase);
    if phase == Some(ReviewPhase::Diff) {
        let new = file
            .review
            .borrow()
            .as_ref()
            .map(|review| review.new.clone())
            .unwrap_or_default();
        file.ai_owned.set(true);
        let buffer = file.text.buffer();
        buffer.set_text(&new);
        buffer.set_modified(false);
        file.ai_owned.set(false);
    }
    clear_review(file);
}

/// Drop the toolbar, tags and review record, leaving the buffer untouched.
fn clear_review(file: &OpenFile) {
    file.diff_gen.set(file.diff_gen.get() + 1);
    file.review.borrow_mut().take();
    file.pending_review.borrow_mut().take();
    clear_diff_tags(file);
    clear_diff_marks(file);
    file.diff_bar.set_visible(false);
    file.revert_button.set_visible(true);
    file.ai_badge.set_visible(false);
    file.ai_badge.set_text("AI editing");
    file.ai_owned.set(false);
}

/// Show a diff that arrived for a streamed edit. The previous content is kept
/// in memory until the user clicks Accept or Revert.
pub fn show_diff_review(
    state: &Rc<UiState>,
    path: &Path,
    diff: &link_core::diff::FileDiff,
) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    let absolute = state
        .workspace_path()
        .map(|root| root.join(path))
        .unwrap_or_else(|| path.to_path_buf());
    let canonical = absolute.canonicalize().unwrap_or(absolute);
    let Some(file) = widgets
        .editor
        .open
        .borrow()
        .iter()
        .find(|file| file.path == canonical)
        .cloned()
    else {
        reload_with_diff(state, path, diff);
        return;
    };
    let Some(old) = file.previous.borrow().clone() else {
        reload_with_diff(state, path, diff);
        return;
    };
    let new = std::fs::read_to_string(&canonical).unwrap_or_default();

    // The typing animation may still be draining; render the review when the
    // last character lands so the diff is not wiped mid-stream.
    if widgets.editor.animating.get() > 0 {
        *file.pending_review.borrow_mut() =
            Some((old, new, diff.added, diff.removed));
        return;
    }
    apply_review(&file, old, new, diff.added, diff.removed);
}

fn apply_review(file: &Rc<OpenFile>, old: String, new: String, added: usize, removed: usize) {
    let generation = file.diff_gen.get() + 1;
    file.diff_gen.set(generation);
    *file.review.borrow_mut() = Some(ReviewState {
        old,
        new,
        phase: ReviewPhase::Diff,
    });
    file.previous.borrow_mut().take();

    file.ai_owned.set(true);
    file.text.set_editable(false);
    file.ai_badge.set_text("Review changes");
    file.ai_badge.set_visible(true);
    file.ai_spinner.stop();
    file.ai_spinner.set_visible(false);

    paint_review(file);
    file.diff_summary
        .set_text(&format!("+{added} \u{2212}{removed}"));
    file.revert_button.set_visible(true);
    file.diff_bar.set_visible(true);

    // Keep the colours for three seconds, then settle onto the real new file
    // while leaving the Accept/Revert toolbar in place.
    let weak = Rc::downgrade(file);
    glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
        let Some(file) = weak.upgrade() else {
            return;
        };
        if file.diff_gen.get() != generation {
            return;
        }
        if file.review.borrow().as_ref().map(|review| review.phase) == Some(ReviewPhase::Diff) {
            settle_review(&file);
        }
    });
}

/// Render the full old/new line diff into the page buffer. Every removed line
/// is followed by the added replacement, so the change reads top-to-bottom.
fn paint_review(file: &OpenFile) {
    let Some((old, new)) = file
        .review
        .borrow()
        .as_ref()
        .map(|review| (review.old.clone(), review.new.clone()))
    else {
        return;
    };
    let lines = link_core::diff::inline_diff_lines(&old, &new);

    let mut text = String::new();
    for line in &lines {
        text.push_str(&line.text);
        text.push('\n');
    }

    let buffer = file.text.buffer();
    buffer.set_text(&text);
    buffer.set_modified(false);
    clear_diff_marks(file);
    ensure_diff_tags(&buffer);

    for (line_number, line) in lines.iter().enumerate() {
        let kind = match line.kind {
            link_core::diff::DiffLineKind::Addition => Some((DiffKind::Added, "ai-new")),
            link_core::diff::DiffLineKind::Removal => Some((DiffKind::Removed, "ai-old")),
            link_core::diff::DiffLineKind::Context => None,
        };
        if let Some((mark_kind, tag)) = kind {
            if let (Some(start), Some(end)) = (
                buffer.iter_at_line(line_number as i32),
                buffer.iter_at_line(line_number as i32 + 1),
            ) {
                buffer.apply_tag_by_name(tag, &start, &end);
                add_line_marks(file, mark_kind, &start, &end);
            }
        }
    }
    file.gutter.set_size_request(gutter_width(&buffer), -1);
    file.gutter.queue_draw();
}

/// After the colour window, replace the inline diff with the exact file on
/// disk. The review record stays alive so Revert still works.
fn settle_review(file: &OpenFile) {
    let new = {
        let mut review = file.review.borrow_mut();
        let Some(state) = review.as_mut() else {
            return;
        };
        state.phase = ReviewPhase::Clean;
        state.new.clone()
    };
    let buffer = file.text.buffer();
    file.ai_owned.set(true);
    buffer.set_text(&new);
    buffer.set_modified(false);
    file.dirty.set(false);
    file.ai_owned.set(false);
    file.text.set_editable(true);
    clear_diff_tags(file);
    clear_diff_marks(file);
    file.ai_badge.set_text("Review changes");
    file.ai_badge.set_visible(true);
    let name = file
        .path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    file.tab.set_label(&name);
    file.gutter.queue_draw();
}

fn accept_review(file: &Rc<OpenFile>, toast: bool) {
    let phase = file.review.borrow().as_ref().map(|review| review.phase);
    let new = file
        .review
        .borrow()
        .as_ref()
        .map(|review| review.new.clone());
    let had_review = phase.is_some();
    clear_review(file);
    file.text.set_editable(true);

    // In the inline-diff phase the buffer is not the real file yet, so accept
    // by loading the bytes the tool wrote. In the clean phase the buffer is
    // already the file and any user tweaks are left alone.
    if phase == Some(ReviewPhase::Diff) {
        let buffer = file.text.buffer();
        file.ai_owned.set(true);
        buffer.set_text(new.as_deref().unwrap_or_default());
        buffer.set_modified(false);
        file.ai_owned.set(false);
        file.dirty.set(false);
        let name = file
            .path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        file.tab.set_label(&name);
    }
    file.text.grab_focus();
    if toast && had_review {
        if let Some(state) = file.state.upgrade() {
            state.toast("Kept the AI changes");
        }
    }
}

fn revert_review(file: &Rc<OpenFile>) {
    let old = file
        .review
        .borrow()
        .as_ref()
        .map(|review| review.old.clone())
        .or_else(|| file.previous.borrow_mut().take());
    let Some(old) = old else {
        return;
    };

    clear_review(file);
    let buffer = file.text.buffer();
    file.ai_owned.set(true);
    buffer.set_text(&old);
    buffer.set_modified(false);
    file.ai_owned.set(false);
    file.dirty.set(false);
    file.text.set_editable(true);

    // Restore the file on disk too; otherwise the next reload would make the
    // reverted content vanish again.
    if !file.path.starts_with("__ai_preview__") {
        if let Err(error) = std::fs::write(&file.path, &old) {
            if let Some(state) = file.state.upgrade() {
                state.toast(&format!("Could not revert {}: {error}", file.path.display()));
            }
        }
    }

    let name = file
        .path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    file.tab.set_label(&name);
    file.gutter.queue_draw();
    if let Some(state) = file.state.upgrade() {
        state.toast(&format!("Reverted the AI edit to {name}"));
        if let Some(widgets) = state.widgets() {
            crate::panels::sidebar::refresh_files_deferred(&state, &widgets.sidebar);
        }
    }
}

/// Open/focus the target file, lock it and show the AI badge before the first
/// argument fragment arrives.
pub fn begin_ai_edit(
    state: &Rc<UiState>,
    call_id: &str,
    tool: &str,
    path: &str,
    _new_file: bool,
) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    let absolute = state
        .workspace_path()
        .map(|root| root.join(path))
        .unwrap_or_else(|| PathBuf::from(path));
    let Some(file) = open_path(state, &absolute, true) else {
        return;
    };
    let whole_file = matches!(tool, "write_file" | "create_file");
    let buffer = file.text.buffer();

    // If the page is still showing an inline review, put the real file back
    // first so the new edit starts from the actual content.
    if file.review.borrow().is_some() {
        normalise_review(&file);
    }
    clear_review(&file);
    let original = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
    *file.previous.borrow_mut() = Some(original.to_string());

    file.ai_owned.set(true);
    file.text.set_editable(false);
    file.ai_badge.set_text("AI editing");
    file.ai_badge.set_visible(true);
    file.ai_spinner.set_visible(true);
    file.ai_spinner.start();
    ensure_diff_tags(&buffer);

    // A whole-file write replaces the old contents; clear the buffer so the new
    // text is typed into a blank page rather than appended to the old file.
    if whole_file {
        buffer.set_text("");
    }
    let mark = buffer.create_mark(None, &buffer.start_iter(), true);

    widgets.editor.streams.borrow_mut().insert(
        call_id.to_string(),
        AiStream {
            file: file.clone(),
            old_text: String::new(),
            old_start: None,
            old_end: None,
            replacement: if whole_file { Some(mark) } else { None },
            skip_typing: false,
            kept_old: false,
            anim: Rc::new(AnimState::new()),
        },
    );
    file.tab.set_active(true);
    widgets.editor.stack.set_visible_child_name(&file.key);
}

/// Append one decoded argument fragment to the live editor buffer.
///
/// Small fragments type in immediately. A large fragment almost always means
/// the provider sent the whole tool call at once; it is queued and typed in
/// over a second or two so the editor still looks live.
pub fn push_ai_delta(
    state: &Rc<UiState>,
    call_id: &str,
    field: link_core::event::EditorStreamField,
    text: &str,
) {
    use link_core::event::EditorStreamField;
    let Some(widgets) = state.widgets() else {
        return;
    };
    if text.is_empty() {
        return;
    }
    let mut streams = widgets.editor.streams.borrow_mut();
    let Some(stream) = streams.get_mut(call_id) else {
        return;
    };
    let file = stream.file.clone();
    let buffer = file.text.buffer();

    match field {
        EditorStreamField::Content => {
            if stream.anim.draining.get() {
                stream.anim.queue.borrow_mut().push_str(text);
            } else if text.chars().count() > LARGE_DELTA {
                stream.anim.queue.borrow_mut().push_str(text);
                start_drain(&widgets, &file, &stream.anim, None);
            } else {
                append_content(&file, text);
            }
        }
        EditorStreamField::OldString => {
            stream.old_text.push_str(text);
            ensure_diff_tags(&buffer);
            let all_start = buffer.start_iter();
            let all_end = buffer.end_iter();
            buffer.remove_tag_by_name("ai-old", &all_start, &all_end);
            if let Some((start, end)) = find_text(&buffer, &stream.old_text) {
                highlight_old(&file, &start, &end);
                stream.old_start = Some(buffer.create_mark(None, &start, true));
                stream.old_end = Some(buffer.create_mark(None, &end, false));
                scroll_to_iter(&file, &start);
            }
        }
        EditorStreamField::NewString => {
            if stream.skip_typing {
                return;
            }
            if stream.replacement.is_none() {
                // Keep the old text visible (red and struck through) and start
                // typing the replacement on a fresh line directly below it.
                let mut at = match &stream.old_end {
                    Some(mark) => buffer.iter_at_mark(mark),
                    None => buffer.end_iter(),
                };
                if !at.starts_line() {
                    buffer.insert(&mut at, "\n");
                }
                let mark = buffer.create_mark(None, &at, false);
                stream.replacement = Some(mark);
                stream.kept_old = stream.old_end.is_some();
            }
            let Some(mark) = stream.replacement.clone() else {
                return;
            };
            if stream.anim.draining.get() {
                stream.anim.queue.borrow_mut().push_str(text);
            } else if text.chars().count() > LARGE_DELTA {
                stream.anim.queue.borrow_mut().push_str(text);
                start_drain(&widgets, &file, &stream.anim, Some(mark));
            } else {
                insert_at_mark(&file, &mark, text);
            }
        }
    }
}

/// Deltas above this many characters are treated as a whole-tool-call burst
/// and animated instead of pasted.
const LARGE_DELTA: usize = 200;

/// Queue `text` and type it in over roughly 1-2 seconds.
fn start_drain(
    widgets: &Rc<crate::state::Widgets>,
    file: &Rc<OpenFile>,
    anim: &Rc<AnimState>,
    mark: Option<gtk::TextMark>,
) {
    if anim.draining.replace(true) {
        return;
    }
    let total = anim.queue.borrow().chars().count();
    if total == 0 {
        anim.draining.set(false);
        return;
    }
    // ~50 ticks at 30 ms is about 1.5 s for a normal file, with a floor so
    // short bursts do not crawl.
    let per_tick = (total / 50).max(12);
    widgets.editor.animating.set(widgets.editor.animating.get() + 1);

    let weak_widgets = Rc::downgrade(widgets);
    let file = file.clone();
    let anim = anim.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(30), move || {
        let chunk = take_chars(&anim.queue, per_tick);
        if chunk.is_empty() {
            anim.draining.set(false);
            if anim.ended.replace(false) {
                finalise_edit(&file);
            }
            if let Some(widgets) = weak_widgets.upgrade() {
                finish_animation(&widgets);
            }
            return glib::ControlFlow::Break;
        }
        match &mark {
            Some(mark) => insert_at_mark(&file, mark, &chunk),
            None => append_content(&file, &chunk),
        }
        glib::ControlFlow::Continue
    });
}

/// Remove and return up to `count` characters from the front of the queue.
fn take_chars(queue: &RefCell<String>, count: usize) -> String {
    let mut queue = queue.borrow_mut();
    let split = queue
        .char_indices()
        .nth(count)
        .map(|(index, _)| index)
        .unwrap_or(queue.len());
    let taken = queue[..split].to_string();
    queue.drain(..split);
    taken
}

fn append_content(file: &OpenFile, text: &str) {
    let buffer = file.text.buffer();
    ensure_diff_tags(&buffer);
    let mut start = buffer.end_iter();
    buffer.insert(&mut start, text);
    let mut end = start;
    end.forward_chars(text.chars().count() as i32);
    if end.line_offset() == 0 && end.line() > start.line() {
        // Keep the trailing newline itself out of the marker range.
        let _ = end.backward_char();
    }
    buffer.apply_tag_by_name("ai-new", &start, &end);
    add_line_marks(file, DiffKind::Added, &start, &end);
    let end = buffer.end_iter();
    buffer.place_cursor(&end);
    scroll_to_iter(file, &end);
}

fn insert_at_mark(file: &OpenFile, mark: &gtk::TextMark, text: &str) {
    let buffer = file.text.buffer();
    ensure_diff_tags(&buffer);
    let start = buffer.iter_at_mark(mark);
    let mut end = start;
    end.forward_chars(text.chars().count() as i32);
    let mut insert_at = start;
    buffer.insert(&mut insert_at, text);
    buffer.apply_tag_by_name("ai-new", &start, &end);
    add_line_marks(file, DiffKind::Added, &start, &end);
    scroll_to_iter(file, &start);
}

/// Called by the drain timer once the last character is in the buffer.
fn finish_animation(widgets: &Rc<crate::state::Widgets>) {
    let remaining = widgets.editor.animating.get().saturating_sub(1);
    widgets.editor.animating.set(remaining);
    if remaining > 0 {
        return;
    }
    let deferred: Vec<PathBuf> = widgets.editor.deferred_reloads.borrow_mut().drain(..).collect();
    for path in deferred {
        reload_file(widgets, &path, true);
    }
    // A diff can arrive before the typing animation drains. Render it now that
    // the last character is in the buffer.
    let files: Vec<Rc<OpenFile>> = widgets.editor.open.borrow().iter().cloned().collect();
    for file in files {
        let pending = file.pending_review.borrow_mut().take();
        if let Some((old, new, added, removed)) = pending {
            apply_review(&file, old, new, added, removed);
        }
    }
}

/// Unlock the buffer when the argument stream ends, leaving it marked modified
/// until the tool result reconciles it with disk.
pub fn end_ai_edit(state: &Rc<UiState>, call_id: &str) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    let Some(stream) = widgets.editor.streams.borrow_mut().remove(call_id) else {
        return;
    };
    if stream.anim.draining.get() {
        // Keep the buffer locked and let the animation finish first; the drain
        // timer finalises it when the queue empties.
        stream.anim.ended.set(true);
        return;
    }
    finalise_edit(&stream.file);
}

fn finalise_edit(file: &Rc<OpenFile>) {
    file.text.set_editable(true);
    file.ai_badge.set_visible(false);
    file.ai_spinner.stop();
    file.ai_spinner.set_visible(false);
    file.ai_owned.set(false);

    let buffer = file.text.buffer();
    file.dirty.set(true);
    buffer.set_modified(true);
    let name = file
        .path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    file.tab.set_label(&format!("\u{25cf} {name}"));

    // If the tool result has already rendered a review, its own 3 s timer
    // owns the colours. Otherwise clear the live highlights after a grace
    // period. `diff_gen` makes a review that arrives meanwhile win.
    let generation = file.diff_gen.get();
    let weak = Rc::downgrade(file);
    glib::timeout_add_local_once(std::time::Duration::from_secs(4), move || {
        let Some(file) = weak.upgrade() else {
            return;
        };
        if file.diff_gen.get() != generation || file.review.borrow().is_some() {
            return;
        }
        clear_diff_tags(&file);
        clear_diff_marks(&file);
    });
}

/// Focus the tab for `path`, opening it if needed (used by the chat chip).
pub fn focus_file(state: &Rc<UiState>, path: &Path) {
    let absolute = state
        .workspace_path()
        .map(|root| root.join(path))
        .unwrap_or_else(|| path.to_path_buf());
    let _ = open_path(state, &absolute, true);
}

fn close_file(state: &Rc<UiState>, file: &OpenFile) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    let editor = widgets.editor.clone();
    let mut open = editor.open.borrow_mut();
    let Some(position) = open.iter().position(|entry| entry.key == file.key) else {
        return;
    };
    let removed = open.remove(position);
    editor.stack.remove(&removed.page);
    editor.tabs.remove(&removed.tab);
    match open.first() {
        Some(first) => {
            first.tab.set_active(true);
            editor.stack.set_visible_child_name(&first.key);
        }
        None => editor.stack.set_visible_child_name("welcome"),
    }
}

pub fn save_current(state: &Rc<UiState>) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    let Some(file) = widgets.editor.active() else {
        state.toast("No file is open");
        return;
    };
    if !file.text.is_editable() {
        state.toast("The AI is editing this file; wait for it to finish");
        return;
    }
    let buffer = file.text.buffer();
    let text = buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string();
    match std::fs::write(&file.path, text) {
        Ok(()) => {
            file.dirty.set(false);
            buffer.set_modified(false);
            let name = file
                .path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            file.tab.set_label(&name);
            state.toast(&format!("Saved {name}"));
        }
        Err(error) => state.toast(&format!("Could not save: {error}")),
    }
}

/// Reload an open editor page after the agent changed the file on disk.
pub fn reload(state: &Rc<UiState>, path: &Path) {
    reload_inner(state, path, false);
}

/// Reload even when the buffer is dirty. Used when the agent has just written
/// the file, so the buffer's live-streamed content is replaced by the exact
/// bytes now on disk.
pub fn reload_force(state: &Rc<UiState>, path: &Path) {
    reload_inner(state, path, true);
}

fn reload_inner(state: &Rc<UiState>, path: &Path, force: bool) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    let absolute = state
        .workspace_path()
        .map(|root| root.join(path))
        .unwrap_or_else(|| path.to_path_buf());
    let canonical = absolute.canonicalize().unwrap_or(absolute);
    // A reload would wipe the buffer mid-animation. Remember it and replay it
    // once the last character has been typed.
    if force && widgets.editor.animating.get() > 0 {
        widgets.editor.deferred_reloads.borrow_mut().push(canonical);
        return;
    }
    reload_file(&widgets, &canonical, force);
}

fn reload_file(widgets: &Rc<crate::state::Widgets>, canonical: &Path, force: bool) {
    let Some(file) = widgets
        .editor
        .open
        .borrow()
        .iter()
        .find(|file| file.path == canonical)
        .cloned()
    else {
        return;
    };
    if file.dirty.get() && !force {
        return;
    }
    if file.review.borrow().is_some() || file.previous.borrow().is_some() {
        clear_review(&file);
        file.previous.borrow_mut().take();
    }
    if let Ok(text) = std::fs::read_to_string(canonical) {
        let buffer = file.text.buffer();
        file.ai_owned.set(true);
        buffer.set_text(&text);
        buffer.set_modified(false);
        file.ai_owned.set(false);
        // `set_text` fires `changed`, which marks the tab dirty; the buffer now
        // matches disk, so clear the marker again.
        file.dirty.set(false);
        let name = file
            .path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        file.tab.set_label(&name);
    } else if force && !canonical.exists() {
        // The file was never written (an interrupted `create_file` or
        // `write_file`). Drop the half-streamed buffer so the editor does not
        // show a phantom file.
        let buffer = file.text.buffer();
        file.ai_owned.set(true);
        buffer.set_text("");
        buffer.set_modified(false);
        file.ai_owned.set(false);
        file.dirty.set(false);
        let name = file
            .path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        file.tab.set_label(&name);
    }
}

pub fn close_all(state: &Rc<UiState>) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    let editor = widgets.editor.clone();
    *editor.open.borrow_mut() = Vec::new();
    while let Some(child) = editor.tabs.first_child() {
        editor.tabs.remove(&child);
    }
    let mut children = Vec::new();
    let mut child = editor.stack.first_child();
    while let Some(current) = child {
        children.push(current.clone());
        child = current.next_sibling();
    }
    for child in children {
        let name = editor.stack.page(&child).name();
        if name.as_deref() != Some("welcome") {
            editor.stack.remove(&child);
        }
    }
    editor.stack.set_visible_child_name("welcome");
}

/// Open (or update) an in-memory "AI Code Preview" tab showing code that the
/// agent has written in the chat.  The tab is identified by `tab_key` so that
/// repeated calls for the same block update the same tab instead of opening a
/// new one on every streaming chunk.
///
/// The tab is marked with a synthetic path under `__ai_preview__/` so it is
/// never confused with a real workspace file.  Saving from this tab is a no-op.
pub fn open_code_preview(state: &Rc<UiState>, tab_key: &str, label: &str, code: &str) {
    let Some(widgets) = state.widgets() else { return; };
    let editor = widgets.editor.clone();
    let sentinel_key = format!("ai-preview-{tab_key}");

    // If the tab already exists, just update its buffer.
    if let Some(existing) = editor.open.borrow().iter().find(|f| f.key == sentinel_key).cloned() {
        let buffer = existing.text.buffer();
        buffer.set_text(code);
        buffer.set_modified(false);
        existing.dirty.set(false);
        existing.tab.set_active(true);
        editor.stack.set_visible_child_name(&sentinel_key);
        return;
    }

    // Build a fake path so the tab has a name and tooltip.
    let fake_path = PathBuf::from(format!("__ai_preview__/{label}"));

    // ---- page ---------------------------------------------------------------
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let page_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    page_header.set_margin_start(8);
    page_header.set_margin_end(6);
    page_header.set_margin_top(3);
    page_header.set_margin_bottom(3);
    page_header.add_css_class("lc-tabstrip");
    let path_label = gtk::Label::new(Some(&format!("✦ AI — {label}")));
    path_label.add_css_class("lc-mono");
    path_label.add_css_class("lc-tiny");
    path_label.add_css_class("lc-dim");
    path_label.set_xalign(0.0);
    path_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    path_label.set_width_chars(20);
    path_label.set_hexpand(true);
    page_header.append(&path_label);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    page_header.append(&spacer);
    let wrap_button = wrap_toggle(state);
    page_header.append(&wrap_button);
    page.append(&page_header);

    let (diff_bar, diff_summary, accept_button, revert_button) = build_diff_bar();
    page.append(&diff_bar);

    let buffer = gtk::TextBuffer::new(None);
    buffer.set_text(code);
    buffer.set_modified(false);
    let view = gtk::TextView::with_buffer(&buffer);
    view.set_monospace(true);
    view.set_left_margin(10);
    view.set_right_margin(12);
    view.set_top_margin(8);
    view.set_bottom_margin(8);
    view.set_wrap_mode(if state.config.borrow().editor.word_wrap {
        gtk::WrapMode::WordChar
    } else {
        gtk::WrapMode::None
    });
    view.set_tabs(&code_tabs(state.config.borrow().editor.tab_width));
    view.add_css_class("lc-code");
    view.set_vexpand(true);
    // This is a preview — make it editable so the user can tweak it.
    view.set_editable(true);

    let (body, gutter) = build_code_area(&view);
    page.append(&body);

    let tab = gtk::ToggleButton::with_label(label);
    tab.add_css_class("lc-tab");
    tab.set_tooltip_text(Some(&format!("AI code preview — {label}")));

    let file = Rc::new(OpenFile {
        key: sentinel_key.clone(),
        path: fake_path,
        text: view,
        page: page.clone(),
        tab,
        dirty: Cell::new(false),
        ai_badge: gtk::Label::new(None),
        ai_spinner: gtk::Spinner::new(),
        ai_owned: Cell::new(false),
        gutter,
        wrap_button,
        diff_bar,
        diff_summary,
        accept_button,
        revert_button,
        previous: RefCell::new(None),
        diff_gen: Cell::new(0),
        state: Rc::downgrade(state),
        diff_marks: RefCell::new(Vec::new()),
        review: RefCell::new(None),
        pending_review: RefCell::new(None),
    });
    wire_editor(&file);

    // Tab switch.
    {
        let editor2 = editor.clone();
        let key2 = sentinel_key.clone();
        file.tab.connect_toggled(move |button| {
            if button.is_active() {
                editor2.stack.set_visible_child_name(&key2);
            }
        });
    }

    // Close button.
    let tab_child = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let tab_lbl = gtk::Label::new(Some(label));
    tab_lbl.add_css_class("lc-tiny");
    tab_child.append(&tab_lbl);
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.add_css_class("lc-header-button");
    close.set_has_frame(false);
    tab_child.append(&close);
    {
        let weak: Weak<OpenFile> = Rc::downgrade(&file);
        let state2 = state.clone();
        close.connect_clicked(move |_| {
            if let Some(f) = weak.upgrade() {
                close_file(&state2, &f);
            }
        });
    }
    file.tab.set_child(Some(&tab_child));

    editor.stack.add_named(&page, Some(&sentinel_key));
    editor.open.borrow_mut().push(file.clone());
    editor.attach_tab(&file, editor.open.borrow().len());
    editor.stack.set_visible_child_name(&sentinel_key);
}

/// Reload a file and show the diff as an inline review.
///
/// This is the fallback for diffs that arrive without a streamed buffer (for
/// example after a restart). The new file is loaded from disk and the removed
/// lines from the diff are injected in red above their replacement, so the
/// user still sees what changed. There is no previous content to restore, so
/// only Accept is offered.
pub fn reload_with_diff(
    state: &Rc<UiState>,
    path: &Path,
    diff: &link_core::diff::FileDiff,
) {
    reload_force(state, path);

    let Some(widgets) = state.widgets() else { return; };
    let absolute = state
        .workspace_path()
        .map(|root| root.join(path))
        .unwrap_or_else(|| path.to_path_buf());
    let canonical = absolute.canonicalize().unwrap_or(absolute);
    let Some(file) = widgets
        .editor
        .open
        .borrow()
        .iter()
        .find(|f| f.path == canonical)
        .cloned()
    else {
        return;
    };

    let new = match std::fs::read_to_string(&canonical) {
        Ok(text) => text,
        Err(_) => {
            let buffer = file.text.buffer();
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), true).to_string()
        }
    };
    let lines = inject_removals(&new, diff);

    let generation = file.diff_gen.get() + 1;
    file.diff_gen.set(generation);
    *file.review.borrow_mut() = Some(ReviewState {
        old: new.clone(),
        new,
        phase: ReviewPhase::Diff,
    });
    file.previous.borrow_mut().take();

    let mut text = String::new();
    for (_, line) in &lines {
        text.push_str(line);
        text.push('\n');
    }

    let buffer = file.text.buffer();
    file.ai_owned.set(true);
    buffer.set_text(&text);
    buffer.set_modified(false);
    clear_diff_marks(&file);
    ensure_diff_tags(&buffer);

    for (index, (kind, _)) in lines.iter().enumerate() {
        let (mark_kind, tag) = match kind {
            link_core::diff::DiffLineKind::Addition => (DiffKind::Added, "ai-new"),
            link_core::diff::DiffLineKind::Removal => (DiffKind::Removed, "ai-old"),
            link_core::diff::DiffLineKind::Context => continue,
        };
        if let (Some(start), Some(end)) = (
            buffer.iter_at_line(index as i32),
            buffer.iter_at_line(index as i32 + 1),
        ) {
            buffer.apply_tag_by_name(tag, &start, &end);
            add_line_marks(&file, mark_kind, &start, &end);
        }
    }
    file.ai_owned.set(false);
    file.dirty.set(false);
    file.gutter.set_size_request(gutter_width(&buffer), -1);
    file.gutter.queue_draw();

    file.text.set_editable(false);
    file.ai_badge.set_text("Review changes");
    file.ai_badge.set_visible(true);
    file.ai_spinner.stop();
    file.ai_spinner.set_visible(false);
    file.diff_summary.set_text(&diff.summary());
    file.diff_bar.set_visible(true);
    // No previous content is available in this path, so there is nothing to
    // restore; hide Revert rather than letting it blank the file.
    file.revert_button.set_visible(false);

    let weak = Rc::downgrade(&file);
    glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
        let Some(file) = weak.upgrade() else {
            return;
        };
        if file.diff_gen.get() != generation {
            return;
        }
        if file.review.borrow().as_ref().map(|review| review.phase) == Some(ReviewPhase::Diff) {
            settle_review(&file);
        }
        file.revert_button.set_visible(false);
    });
}

/// Build a full inline-review line stream from the new file plus the removed
/// lines recorded in `diff`. Context and additions come from the file on disk;
/// removals are re-inserted directly above their replacement.
fn inject_removals(
    new: &str,
    diff: &link_core::diff::FileDiff,
) -> Vec<(link_core::diff::DiffLineKind, String)> {
    use link_core::diff::DiffLineKind;

    let mut new_lines: Vec<&str> = new.split('\n').collect();
    if new.ends_with('\n') || new.is_empty() {
        // `split` leaves a trailing empty entry for a final newline.
        new_lines.pop();
    }

    let mut out: Vec<(DiffLineKind, String)> = Vec::new();
    let mut cursor = 0usize;
    for hunk in &diff.hunks {
        let start = (hunk.new_start as usize).saturating_sub(1);
        while cursor < start && cursor < new_lines.len() {
            out.push((DiffLineKind::Context, new_lines[cursor].to_string()));
            cursor += 1;
        }
        for line in &hunk.lines {
            match line.kind {
                DiffLineKind::Context | DiffLineKind::Addition => {
                    if cursor < new_lines.len() {
                        out.push((line.kind, new_lines[cursor].to_string()));
                    } else {
                        out.push((line.kind, line.text.clone()));
                    }
                    cursor += 1;
                }
                DiffLineKind::Removal => out.push((DiffLineKind::Removal, line.text.clone())),
            }
        }
    }
    while cursor < new_lines.len() {
        out.push((DiffLineKind::Context, new_lines[cursor].to_string()));
        cursor += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_chars_never_splits_a_code_point() {
        let queue = RefCell::new("héllo wörld".to_string());
        assert_eq!(take_chars(&queue, 2), "hé");
        assert_eq!(take_chars(&queue, 100), "llo wörld");
        assert!(take_chars(&queue, 5).is_empty());
    }

    #[test]
    fn inject_removals_rebuilds_the_inline_review() {
        use link_core::diff::DiffLineKind;

        let old = "alpha\nbeta\ngamma\n";
        let new = "alpha\ndelta\ngamma\nepsilon\n";
        let diff = link_core::diff::FileDiff::compute("x", old, new);
        let lines = inject_removals(new, &diff);
        let rendered: Vec<(DiffLineKind, &str)> = lines
            .iter()
            .map(|(kind, text)| (*kind, text.as_str()))
            .collect();
        assert_eq!(
            rendered,
            vec![
                (DiffLineKind::Context, "alpha"),
                (DiffLineKind::Removal, "beta"),
                (DiffLineKind::Addition, "delta"),
                (DiffLineKind::Context, "gamma"),
                (DiffLineKind::Addition, "epsilon"),
            ]
        );
    }
}
