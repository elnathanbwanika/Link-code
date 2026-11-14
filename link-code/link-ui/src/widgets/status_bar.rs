//! The 26px status bar pinned to the bottom of the window.

use std::rc::Rc;

use gtk::prelude::*;

use crate::state::UiState;

pub struct StatusBar {
    pub root: gtk::Box,
    pub workspace: gtk::Button,
    pub model: gtk::Button,
    pub mode: gtk::Button,
    pub tokens: gtk::Label,
    pub cost: gtk::Label,
    pub cost_sep: gtk::Label,
}

pub fn build(state: &Rc<UiState>) -> StatusBar {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    root.add_css_class("lc-status");
    root.set_margin_start(2);
    root.set_margin_end(2);

    let workspace = gtk::Button::new();
    super::set_button_text(&workspace, "no folder open", 28);
    workspace.add_css_class("lc-header-button");
    workspace.add_css_class("lc-path");
    workspace.set_tooltip_text(Some("Open a folder (Ctrl+O)"));
    {
        let state = state.clone();
        workspace.connect_clicked(move |_| crate::window::open_folder_dialog(&state));
    }

    let model = gtk::Button::new();
    super::set_button_text_for(
        &model,
        "no model",
        24,
        gtk::pango::EllipsizeMode::End,
    );
    model.add_css_class("lc-header-button");
    model.add_css_class("lc-mono");
    {
        let state = state.clone();
        model.connect_clicked(move |button| {
            crate::widgets::model_badge::show_picker(&state, button.upcast_ref());
        });
    }

    let mode = gtk::Button::with_label("Ask");
    mode.add_css_class("lc-header-button");
    mode.set_tooltip_text(Some("Cycle permission mode (Shift+Tab)"));
    {
        let state = state.clone();
        mode.connect_clicked(move |_| state.cycle_mode());
    }

    let tokens = gtk::Label::new(Some("\u{2014} tokens"));
    tokens.add_css_class("lc-mono");
    tokens.add_css_class("lc-tiny");

    let cost = gtk::Label::new(Some(""));
    cost.add_css_class("lc-mono");
    cost.add_css_class("lc-tiny");
    cost.add_css_class("lc-ok");

    let separator = |text: &str| {
        let label = gtk::Label::new(Some(text));
        label.add_css_class("lc-sep");
        label
    };

    root.append(&workspace);
    root.append(&separator("\u{2502}"));
    root.append(&model);
    root.append(&separator("\u{2502}"));
    root.append(&mode);

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    root.append(&spacer);

    root.append(&tokens);
    let cost_sep = separator("\u{2502}");
    root.append(&cost_sep);
    root.append(&cost);

    StatusBar {
        root,
        workspace,
        model,
        mode,
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
    super::set_button_text(&widgets.status_workspace, &workspace, 28);
    widgets
        .status_workspace
        .set_tooltip_text(Some(&workspace));
    super::set_button_text_for(
        &widgets.status_model,
        &state.chat_model_label(),
        24,
        gtk::pango::EllipsizeMode::End,
    );
    widgets.status_mode.set_label(state.mode().label());
    widgets
        .status_mode
        .set_tooltip_text(Some(state.mode().description()));

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
