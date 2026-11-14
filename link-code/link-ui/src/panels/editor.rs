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
}

/// State for one streamed file edit, keyed by tool call id.
pub struct AiStream {
    file: Rc<OpenFile>,
    old_text: String,
    replacement: Option<gtk::TextMark>,
    skip_typing: bool,
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

    let buffer = gtk::TextBuffer::new(None);
    buffer.set_text(&text);
    buffer.set_modified(false);
    let view = gtk::TextView::with_buffer(&buffer);
    view.set_monospace(true);
    view.set_left_margin(12);
    view.set_right_margin(12);
    view.set_top_margin(8);
    view.set_bottom_margin(8);
    view.set_wrap_mode(if state.config.borrow().editor.word_wrap {
        gtk::WrapMode::WordChar
    } else {
        gtk::WrapMode::None
    });
    view.add_css_class("lc-code");
    view.set_vexpand(true);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .child(&view)
        .build();
    page.append(&scroll);

    let tab = gtk::ToggleButton::with_label(&name);
    tab.add_css_class("lc-tab");
    tab.set_tooltip_text(Some(&relative));

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
    });

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

/// Make sure a named highlight tag exists on the buffer.
fn ensure_tag(buffer: &gtk::TextBuffer, name: &str, background: &str) {
    if buffer.tag_table().lookup(name).is_none() {
        if let Some(tag) = buffer.create_tag(Some(name), &[]) {
            tag.set_background(Some(background));
        }
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
    let mut iter = iter.clone();
    file.text.scroll_to_iter(&mut iter, 0.0, false, 0.0, 0.35);
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
    let original = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);

    file.ai_owned.set(true);
    file.text.set_editable(false);
    file.ai_badge.set_visible(true);
    file.ai_spinner.set_visible(true);
    file.ai_spinner.start();

    // A whole-file write replaces the old contents; clear the buffer so the new
    // text is typed into a blank page rather than appended to the old file.
    if whole_file {
        buffer.set_text("");
    }
    let mark = buffer.create_mark(None, &buffer.start_iter(), true);
    let _ = original;

    widgets.editor.streams.borrow_mut().insert(
        call_id.to_string(),
        AiStream {
            file: file.clone(),
            old_text: String::new(),
            replacement: if whole_file { Some(mark) } else { None },
            skip_typing: false,
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
            ensure_tag(&buffer, "ai-old", "#5c1f1f");
            let all_start = buffer.start_iter();
            let all_end = buffer.end_iter();
            buffer.remove_tag_by_name("ai-old", &all_start, &all_end);
            if let Some((start, end)) = find_text(&buffer, &stream.old_text) {
                buffer.apply_tag_by_name("ai-old", &start, &end);
                scroll_to_iter(&file, &start);
            }
        }
        EditorStreamField::NewString => {
            if stream.skip_typing {
                return;
            }
            if stream.replacement.is_none() {
                // The old text is complete now: drop the red highlight and
                // replace it, keeping a mark at the insertion point.
                let all_start = buffer.start_iter();
                let all_end = buffer.end_iter();
                buffer.remove_tag_by_name("ai-old", &all_start, &all_end);
                match find_text(&buffer, &stream.old_text) {
                    Some((mut start, mut end)) => {
                        buffer.delete(&mut start, &mut end);
                        let mark = buffer.create_mark(None, &start, false);
                        stream.replacement = Some(mark);
                    }
                    None => {
                        // The buffer changed under us; the disk reload at the
                        // end of the call will reconcile it.
                        stream.skip_typing = true;
                        return;
                    }
                }
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
    let mut end = buffer.end_iter();
    buffer.insert(&mut end, text);
    let end = buffer.end_iter();
    buffer.place_cursor(&end);
    scroll_to_iter(file, &end);
}

fn insert_at_mark(file: &OpenFile, mark: &gtk::TextMark, text: &str) {
    let buffer = file.text.buffer();
    ensure_tag(&buffer, "ai-new", "#173d24");
    let start = buffer.iter_at_mark(mark);
    let mut end = start.clone();
    end.forward_chars(text.chars().count() as i32);
    let mut insert_at = start.clone();
    buffer.insert(&mut insert_at, text);
    buffer.apply_tag_by_name("ai-new", &start, &end);
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

    // Fade the red/green highlights out once the edit has settled.
    let buffer = buffer.downgrade();
    glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
        if let Some(buffer) = buffer.upgrade() {
            let start = buffer.start_iter();
            let end = buffer.end_iter();
            for tag in ["ai-old", "ai-new"] {
                buffer.remove_tag_by_name(tag, &start, &end);
            }
        }
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
    if let Ok(text) = std::fs::read_to_string(canonical) {
        let buffer = file.text.buffer();
        buffer.set_text(&text);
        buffer.set_modified(false);
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
        buffer.set_text("");
        buffer.set_modified(false);
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
    path_label.set_hexpand(true);
    page_header.append(&path_label);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    page_header.append(&spacer);
    page.append(&page_header);

    let buffer = gtk::TextBuffer::new(None);
    buffer.set_text(code);
    buffer.set_modified(false);
    let view = gtk::TextView::with_buffer(&buffer);
    view.set_monospace(true);
    view.set_left_margin(12);
    view.set_right_margin(12);
    view.set_top_margin(8);
    view.set_bottom_margin(8);
    view.set_wrap_mode(gtk::WrapMode::None);
    view.add_css_class("lc-code");
    view.set_vexpand(true);
    // This is a preview — make it editable so the user can tweak it.
    view.set_editable(true);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .child(&view)
        .build();
    page.append(&scroll);

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
    });

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

/// Reload a file and overlay green/red line highlights for the diff hunks.
///
/// Added lines get a dark-green background tag; removed lines (which no longer
/// exist in the new file) are shown as a red marker on the line just before
/// where the deletion occurred.  After 1.5 s the highlights are cleared so the
/// editor returns to its normal appearance.
pub fn reload_with_diff(
    state: &Rc<UiState>,
    path: &Path,
    diff: &link_core::diff::FileDiff,
) {
    // First do the normal reload from disk.
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

    let buffer = file.text.buffer();

    // Create highlight tags on first use.
    let tag_table = buffer.tag_table();
    if tag_table.lookup("diff-added").is_none() {
        if let Some(tag) = buffer.create_tag(Some("diff-added"), &[]) {
            tag.set_background(Some("#1a3a1a"));
        }
    }
    if tag_table.lookup("diff-removed").is_none() {
        if let Some(tag) = buffer.create_tag(Some("diff-removed"), &[]) {
            tag.set_background(Some("#3a1a1a"));
        }
    }

    // Walk every hunk and paint lines.
    for hunk in &diff.hunks {
        // new_start is 1-based; convert to 0-based GTK line index.
        let mut line_no: i32 = (hunk.new_start as i32).saturating_sub(1);
        for dl in &hunk.lines {
            use link_core::diff::DiffLineKind;
            match dl.kind {
                DiffLineKind::Addition => {
                    if let (Some(s), Some(e)) = (
                        buffer.iter_at_line(line_no),
                        buffer.iter_at_line(line_no + 1),
                    ) {
                        buffer.apply_tag_by_name("diff-added", &s, &e);
                    }
                    line_no += 1;
                }
                DiffLineKind::Context => {
                    line_no += 1;
                }
                DiffLineKind::Removal => {
                    // Removed lines are gone from the file; highlight the
                    // surrounding context line so the user can see the deletion.
                    let show_at = line_no.max(0);
                    if let (Some(s), Some(e)) = (
                        buffer.iter_at_line(show_at),
                        buffer.iter_at_line(show_at + 1),
                    ) {
                        buffer.apply_tag_by_name("diff-removed", &s, &e);
                    }
                    // Removal lines don't advance the new-file line counter.
                }
            }
        }
    }

    // Clear highlights after 1.5 s.
    let buffer_weak = buffer.downgrade();
    glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
        if let Some(buf) = buffer_weak.upgrade() {
            let start = buf.start_iter();
            let end = buf.end_iter();
            buf.remove_tag_by_name("diff-added", &start, &end);
            buf.remove_tag_by_name("diff-removed", &start, &end);
        }
    });
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
}
