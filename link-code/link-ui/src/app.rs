//! Application bootstrap: the welcome screen, global actions and preferences.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::{gio, Orientation};

use link_core::config::{Config, ConfigStore};

use crate::state::UiState;
use crate::window::LinkWindow;
use crate::{style, APP_ID};

/// Run the application. Blocks until the last window closes.
pub fn run() -> glib::ExitCode {
    // Log to stderr with a filter that keeps Link Code's own messages visible
    // and silences the noise from dependencies.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("link_core=info,link_models=info,link_tools=info,link_ui=info,warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();

    let application = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    application.connect_startup(|_| {
        style::install();
    });

    application.connect_activate(|application| {
        let config_store = ConfigStore::default_location();
        let mut config = config_store.load_or_default();
        // Upgrade the old 40-iteration default to 80 for better task completion.
        if config.agent.max_tool_iterations == 40 {
            config.agent.max_tool_iterations = 80;
        }
        style::apply_preferences(&config.appearance);
        if let Some(path) = config.last_workspace.clone().filter(|path| path.is_dir()) {
            if config.general.reopen_last_workspace {
                LinkWindow::open(application, UiState::new(config_store, config), Some(path)).present();
                return;
            }
        }
        // Otherwise: welcome screen.
        LinkWindow::new(application, UiState::new(config_store, config)).present();
    });

    application.connect_open(|application, files, _hint| {
        let config_store = ConfigStore::default_location();
        let mut config = config_store.load_or_default();
        if config.agent.max_tool_iterations == 40 {
            config.agent.max_tool_iterations = 80;
        }
        style::apply_preferences(&config.appearance);
        let path = files.first().and_then(|file| file.path());
        LinkWindow::open(application, UiState::new(config_store, config), path).present();
    });

    application.run()
}

/// `link-code [path]` entry point used by the binary, so a CLI argument opens a
/// folder directly.
pub fn run_with_paths(paths: Vec<PathBuf>) -> glib::ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("link_core=info,link_models=info,link_tools=info,link_ui=info,warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();

    let application = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    application.connect_startup(|_| style::install());

    let paths = Rc::new(RefCell::new(paths));
    application.connect_activate(move |application| {
        let config_store = ConfigStore::default_location();
        let mut config = config_store.load_or_default();
        if config.agent.max_tool_iterations == 40 {
            config.agent.max_tool_iterations = 80;
        }
        style::apply_preferences(&config.appearance);
        let state = UiState::new(config_store, config);
        let path = paths.borrow_mut().drain(..).next();
        match path.or_else(|| {
            let config = state.config.borrow().clone();
            config.last_workspace.clone()
        }) {
            Some(path) if path.is_dir() => LinkWindow::open(application, state, Some(path)).present(),
            _ => LinkWindow::new(application, state).present(),
        }
    });

    application.connect_open(move |application, files, _hint| {
        let config_store = ConfigStore::default_location();
        let mut config = config_store.load_or_default();
        if config.agent.max_tool_iterations == 40 {
            config.agent.max_tool_iterations = 80;
        }
        style::apply_preferences(&config.appearance);
        let state = UiState::new(config_store, config);
        let path = files.first().and_then(|file| file.path());
        LinkWindow::open(application, state, path).present();
    });

    application.run()
}

/// Register the default accelerators, so everything is remappable in one place.
pub fn install_accels(application: &adw::Application, config: &Config) {
    // (config key, window action, fallback accelerators)
    let bindings: [(&str, &str, &str); 14] = [
        ("app.command_palette", "win.command-palette", "<Control><Shift>p|<Control>k"),
        ("file.open_folder", "win.open-folder", "<Control>o"),
        ("file.quick_open", "win.quick-open", "<Control>p"),
        ("file.save", "win.save", "<Control>s"),
        ("agent.focus", "win.focus-agent", "<Control>l"),
        ("agent.new_session", "win.new-session", "<Control><Shift>n"),
        ("view.toggle_sidebar", "win.toggle-sidebar", "<Control>b"),
        ("view.toggle_bottom", "win.toggle-bottom", "<Control>j"),
        ("view.toggle_agent", "win.toggle-agent", "<Control>backslash"),
        ("view.toggle_wrap", "win.toggle-wrap", "<Alt>z"),
        ("app.settings", "win.settings", "<Control>comma"),
        ("app.models", "win.models", "<Control>m"),
        ("agent.cycle_mode", "win.cycle-mode", "<Shift>Tab"),
        ("agent.stop", "win.stop", "Escape"),
    ];
    for (command, action, fallback) in bindings {
        let configured = config.keybindings.get(command).map(String::as_str).unwrap_or("");
        // The config always contains the backend defaults, so merge the
        // built-in shortcuts with the saved ones: a documented accelerator
        // like Ctrl+K must keep working even when the config lists only the
        // other key for the same action.
        let mut accels: Vec<&str> = Vec::new();
        for key in configured.split('|').chain(fallback.split('|')) {
            let key = key.trim();
            if !key.is_empty() && !accels.iter().any(|existing| existing.eq_ignore_ascii_case(key)) {
                accels.push(key);
            }
        }
        application.set_accels_for_action(action, &accels);
    }
}

/// The `agent.send` shortcut (Ctrl+Enter) is installed by the composer itself.
pub fn send_shortcut(config: &Config) -> String {
    config
        .keybindings
        .get("agent.send")
        .cloned()
        .unwrap_or_else(|| "<Control>Return".to_string())
}

/// Kept for completeness: the orientation helper used by split view.
pub fn orientation_name(orientation: Orientation) -> &'static str {
    match orientation {
        Orientation::Horizontal => "horizontal",
        _ => "vertical",
    }
}
