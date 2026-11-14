//! The main window: header bar, icon rail, three columns and the status bar.
//!
//! Layout:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │ HeaderBar                                                    │
//! ├────┬───────────┬──────────────────────┬─────────────────────┤
//! │rail│ sidebar   │ editor               │ agent               │
//! │56px│ (files /  │──────────────────────│ chat + composer     │
//! │    │ history)  │ bottom panel         │                     │
//! ├────┴───────────┴──────────────────────┴─────────────────────┤
//! │ Status bar: workspace · model · mode · tokens · cost        │
//! └──────────────────────────────────────────────────────────────┘
//! ```

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

use link_core::session::Session;

use crate::panels;
use crate::state::{UiState, Widgets};
use crate::widgets;

/// Default window size: roughly a large terminal window, not a full-screen IDE.
const DEFAULT_WIDTH: i32 = 960;
const DEFAULT_HEIGHT: i32 = 620;
/// Smallest size the three columns stay usable at.
const MIN_WIDTH: i32 = 960;
const MIN_HEIGHT: i32 = 480;

/// Minimum widths for the three columns, in pixels.
const SIDEBAR_MIN: i32 = 200;
const EDITOR_MIN: i32 = 360;
const AGENT_MIN: i32 = 300;
/// Height kept for the editor and the bottom panel.
const EDITOR_MIN_HEIGHT: i32 = 180;
const BOTTOM_MIN_HEIGHT: i32 = 130;

fn clamp_paned(paned: &gtk::Paned, start_min: i32, end_min: i32) {
    let total = if paned.orientation() == gtk::Orientation::Horizontal {
        paned.width()
    } else {
        paned.height()
    };
    if total <= 0 {
        return;
    }
    let max = (total - end_min).max(start_min);
    let target = paned.position().clamp(start_min, max);
    if target != paned.position() {
        paned.set_position(target);
    }
}

pub struct LinkWindow {
    window: adw::ApplicationWindow,
    _state: Rc<UiState>,
}

impl LinkWindow {
    pub fn new(application: &adw::Application, state: Rc<UiState>) -> Self {
        Self::build(application, state)
    }

    pub fn open(
        application: &adw::Application,
        state: Rc<UiState>,
        workspace: Option<PathBuf>,
    ) -> Self {
        if let Some(path) = workspace {
            if let Err(error) = state.set_workspace(&path) {
                tracing::warn!(%error, "could not open workspace");
                state.toast(&error);
            }
        }
        Self::build(application, state)
    }

    fn build(application: &adw::Application, state: Rc<UiState>) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("Link Code")
            .default_width(DEFAULT_WIDTH)
            .default_height(DEFAULT_HEIGHT)
            .build();
        window.set_size_request(MIN_WIDTH, MIN_HEIGHT);
        window.add_css_class("lc-root");

        // Do not swallow the screen on a small display: shrink the default
        // size to fit inside the monitor with some margin to spare.
        if let Some(monitor) = gtk::prelude::WidgetExt::display(&window)
            .monitors()
            .item(0)
            .and_then(|item| item.downcast::<gtk::gdk::Monitor>().ok())
        {
            let geometry = monitor.geometry();
            let width = (geometry.width() - 160).max(MIN_WIDTH);
            let height = (geometry.height() - 160).max(MIN_HEIGHT);
            if width < DEFAULT_WIDTH || height < DEFAULT_HEIGHT {
                window.set_default_size(DEFAULT_WIDTH.min(width), DEFAULT_HEIGHT.min(height));
            }
        }

        let toolbar = adw::ToolbarView::new();

        // ---- header ------------------------------------------------------
        let header = gtk::HeaderBar::new();
        header.add_css_class("lc-header");
        header.set_show_title_buttons(true);

        let logo = gtk::Label::new(Some("\u{25c6} LINK CODE"));
        logo.add_css_class("lc-title");
        logo.add_css_class("lc-accent");
        header.set_title_widget(Some(&logo));

        let open_folder = gtk::Button::from_icon_name("folder-open-symbolic");
        open_folder.add_css_class("lc-header-button");
        open_folder.set_tooltip_text(Some("Open folder (Ctrl+O)"));
        {
            let state = state.clone();
            open_folder.connect_clicked(move |_| open_folder_dialog(&state));
        }
        header.pack_start(&open_folder);

        let new_chat = gtk::Button::from_icon_name("list-add-symbolic");
        new_chat.add_css_class("lc-header-button");
        new_chat.set_tooltip_text(Some("New session"));
        {
            let state = state.clone();
            new_chat.connect_clicked(move |_| new_session(&state));
        }
        header.pack_start(&new_chat);

        let model_button = gtk::Button::new();
        widgets::set_button_text_for(
            &model_button,
            "no model",
            24,
            gtk::pango::EllipsizeMode::End,
        );
        model_button.add_css_class("lc-ghost");
        model_button.add_css_class("lc-mono");
        model_button.add_css_class("lc-tiny");
        {
            let state = state.clone();
            model_button.connect_clicked(move |button| {
                widgets::model_badge::show_picker(&state, button.upcast_ref());
            });
        }
        header.pack_end(&model_button);

        let palette = gtk::Button::from_icon_name("system-search-symbolic");
        palette.add_css_class("lc-header-button");
        palette.set_tooltip_text(Some("Command palette (Ctrl+K)"));
        {
            let state = state.clone();
            palette.connect_clicked(move |_| crate::dialogs::command_palette::present(&state));
        }
        header.pack_end(&palette);

        let models = gtk::Button::from_icon_name("applications-system-symbolic");
        models.add_css_class("lc-header-button");
        models.set_tooltip_text(Some("Models (Ctrl+M)"));
        {
            let state = state.clone();
            models.connect_clicked(move |_| crate::dialogs::models::present(&state));
        }
        header.pack_end(&models);

        let settings = gtk::Button::from_icon_name("emblem-system-symbolic");
        settings.add_css_class("lc-header-button");
        settings.set_tooltip_text(Some("Settings (Ctrl+,)"));
        {
            let state = state.clone();
            settings.connect_clicked(move |_| crate::dialogs::settings::present(&state));
        }
        header.pack_end(&settings);

        toolbar.add_top_bar(&header);

        // ---- rail --------------------------------------------------------
        let rail = gtk::Box::new(gtk::Orientation::Vertical, 4);
        rail.add_css_class("lc-rail");
        rail.set_width_request(56);
        rail.set_margin_top(8);
        rail.set_margin_bottom(8);

        let rail_explorer = rail_button("view-list-symbolic", "Explorer (Ctrl+B)");
        let rail_history = rail_button("document-open-recent-symbolic", "Session history");
        let rail_agent = rail_button("utilities-terminal-symbolic", "Agent (Ctrl+\\)");
        let rail_bottom = rail_button("utilities-system-monitor-symbolic", "Terminal (Ctrl+J)");
        rail.append(&rail_explorer);
        rail.append(&rail_history);
        rail.append(&rail_agent);
        rail.append(&rail_bottom);
        let rail_spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        rail_spacer.set_vexpand(true);
        rail.append(&rail_spacer);
        let rail_settings = rail_button("emblem-system-symbolic", "Settings (Ctrl+,)");
        rail.append(&rail_settings);

        // ---- panels ------------------------------------------------------
        let sidebar = panels::sidebar::build(&state);
        let editor = panels::editor::build(&state);
        let bottom = panels::bottom::build(&state);
        let agent = panels::agent::build(&state);

        // Each column owns a minimum width and clips its own overflow. Without
        // this a long unwrapped line hands the editor a huge minimum width,
        // which pushes the agent panel over it instead of scrolling.
        sidebar.root.set_size_request(SIDEBAR_MIN, -1);
        editor.root.set_size_request(EDITOR_MIN, -1);
        agent.root.set_size_request(AGENT_MIN, -1);
        for root in [
            &sidebar.root,
            &editor.root,
            &bottom.root,
            &agent.root,
        ] {
            root.set_overflow(gtk::Overflow::Hidden);
        }

        let v_paned = gtk::Paned::new(gtk::Orientation::Vertical);
        v_paned.set_start_child(Some(&editor.root));
        v_paned.set_end_child(Some(&bottom.root));
        v_paned.set_position(400);
        v_paned.set_resize_start_child(true);
        v_paned.set_shrink_start_child(false);
        v_paned.set_resize_end_child(false);
        v_paned.set_shrink_end_child(false);

        let agent_paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        agent_paned.set_start_child(Some(&v_paned));
        agent_paned.set_end_child(Some(&agent.root));
        agent_paned.set_position(620);
        agent_paned.set_resize_start_child(true);
        agent_paned.set_shrink_start_child(false);
        agent_paned.set_resize_end_child(false);
        agent_paned.set_shrink_end_child(false);

        let h_paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        h_paned.set_start_child(Some(&sidebar.root));
        h_paned.set_end_child(Some(&agent_paned));
        h_paned.set_position(220);
        h_paned.set_resize_start_child(false);
        h_paned.set_shrink_start_child(false);
        h_paned.set_resize_end_child(true);
        h_paned.set_shrink_end_child(false);

        let columns = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        columns.set_vexpand(true);
        columns.append(&rail);
        h_paned.set_hexpand(true);
        columns.append(&h_paned);
        toolbar.set_content(Some(&columns));

        // ---- status bar ---------------------------------------------------
        let status = widgets::status_bar::build(&state);
        toolbar.add_bottom_bar(&status.root);

        // ---- install state ------------------------------------------------
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&toolbar));
        window.set_content(Some(&toasts));

        let widgets = Rc::new(Widgets {
            window: window.clone(),
            h_paned: h_paned.clone(),
            agent_paned: agent_paned.clone(),
            v_paned: v_paned.clone(),
            sidebar_root: sidebar.root.clone(),
            editor_root: editor.root.clone(),
            bottom_root: bottom.root.clone(),
            agent_root: agent.root.clone(),
            rail: rail.clone(),
            sidebar: sidebar.clone(),
            editor: editor.clone(),
            bottom: bottom.clone(),
            chat_list: agent.chat_list.clone(),
            chat_scroll: agent.chat_scroll.clone(),
            activity: agent.activity.clone(),
            activity_spinner: agent.activity_spinner.clone(),
            activity_label: agent.activity_label.clone(),
            input: agent.input.clone(),
            send_button: agent.send_button.clone(),
            stop_button: agent.stop_button.clone(),
            mode_button: agent.mode_button.clone(),
            model_button: model_button.clone(),
            context_bar: agent.context_bar.clone(),
            tasks: agent.tasks.clone(),
            suggestions_box: agent.suggestions_box.clone(),
            attachments_box: agent.attachments_box.clone(),
            attachments_scroll: agent.attachments_scroll.clone(),
            status_workspace: status.workspace.clone(),
            status_model: status.model.clone(),
            status_mode: status.mode.clone(),
            status_tokens: status.tokens.clone(),
            status_cost: status.cost.clone(),
            status_cost_sep: status.cost_sep.clone(),
            toasts: toasts.clone(),
            rail_explorer: rail_explorer.clone(),
            rail_history: rail_history.clone(),
            rail_agent: rail_agent.clone(),
            rail_bottom: rail_bottom.clone(),
        });
        *state.widgets.borrow_mut() = Some(widgets.clone());

        // Restore the remembered column widths, then keep them usable as the
        // window shrinks. Every position is clamped so one column can never
        // eat the minimum width of its neighbour.
        {
            let state = state.clone();
            let fitted = Rc::new(std::cell::Cell::new(false));
            window.connect_map(move |_| {
                if fitted.replace(true) {
                    return;
                }
                // `GtkWindow` has not been allocated at map time, so measure
                // after the first layout pass instead.
                let state = state.clone();
                glib::idle_add_local_once(move || {
                    let Some(widgets) = state.widgets() else {
                        return;
                    };
                    let width = if widgets.window.width() > 0 {
                        widgets.window.width()
                    } else {
                        DEFAULT_WIDTH
                    };
                    let height = if widgets.window.height() > 0 {
                        widgets.window.height()
                    } else {
                        DEFAULT_HEIGHT
                    };
                    let layout = state.config.borrow().layout.clone();

                    let sidebar = layout
                        .get("sidebar")
                        .copied()
                        .unwrap_or_else(|| (width / 4).clamp(SIDEBAR_MIN, 240));
                    let max_sidebar =
                        (width - 56 - EDITOR_MIN - AGENT_MIN - 10).max(SIDEBAR_MIN);
                    widgets
                        .h_paned
                        .set_position(sidebar.clamp(SIDEBAR_MIN, max_sidebar));

                    let columns = if widgets.agent_paned.width() > 0 {
                        widgets.agent_paned.width()
                    } else {
                        width - 56 - sidebar
                    };
                    let agent = layout.get("agent").copied().unwrap_or_else(|| {
                        (columns / 3).clamp(AGENT_MIN, 360)
                    });
                    let max_agent = (columns - EDITOR_MIN - 8).max(AGENT_MIN);
                    widgets
                        .agent_paned
                        .set_position((columns - agent.clamp(AGENT_MIN, max_agent)).max(EDITOR_MIN));

                    let vheight = if widgets.v_paned.height() > 0 {
                        widgets.v_paned.height()
                    } else {
                        (height - 90).max(EDITOR_MIN_HEIGHT + BOTTOM_MIN_HEIGHT)
                    };
                    let bottom = layout.get("bottom").copied().unwrap_or_else(|| {
                        (vheight / 4).clamp(BOTTOM_MIN_HEIGHT, 220)
                    });
                    let max_bottom = (vheight - EDITOR_MIN_HEIGHT).max(BOTTOM_MIN_HEIGHT);
                    widgets
                        .v_paned
                        .set_position((vheight - bottom.clamp(BOTTOM_MIN_HEIGHT, max_bottom))
                            .max(EDITOR_MIN_HEIGHT));
                });
            });
        }

        // Persist pane sizes to `config.layout` after a drag, debounced so a
        // drag writes the config once instead of on every pixel.
        {
            let state = state.clone();
            let pending = Rc::new(std::cell::Cell::new(false));
            let schedule_save: Rc<dyn Fn()> = {
                let state = state.clone();
                let pending = pending.clone();
                Rc::new(move || {
                    if pending.replace(true) {
                        return;
                    }
                    let state = state.clone();
                    let pending = pending.clone();
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(450),
                        move || {
                            pending.set(false);
                            let _ = state.save_config();
                        },
                    );
                })
            };

            {
                let state = state.clone();
                let schedule_save = schedule_save.clone();
                h_paned.connect_notify_local(Some("position"), move |paned, _| {
                    clamp_paned(paned, SIDEBAR_MIN, EDITOR_MIN + AGENT_MIN + 8);
                    let position = paned.position();
                    if position > 0 {
                        state
                            .config
                            .borrow_mut()
                            .layout
                            .insert("sidebar".to_string(), position);
                        schedule_save();
                    }
                });
            }
            {
                let state = state.clone();
                let schedule_save = schedule_save.clone();
                agent_paned.connect_notify_local(Some("position"), move |paned, _| {
                    clamp_paned(paned, EDITOR_MIN, AGENT_MIN);
                    let position = paned.position();
                    let width = paned.width();
                    if position > 0 && width > position {
                        state
                            .config
                            .borrow_mut()
                            .layout
                            .insert("agent".to_string(), width - position);
                        schedule_save();
                    }
                });
            }
            {
                let state = state.clone();
                let schedule_save = schedule_save.clone();
                v_paned.connect_notify_local(Some("position"), move |paned, _| {
                    clamp_paned(paned, EDITOR_MIN_HEIGHT, BOTTOM_MIN_HEIGHT);
                    let position = paned.position();
                    let height = paned.height();
                    if position > 0 && height > position {
                        state
                            .config
                            .borrow_mut()
                            .layout
                            .insert("bottom".to_string(), height - position);
                        schedule_save();
                    }
                });
            }

            // A window resize must not leave a pane wider than the space it has.
            h_paned.connect_notify_local(Some("width"), |paned, _| {
                clamp_paned(paned, SIDEBAR_MIN, EDITOR_MIN + AGENT_MIN + 8);
            });
            agent_paned.connect_notify_local(Some("width"), |paned, _| {
                clamp_paned(paned, EDITOR_MIN, AGENT_MIN);
            });
            v_paned.connect_notify_local(Some("height"), |paned, _| {
                clamp_paned(paned, EDITOR_MIN_HEIGHT, BOTTOM_MIN_HEIGHT);
            });
        }

        // ---- wiring ---------------------------------------------------------
        {
            let state = state.clone();
            rail_explorer.connect_clicked(move |_| {
                toggle_sidebar(&state);
                if let Some(widgets) = state.widgets() {
                    widgets.sidebar.files_tab.set_active(true);
                }
            });
        }
        {
            let state = state.clone();
            rail_history.connect_clicked(move |_| {
                if let Some(widgets) = state.widgets() {
                    widgets.sidebar_root.set_visible(true);
                    panels::sidebar::show_history(&state, &widgets.sidebar);
                }
            });
        }
        {
            let state = state.clone();
            rail_agent.connect_clicked(move |_| toggle_agent(&state));
        }
        {
            let state = state.clone();
            rail_bottom.connect_clicked(move |_| {
                if let Some(widgets) = state.widgets() {
                    let visible = widgets.bottom_root.is_visible();
                    widgets.bottom_root.set_visible(!visible);
                    if !visible {
                        widgets.bottom.focus_terminal();
                    }
                }
            });
        }
        {
            let state = state.clone();
            rail_settings.connect_clicked(move |_| crate::dialogs::settings::present(&state));
        }

        register_actions(&window, &state);
        crate::app::install_accels(application, &state.config.borrow().clone());

        // Initial paint.
        state.refresh_model_badges();
        state.refresh_mode_chip();
        if let Some(widgets) = state.widgets() {
            panels::sidebar::refresh_files(&state, &widgets.sidebar);
            panels::sidebar::refresh_history(&state, &widgets.sidebar);
        }
        panels::agent::refresh_composer(&state);
        widgets::status_bar::refresh(&state);

        if state.workspace_path().is_none() {
            glib::idle_add_local_once({
                let state = state.clone();
                move || state.toast("Open a folder with Ctrl+O to get started")
            });
        }

        LinkWindow {
            window,
            _state: state,
        }
    }

    pub fn present(&self) {
        self.window.present();
    }

    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.window
    }
}

fn rail_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("lc-header-button");
    button.set_tooltip_text(Some(tooltip));
    button.set_width_request(34);
    button.set_height_request(34);
    button
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

fn register_actions(window: &adw::ApplicationWindow, state: &Rc<UiState>) {
    let add = |name: &str, callback: Box<dyn Fn()>| {
        let action = gio::SimpleAction::new(name, None);
        action.connect_activate(move |_, _| callback());
        window.add_action(&action);
    };

    {
        let state = state.clone();
        add("toggle-sidebar", Box::new(move || toggle_sidebar(&state)));
    }
    {
        let state = state.clone();
        add("toggle-bottom", Box::new(move || toggle_bottom(&state)));
    }
    {
        let state = state.clone();
        add("toggle-agent", Box::new(move || toggle_agent(&state)));
    }
    {
        let state = state.clone();
        add(
            "toggle-wrap",
            Box::new(move || panels::editor::toggle_word_wrap(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            "command-palette",
            Box::new(move || crate::dialogs::command_palette::present(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            "quick-open",
            Box::new(move || crate::dialogs::command_palette::present_quick_open(&state)),
        );
    }
    {
        let state = state.clone();
        add("open-folder", Box::new(move || open_folder_dialog(&state)));
    }
    {
        let state = state.clone();
        add(
            "save",
            Box::new(move || panels::editor::save_current(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            "settings",
            Box::new(move || crate::dialogs::settings::present(&state)),
        );
    }
    {
        let state = state.clone();
        add(
            "models",
            Box::new(move || crate::dialogs::models::present(&state)),
        );
    }
    {
        let state = state.clone();
        add("cycle-mode", Box::new(move || {
            state.cycle_mode();
            panels::agent::refresh_composer(&state);
        }));
    }
    {
        let state = state.clone();
        add("stop", Box::new(move || panels::agent::stop(&state)));
    }
    {
        let state = state.clone();
        add("focus-agent", Box::new(move || focus_agent(&state)));
    }
    {
        let state = state.clone();
        add("new-session", Box::new(move || new_session(&state)));
    }
}

// ---------------------------------------------------------------------------
// Commands shared with the palette and the rail
// ---------------------------------------------------------------------------

pub fn toggle_sidebar(state: &Rc<UiState>) {
    if let Some(widgets) = state.widgets() {
        let visible = widgets.sidebar_root.is_visible();
        widgets.sidebar_root.set_visible(!visible);
        if !visible {
            widgets.sidebar_root.set_width_request(SIDEBAR_MIN);
        }
    }
}

pub fn toggle_bottom(state: &Rc<UiState>) {
    if let Some(widgets) = state.widgets() {
        let visible = widgets.bottom_root.is_visible();
        widgets.bottom_root.set_visible(!visible);
    }
}

pub fn toggle_agent(state: &Rc<UiState>) {
    if let Some(widgets) = state.widgets() {
        if widgets.agent_root.is_visible() {
            // Remember how wide the panel is so showing it restores it.
            let width = widgets.agent_paned.width();
            let position = widgets.agent_paned.position();
            if width > 0 && position > 0 && position < width {
                state.agent_width.set((width - position).max(AGENT_MIN));
            }
            widgets.agent_root.set_visible(false);
        } else {
            widgets.agent_root.set_visible(true);
            let width = widgets.agent_paned.width();
            if width > 0 {
                let target = (width - state.agent_width.get()).max(EDITOR_MIN);
                widgets.agent_paned.set_position(target.min(width - AGENT_MIN));
            }
        }
    }
}

pub fn focus_agent(state: &Rc<UiState>) {
    if let Some(widgets) = state.widgets() {
        if !widgets.agent_root.is_visible() {
            toggle_agent(state);
        }
        widgets.input.grab_focus();
    }
}

pub fn open_folder_dialog(state: &Rc<UiState>) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Open a folder")
        .modal(true)
        .build();
    let state = state.clone();
    dialog.select_folder(
        Some(&widgets.window),
        None::<&gio::Cancellable>,
        move |result| {
            if let Ok(file) = result {
                if let Some(path) = file.path() {
                    open_workspace(&state, &path);
                }
            }
        },
    );
}

pub fn open_workspace(state: &Rc<UiState>, path: &Path) {
    if let Err(error) = state.set_workspace(path) {
        state.toast(&error);
        return;
    }
    *state.session.borrow_mut() = None;
    state.blocks.borrow_mut().clear();
    state.block_order.borrow_mut().clear();
    state.tools.borrow_mut().clear();
    state.edits.borrow_mut().clear();
    state.context_used.set(0);
    state.context_max.set(0);
    panels::editor::close_all(state);

    if let Some(widgets) = state.widgets() {
        widgets::chat_message::clear(&widgets.chat_list);
        panels::sidebar::refresh_files(state, &widgets.sidebar);
        panels::sidebar::refresh_history(state, &widgets.sidebar);
        widgets.bottom.restart(state);
    }
    widgets::status_bar::refresh(state);
    state.toast(&format!("Opened {}", path.display()));
}

pub fn new_session(state: &Rc<UiState>) {
    state.save_session();
    let Some(workspace) = state.workspace_path() else {
        state.toast("Open a folder first (Ctrl+O)");
        return;
    };
    let session = Session::new(&workspace, state.chat_model_label());
    *state.session.borrow_mut() = Some(session);
    state.blocks.borrow_mut().clear();
    state.block_order.borrow_mut().clear();
    state.tools.borrow_mut().clear();
    state.edits.borrow_mut().clear();
    state.context_used.set(0);
    state.cost.set(0.0);
    *state.turn_text.borrow_mut() = String::new();
    *state.turn_thinking.borrow_mut() = String::new();
    *state.turn_model.borrow_mut() = String::new();

    if let Some(widgets) = state.widgets() {
        widgets::chat_message::clear(&widgets.chat_list);
        widgets.tasks.clear();
        widgets::chat_message::clear(&widgets.suggestions_box);
        widgets.suggestions_box.set_visible(false);
        widgets.input.grab_focus();
        panels::sidebar::refresh_history(state, &widgets.sidebar);
    }
    widgets::status_bar::refresh(state);
    state.toast("New session");
}

pub fn clear_conversation(state: &Rc<UiState>) {
    state.blocks.borrow_mut().clear();
    state.block_order.borrow_mut().clear();
    state.tools.borrow_mut().clear();
    state.edits.borrow_mut().clear();
    if let Some(widgets) = state.widgets() {
        widgets::chat_message::clear(&widgets.chat_list);
        widgets.tasks.clear();
        widgets::chat_message::clear(&widgets.suggestions_box);
        widgets.suggestions_box.set_visible(false);
    }
    if let Some(session) = state.session.borrow_mut().as_mut() {
        session.messages.clear();
    }
    state.save_session();
}
