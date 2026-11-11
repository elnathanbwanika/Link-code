//! The application window and its workspace state.
//!
//! The original UI shell referred to panels that were never included in the
//! source tree. This module owns a small, complete window while those panels
//! are developed as separate units.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::Orientation;
use link_core::config::{Config, ConfigStore};

/// State shared by a window and its callbacks.
pub struct UiState {
    pub config_store: ConfigStore,
    pub config: RefCell<Config>,
    pub workspace: RefCell<Option<PathBuf>>,
}

impl UiState {
    pub fn new(config_store: ConfigStore, config: Config) -> Rc<Self> {
        Rc::new(Self {
            config_store,
            config: RefCell::new(config),
            workspace: RefCell::new(None),
        })
    }

    pub fn set_workspace(&self, path: &Path) -> Result<(), String> {
        let path = path
            .canonicalize()
            .map_err(|error| format!("could not open {}: {error}", path.display()))?;
        if !path.is_dir() {
            return Err(format!("{} is not a folder", path.display()));
        }

        *self.workspace.borrow_mut() = Some(path.clone());
        let mut config = self.config.borrow_mut();
        config.last_workspace = Some(path.clone());
        config
            .general
            .recent_workspaces
            .retain(|entry| entry != &path);
        config.general.recent_workspaces.insert(0, path);
        let limit = config.general.recent_limit;
        config.general.recent_workspaces.truncate(limit);
        if let Err(error) = self.config_store.save(&config) {
            tracing::warn!(?error, "could not save the selected workspace");
        }
        Ok(())
    }
}

/// A complete, lightweight application window.
pub struct LinkWindow {
    window: adw::ApplicationWindow,
    _state: Rc<UiState>,
}

impl LinkWindow {
    pub fn new(application: &adw::Application, state: Rc<UiState>) -> Self {
        Self::build(application, state, None)
    }

    pub fn open(
        application: &adw::Application,
        state: Rc<UiState>,
        workspace: Option<PathBuf>,
    ) -> Self {
        let workspace = workspace.and_then(|path| match state.set_workspace(&path) {
            Ok(()) => Some(path),
            Err(error) => {
                tracing::warn!(%error, "could not open workspace");
                None
            }
        });
        Self::build(application, state, workspace)
    }

    fn build(
        application: &adw::Application,
        state: Rc<UiState>,
        workspace: Option<PathBuf>,
    ) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("Link Code")
            .default_width(1_080)
            .default_height(720)
            .build();

        let header = adw::HeaderBar::new();
        let heading = gtk::Label::new(Some("LINK CODE"));
        heading.add_css_class("title-2");
        header.set_title_widget(Some(&heading));
        window.set_titlebar(Some(&header));

        let content = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(12)
            .margin_top(32)
            .margin_bottom(32)
            .margin_start(40)
            .margin_end(40)
            .valign(gtk::Align::Center)
            .build();
        let title = gtk::Label::new(Some("Link Code"));
        title.add_css_class("title-1");
        let message = match workspace {
            Some(path) => format!("Workspace: {}", path.display()),
            None => "Open Link Code with a folder path to select a workspace.".to_string(),
        };
        let detail = gtk::Label::new(Some(&message));
        detail.set_wrap(true);
        detail.add_css_class("dim-label");
        content.append(&title);
        content.append(&detail);
        window.set_content(Some(&content));

        Self {
            window,
            _state: state,
        }
    }

    pub fn present(&self) {
        self.window.present();
    }
}
