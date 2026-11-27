//! The compact status bar pinned to the bottom of the window.
//!
//! It carries read-only information. The actions that used to live here are
//! available once each in the header and the composer, so there is a single
//! place to open a folder, pick a model or change the permission mode.

use std::rc::Rc;

use gtk::prelude::*;

use crate::state::UiState;

pub struct StatusBar {
    pub root: gtk::Box,
    pub workspace: gtk::Label,
    pub tokens: gtk::Label,
    pub cost: gtk::Label,
    pub cost_sep: gtk::Label,
}

pub fn build(_state: &Rc<UiState>) -> StatusBar {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    root.add_css_class("lc-status");
    root.set_margin_start(2);
    root.set_margin_end(2);

    let workspace = gtk::Label::new(Some("no folder open"));
    workspace.add_css_class("lc-path");
    workspace.set_xalign(0.0);
    workspace.set_ellipsize(gtk::pango::EllipsizeMode::End);
    workspace.set_width_chars(24);
    workspace.set_tooltip_text(Some("Current workspace"));
    root.append(&workspace);

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    root.append(&spacer);

    let tokens = gtk::Label::new(Some("\u{2014} tokens"));
    tokens.add_css_class("lc-mono");
    tokens.add_css_class("lc-tiny");

    let cost = gtk::Label::new(Some(""));
    cost.add_css_class("lc-mono");
    cost.add_css_class("lc-tiny");
    cost.add_css_class("lc-ok");

    let cost_sep = gtk::Label::new(Some("\u{2502}"));
    cost_sep.add_css_class("lc-sep");

    root.append(&tokens);
    root.append(&cost_sep);
    root.append(&cost);

    StatusBar {
        root,
        workspace,
        tokens,
        cost,
        cost_sep,
    }
}

pub fn refresh(state: &Rc<UiState>) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    let workspace = state
        .workspace_path()
        .map(|path| {
            let text = path.display().to_string();
            if let Some(home) = std::env::var_os("HOME") {
                let home = std::path::PathBuf::from(home);
                if let Ok(rest) = path.strip_prefix(&home) {
                    return format!("~/{}", rest.display());
                }
            }
            text
        })
        .unwrap_or_else(|| "no folder open".to_string());
    widgets.status_workspace.set_label(&workspace);
    widgets.status_workspace.set_tooltip_text(Some(&workspace));

    let used = state.context_used.get();
    let max = state.context_max.get();
    if max > 0 {
        widgets.status_tokens.set_label(&format!(
            "{} / {} tokens ({:.0}%)",
            crate::format_number(used),
            crate::format_number(max),
            (used as f64 / max as f64) * 100.0
        ));
    } else {
        widgets
            .status_tokens
            .set_label(&format!("{} tokens", crate::format_number(used)));
    }

    let cost = state.cost.get();
    if cost > 0.0 {
        widgets.status_cost.set_label(&format!("${cost:.4}"));
        widgets.status_cost.set_visible(true);
        widgets.status_cost_sep.set_visible(true);
    } else {
        widgets.status_cost.set_label("");
        widgets.status_cost.set_visible(false);
        widgets.status_cost_sep.set_visible(false);
    }
}
