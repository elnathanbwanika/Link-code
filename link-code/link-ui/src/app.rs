//! Application bootstrap: the welcome screen, global actions and preferences.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::{gio, Orientation};

use link_core::config::{Config, ConfigStore};

use crate::window::{LinkWindow, UiState};
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
        let config = config_store.load_or_default();
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
        let config = config_store.load_or_default();
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
        let config = config_store.load_or_default();
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
        let config = config_store.load_or_default();
        let state = UiState::new(config_store, config);
        let path = files.first().and_then(|file| file.path());
        LinkWindow::open(application, state, path).present();
    });

    application.run()
}

/// Register the default accelerators, so everything is remappable in one place.
pub fn install_accels(application: &adw::Application, config: &Config) {
    let accel = |command: &str, fallback: &str| -> Vec<String> {
        config
            .keybindings
            .get(command)
            .cloned()
            .unwrap_or_else(|| fallback.to_string())
            .split('|')
            .map(str::to_string)
            .collect()
    };
    let bindings: [(&str, &str); 12] = [
        ("app.command_palette", "<Control><Shift>p|<Control>k"),
        ("file.open_folder", "<Control>o"),
        ("file.quick_open", "<Control>p"),
        ("file.save", "<Control>s"),
        ("agent.focus", "<Control>l"),
        ("view.toggle_sidebar", "<Control>b"),
        ("view.toggle_bottom", "<Control>j"),
        ("view.toggle_agent", "<Control>backslash"),
        ("app.settings", "<Control>comma"),
        ("app.models", "<Control>m"),
        ("agent.cycle_mode", "<Shift>Tab"),
        ("agent.stop", "Escape"),
    ];
    for (command, fallback) in bindings {
        application.set_accels_for_action(command, &accel(command, fallback).iter().map(String::as_str).collect::<Vec<_>>());
    }
}

/// Kept for completeness: the orientation helper used by split view.
pub fn orientation_name(orientation: Orientation) -> &'static str {
    match orientation {
        Orientation::Horizontal => "horizontal",
        _ => "vertical",
    }
}
