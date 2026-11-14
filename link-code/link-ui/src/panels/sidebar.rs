//! The left sidebar: workspace file tree and session history.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;

use link_core::session::Session;

use crate::state::UiState;

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".cache",
    ".gradle",
    ".idea",
    ".vscode",
    "vendor",
];

const MAX_ENTRIES: usize = 400;
const MAX_DEPTH: usize = 12;

pub struct SidebarPanel {
    pub root: gtk::Box,
    pub stack: gtk::Stack,
    pub files_box: gtk::Box,
    pub history_box: gtk::Box,
    pub search: gtk::SearchEntry,
    pub files_tab: gtk::ToggleButton,
    pub history_tab: gtk::ToggleButton,
}

pub fn build(state: &Rc<UiState>) -> Rc<SidebarPanel> {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("lc-sidebar");
    root.set_width_request(220);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    header.set_margin_start(8);
    header.set_margin_end(6);
    header.set_margin_top(6);
    header.set_margin_bottom(3);
    let title = gtk::Label::new(Some("EXPLORER"));
    title.add_css_class("lc-section");
    title.set_xalign(0.0);
    header.append(&title);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    header.append(&spacer);

    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.add_css_class("lc-header-button");
    refresh.set_tooltip_text(Some("Refresh"));
    header.append(&refresh);
    root.append(&header);

    // ---- tab strip ------------------------------------------------------
    let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tabs.add_css_class("lc-tabstrip");
    let files_tab = gtk::ToggleButton::with_label("Files");
    files_tab.add_css_class("lc-tab");
    files_tab.set_active(true);
    let history_tab = gtk::ToggleButton::with_label("History");
    history_tab.add_css_class("lc-tab");
    history_tab.set_group(Some(&files_tab));
    files_tab.set_hexpand(true);
    history_tab.set_hexpand(true);
    tabs.append(&files_tab);
    tabs.append(&history_tab);
    root.append(&tabs);

    let stack = gtk::Stack::new();
    stack.set_vexpand(true);

    // ---- files page -----------------------------------------------------
    let files_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Filter files\u{2026}"));
    search.add_css_class("lc-input");
    search.set_margin_start(6);
    search.set_margin_end(6);
    search.set_margin_top(4);
    search.set_margin_bottom(3);
    files_page.append(&search);

    let files_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    files_box.set_margin_start(4);
    files_box.set_margin_end(4);
    files_box.set_margin_bottom(8);
    let files_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&files_box)
        .build();
    files_page.append(&files_scroll);
    stack.add_named(&files_page, Some("files"));

    // ---- history page ---------------------------------------------------
    let history_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    history_box.set_margin_start(4);
    history_box.set_margin_end(4);
    history_box.set_margin_top(4);
    history_box.set_margin_bottom(6);
    let history_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&history_box)
        .build();
    stack.add_named(&history_scroll, Some("history"));
    stack.set_visible_child_name("files");
    root.append(&stack);

    let panel = Rc::new(SidebarPanel {
        root,
        stack,
        files_box,
        history_box,
        search: search.clone(),
        files_tab: files_tab.clone(),
        history_tab: history_tab.clone(),
    });

    {
        let panel = panel.clone();
        files_tab.connect_toggled(move |button| {
            if button.is_active() {
                panel.stack.set_visible_child_name("files");
            }
        });
    }
    {
        let panel = panel.clone();
        history_tab.connect_toggled(move |button| {
            if button.is_active() {
                panel.stack.set_visible_child_name("history");
            }
        });
    }
    {
        let state = state.clone();
        let panel = panel.clone();
        refresh.connect_clicked(move |_| refresh_files(&state, &panel));
    }
    {
        let state = state.clone();
        let panel = panel.clone();
        search.connect_search_changed(move |entry| {
            let text = entry.text().to_string();
            if text.trim().is_empty() {
                refresh_files(&state, &panel);
            } else {
                filter_files(&state, &panel, &text);
            }
        });
    }

    panel
}

// ---------------------------------------------------------------------------
// File tree
// ---------------------------------------------------------------------------

fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

pub fn refresh_files(state: &Rc<UiState>, panel: &SidebarPanel) {
    clear(&panel.files_box);
    let Some(root) = state.workspace_path() else {
        let hint = gtk::Label::new(Some("Open a folder with Ctrl+O"));
        hint.add_css_class("lc-dim");
        hint.add_css_class("lc-tiny");
        hint.set_margin_start(10);
        hint.set_margin_top(10);
        hint.set_xalign(0.0);
        panel.files_box.append(&hint);
        return;
    };
    populate_dir(state, &panel.files_box, &root, 0);
}

/// Rebuild the tree at most once every 120 ms. A turn that writes ten files
/// used to rebuild the whole explorer ten times, on the GTK thread.
pub fn refresh_files_deferred(state: &Rc<UiState>, panel: &Rc<SidebarPanel>) {
    thread_local! {
        static PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if PENDING.with(|pending| pending.replace(true)) {
        return;
    }
    let state = state.clone();
    let panel = panel.clone();
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
        PENDING.with(|pending| pending.set(false));
        refresh_files(&state, panel.as_ref());
    });
}

fn read_entries(dir: &Path) -> Vec<(PathBuf, bool)> {
    let mut entries = Vec::new();
    let Ok(read) = std::fs::read_dir(dir) else {
        return entries;
    };
    for entry in read.flatten().take(MAX_ENTRIES) {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') && name != ".github" && name != ".link" {
            continue;
        }
        let path = entry.path();
        let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        if is_dir && SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        entries.push((path, is_dir));
    }
    entries.sort_by(|a, b| match (a.1, b.1) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a
            .0
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .cmp(&b.0.file_name().map(|name| name.to_string_lossy().to_lowercase())),
    });
    entries
}

fn populate_dir(state: &Rc<UiState>, parent: &gtk::Box, dir: &Path, depth: usize) {
    if depth >= MAX_DEPTH {
        return;
    }
    for (path, is_dir) in read_entries(dir) {
        if is_dir {
            add_dir_row(state, parent, &path, depth);
        } else {
            add_file_row(state, parent, &path, depth);
        }
    }
}

fn row_indent(depth: usize) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    row.set_margin_start((depth * 10) as i32);
    row
}

fn add_dir_row(state: &Rc<UiState>, parent: &gtk::Box, path: &Path, depth: usize) {
    let row = row_indent(depth);
    row.set_margin_top(1);
    row.set_margin_bottom(1);

    let arrow = gtk::Button::with_label("\u{25b8}");
    arrow.add_css_class("lc-header-button");
    arrow.set_has_frame(false);
    arrow.set_width_request(18);
    row.append(&arrow);

    let icon = gtk::Image::from_icon_name("folder-symbolic");
    icon.set_pixel_size(14);
    row.append(&icon);

    let name = gtk::Label::new(Some(&path.file_name().unwrap_or_default().to_string_lossy()));
    name.set_xalign(0.0);
    name.add_css_class("lc-tiny");
    row.append(&name);

    let children = gtk::Box::new(gtk::Orientation::Vertical, 0);
    children.set_visible(false);

    {
        let children = children.clone();
        let arrow_inner = arrow.clone();
        let state = state.clone();
        let path = path.to_path_buf();
        arrow.connect_clicked(move |_| {
            let showing = !children.is_visible();
            if showing && children.first_child().is_none() {
                populate_dir(&state, &children, &path, depth + 1);
            }
            children.set_visible(showing);
            arrow_inner.set_label(if showing { "\u{25be}" } else { "\u{25b8}" });
        });
    }

    parent.append(&row);
    parent.append(&children);
}

fn add_file_row(state: &Rc<UiState>, parent: &gtk::Box, path: &Path, depth: usize) {
    let row = row_indent(depth);
    row.set_margin_top(1);
    row.set_margin_bottom(1);
    row.set_margin_start(18 + (depth * 10) as i32);

    let icon = gtk::Image::from_icon_name(icon_for(path));
    icon.set_pixel_size(14);
    row.append(&icon);

    let label = gtk::Label::new(Some(&path.file_name().unwrap_or_default().to_string_lossy()));
    label.set_xalign(0.0);
    label.add_css_class("lc-tiny");
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    row.append(&label);

    let button = gtk::Button::builder()
        .child(&row)
        .has_frame(false)
        .build();
    button.add_css_class("lc-recent-row");
    button.set_tooltip_text(Some(&path.display().to_string()));
    {
        let state = state.clone();
        let path = path.to_path_buf();
        button.connect_clicked(move |_| open_path(&state, &path));
    }
    attach_context_menu(&button, state, path);
    parent.append(&button);
}

fn icon_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "text-x-rust-symbolic",
        "md" | "markdown" => "text-x-generic-symbolic",
        "json" | "toml" | "yaml" | "yml" => "text-x-generic-symbolic",
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" => "image-x-generic-symbolic",
        "sh" => "text-x-script-symbolic",
        _ => "text-x-generic-symbolic",
    }
}

fn open_path(state: &Rc<UiState>, path: &Path) {
    if path.is_dir() {
        return;
    }
    crate::panels::editor::open_file(state, path);
}

fn attach_context_menu(button: &gtk::Button, state: &Rc<UiState>, path: &Path) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    let path_buf = path.to_path_buf();
    let button_clone = button.clone();
    let state = state.clone();
    gesture.connect_pressed(move |_, _, x, y| {
        let popover = gtk::Popover::new();
        popover.add_css_class("lc-popover");
        popover.set_parent(&button_clone);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 2);

        let open = gtk::Button::with_label("Open");
        {
            let state = state.clone();
            let path = path_buf.clone();
            let popover = popover.clone();
            open.connect_clicked(move |_| {
                open_path(&state, &path);
                popover.popdown();
            });
        }
        content.append(&open);

        let copy = gtk::Button::with_label("Copy path");
        {
            let path = path_buf.clone();
            let popover = popover.clone();
            let button = button_clone.clone();
            copy.connect_clicked(move |_| {
                button.display().clipboard().set_text(&path.display().to_string());
                popover.popdown();
            });
        }
        content.append(&copy);

        let reveal = gtk::Button::with_label("Reveal in Files");
        {
            let path = path_buf.clone();
            let popover = popover.clone();
            reveal.connect_clicked(move |_| {
                let uri = format!("file://{}", path.display());
                let _ = gtk::gio::AppInfo::launch_default_for_uri(
                    &uri,
                    None::<&gtk::gio::AppLaunchContext>,
                );
                popover.popdown();
            });
        }
        content.append(&reveal);

        popover.set_child(Some(&content));
        popover.connect_closed(|popover| popover.unparent());
        popover.popup();
    });
    button.add_controller(gesture);
}

fn filter_files(state: &Rc<UiState>, panel: &SidebarPanel, needle: &str) {
    clear(&panel.files_box);
    let Some(root) = state.workspace_path() else {
        return;
    };
    let needle = needle.to_lowercase();
    let mut matches = Vec::new();
    collect_files(&root, 0, &mut matches);
    let mut shown = 0;
    for path in matches {
        if shown >= 200 {
            break;
        }
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_lowercase();
        if relative.contains(&needle) {
            shown += 1;
            add_file_row(state, &panel.files_box, &path, 0);
        }
    }
    if shown == 0 {
        let hint = gtk::Label::new(Some("No matching files"));
        hint.add_css_class("lc-dim");
        hint.add_css_class("lc-tiny");
        hint.set_margin_start(10);
        hint.set_margin_top(10);
        hint.set_xalign(0.0);
        panel.files_box.append(&hint);
    }
}

fn collect_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth >= MAX_DEPTH || out.len() > 4000 {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, depth + 1, out);
        } else {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// Session history
// ---------------------------------------------------------------------------

pub fn refresh_history(state: &Rc<UiState>, panel: &Rc<SidebarPanel>) {
    clear(&panel.history_box);
    let Some(workspace) = state.workspace_path() else {
        return;
    };
    thread_local! {
        static GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    let generation = GENERATION.with(|generation| {
        let next = generation.get() + 1;
        generation.set(next);
        next
    });
    // Listing reads every session file. Keep that I/O off the GTK thread so a
    // turn finishing never stalls the window.
    let store = state.session_store.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    crate::state::runtime().spawn(async move {
        let _ = tx.send(store.list(&workspace));
    });
    let state = state.clone();
    let panel = panel.clone();
    gtk::glib::MainContext::default().spawn_local(async move {
        let Ok(sessions) = rx.await else {
            return;
        };
        // A newer refresh already cleared the list; do not append twice.
        if GENERATION.with(|current| current.get()) != generation {
            return;
        }
        render_history(&state, &panel, sessions);
    });
}

fn render_history(
    state: &Rc<UiState>,
    panel: &Rc<SidebarPanel>,
    sessions: Vec<link_core::session::SessionSummary>,
) {
    if sessions.is_empty() {
        let hint = gtk::Label::new(Some("No saved sessions yet."));
        hint.add_css_class("lc-dim");
        hint.add_css_class("lc-tiny");
        hint.set_margin_start(6);
        hint.set_margin_top(10);
        hint.set_xalign(0.0);
        panel.history_box.append(&hint);
        return;
    }
    for summary in sessions {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
        row.set_margin_start(4);
        row.set_margin_end(4);
        let title = gtk::Label::new(Some(&summary.title));
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.add_css_class("lc-tiny");
        let meta = gtk::Label::new(Some(&format!(
            "{} \u{00b7} {} messages",
            summary.updated_at.format("%b %d %H:%M"),
            summary.message_count
        )));
        meta.set_xalign(0.0);
        meta.add_css_class("lc-tiny");
        meta.add_css_class("lc-dim");
        row.append(&title);
        row.append(&meta);

        let button = gtk::Button::builder().child(&row).has_frame(false).build();
        button.add_css_class("lc-recent-row");
        {
            let state = state.clone();
            let id = summary.id.clone();
            button.connect_clicked(move |_| match state.load_session(&id) {
                Ok(session) => show_session(&state, session),
                Err(error) => state.toast(&error),
            });
        }
        attach_history_menu(&button, state, &summary.id);
        panel.history_box.append(&button);
    }
}

fn attach_history_menu(button: &gtk::Button, state: &Rc<UiState>, session_id: &str) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    let button_clone = button.clone();
    let state = state.clone();
    let session_id = session_id.to_string();
    gesture.connect_pressed(move |_, _, x, y| {
        let popover = gtk::Popover::new();
        popover.add_css_class("lc-popover");
        popover.set_parent(&button_clone);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 2);

        let export = gtk::Button::with_label("Export as Markdown");
        {
            let state = state.clone();
            let session_id = session_id.clone();
            let popover = popover.clone();
            export.connect_clicked(move |_| {
                if let Ok(session) = state.load_session(&session_id) {
                    let markdown = link_core::session::SessionStore::export_markdown(&session);
                    let dialog = gtk::FileDialog::builder()
                        .title("Export session")
                        .initial_name(&format!("{}.md", session.title))
                        .build();
                    let parent = state.widgets().map(|widgets| widgets.window.clone());
                    let state = state.clone();
                    dialog.save(
                        parent.as_ref(),
                        None::<&gtk::gio::Cancellable>,
                        move |result| {
                            if let Ok(file) = result {
                                if let Some(path) = file.path() {
                                    let _ = std::fs::write(&path, &markdown);
                                    state.toast(&format!("Exported to {}", path.display()));
                                }
                            }
                        },
                    );
                }
                popover.popdown();
            });
        }
        content.append(&export);

        let delete = gtk::Button::with_label("Delete");
        delete.add_css_class("lc-danger");
        {
            let state = state.clone();
            let session_id = session_id.clone();
            let popover = popover.clone();
            delete.connect_clicked(move |_| {
                if let Some(workspace) = state.workspace_path() {
                    let _ = state.session_store.delete(&workspace, &session_id);
                    if let Some(widgets) = state.widgets() {
                        refresh_history(&state, &widgets.sidebar);
                    }
                }
                popover.popdown();
            });
        }
        content.append(&delete);

        popover.set_child(Some(&content));
        popover.connect_closed(|popover| popover.unparent());
        popover.popup();
    });
    button.add_controller(gesture);
}

pub fn show_history(state: &Rc<UiState>, panel: &Rc<SidebarPanel>) {
    panel.history_tab.set_active(true);
    panel.stack.set_visible_child_name("history");
    refresh_history(state, panel);
}

/// Load a stored session into the chat panel.
pub fn show_session(state: &Rc<UiState>, session: Session) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    crate::widgets::chat_message::clear(&widgets.chat_list);
    state.blocks.borrow_mut().clear();
    state.block_order.borrow_mut().clear();
    state.tools.borrow_mut().clear();
    state.edits.borrow_mut().clear();

    for message in &session.messages {
        match message.role {
            link_core::session::StoredRole::User => {
                widgets
                    .chat_list
                    .append(&crate::widgets::chat_message::user_bubble(&message.content));
            }
            link_core::session::StoredRole::Assistant => {
                let block = std::rc::Rc::new(crate::widgets::chat_message::BlockView::new());
                crate::widgets::chat_message::BlockView::wire_copy_btn(&block);
                if let Some(thinking) = &message.thinking {
                    block.append_thinking(thinking);
                }
                block.complete(&message.content);
                block.finish();
                widgets.chat_list.append(&block.root);
            }
            link_core::session::StoredRole::Tool => {
                widgets.chat_list.append(&crate::widgets::chat_message::notice(
                    &format!("\u{2699} {}", message.content),
                ));
            }
            link_core::session::StoredRole::System => {}
        }
    }

    let title = session.title.clone();
    *state.session.borrow_mut() = Some(session);
    state.toast(&format!("Loaded session \u{201c}{title}\u{201d}"));
    crate::panels::agent::scroll_to_bottom(&widgets);
}
