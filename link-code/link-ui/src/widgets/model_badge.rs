//! The model selector popover.
//!
//! One popover serves both the header model button and the chip inside the
//! composer: a title row with a Providers shortcut, horizontally scrolling
//! provider tabs, a search field and a scrollable list of model cards. The
//! selected card keeps an accent tint and a thin accent border rather than a
//! bright fill.

use std::rc::Rc;

use gtk::prelude::*;

use link_models::registry::{ModelConfig, ModelRole, ProviderKind};

use crate::state::UiState;

pub fn show_picker(state: &Rc<UiState>, anchor: &gtk::Widget) {
    let popover = gtk::Popover::new();
    popover.add_css_class("lc-model-popover");
    popover.set_parent(anchor);
    popover.set_has_arrow(false);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.add_css_class("lc-model-popover-inner");
    content.set_width_request(380);

    let models = Rc::new(state.registry.borrow().models.clone());
    let active = state.chat_model().map(|model| model.id);
    let chat_role = state
        .registry
        .borrow()
        .roles
        .get(ModelRole::Chat.key())
        .cloned();

    // ---- title row ------------------------------------------------------
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 1);
    titles.set_hexpand(true);
    let title = gtk::Label::new(Some("Select model"));
    title.add_css_class("lc-model-title");
    title.set_xalign(0.0);
    titles.append(&title);
    let mut unique_providers: Vec<ProviderKind> = Vec::new();
    for model in models.iter() {
        if !unique_providers.contains(&model.provider) {
            unique_providers.push(model.provider);
        }
    }
    let providers = unique_providers.len();
    let subtitle = gtk::Label::new(Some(&format!(
        "{} providers \u{00b7} {} models",
        providers,
        models.len()
    )));
    subtitle.add_css_class("lc-model-subtitle");
    subtitle.set_xalign(0.0);
    titles.append(&subtitle);
    head.append(&titles);

    let providers_button = gtk::Button::with_label("Providers");
    providers_button.add_css_class("lc-accent-outline");
    {
        let state = state.clone();
        let popover = popover.clone();
        providers_button.connect_clicked(move |_| {
            popover.popdown();
            crate::dialogs::models::present(&state);
        });
    }
    head.append(&providers_button);
    content.append(&head);

    // ---- search ---------------------------------------------------------
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search models\u{2026}"));
    search.add_css_class("lc-input");
    content.append(&search);

    // ---- provider tabs --------------------------------------------------
    let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tabs.add_css_class("lc-provider-tabs");
    let tabs_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&tabs)
        .build();
    let hosted = provider_tab("Hosted");
    let free = provider_tab("Free Models");
    let mine = provider_tab("My Agent");
    free.set_group(Some(&hosted));
    mine.set_group(Some(&hosted));
    hosted.set_active(true);
    tabs.append(&hosted);
    tabs.append(&free);
    tabs.append(&mine);
    content.append(&tabs_scroll);

    // ---- model cards ----------------------------------------------------
    let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    list.set_margin_top(2);

    let rebuild: Rc<dyn Fn(&str, &str)> = {
        let list = list.clone();
        let models = models.clone();
        let active = active.clone();
        let chat_role = chat_role.clone();
        let popover = popover.clone();
        let state = state.clone();
        Rc::new(move |tab: &str, query: &str| {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let query = query.trim().to_lowercase();
            let mut any = false;
            for model in models.iter() {
                if !matches_tab(model, tab) {
                    continue;
                }
                let haystack = format!(
                    "{} {} {}",
                    model.display_name,
                    model.model_id,
                    model.provider.label()
                )
                .to_lowercase();
                if !query.is_empty() && !haystack.contains(&query) {
                    continue;
                }
                any = true;
                let selected = active.as_deref() == Some(model.id.as_str());
                let card = model_card(
                    &state,
                    &popover,
                    model,
                    selected,
                    chat_role.as_deref() == Some(model.id.as_str()),
                );
                list.append(&card);
            }
            if !any {
                let empty = gtk::Label::new(Some("No models match."));
                empty.add_css_class("lc-dim");
                empty.add_css_class("lc-tiny");
                empty.set_margin_top(12);
                empty.set_margin_bottom(12);
                empty.set_xalign(0.5);
                list.append(&empty);
            }
        })
    };

    {
        let rebuild = rebuild.clone();
        let h = hosted.clone();
        let f = free.clone();
        let m = mine.clone();
        let s = search.clone();
        let rerun = Rc::new(move || {
            let tab = if h.is_active() {
                "Hosted"
            } else if f.is_active() {
                "Free Models"
            } else if m.is_active() {
                "My Agent"
            } else {
                "Hosted"
            };
            rebuild(tab, &s.text());
        });
        {
            let rerun = rerun.clone();
            hosted.connect_toggled(move |button| {
                if button.is_active() {
                    rerun();
                }
            });
        }
        {
            let rerun = rerun.clone();
            free.connect_toggled(move |button| {
                if button.is_active() {
                    rerun();
                }
            });
        }
        {
            let rerun = rerun.clone();
            mine.connect_toggled(move |button| {
                if button.is_active() {
                    rerun();
                }
            });
        }
        {
            let rerun = rerun.clone();
            search.connect_search_changed(move |_| rerun());
        }
        rerun();
    }

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .max_content_height(420)
        .propagate_natural_height(true)
        .child(&list)
        .build();
    content.append(&scroll);

    popover.set_child(Some(&content));
    // Popovers are parented manually; drop them when dismissed so repeated
    // clicks do not accumulate orphaned widgets.
    popover.connect_closed(|popover| popover.unparent());
    popover.popup();
}

/// True when the model belongs in the given provider tab. The grouping is
/// deliberately simple: local Ollama is "Free Models", everything hosted is
/// "Hosted", and "My Agent" shows every configured endpoint.
fn matches_tab(model: &ModelConfig, tab: &str) -> bool {
    match tab {
        "Hosted" => model.provider != ProviderKind::Ollama,
        "Free Models" => model.provider == ProviderKind::Ollama,
        _ => true,
    }
}

fn provider_tab(label: &str) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::with_label(label);
    button.add_css_class("lc-provider-tab");
    button
}

fn model_card(
    state: &Rc<UiState>,
    popover: &gtk::Popover,
    model: &ModelConfig,
    selected: bool,
    is_role: bool,
) -> gtk::Button {
    let card = gtk::Button::new();
    card.add_css_class("lc-model-card");
    card.set_has_frame(false);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);

    let tile = gtk::Box::new(gtk::Orientation::Vertical, 0);
    tile.add_css_class("lc-model-icon");
    tile.add_css_class(provider_class(model.provider));
    let glyph = gtk::Label::new(Some(provider_glyph(model.provider)));
    glyph.add_css_class("lc-mono");
    glyph.set_halign(gtk::Align::Center);
    glyph.set_valign(gtk::Align::Center);
    tile.append(&glyph);
    row.append(&tile);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    text.set_hexpand(true);

    let name_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let name = gtk::Label::new(Some(&model.display_name));
    name.add_css_class("lc-model-name");
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name_row.append(&name);
    if model.capabilities.tools {
        let wrench = gtk::Image::from_icon_name("applications-engineering-symbolic");
        wrench.add_css_class("lc-model-wrench");
        wrench.set_pixel_size(12);
        wrench.set_tooltip_text(Some("Tool calling"));
        name_row.append(&wrench);
    }
    if is_role {
        let badge = gtk::Label::new(Some("chat"));
        badge.add_css_class("lc-badge");
        badge.add_css_class("lc-badge-accent");
        name_row.append(&badge);
    }
    text.append(&name_row);

    let id = gtk::Label::new(Some(&model.model_id));
    id.add_css_class("lc-model-id");
    id.set_xalign(0.0);
    id.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    text.append(&id);

    let meta = gtk::Label::new(Some(&format!(
        "{} \u{00b7} {} ctx",
        model.provider.label(),
        human_tokens(model.context_window)
    )));
    meta.add_css_class("lc-model-ctx");
    meta.set_xalign(0.0);
    text.append(&meta);

    row.append(&text);

    let radio = gtk::Box::new(gtk::Orientation::Vertical, 0);
    radio.add_css_class("lc-model-radio");
    radio.set_valign(gtk::Align::Center);
    if selected {
        let check = gtk::Label::new(Some("\u{2713}"));
        check.add_css_class("lc-tiny");
        check.set_halign(gtk::Align::Center);
        check.set_valign(gtk::Align::Center);
        radio.append(&check);
        card.add_css_class("lc-model-card-selected");
    }
    row.append(&radio);

    card.set_child(Some(&row));
    {
        let state = state.clone();
        let popover = popover.clone();
        let id = model.id.clone();
        card.connect_clicked(move |_| {
            state.set_chat_model(&id);
            state.toast(&format!("Chat model: {}", state.chat_model_label()));
            popover.popdown();
        });
    }
    card
}

fn provider_glyph(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Anthropic => "A",
        ProviderKind::Gemini => "G",
        ProviderKind::Ollama => "O",
        ProviderKind::OpenAiCompatible => "\u{25c6}",
    }
}

fn provider_class(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::Gemini => "gemini",
        ProviderKind::Ollama => "ollama",
        ProviderKind::OpenAiCompatible => "openai",
    }
}

fn human_tokens(value: usize) -> String {
    let value = value as f64;
    if value >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value >= 1_000.0 {
        format!("{:.0}k", value / 1_000.0)
    } else {
        format!("{}", value as usize)
    }
}