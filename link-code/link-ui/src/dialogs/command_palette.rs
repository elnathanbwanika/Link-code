//! Command palette (Ctrl+K / Ctrl+Shift+P) and quick open (Ctrl+P).

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use crate::state::UiState;

struct Item {
    title: String,
    subtitle: String,
    action: Rc<dyn Fn()>,
}

const SKIP: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    ".next",
    ".cache",
    ".gradle",
    "vendor",
    ".hg",
    ".svn",
];

pub fn present(state: &Rc<UiState>) {
    let mut items: Vec<Item> = Vec::new();

    let add = |items: &mut Vec<Item>, title: &str, subtitle: &str, action: Rc<dyn Fn()>| {
        items.push(Item {
            title: title.to_string(),
            subtitle: subtitle.to_string(),
            action,
        });
    };

    {
        let state = state.clone();
        add(
            &mut items,
            "Open folder\u{2026}",
            "Choose a workspace",
            Rc::new(move || crate::window::open_folder_dialog(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            &mut items,
            "Quick open file\u{2026}",
            "Jump to a file in the workspace",
            Rc::new(move || present_quick_open(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            &mut items,
            "New session",
            "Start a clean conversation",
            Rc::new(move || crate::window::new_session(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            &mut items,
            "Manage models\u{2026}",
            "Add, edit or test a model endpoint",
            Rc::new(move || crate::dialogs::models::present(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            &mut items,
            "Settings\u{2026}",
            "General, appearance, agent, editor, terminal",
            Rc::new(move || crate::dialogs::settings::present(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            &mut items,
            "Toggle sidebar",
            "Show or hide the file explorer",
            Rc::new(move || crate::window::toggle_sidebar(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            &mut items,
            "Toggle bottom panel",
            "Terminal, output and problems",
            Rc::new(move || crate::window::toggle_bottom(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            &mut items,
            "Toggle agent panel",
            "Show or hide the agent chat column",
            Rc::new(move || crate::window::toggle_agent(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            &mut items,
            "Focus agent input",
            "Ctrl+L",
            Rc::new(move || crate::window::focus_agent(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            &mut items,
            "Save file",
            "Ctrl+S",
            Rc::new(move || crate::panels::editor::save_current(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            &mut items,
            "Cycle permission mode",
            "Ask \u{2192} auto-accept \u{2192} plan \u{2192} full auto",
            Rc::new(move || {
                state.cycle_mode();
                crate::panels::agent::refresh_composer(&state);
            }),
        );
    }
    {
        let state = state.clone();
        add(
            &mut items,
            "Stop the agent",
            "Esc",
            Rc::new(move || crate::panels::agent::stop(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            &mut items,
            "Clear conversation",
            "Remove the on-screen messages (the session stays saved)",
            Rc::new(move || crate::window::clear_conversation(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            &mut items,
            "Session history",
            "Browse saved sessions for this workspace",
            Rc::new(move || {
                if let Some(widgets) = state.widgets() {
                    crate::panels::sidebar::show_history(&state, &widgets.sidebar);
                }
            }),
        );
    }
    {
        let state = state.clone();
        add(
            &mut items,
            "Show models in the sidebar",
            "Switch the explorer to session history",
            Rc::new(move || {
                if let Some(widgets) = state.widgets() {
                    crate::panels::sidebar::refresh_history(&state, &widgets.sidebar);
                }
            }),
        );
    }

    show_palette(state, "COMMANDS", "Type a command\u{2026}", items);
}

pub fn present_quick_open(state: &Rc<UiState>) {
    let Some(root) = state.workspace_path() else {
        state.toast("Open a folder first (Ctrl+O)");
        return;
    };
    let palette = build_palette(state, "QUICK OPEN", "Type a file name\u{2026}");
    (palette.set_loading)(true);
    palette.window.present();
    palette.search.grab_focus();

    // Scan the tree off the GTK thread: a large repository must not freeze the
    // window before the list appears.
    let scan_root = root.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    crate::state::runtime().spawn(async move {
        let files = tokio::task::spawn_blocking(move || {
            let mut files = Vec::new();
            collect_files(&scan_root, 0, &mut files);
            files
        })
        .await
        .unwrap_or_default();
        let _ = tx.send(files);
    });

    let state = state.clone();
    let set_items = palette.set_items.clone();
    glib::MainContext::default().spawn_local(async move {
        let Ok(files) = rx.await else {
            return;
        };
        let items: Vec<Item> = files
            .into_iter()
            .take(4_000)
            .map(|path| quick_open_item(&state, &root, path))
            .collect();
        if items.is_empty() {
            state.toast("This folder has no files to open");
        }
        (set_items)(items);
    });
}

fn quick_open_item(state: &Rc<UiState>, root: &Path, path: PathBuf) -> Item {
    let title = path
        .strip_prefix(root)
        .unwrap_or(&path)
        .to_string_lossy()
        .to_string();
    let subtitle = path
        .parent()
        .map(|parent| parent.display().to_string())
        .unwrap_or_default();
    let state = state.clone();
    Item {
        title,
        subtitle,
        action: Rc::new(move || crate::panels::editor::open_file(&state, &path)),
    }
}

struct Palette {
    window: gtk::Window,
    search: gtk::SearchEntry,
    set_items: Rc<dyn Fn(Vec<Item>)>,
    set_loading: Rc<dyn Fn(bool)>,
}

fn show_palette(
    state: &Rc<UiState>,
    title_text: &str,
    placeholder: &str,
    items: Vec<Item>,
) {
    let palette = build_palette(state, title_text, placeholder);
    (palette.set_items)(items);
    palette.window.present();
    palette.search.grab_focus();
}

/// Build the palette window without presenting it, so quick open can appear
/// instantly and fill the list once its background scan finishes.
fn build_palette(state: &Rc<UiState>, title_text: &str, placeholder: &str) -> Palette {
    let window = gtk::Window::builder()
        .title(title_text)
        .modal(true)
        .resizable(true)
        .default_width(520)
        .default_height(400)
        .build();
    window.add_css_class("lc-dialog");
    if let Some(parent) = state.widgets().map(|widgets| widgets.window.clone()) {
        window.set_transient_for(Some(&parent));
    }

    let header = gtk::HeaderBar::new();
    let title = gtk::Label::new(Some(title_text));
    title.add_css_class("lc-section");
    title.add_css_class("lc-accent");
    header.set_title_widget(Some(&title));
    window.set_titlebar(Some(&header));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("lc-root");

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(placeholder));
    search.add_css_class("lc-input");
    {
        // `GtkSearchEntry` consumes Escape to stop search, so the window's key
        // controller never sees it. Its `stop-search` signal is the reliable
        // close hook while the entry has focus.
        let window = window.clone();
        search.connect_stop_search(move |_| window.close());
    }
    search.set_margin_start(10);
    search.set_margin_end(10);
    search.set_margin_top(10);
    search.set_margin_bottom(6);
    root.append(&search);

    let list = gtk::ListBox::new();
    list.add_css_class("lc-treeview");
    list.set_selection_mode(gtk::SelectionMode::Single);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&list)
        .build();
    root.append(&scroll);

    let items: Rc<RefCell<Vec<Item>>> = Rc::new(RefCell::new(Vec::new()));
    let loading = Rc::new(Cell::new(false));
    let first_actions: Rc<RefCell<Vec<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(Vec::new()));

    let window_for_rebuild = window.clone();
    let rebuild = {
        let list = list.clone();
        let items = items.clone();
        let loading = loading.clone();
        let first_actions = first_actions.clone();
        Rc::new(move |needle: &str| {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            first_actions.borrow_mut().clear();
            let needle = needle.trim().to_lowercase();
            let mut shown = 0usize;
            for item in items.borrow().iter() {
                let haystack = format!("{} {}", item.title, item.subtitle).to_lowercase();
                if !needle.is_empty() && !haystack.contains(&needle) {
                    continue;
                }
                shown += 1;
                if shown > 300 {
                    break;
                }
                let row = gtk::Box::new(gtk::Orientation::Vertical, 1);
                row.set_margin_start(8);
                row.set_margin_end(8);
                row.set_margin_top(5);
                row.set_margin_bottom(5);
                let name = gtk::Label::new(Some(&item.title));
                name.set_xalign(0.0);
                name.set_ellipsize(gtk::pango::EllipsizeMode::End);
                row.append(&name);
                if !item.subtitle.is_empty() {
                    let subtitle = gtk::Label::new(Some(&item.subtitle));
                    subtitle.add_css_class("lc-tiny");
                    subtitle.add_css_class("lc-dim");
                    subtitle.set_xalign(0.0);
                    subtitle.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                    row.append(&subtitle);
                }
                let action = item.action.clone();
                first_actions.borrow_mut().push(action.clone());
                let button = gtk::Button::builder().child(&row).has_frame(false).build();
                button.add_css_class("lc-recent-row");
                {
                    let action = action.clone();
                    let window = window_for_rebuild.clone();
                    button.connect_clicked(move |_| {
                        window.close();
                        (action)();
                    });
                }
                list.append(&button);
            }
            if shown == 0 {
                let text = if loading.get() {
                    "Scanning files\u{2026}"
                } else {
                    "No matches"
                };
                let empty = gtk::Label::new(Some(text));
                empty.add_css_class("lc-dim");
                empty.add_css_class("lc-tiny");
                empty.set_margin_top(16);
                list.append(&empty);
            }
            if let Some(first) = list.row_at_index(0) {
                list.select_row(Some(&first));
            }
        })
    };

    let set_loading = {
        let loading = loading.clone();
        let rebuild = rebuild.clone();
        Rc::new(move |value: bool| {
            loading.set(value);
            rebuild("");
        })
    };
    let set_items = {
        let items = items.clone();
        let loading = loading.clone();
        let rebuild = rebuild.clone();
        Rc::new(move |next: Vec<Item>| {
            *items.borrow_mut() = next;
            loading.set(false);
            rebuild("");
        })
    };

    {
        let rebuild = rebuild.clone();
        search.connect_search_changed(move |entry| rebuild(&entry.text()));
    }

    // Activating a row must honour the keyboard selection, not just the first
    // match, and must never run twice if both the entry and the window see the
    // same Return press.
    let fired = Rc::new(Cell::new(false));
    let activate_selected: Rc<dyn Fn()> = {
        let list = list.clone();
        let first_actions = first_actions.clone();
        let window = window.clone();
        let fired = fired.clone();
        Rc::new(move || {
            if fired.replace(true) {
                return;
            }
            let index = list.selected_row().map(|row| row.index()).unwrap_or(0) as usize;
            let action = first_actions.borrow().get(index).cloned();
            if let Some(action) = action {
                window.close();
                (action)();
            } else {
                fired.set(false);
            }
        })
    };
    {
        let activate_selected = activate_selected.clone();
        search.connect_activate(move |_| (activate_selected)());
    }
    let key = gtk::EventControllerKey::new();
    {
        let list = list.clone();
        let window = window.clone();
        let activate_selected = activate_selected.clone();
        key.connect_key_pressed(move |_, keyval, _, _| match keyval {
            gtk::gdk::Key::Escape => {
                window.close();
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Down => {
                let current = list.selected_row().map(|row| row.index()).unwrap_or(0);
                let next = current + 1;
                if let Some(row) = list.row_at_index(next) {
                    list.select_row(Some(&row));
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Up => {
                let current = list.selected_row().map(|row| row.index()).unwrap_or(0);
                let previous = (current - 1).max(0);
                if let Some(row) = list.row_at_index(previous) {
                    list.select_row(Some(&row));
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => {
                (activate_selected)();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
    }
    window.add_controller(key);

    window.set_child(Some(&root));
    Palette {
        window,
        search,
        set_items,
        set_loading,
    }
}
use std::cell::{Cell, RefCell};

fn collect_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth >= 12 || out.len() > 4_000 {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') && name != ".github" && name != ".link" {
            continue;
        }
        if SKIP.contains(&name.as_str()) {
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
