//! The left sidebar: workspace card, saved threads and the project file tree.
//!
//! Everything the sidebar offers lives in one scrollable column: the current
//! workspace at the top, the conversation history under THREADS and the
//! project files under PROJECT FILES. A compact dock at the bottom mirrors the
//! rail actions.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use link_core::session::Session;

use crate::state::UiState;

const SKIP_DIRS: &[&str] = &[
    ".git", ".hg", ".svn", "target", "node_modules", ".venv", "venv",
    "__pycache__", "dist", "build", ".next", ".nuxt", ".cache", ".gradle",
    ".idea", ".vscode", "vendor",
];

const MAX_ENTRIES: usize = 400;
const MAX_DEPTH: usize = 12;

pub struct SidebarPanel {
    pub root: gtk::Box,
    /// Compatibility shim for the old tab API; the sidebar is one column now.
    pub files_tab: gtk::ToggleButton,
    pub search: gtk::SearchEntry,
    pub files_box: gtk::Box,
    pub history_box: gtk::Box,
    thread_count: gtk::Label,
    file_rows: RefCell<Vec<(PathBuf, gtk::Button)>>,
    selected_file: RefCell<Option<PathBuf>>,
}

pub fn build(state: &Rc<UiState>) -> Rc<SidebarPanel> {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("lc-sidebar");
    root.set_width_request(crate::window::SIDEBAR_MIN);

    let files_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    files_box.set_margin_start(4);
    files_box.set_margin_end(4);
    files_box.set_margin_bottom(8);

    let history_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    history_box.set_margin_start(6);
    history_box.set_margin_end(6);

    let files_container = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let thread_count = gtk::Label::new(Some("0"));
    let search = gtk::SearchEntry::new();
    let files_tab = gtk::ToggleButton::new();

    // ---- header ---------------------------------------------------------
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.add_css_class("lc-sidebar-header");
    let tile = icon_tile("utilities-terminal-symbolic", "lc-tile-sm");
    header.append(&tile);
    let title = gtk::Label::new(Some("WORKSPACE"));
    title.add_css_class("lc-section");
    title.set_xalign(0.0);
    header.append(&title);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    header.append(&spacer);
    let collapse = gtk::Button::with_label("«");
    collapse.add_css_class("lc-header-button");
    collapse.set_tooltip_text(Some("Collapse sidebar (Ctrl+B)"));
    header.append(&collapse);
    root.append(&header);

    // ---- scrollable column ---------------------------------------------
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.set_margin_top(2);

    // Workspace card -----------------------------------------------------
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("lc-workspace-card");
    card.set_margin_start(12);
    card.set_margin_end(12);
    card.set_margin_bottom(10);

    let card_top = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let folder_tile = icon_tile("folder-symbolic", "lc-tile");
    card_top.append(&folder_tile);

    let names = gtk::Box::new(gtk::Orientation::Vertical, 1);
    names.set_hexpand(true);
    let workspace_name = gtk::Label::new(Some(&workspace_name(state)));
    workspace_name.add_css_class("lc-workspace-name");
    workspace_name.set_xalign(0.0);
    workspace_name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    names.append(&workspace_name);
    let workspace_path = gtk::Label::new(Some(&workspace_hint(state)));
    workspace_path.add_css_class("lc-path");
    workspace_path.set_xalign(0.0);
    workspace_path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    names.append(&workspace_path);
    card_top.append(&names);

    let run = gtk::Button::from_icon_name("media-playback-start-symbolic");
    run.add_css_class("lc-run-btn");
    run.set_tooltip_text(Some("Open terminal in this workspace"));
    card_top.append(&run);
    card.append(&card_top);

    let pills = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let git = gtk::Button::with_label("Git");
    git.add_css_class("lc-pill-btn");
    git.set_tooltip_text(Some("GitHub account, clone and commit"));
    git.set_hexpand(true);
    pills.append(&git);
    card.append(&pills);
    column.append(&card);

    // THREADS ------------------------------------------------------------
    let threads_head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    threads_head.add_css_class("lc-side-section");
    let threads_label = gtk::Label::new(Some("THREADS"));
    threads_label.add_css_class("lc-section");
    threads_head.append(&threads_label);
    thread_count.add_css_class("lc-count-badge");
    threads_head.append(&thread_count);
    let threads_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    threads_spacer.set_hexpand(true);
    threads_head.append(&threads_spacer);
    let new_thread = gtk::Button::from_icon_name("list-add-symbolic");
    new_thread.add_css_class("lc-plus");
    new_thread.set_tooltip_text(Some("New session"));
    threads_head.append(&new_thread);
    column.append(&threads_head);
    column.append(&history_box);

    // PROJECT FILES ------------------------------------------------------
    let files_head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    files_head.add_css_class("lc-side-section");
    let files_toggle = gtk::ToggleButton::new();
    files_toggle.add_css_class("lc-header-button");
    files_toggle.set_active(true);
    files_toggle.set_child(Some(&gtk::Label::new(Some("▾"))));
    files_toggle.set_tooltip_text(Some("Collapse project files"));
    files_head.append(&files_toggle);
    let files_label = gtk::Label::new(Some("PROJECT FILES"));
    files_label.add_css_class("lc-section");
    files_head.append(&files_label);
    let files_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    files_spacer.set_hexpand(true);
    files_head.append(&files_spacer);
    let ide = gtk::Button::with_label("IDE");
    ide.add_css_class("lc-ide-pill");
    ide.set_tooltip_text(Some("Files open in the editor"));
    files_head.append(&ide);
    column.append(&files_head);

    search.set_placeholder_text(Some("Filter files…"));
    search.add_css_class("lc-filter");
    search.set_margin_start(12);
    search.set_margin_end(12);
    search.set_margin_top(2);
    search.set_margin_bottom(4);
    files_container.append(&search);
    files_container.append(&files_box);

    files_toggle.connect_toggled({
        let files_container = files_container.clone();
        let files_toggle = files_toggle.clone();
        move |_| {
            let showing = files_toggle.is_active();
            files_container.set_visible(showing);
            files_toggle.set_child(Some(&gtk::Label::new(Some(if showing { "▾" } else { "▸" }))));
        }
    });

    column.append(&files_container);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&column)
        .build();
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&scroll));
    let watermark = gtk::Label::new(Some(">_"));
    watermark.add_css_class("lc-watermark");
    watermark.add_css_class("lc-mono");
    watermark.set_halign(gtk::Align::End);
    watermark.set_valign(gtk::Align::End);
    watermark.set_margin_end(18);
    watermark.set_margin_bottom(10);
    watermark.set_can_target(false);
    overlay.add_overlay(&watermark);
    overlay.set_vexpand(true);
    root.append(&overlay);

    // Bottom dock --------------------------------------------------------
    let dock = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    dock.add_css_class("lc-sidebar-bottom");
    let apps = dock_button("view-grid-symbolic", "Command palette (Ctrl+K)");
    let dock_refresh = dock_button("view-refresh-symbolic", "Refresh files");
    let dock_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dock_spacer.set_hexpand(true);
    dock.append(&apps);
    dock.append(&dock_spacer);
    dock.append(&dock_refresh);
    root.append(&dock);

    let panel = Rc::new(SidebarPanel {
        root,
        files_tab: files_tab.clone(),
        search: search.clone(),
        files_box,
        history_box,
        thread_count: thread_count.clone(),
        file_rows: RefCell::new(Vec::new()),
        selected_file: RefCell::new(None),
    });

    // ---- wiring --------------------------------------------------------
    {
        let state = state.clone();
        collapse.connect_clicked(move |_| crate::window::toggle_sidebar(&state));
    }
    {
        let state = state.clone();
        run.connect_clicked(move |_| {
            if state.workspace_path().is_none() {
                crate::window::open_folder_dialog(&state);
                return;
            }
            if let Some(widgets) = state.widgets() {
                widgets.bottom_root.set_visible(true);
                widgets.bottom.restart(&state);
                widgets.bottom.focus_terminal();
            }
        });
    }
    {
        let state = state.clone();
        git.connect_clicked(move |_| {
            if state.workspace_path().is_none() {
                state.toast("Open a folder first (Ctrl+O)");
                return;
            }
            crate::panels::git::present(&state);
        });
    }
    {
        let state = state.clone();
        new_thread.connect_clicked(move |_| crate::window::new_session(&state));
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
    {
        let state = state.clone();
        apps.connect_clicked(move |_| crate::dialogs::command_palette::present(&state));
    }
    {
        let state = state.clone();
        let panel = panel.clone();
        dock_refresh.connect_clicked(move |_| {
            refresh_files(&state, &panel);
            refresh_history(&state, &panel);
        });
    }

    panel
}

fn icon_tile(icon: &str, class: &str) -> gtk::Box {
    let tile = gtk::Box::new(gtk::Orientation::Vertical, 0);
    tile.add_css_class(class);
    tile.set_halign(gtk::Align::Center);
    tile.set_valign(gtk::Align::Center);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(17);
    image.set_halign(gtk::Align::Center);
    image.set_valign(gtk::Align::Center);
    tile.append(&image);
    tile
}

fn dock_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button
}

fn workspace_name(state: &UiState) -> String {
    state
        .workspace_path()
        .and_then(|path| path.file_name().map(|name| name.to_string_lossy().to_string()))
        .unwrap_or_else(|| "No folder open".to_string())
}

fn workspace_hint(state: &UiState) -> String {
    state
        .workspace_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "Open a folder with Ctrl+O".to_string())
}

// ---------------------------------------------------------------------------
// File tree
// ---------------------------------------------------------------------------

fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

pub fn refresh_files(state: &Rc<UiState>, panel: &Rc<SidebarPanel>) {
    panel.file_rows.borrow_mut().clear();
    clear(&panel.files_box);
    let Some(root) = state.workspace_path() else {
        let hint = gtk::Label::new(Some("Open a folder with Ctrl+O"));
        hint.add_css_class("lc-dim");
        hint.add_css_class("lc-tiny");
        hint.set_margin_start(16);
        hint.set_margin_top(10);
        hint.set_xalign(0.0);
        panel.files_box.append(&hint);
        return;
    };
    populate_dir(state, panel, &panel.files_box, &root, 0);
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
        refresh_files(&state, &panel);
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

fn populate_dir(
    state: &Rc<UiState>,
    panel: &Rc<SidebarPanel>,
    parent: &gtk::Box,
    dir: &Path,
    depth: usize,
) {
    if depth >= MAX_DEPTH {
        return;
    }
    for (path, is_dir) in read_entries(dir) {
        if is_dir {
            add_dir_row(state, panel, parent, &path, depth);
        } else {
            add_file_row(state, panel, parent, &path, depth);
        }
    }
}

fn row_body(depth: usize) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_margin_start(8 + (depth as i32) * 16);
    row.set_margin_end(6);
    row
}

fn add_dir_row(
    state: &Rc<UiState>,
    panel: &Rc<SidebarPanel>,
    parent: &gtk::Box,
    path: &Path,
    depth: usize,
) {
    let row = row_body(depth);
    let arrow = gtk::Label::new(Some("▸"));
    arrow.add_css_class("lc-folder-arrow");
    row.append(&arrow);
    let icon = gtk::Image::from_icon_name("folder-symbolic");
    icon.set_pixel_size(15);
    icon.add_css_class("lc-folder");
    row.append(&icon);
    let name = gtk::Label::new(Some(&path.file_name().unwrap_or_default().to_string_lossy()));
    name.set_xalign(0.0);
    name.add_css_class("lc-tiny");
    row.append(&name);

    let children = gtk::Box::new(gtk::Orientation::Vertical, 0);
    children.set_visible(false);

    let button = gtk::Button::builder().child(&row).has_frame(false).build();
    button.add_css_class("lc-file-row");
    button.set_tooltip_text(Some(&path.display().to_string()));

    {
        let children = children.clone();
        let arrow = arrow.clone();
        let state = state.clone();
        let panel = panel.clone();
        let path = path.to_path_buf();
        button.connect_clicked(move |_| {
            let showing = !children.is_visible();
            if showing && children.first_child().is_none() {
                populate_dir(&state, &panel, &children, &path, depth + 1);
            }
            children.set_visible(showing);
            arrow.set_text(if showing { "▾" } else { "▸" });
        });
    }

    parent.append(&button);
    parent.append(&children);
}

fn add_file_row(
    state: &Rc<UiState>,
    panel: &Rc<SidebarPanel>,
    parent: &gtk::Box,
    path: &Path,
    depth: usize,
) {
    let row = row_body(depth + 1);
    let (glyph, class) = file_glyph(path);
    let icon = gtk::Label::new(Some(glyph));
    icon.add_css_class("lc-ftype");
    icon.add_css_class(class);
    icon.set_width_chars(3);
    row.append(&icon);

    let label = gtk::Label::new(Some(&path.file_name().unwrap_or_default().to_string_lossy()));
    label.set_xalign(0.0);
    label.add_css_class("lc-tiny");
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    row.append(&label);

    let button = gtk::Button::builder().child(&row).has_frame(false).build();
    button.add_css_class("lc-file-row");
    if panel.selected_file.borrow().as_deref() == Some(path) {
        button.add_css_class("file-selected");
    }
    button.set_tooltip_text(Some(&path.display().to_string()));
    panel
        .file_rows
        .borrow_mut()
        .push((path.to_path_buf(), button.clone()));

    {
        let state = state.clone();
        let panel = panel.clone();
        let path = path.to_path_buf();
        button.connect_clicked(move |_| {
            *panel.selected_file.borrow_mut() = Some(path.clone());
            for (candidate, row) in panel.file_rows.borrow().iter() {
                if candidate == &path {
                    row.add_css_class("file-selected");
                } else {
                    row.remove_css_class("file-selected");
                }
            }
            open_path(&state, &path);
        });
    }
    attach_context_menu(&button, state, path);
    parent.append(&button);
}

/// A short, colour-coded glyph standing in for a file-type icon.
fn file_glyph(path: &Path) -> (&'static str, &'static str) {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => ("RS", "lc-ftype-rs"),
        "md" | "markdown" => ("MD", "lc-ftype-md"),
        "json" => ("{}", "lc-ftype-json"),
        "toml" => ("TM", "lc-ftype-toml"),
        "yaml" | "yml" => ("YM", "lc-ftype-toml"),
        "js" | "mjs" | "cjs" => ("JS", "lc-ftype-js"),
        "ts" | "tsx" => ("TS", "lc-ftype-js"),
        "jsx" => ("JX", "lc-ftype-js"),
        "html" | "htm" => ("<>", "lc-ftype-html"),
        "css" | "scss" => ("#", "lc-ftype-css"),
        "kt" => ("KT", "lc-ftype-kt"),
        "py" => ("PY", "lc-ftype-py"),
        "go" => ("GO", "lc-ftype-gradle"),
        "c" | "h" => ("C", "lc-ftype-default"),
        "cpp" | "hpp" => ("C+", "lc-ftype-default"),
        "sh" | "bash" | "zsh" => ("SH", "lc-ftype-git"),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" => ("IM", "lc-ftype-img"),
        "lock" => ("LK", "lc-ftype-git"),
        _ => ("·", "lc-ftype-default"),
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

fn filter_files(state: &Rc<UiState>, panel: &Rc<SidebarPanel>, needle: &str) {
    panel.file_rows.borrow_mut().clear();
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
            add_file_row(state, panel, &panel.files_box, &path, 0);
        }
    }
    if shown == 0 {
        let hint = gtk::Label::new(Some("No matching files"));
        hint.add_css_class("lc-dim");
        hint.add_css_class("lc-tiny");
        hint.set_margin_start(16);
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
    panel.thread_count.set_text("0");
    let Some(workspace) = state.workspace_path() else {
        let hint = gtk::Label::new(Some("Open a folder to see threads."));
        hint.add_css_class("lc-dim");
        hint.add_css_class("lc-tiny");
        hint.set_margin_start(14);
        hint.set_margin_top(8);
        hint.set_xalign(0.0);
        panel.history_box.append(&hint);
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
    clear(&panel.history_box);
    panel.thread_count.set_text(&sessions.len().to_string());
    if sessions.is_empty() {
        let hint = gtk::Label::new(Some("No saved sessions yet."));
        hint.add_css_class("lc-dim");
        hint.add_css_class("lc-tiny");
        hint.set_margin_start(14);
        hint.set_margin_top(8);
        hint.set_margin_bottom(6);
        hint.set_xalign(0.0);
        panel.history_box.append(&hint);
        return;
    }
    let current = state.session.borrow().as_ref().map(|session| session.id.clone());
    for summary in sessions {
        let selected = current.as_deref() == Some(summary.id.as_str());
        panel
            .history_box
            .append(&thread_card(state, panel, &summary, selected));
    }
}

fn thread_card(
    state: &Rc<UiState>,
    panel: &Rc<SidebarPanel>,
    summary: &link_core::session::SessionSummary,
    selected: bool,
) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    card.add_css_class("lc-thread-card");
    if selected {
        card.add_css_class("lc-card-selected");
    }

    let texts = gtk::Box::new(gtk::Orientation::Vertical, 1);
    texts.set_hexpand(true);
    let title = gtk::Label::new(Some(&summary.title));
    title.add_css_class("lc-thread-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    texts.append(&title);
    let preview = gtk::Label::new(Some(&format!(
        "{} · {} messages",
        summary.updated_at.format("%b %d"),
        summary.message_count
    )));
    preview.add_css_class("lc-thread-preview");
    preview.set_xalign(0.0);
    preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
    texts.append(&preview);
    card.append(&texts);

    // Only the text area opens the thread, so the small action buttons keep
    // their own clicks.
    {
        let state = state.clone();
        let panel = panel.clone();
        let id = summary.id.clone();
        let gesture = gtk::GestureClick::new();
        gesture.set_button(gtk::gdk::BUTTON_PRIMARY);
        gesture.connect_pressed(move |_, _, _, _| match state.load_session(&id) {
            Ok(session) => {
                show_session(&state, session);
                refresh_history(&state, &panel);
            }
            Err(error) => state.toast(&error),
        });
        texts.add_controller(gesture);
    }

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    actions.set_valign(gtk::Align::Start);

    let rename = gtk::Button::from_icon_name("document-edit-symbolic");
    rename.add_css_class("lc-thread-action");
    rename.set_tooltip_text(Some("Rename thread"));
    {
        let state = state.clone();
        let panel = panel.clone();
        let id = summary.id.clone();
        let title = summary.title.clone();
        rename.connect_clicked(move |_| prompt_rename(&state, &panel, &id, &title));
    }
    actions.append(&rename);

    let remove = gtk::Button::from_icon_name("edit-delete-symbolic");
    remove.add_css_class("lc-thread-action");
    remove.set_tooltip_text(Some("Delete thread"));
    {
        let state = state.clone();
        let panel = panel.clone();
        let id = summary.id.clone();
        remove.connect_clicked(move |_| confirm_delete(&state, &panel, &id));
    }
    actions.append(&remove);
    card.append(&actions);
    card
}

fn prompt_rename(
    state: &Rc<UiState>,
    panel: &Rc<SidebarPanel>,
    id: &str,
    current: &str,
) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    let dialog = adw::Window::builder()
        .title("Rename thread")
        .modal(true)
        .transient_for(&widgets.window)
        .default_width(380)
        .build();
    dialog.add_css_class("lc-dialog");

    let header = gtk::HeaderBar::new();
    header.add_css_class("lc-header");
    header.set_show_title_buttons(true);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.set_margin_top(14);
    body.set_margin_bottom(16);

    let label = gtk::Label::new(Some("THREAD NAME"));
    label.add_css_class("lc-form-label");
    label.set_xalign(0.0);
    body.append(&label);
    let entry = gtk::Entry::new();
    entry.set_text(current);
    entry.add_css_class("lc-input");
    body.append(&entry);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.set_margin_top(4);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("lc-ghost");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("lc-primary");
    buttons.append(&cancel);
    buttons.append(&save);
    body.append(&buttons);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));
    dialog.set_content(Some(&toolbar));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    {
        let state = state.clone();
        let panel = panel.clone();
        let dialog = dialog.clone();
        let entry = entry.clone();
        let id = id.to_string();
        save.connect_clicked(move |_| {
            let title = entry.text().trim().to_string();
            if title.is_empty() {
                entry.add_css_class("lc-error");
                return;
            }
            if let Some(workspace) = state.workspace_path() {
                if let Ok(mut session) = state.session_store.load(&workspace, &id) {
                    session.rename(title.clone());
                    let _ = state.session_store.save(&session);
                }
            }
            if let Some(session) = state.session.borrow_mut().as_mut() {
                if session.id == id {
                    session.rename(title);
                }
            }
            refresh_history(&state, &panel);
            dialog.close();
        });
    }

    dialog.present();
    entry.grab_focus();
}

fn confirm_delete(state: &Rc<UiState>, panel: &Rc<SidebarPanel>, id: &str) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    let dialog = gtk::AlertDialog::builder()
        .modal(true)
        .message("Delete this thread?")
        .detail("The conversation will be removed from disk.")
        .buttons(["Cancel", "Delete"])
        .cancel_button(0)
        .default_button(0)
        .build();
    let state = state.clone();
    let panel = panel.clone();
    let id = id.to_string();
    dialog.choose(
        Some(&widgets.window),
        None::<&gio::Cancellable>,
        move |result| {
            if !matches!(result, Ok(1)) {
                return;
            }
            if let Some(workspace) = state.workspace_path() {
                let _ = state.session_store.delete(&workspace, &id);
            }
            let active = state
                .session
                .borrow()
                .as_ref()
                .map(|session| session.id == id)
                .unwrap_or(false);
            if active {
                forget_active_session(&state);
            }
            refresh_history(&state, &panel);
        },
    );
}

fn forget_active_session(state: &Rc<UiState>) {
    state.blocks.borrow_mut().clear();
    state.block_order.borrow_mut().clear();
    state.tools.borrow_mut().clear();
    state.edits.borrow_mut().clear();
    *state.session.borrow_mut() = None;
    if let Some(widgets) = state.widgets() {
        crate::widgets::chat_message::clear(&widgets.chat_list);
        widgets.tasks.clear();
    }
}

pub fn show_history(state: &Rc<UiState>, panel: &Rc<SidebarPanel>) {
    panel.files_tab.set_active(true);
    if let Some(widgets) = state.widgets() {
        widgets.sidebar_root.set_visible(true);
    }
    refresh_history(state, panel);
}

/// Load a stored session into the chat panel.
pub fn show_session(state: &Rc<UiState>, session: Session) {
    show_session_inner(state, session, true);
}

fn show_session_inner(state: &Rc<UiState>, session: Session, notify: bool) {
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
                    &format!("⚙ {}", message.content),
                ));
            }
            link_core::session::StoredRole::System => {}
        }
    }

    let title = session.title.clone();
    let cost = session.total_cost();
    // Restore the live checklist so the user sees the same progress the agent
    // is resuming from.
    if session.work.todos.is_empty() {
        widgets.tasks.clear();
    } else {
        widgets.tasks.render(&session.work.todos);
    }
    *state.session.borrow_mut() = Some(session);
    state.session_handoff.set(true);
    state.handoff_from_model_change.set(false);
    state.cost.set(cost);
    if notify {
        state.toast(&format!("Loaded session “{title}”"));
    }
    crate::panels::agent::scroll_to_bottom(&widgets);
}

/// Reopen the most recent conversation for the current workspace.
///
/// Called when a workspace is opened (including the last workspace on launch)
/// so the agent continues the existing task instead of starting a new session
/// and repeating completed work.
pub fn resume_last(state: &Rc<UiState>) {
    let Some(workspace) = state.workspace_path() else {
        return;
    };
    if state.session.borrow().is_some() {
        return;
    }
    let Some(session) = state.session_store.most_recent(&workspace) else {
        return;
    };
    let title = session.title.clone();
    let open = session.open_task_count();
    show_session_inner(state, session, false);
    if open > 0 {
        state.toast(&format!(
            "Resumed “{title}” \u{00b7} {open} unfinished task{}",
            if open == 1 { "" } else { "s" }
        ));
    } else {
        state.toast(&format!("Resumed “{title}”"));
    }
}
