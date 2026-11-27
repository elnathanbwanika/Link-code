//! The AI Providers screen and the Add Provider form.
//!
//! The list shows one card per configured endpoint with its provider tile,
//! status badge and current chat role. The `+` opens a separate form built
//! from surface cards: provider type, name, API keys, model and an advanced
//! section. Multiple keys can be stored per base URL; they are written to the
//! OS keyring and never logged.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use link_models::registry::{
    normalize_base_url, ApiKeyEntry, ApiKeyPool, ModelConfig, ModelRegistry, ModelRole,
    ProviderKind, MAX_KEYS_PER_POOL,
};
use link_models::sanitize_api_key;

use crate::state::{runtime, save_registry, UiState};

// ---------------------------------------------------------------------------
// Providers screen
// ---------------------------------------------------------------------------

pub fn present(state: &Rc<UiState>) {
    let window = gtk::Window::builder()
        .title("AI Providers")
        .modal(true)
        .resizable(true)
        .default_width(620)
        .default_height(540)
        .build();
    window.add_css_class("lc-dialog");
    window.add_css_class("lc-providers");
    if let Some(parent) = state.widgets().map(|widgets| widgets.window.clone()) {
        window.set_transient_for(Some(&parent));
    }
    add_escape(&window);

    // ---- header ---------------------------------------------------------
    let header = gtk::HeaderBar::new();
    let back = gtk::Button::from_icon_name("go-previous-symbolic");
    back.add_css_class("lc-icon-btn");
    back.add_css_class("flat");
    back.set_tooltip_text(Some("Back"));
    {
        let window = window.clone();
        back.connect_clicked(move |_| window.close());
    }
    header.pack_start(&back);

    let title = gtk::Label::new(Some("AI Providers"));
    title.add_css_class("lc-providers-title");
    header.set_title_widget(Some(&title));

    let help = gtk::Button::from_icon_name("help-about-symbolic");
    help.add_css_class("lc-outline-circle");
    help.set_tooltip_text(Some("About providers"));
    {
        let popover = gtk::Popover::new();
        popover.add_css_class("lc-popover");
        popover.set_parent(&help);
        let body = gtk::Label::new(Some(
            "Providers are the endpoints Link Code talks to. Add one, paste its \
             key, then pick the model from the chat header. Ollama and other \
             local servers need no key.",
        ));
        body.set_wrap(true);
        body.set_max_width_chars(34);
        body.add_css_class("lc-tiny");
        body.set_margin_top(6);
        body.set_margin_bottom(6);
        body.set_margin_start(6);
        body.set_margin_end(6);
        popover.set_child(Some(&body));
        let help_for_click = help.clone();
        help.connect_clicked(move |_| popover.popup());
        let _ = &help_for_click;
    }
    header.pack_end(&help);

    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.add_css_class("lc-outline-circle");
    add.set_tooltip_text(Some("Add provider"));
    header.pack_end(&add);

    window.set_titlebar(Some(&header));

    // ---- body -----------------------------------------------------------
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("lc-root");

    let banner = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    banner.add_css_class("lc-banner");
    banner.set_margin_start(16);
    banner.set_margin_end(16);
    banner.set_margin_top(14);
    banner.set_margin_bottom(6);
    let robot = gtk::Box::new(gtk::Orientation::Vertical, 0);
    robot.add_css_class("lc-provider-tile");
    robot.add_css_class("lc-tile-lg");
    robot.add_css_class("local");
    let robot_icon = gtk::Image::from_icon_name("applications-science-symbolic");
    robot_icon.set_pixel_size(20);
    robot.append(&robot_icon);
    banner.append(&robot);
    let banner_text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    banner_text.set_hexpand(true);
    let banner_label = gtk::Label::new(Some("Connect your models"));
    banner_label.add_css_class("lc-banner-label");
    banner_label.set_xalign(0.0);
    banner_text.append(&banner_label);
    let banner_sub = gtk::Label::new(Some(
        "Bring your own key or point at a local server. Keys stay in the OS keyring.",
    ));
    banner_sub.add_css_class("lc-banner-text");
    banner_sub.set_xalign(0.0);
    banner_sub.set_wrap(true);
    banner_text.append(&banner_sub);
    banner.append(&banner_text);
    root.append(&banner);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
    list.set_margin_start(16);
    list.set_margin_end(16);
    list.set_margin_top(10);
    list.set_margin_bottom(16);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&list)
        .build();
    scroll.add_css_class("lc-dialog-scroll");
    root.append(&scroll);

    window.set_child(Some(&root));
    window.present();

    // ---- list refresh ---------------------------------------------------
    let refresh_slot: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let refresh: Rc<dyn Fn()> = {
        let slot = refresh_slot.clone();
        let state = state.clone();
        let window = window.clone();
        let list = list.clone();
        Rc::new(move || {
            let Some(refresh) = slot.borrow().clone() else {
                return;
            };
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let models = state.registry.borrow().models.clone();
            if models.is_empty() {
                let empty = gtk::Box::new(gtk::Orientation::Vertical, 8);
                empty.set_valign(gtk::Align::Center);
                empty.set_halign(gtk::Align::Center);
                empty.set_margin_top(60);
                let tile = gtk::Box::new(gtk::Orientation::Vertical, 0);
                tile.add_css_class("lc-provider-tile");
                tile.add_css_class("lc-tile-lg");
                tile.set_halign(gtk::Align::Center);
                let icon = gtk::Image::from_icon_name("applications-science-symbolic");
                icon.set_pixel_size(20);
                tile.append(&icon);
                empty.append(&tile);
                let heading = gtk::Label::new(Some("No providers yet"));
                heading.add_css_class("lc-providers-title");
                empty.append(&heading);
                let sub = gtk::Label::new(Some("Add an endpoint to start chatting."));
                sub.add_css_class("lc-muted");
                empty.append(&sub);
                let add_empty = gtk::Button::with_label("Add provider");
                add_empty.add_css_class("lc-accent-outline");
                let state_for_empty = state.clone();
                let refresh_for_empty = refresh.clone();
                add_empty.connect_clicked(move |_| {
                    present_form(
                        &state_for_empty,
                        None,
                        Rc::new({
                            let refresh = refresh_for_empty.clone();
                            move || refresh()
                        }),
                    );
                });
                empty.append(&add_empty);
                list.append(&empty);
                return;
            }
            let active = state.chat_model().map(|model| model.id);
            for model in models.iter() {
                let selected = active.as_deref() == Some(model.id.as_str());
                let card = provider_card(&state, &window, model, selected, refresh.clone());
                list.append(&card);
            }
        })
    };
    *refresh_slot.borrow_mut() = Some(refresh.clone());
    refresh();

    {
        let state = state.clone();
        let refresh = refresh.clone();
        add.connect_clicked(move |_| {
            let refresh = refresh.clone();
            present_form(&state, None, Rc::new(move || refresh()));
        });
    }
}

// ---------------------------------------------------------------------------
// Provider card
// ---------------------------------------------------------------------------

fn provider_card(
    state: &Rc<UiState>,
    window: &gtk::Window,
    model: &ModelConfig,
    selected: bool,
    refresh: Rc<dyn Fn()>,
) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.add_css_class("lc-provider-card");
    if selected {
        card.add_css_class("lc-provider-card-selected");
    }
    card.set_cursor_from_name(Some("pointer"));
    card.set_tooltip_text(Some("Set as the chat model"));

    let tile = gtk::Box::new(gtk::Orientation::Vertical, 0);
    tile.add_css_class("lc-provider-tile");
    tile.add_css_class("lc-tile");
    tile.add_css_class(provider_class(model.provider));
    tile.set_valign(gtk::Align::Center);
    let glyph = gtk::Label::new(Some(provider_glyph(model.provider)));
    glyph.add_css_class("lc-mono");
    glyph.set_halign(gtk::Align::Center);
    glyph.set_valign(gtk::Align::Center);
    tile.append(&glyph);
    card.append(&tile);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    let name_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let name = gtk::Label::new(Some(&model.display_name));
    name.add_css_class("lc-provider-name");
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name_row.append(&name);
    if selected {
        let chat = gtk::Label::new(Some("chat"));
        chat.add_css_class("lc-badge");
        chat.add_css_class("lc-badge-accent");
        name_row.append(&chat);
    }
    text.append(&name_row);

    let endpoint = gtk::Label::new(Some(&format!(
        "{} · {}",
        model.provider.label(),
        model.model_id
    )));
    endpoint.add_css_class("lc-provider-model");
    endpoint.set_xalign(0.0);
    endpoint.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    text.append(&endpoint);
    card.append(&text);

    let (badge_text, badge_class) = if model.provider == ProviderKind::Ollama {
        ("Local", "local")
    } else if model.has_api_key {
        ("Own key", "own-key")
    } else {
        ("Hosted", "hosted")
    };
    let badge = gtk::Label::new(Some(badge_text));
    badge.add_css_class("lc-status-badge");
    badge.add_css_class(badge_class);
    badge.set_valign(gtk::Align::Center);
    card.append(&badge);

    // Sparkle / chart: test the endpoint without leaving the screen.
    let test = gtk::Button::from_icon_name("starred-symbolic");
    test.add_css_class("lc-icon-btn");
    test.add_css_class("flat");
    test.set_tooltip_text(Some("Test this endpoint"));
    test.set_valign(gtk::Align::Center);
    {
        let state = state.clone();
        let model = model.clone();
        test.connect_clicked(move |_| test_endpoint(&state, &model));
    }
    card.append(&test);

    // Overflow: edit or delete the card.
    let menu = gtk::MenuButton::new();
    menu.set_icon_name("view-more-symbolic");
    menu.add_css_class("lc-icon-btn");
    menu.add_css_class("flat");
    menu.set_tooltip_text(Some("More actions"));
    menu.set_valign(gtk::Align::Center);
    let popover = gtk::Popover::new();
    popover.add_css_class("lc-popover");
    let actions = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let edit = gtk::Button::with_label("Edit provider");
    edit.add_css_class("flat");
    edit.set_halign(gtk::Align::Fill);
    {
        let state = state.clone();
        let model = model.clone();
        let refresh = refresh.clone();
        let popover = popover.clone();
        edit.connect_clicked(move |_| {
            popover.popdown();
            present_form(&state, Some(model.clone()), refresh.clone());
        });
    }
    actions.append(&edit);
    let delete = gtk::Button::with_label("Delete provider");
    delete.add_css_class("flat");
    delete.add_css_class("lc-danger");
    delete.set_halign(gtk::Align::Fill);
    {
        let state = state.clone();
        let model = model.clone();
        let refresh = refresh.clone();
        let popover = popover.clone();
        delete.connect_clicked(move |_| {
            popover.popdown();
            {
                let mut registry = state.registry.borrow_mut();
                registry.remove(&model.id);
            }
            let _ = state.secrets.delete_model_key(&model.id);
            // A base URL shared with another model keeps its keys; otherwise the
            // pool and every secret in it go away with the provider.
            let pool_id = model.key_pool_id();
            let still_used = state
                .registry
                .borrow()
                .models
                .iter()
                .any(|other| other.key_pool_id() == pool_id);
            if !pool_id.is_empty() && !still_used {
                state.delete_key_pool(&pool_id);
            }
            let registry = state.registry.borrow().clone();
            let _ = save_registry(&registry);
            state.refresh_model_badges();
            state.toast(&format!("Deleted {}", model.display_name));
            refresh();
        });
    }
    actions.append(&delete);
    popover.set_child(Some(&actions));
    menu.set_popover(Some(&popover));
    card.append(&menu);

    // Selecting the card makes it the chat model. Attach the gesture to the
    // tile and the text block only, so the test/menu buttons keep their own
    // clicks. A card-wide gesture would fire when those buttons are pressed.
    let select: Rc<dyn Fn()> = {
        let state = state.clone();
        let refresh = refresh.clone();
        let model = model.clone();
        Rc::new(move || {
        state.set_chat_model(&model.id);
        state.toast(&format!(
            "Chat model: {} — next message continues this conversation",
            state.chat_model_label()
        ));
            refresh();
        })
    };
    for target in [tile.clone().upcast::<gtk::Widget>(), text.clone().upcast::<gtk::Widget>()] {
        let gesture = gtk::GestureClick::new();
        let select = select.clone();
        gesture.connect_released(move |_, _, _, _| select());
        target.add_controller(gesture);
    }

    let _ = window;
    card
}

fn test_endpoint(state: &Rc<UiState>, model: &ModelConfig) {
    let provider = state.rotating_provider(model);
    let (tx, rx) = tokio::sync::oneshot::channel();
    runtime().spawn(async move {
        let result = provider.test_connection().await;
        let _ = tx.send(result);
    });
    let state = state.clone();
    let name = model.display_name.clone();
    glib::MainContext::default().spawn_local(async move {
        match rx.await {
            Ok(Ok(report)) => state.toast(&format!("{name}: {}", report.summary())),
            Ok(Err(error)) => state.toast(&format!("{name} failed: {error}")),
            Err(_) => state.toast(&format!("{name}: test cancelled")),
        }
    });
}

fn provider_glyph(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Anthropic => "A",
        ProviderKind::Gemini => "G",
        ProviderKind::Ollama => "O",
        ProviderKind::OpenAiCompatible => "◆",
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

fn add_escape(window: &gtk::Window) {
    let dialog = window.clone();
    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    key.connect_key_pressed(move |_, keyval, _, _| {
        if keyval == gtk::gdk::Key::Escape {
            dialog.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    window.add_controller(key);
}

// ---------------------------------------------------------------------------
// Add / edit form
// ---------------------------------------------------------------------------

struct EditorCtx {
    editing: RefCell<Option<String>>,
}

/// One editable API-key row in the provider form.
#[derive(Clone)]
struct KeyRow {
    /// Unique per UI row, so removal can find it again.
    token: u64,
    /// Existing pool key id, or empty for a row the user just added.
    pool_id: String,
    entry: gtk::Entry,
    row: gtk::Box,
}

#[derive(Clone)]
struct Form {
    root: gtk::Box,
    display: gtk::Entry,
    base_url: gtk::Entry,
    model_id: gtk::Entry,
    load_models: gtk::Button,
    keys_box: gtk::Box,
    keys: Rc<RefCell<Vec<KeyRow>>>,
    key_counter: Rc<Cell<u64>>,
    add_key: gtk::Button,
    key_hint: gtk::Label,
    key_link: gtk::Button,
    on_keys_changed: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    context: gtk::SpinButton,
    max_output: gtk::SpinButton,
    temperature: gtk::SpinButton,
    tools: gtk::Switch,
    system_prompt: gtk::TextView,
    provider: Rc<RefCell<ProviderKind>>,
    provider_buttons: Rc<Vec<(ProviderKind, gtk::ToggleButton)>>,
    provider_checks: Rc<Vec<(ProviderKind, gtk::Image)>>,
    provider_locks: Rc<Vec<(ProviderKind, gtk::Image)>>,
    original: Rc<RefCell<Option<ModelConfig>>>,
    fetched: Rc<RefCell<Vec<String>>>,
    suppress_fetch: Rc<Cell<bool>>,
}

impl Form {
    fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_start(18);
        root.set_margin_end(18);
        root.set_margin_top(16);
        root.set_margin_bottom(18);

        // ---- provider type ---------------------------------------------
        let type_card = form_card("PROVIDER TYPE", &root);
        let type_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let type_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&type_row)
            .build();
        type_card.append(&type_scroll);

        let provider = Rc::new(RefCell::new(ProviderKind::OpenAiCompatible));
        let mut buttons: Vec<(ProviderKind, gtk::ToggleButton)> = Vec::new();
        let mut checks: Vec<(ProviderKind, gtk::Image)> = Vec::new();
        let mut locks: Vec<(ProviderKind, gtk::Image)> = Vec::new();
        for kind in ProviderKind::ALL {
            let button = gtk::ToggleButton::new();
            button.add_css_class("lc-type-chip");
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 7);
            row.set_valign(gtk::Align::Center);
            let glyph = gtk::Label::new(Some(provider_glyph(kind)));
            glyph.add_css_class("lc-mono");
            glyph.add_css_class(provider_class(kind));
            row.append(&glyph);
            let label = gtk::Label::new(Some(short_provider_label(kind)));
            row.append(&label);
            let needs_key = kind != ProviderKind::Ollama;
            let lock = gtk::Image::from_icon_name("changes-prevent-symbolic");
            lock.set_pixel_size(12);
            lock.set_visible(needs_key);
            lock.add_css_class("lc-chip-lock");
            row.append(&lock);
            let check = gtk::Image::from_icon_name("object-select-symbolic");
            check.set_pixel_size(13);
            check.set_visible(false);
            check.add_css_class("lc-chip-check");
            row.append(&check);
            button.set_child(Some(&row));
            type_row.append(&button);
            buttons.push((kind, button));
            checks.push((kind, check));
            locks.push((kind, lock));
        }
        if let Some((_, first)) = buttons.first() {
            for (_, button) in buttons.iter().skip(1) {
                button.set_group(Some(first));
            }
            first.set_active(true);
        }

        // ---- name ------------------------------------------------------
        let name_card = form_card("NAME", &root);
        let display = entry("e.g. Claude Sonnet");
        display.set_width_chars(18);
        name_card.append(&display);
        let name_hint = gtk::Label::new(Some("Shown in the model selector and chat header."));
        name_hint.add_css_class("lc-form-hint");
        name_hint.set_xalign(0.0);
        name_card.append(&name_hint);

        // ---- base URL --------------------------------------------------
        let url_card = form_card("BASE URL", &root);
        let base_url = entry("https://api.example.com/v1");
        base_url.set_width_chars(20);
        url_card.append(&base_url);
        let url_hint = gtk::Label::new(Some(
            "Paste an endpoint, or leave the provider default. Local servers need no key.",
        ));
        url_hint.add_css_class("lc-form-hint");
        url_hint.set_xalign(0.0);
        url_hint.set_wrap(true);
        url_card.append(&url_hint);

        // ---- API key ---------------------------------------------------
        let key_card = form_card("API KEYS", &root);
        let key_head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        key_head.set_hexpand(true);
        let key_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        key_spacer.set_hexpand(true);
        key_head.append(&key_spacer);
        let key_link = gtk::Button::with_label("Get API key");
        key_link.add_css_class("lc-link");
        key_link.set_has_frame(false);
        key_link.set_width_request(88);
        key_head.append(&key_link);
        key_card.append(&key_head);

        let keys_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        key_card.append(&keys_box);

        let key_foot = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let add_key = gtk::Button::with_label("Add key");
        add_key.add_css_class("lc-accent-outline");
        add_key.set_tooltip_text(Some("Add another API key for this base URL"));
        add_key.set_width_request(96);
        key_foot.append(&add_key);
        let key_hint = gtk::Label::new(None);
        key_hint.add_css_class("lc-form-hint");
        key_hint.set_xalign(0.0);
        key_hint.set_hexpand(true);
        key_hint.set_wrap(true);
        key_foot.append(&key_hint);
        key_card.append(&key_foot);

        let key_note = gtk::Label::new(Some(
            "Add one or more keys for this base URL. When a key runs out mid-task, \
             the next one is used automatically. Keys stay in the OS keyring.",
        ));
        key_note.add_css_class("lc-form-hint");
        key_note.set_xalign(0.0);
        key_note.set_wrap(true);
        key_card.append(&key_note);

        // ---- model -----------------------------------------------------
        let model_card = form_card("MODEL", &root);
        let model_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let model_id = entry("model-id");
        model_id.set_width_chars(14);
        model_id.set_hexpand(true);
        model_row.append(&model_id);
        let load_models = gtk::Button::new();
        load_models.add_css_class("lc-accent-outline");
        load_models.set_width_request(104);
        let load_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        load_row.set_valign(gtk::Align::Center);
        let cloud = gtk::Image::from_icon_name("folder-download-symbolic");
        cloud.set_pixel_size(14);
        load_row.append(&cloud);
        let load_text = gtk::Label::new(Some("Load models"));
        load_row.append(&load_text);
        load_models.set_child(Some(&load_row));
        model_row.append(&load_models);
        model_card.append(&model_row);
        let model_hint = gtk::Label::new(Some(
            "Load models asks the endpoint for its catalogue, then lets you pick one.",
        ));
        model_hint.add_css_class("lc-form-hint");
        model_hint.set_xalign(0.0);
        model_hint.set_wrap(true);
        model_card.append(&model_hint);

        // ---- advanced --------------------------------------------------
        let advanced = gtk::Expander::new(Some("Advanced"));
        advanced.add_css_class("lc-advanced");
        advanced.set_margin_start(2);
        let adv = gtk::Box::new(gtk::Orientation::Vertical, 8);
        adv.set_margin_top(8);
        let context = spin(512.0, 2_000_000.0, 1024.0, 32_768.0);
        adv.append(&form_row("Context window", &context));
        let max_output = spin(256.0, 262_144.0, 256.0, 4_096.0);
        adv.append(&form_row("Max output", &max_output));
        let temperature = spin(0.0, 2.0, 0.05, 0.2);
        adv.append(&form_row("Temperature", &temperature));

        let tools_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let tools_label = gtk::Label::new(Some("Tool calling (code editing)"));
        tools_label.add_css_class("lc-tiny");
        tools_label.add_css_class("lc-dim");
        tools_label.set_xalign(0.0);
        tools_label.set_hexpand(true);
        tools_row.append(&tools_label);
        let tools = gtk::Switch::new();
        tools.set_active(true);
        tools.set_halign(gtk::Align::End);
    tools.set_tooltip_text(Some(
        "Offer file, search and shell tools to this model. Keep this on for \
         coding: turning it off prevents the model from reading or writing any \
         file. Turn off only for endpoints that fail whenever a tools field is \
         present.",
    ));
        tools_row.append(&tools);
        adv.append(&tools_row);

        let prompt_label = gtk::Label::new(Some("System prompt"));
        prompt_label.add_css_class("lc-form-hint");
        prompt_label.set_xalign(0.0);
        adv.append(&prompt_label);
        let system_prompt = gtk::TextView::new();
        system_prompt.set_wrap_mode(gtk::WrapMode::WordChar);
        system_prompt.set_top_margin(6);
        system_prompt.set_bottom_margin(6);
        system_prompt.set_left_margin(8);
        system_prompt.set_right_margin(8);
        system_prompt.add_css_class("lc-input-text");
        let prompt_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(70)
            .max_content_height(140)
            .propagate_natural_height(true)
            .child(&system_prompt)
            .build();
        adv.append(&prompt_scroll);
        advanced.set_child(Some(&adv));
        root.append(&advanced);

        let form = Form {
            root,
            display,
            base_url,
            model_id,
            load_models,
            keys_box,
            keys: Rc::new(RefCell::new(Vec::new())),
            key_counter: Rc::new(Cell::new(0)),
            add_key,
            key_hint,
            key_link,
            on_keys_changed: Rc::new(RefCell::new(None)),
            context,
            max_output,
            temperature,
            tools,
            system_prompt,
            provider,
            provider_buttons: Rc::new(buttons),
            provider_checks: Rc::new(checks),
            provider_locks: Rc::new(locks),
            original: Rc::new(RefCell::new(None)),
            fetched: Rc::new(RefCell::new(Vec::new())),
            suppress_fetch: Rc::new(Cell::new(false)),
        };

        {
            let form = form.clone();
            form.add_key.clone().connect_clicked(move |_| {
                if form.keys.borrow().len() >= MAX_KEYS_PER_POOL {
                    set_key_hint(&form, &format!(
                        "Limit reached \u{2014} at most {MAX_KEYS_PER_POOL} keys per base URL"
                    ));
                    return;
                }
                form.add_key_row(String::new(), "");
            });
        }

        // Choosing a provider applies its default URL and updates the key link.
        for (kind, button) in form.provider_buttons.iter() {
            let kind = *kind;
            let form = form.clone();
            button.connect_toggled(move |button| {
                if !button.is_active() {
                    return;
                }
                *form.provider.borrow_mut() = kind;
                for (other, check) in form.provider_checks.iter() {
                    check.set_visible(*other == kind);
                }
                for (other, lock) in form.provider_locks.iter() {
                    lock.set_visible(*other != kind && *other != ProviderKind::Ollama);
                }
                let current = form.base_url.text().to_string();
                let is_default = ProviderKind::ALL
                    .iter()
                    .any(|other| other.default_base_url() == current);
                if current.trim().is_empty() || is_default {
                    form.base_url.set_text(kind.default_base_url());
                }
                let needs_key = kind != ProviderKind::Ollama;
                form.key_link.set_sensitive(needs_key);
                form.key_link.set_label(if needs_key {
                    "Get API key"
                } else {
                    "No key needed"
                });
            });
        }

        for (kind, check) in form.provider_checks.iter() {
            check.set_visible(*kind == *form.provider.borrow());
        }
        for (kind, lock) in form.provider_locks.iter() {
            lock.set_visible(*kind != *form.provider.borrow() && *kind != ProviderKind::Ollama);
        }

        {
            let form = form.clone();
            form.key_link.connect_clicked(move |_| {
                if let Some(url) = provider_docs(*form.provider.borrow()) {
                    let _ = gtk::gio::AppInfo::launch_default_for_uri(
                        url,
                        None::<&gtk::gio::AppLaunchContext>,
                    );
                }
            });
        }

        form
    }

    fn load(&self, model: &ModelConfig, keys: &[(ApiKeyEntry, String)]) {
        self.suppress_fetch.set(true);
        *self.provider.borrow_mut() = model.provider;
        for (kind, button) in self.provider_buttons.iter() {
            if *kind == model.provider {
                button.set_active(true);
            }
        }
        self.display.set_text(&model.display_name);
        self.base_url.set_text(&model.base_url);
        self.model_id.set_text(&model.model_id);
        self.clear_key_rows();
        for (entry, secret) in keys {
            self.add_key_row(entry.id.clone(), secret);
        }
        if keys.is_empty() {
            self.add_key_row(String::new(), "");
        }
        self.context.set_value(model.context_window as f64);
        self.max_output.set_value(model.max_output_tokens as f64);
        self.temperature.set_value(model.temperature.unwrap_or(0.2));
        self.tools.set_active(model.capabilities.tools);
        self.system_prompt
            .buffer()
            .set_text(model.system_prompt.as_deref().unwrap_or(""));
        self.fetched.borrow_mut().clear();
        *self.original.borrow_mut() = Some(model.clone());
        self.suppress_fetch.set(false);
    }

    fn clear(&self) {
        let mut model = ModelConfig::default();
        model.provider = ProviderKind::OpenAiCompatible;
        model.base_url = ProviderKind::OpenAiCompatible.default_base_url().to_string();
        self.load(&model, &[]);
        self.display.set_text("");
        self.model_id.set_text("");
        self.base_url.set_text("");
    }

    fn set_fetched(&self, ids: &[String]) {
        *self.fetched.borrow_mut() = ids.to_vec();
    }

    /// Append one key row. `pool_id` ties the row to an already-stored key so
    /// an unchanged entry keeps its secret.
    fn add_key_row(&self, pool_id: String, secret: &str) {
        let token = self.key_counter.get() + 1;
        self.key_counter.set(token);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.add_css_class("lc-key-row");

        let entry = entry("sk-...");
        entry.set_visibility(false);
        entry.set_input_purpose(gtk::InputPurpose::Password);
        entry.set_width_chars(12);
        entry.set_hexpand(true);
        if !secret.is_empty() {
            entry.set_text(secret);
        }
        {
            let form = self.clone();
            entry.connect_changed(move |_| form.notify_keys_changed());
        }
        row.append(&entry);

        let paste = gtk::Button::from_icon_name("edit-paste-symbolic");
        paste.add_css_class("lc-icon-btn");
        paste.add_css_class("flat");
        paste.set_tooltip_text(Some("Paste from clipboard"));
        {
            let entry = entry.clone();
            paste.connect_clicked(move |_| paste_into(&entry));
        }
        row.append(&paste);

        let eye = gtk::Button::from_icon_name("view-reveal-symbolic");
        eye.add_css_class("lc-icon-btn");
        eye.add_css_class("flat");
        eye.set_tooltip_text(Some("Show or hide the key"));
        {
            let entry = entry.clone();
            let shown = Rc::new(Cell::new(false));
            eye.connect_clicked(move |button| {
                let visible = !shown.get();
                shown.set(visible);
                entry.set_visibility(visible);
                button.set_icon_name(if visible {
                    "view-conceal-symbolic"
                } else {
                    "view-reveal-symbolic"
                });
            });
        }
        row.append(&eye);

        let remove = gtk::Button::from_icon_name("user-trash-symbolic");
        remove.add_css_class("lc-icon-btn");
        remove.add_css_class("flat");
        remove.set_tooltip_text(Some("Remove this key"));
        {
            let form = self.clone();
            let row_widget = row.clone();
            remove.connect_clicked(move |_| {
                form.keys.borrow_mut().retain(|item| item.token != token);
                form.keys_box.remove(&row_widget);
                form.refresh_key_hint();
                form.notify_keys_changed();
            });
        }
        row.append(&remove);

        self.keys_box.append(&row);
        self.keys.borrow_mut().push(KeyRow {
            token,
            pool_id,
            entry,
            row,
        });
        self.refresh_key_hint();
    }

    fn clear_key_rows(&self) {
        for item in self.keys.borrow_mut().drain(..) {
            self.keys_box.remove(&item.row);
        }
        self.refresh_key_hint();
    }

    /// The non-empty secrets currently in the form, in row order.
    fn resolved_keys(&self) -> Vec<(String, String)> {
        self.keys
            .borrow()
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let secret = sanitize_api_key(&item.entry.text());
                if secret.is_empty() {
                    None
                } else {
                    Some((format!("Key {}", index + 1), secret))
                }
            })
            .collect()
    }

    fn refresh_key_hint(&self) {
        let count = self.keys.borrow().len();
        set_key_hint(self, &format!("{count} of {MAX_KEYS_PER_POOL} keys"));
        self.add_key.set_sensitive(count < MAX_KEYS_PER_POOL);
    }

    fn notify_keys_changed(&self) {
        if let Some(callback) = self.on_keys_changed.borrow().clone() {
            callback();
        }
    }

    fn read(&self, ctx: &EditorCtx) -> ModelConfig {
        let provider = *self.provider.borrow();
        let mut config = self.original.borrow().clone().unwrap_or_default();
        config.provider = provider;
        config.display_name = self.display.text().trim().to_string();
        config.base_url = normalize_base_url(provider, &self.base_url.text());
        if config.base_url.is_empty() {
            config.base_url = provider.default_base_url().to_string();
        }
        config.model_id = self.model_id.text().trim().to_string();
        config.context_window = self.context.value() as usize;
        config.max_output_tokens = self.max_output.value() as u32;
        config.temperature = Some(self.temperature.value());
        config.capabilities.tools = self.tools.is_active();
        let prompt = self
            .system_prompt
            .buffer()
            .text(
                &self.system_prompt.buffer().start_iter(),
                &self.system_prompt.buffer().end_iter(),
                true,
            )
            .to_string();
        config.system_prompt = if prompt.trim().is_empty() {
            None
        } else {
            Some(prompt)
        };
        if let Some(id) = ctx.editing.borrow().clone() {
            config.id = id;
        } else if config.id.is_empty() {
            config.id = link_models::registry::generate_id(&config.display_name, &config.model_id);
        }
        config.has_api_key = !self.resolved_keys().is_empty() || config.has_api_key;
        config
    }
}

// ---------------------------------------------------------------------------
// Add Provider form window
// ---------------------------------------------------------------------------

fn present_form(state: &Rc<UiState>, editing: Option<ModelConfig>, on_saved: Rc<dyn Fn()>) {
    let is_edit = editing.is_some();
    let window = gtk::Window::builder()
        .title(if is_edit { "Edit provider" } else { "Add provider" })
        .modal(true)
        .resizable(true)
        .default_width(520)
        .default_height(600)
        .build();
    window.set_size_request(480, 520);
    window.add_css_class("lc-dialog");
    window.add_css_class("lc-providers");
    if let Some(parent) = state.widgets().map(|widgets| widgets.window.clone()) {
        window.set_transient_for(Some(&parent));
    }
    add_escape(&window);

    let header = gtk::HeaderBar::new();
    let back = gtk::Button::from_icon_name("go-previous-symbolic");
    back.add_css_class("lc-icon-btn");
    back.add_css_class("flat");
    back.set_tooltip_text(Some("Back"));
    {
        let window = window.clone();
        back.connect_clicked(move |_| window.close());
    }
    header.pack_start(&back);
    let title = gtk::Label::new(Some(if is_edit { "Edit provider" } else { "Add provider" }));
    title.add_css_class("lc-providers-title");
    header.set_title_widget(Some(&title));
    window.set_titlebar(Some(&header));

    let form = Form::new();
    let ctx = Rc::new(EditorCtx {
        editing: RefCell::new(None),
    });
    if let Some(model) = &editing {
        let pool_id = model.key_pool_id();
        let keys: Vec<(ApiKeyEntry, String)> = state
            .model_key_pool(model)
            .keys
            .iter()
            .map(|entry| {
                let secret = state
                    .secrets
                    .get_pool_key(&pool_id, &entry.id)
                    .unwrap_or_default();
                (entry.clone(), secret)
            })
            .collect();
        form.load(model, &keys);
        *ctx.editing.borrow_mut() = Some(model.id.clone());
    } else {
        form.clear();
    }

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("lc-root");
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&form.root)
        .build();
    scroll.add_css_class("lc-dialog-scroll");
    root.append(&scroll);

    let status = gtk::Label::new(None);
    status.add_css_class("lc-tiny");
    status.add_css_class("lc-dim");
    status.set_xalign(0.0);
    status.set_hexpand(true);
    status.set_wrap(true);

    let foot = gtk::Box::new(gtk::Orientation::Vertical, 8);
    foot.add_css_class("lc-form-foot");
    foot.append(&status);
    let apply = gtk::Button::new();
    apply.add_css_class("lc-primary");
    apply.set_hexpand(true);
    let apply_row = gtk::Box::new(gtk::Orientation::Horizontal, 7);
    apply_row.set_valign(gtk::Align::Center);
    apply_row.set_halign(gtk::Align::Center);
    let apply_check = gtk::Image::from_icon_name("object-select-symbolic");
    apply_check.set_pixel_size(15);
    apply_row.append(&apply_check);
    let apply_text = gtk::Label::new(Some(if is_edit { "Save changes" } else { "Add provider" }));
    apply_row.append(&apply_text);
    apply.set_child(Some(&apply_row));
    foot.append(&apply);
    root.append(&foot);

    window.set_child(Some(&root));
    window.present();
    form.display.grab_focus();

    // ---- fetch the model catalogue ------------------------------------
    let perform_fetch: Rc<dyn Fn(bool)> = {
        let state = state.clone();
        let form = form.clone();
        let ctx = ctx.clone();
        let status = status.clone();
        let window = window.clone();
        Rc::new(move |open_picker: bool| {
            let config = form.read(&ctx);
            if config.base_url.trim().is_empty() {
                set_status(&status, "Set a base URL first", false);
                return;
            }
            let keys = form.resolved_keys();
            set_status(&status, "Loading models…", true);
            let provider = state.ephemeral_provider(&config, keys);
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime().spawn(async move {
                let result = provider.list_models().await;
                let _ = tx.send(result);
            });
            let form = form.clone();
            let status = status.clone();
            let window = window.clone();
            glib::MainContext::default().spawn_local(async move {
                match rx.await {
                    Ok(Ok(models)) => {
                        let ids: Vec<String> = models.into_iter().map(|model| model.id).collect();
                        form.set_fetched(&ids);
                        if ids.is_empty() {
                            set_status(&status, "The endpoint returned no models", false);
                            return;
                        }
                        set_status(&status, &format!("Found {} models", ids.len()), true);
                        if ids.len() == 1 {
                            form.model_id.set_text(&ids[0]);
                            if form.display.text().trim().is_empty() {
                                form.display.set_text(&ids[0]);
                            }
                        } else if open_picker || form.model_id.text().trim().is_empty() {
                            let form_for_pick = form.clone();
                            pick_model(&window, &ids, move |id| {
                                form_for_pick.model_id.set_text(&id);
                                if form_for_pick.display.text().trim().is_empty() {
                                    form_for_pick.display.set_text(&id);
                                }
                            });
                        }
                    }
                    Ok(Err(error)) => set_status(&status, &format!("Load failed: {error}"), false),
                    Err(_) => set_status(&status, "Load cancelled", false),
                }
            });
        })
    };
    {
        let perform_fetch = perform_fetch.clone();
        form.load_models.connect_clicked(move |_| perform_fetch(true));
    }

    // Auto-load once the base URL or key settles, for new providers only.
    {
        let perform_fetch = perform_fetch.clone();
        let form = form.clone();
        let form_for_schedule = form.clone();
        let ctx = ctx.clone();
        let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        let schedule: Rc<dyn Fn()> = Rc::new(move || {
            if form_for_schedule.suppress_fetch.get() || ctx.editing.borrow().is_some() {
                return;
            }
            if form_for_schedule.base_url.text().trim().is_empty() {
                return;
            }
            if let Some(source) = debounce.borrow_mut().take() {
                source.remove();
            }
            let perform_fetch = perform_fetch.clone();
            let debounce_for_timeout = debounce.clone();
            let source = glib::timeout_add_local_once(Duration::from_millis(300), move || {
                *debounce_for_timeout.borrow_mut() = None;
                perform_fetch(false);
            });
            *debounce.borrow_mut() = Some(source);
        });
        {
            let schedule = schedule.clone();
            form.base_url.connect_changed(move |_| schedule());
        }
        *form.on_keys_changed.borrow_mut() = Some(schedule);
    }

    // ---- save ---------------------------------------------------------
    {
        let state = state.clone();
        let form = form.clone();
        let ctx = ctx.clone();
        let status = status.clone();
        let window = window.clone();
        apply.connect_clicked(move |_| {
            let config = form.read(&ctx);
            let problems = config.validate();
            if !problems.is_empty() {
                set_status(&status, &problems.join(" · "), false);
                return;
            }
            let old_pool_id = form
                .original
                .borrow()
                .as_ref()
                .map(|model| model.key_pool_id())
                .unwrap_or_default();
            let new_pool_id = config.key_pool_id();

            let model_id = state.registry.borrow_mut().add(config.clone());
            *ctx.editing.borrow_mut() = Some(model_id.clone());
            // Older builds kept one key per model `id`; pools supersede it.
            let _ = state.secrets.delete_model_key(&model_id);

            let key_problem =
                sync_key_pool(&state, &form, &new_pool_id, old_pool_id == new_pool_id).err();

            // A base URL that no model points at any more keeps no keys behind.
            if !old_pool_id.is_empty() && old_pool_id != new_pool_id {
                let still_used = state
                    .registry
                    .borrow()
                    .models
                    .iter()
                    .any(|model| model.key_pool_id() == old_pool_id);
                if !still_used {
                    state.delete_key_pool(&old_pool_id);
                }
            }
            state.refresh_has_api_key();
            let registry = state.registry.borrow().clone();
            let _ = save_registry(&registry);
            state.refresh_model_badges();
            if let Some(error) = key_problem {
                set_status(&status, &format!("Saved, but {error}"), false);
                return;
            }
            state.toast(&format!("Saved {}", config.display_name));
            on_saved();
            window.close();
        });
    }
}

fn set_status(label: &gtk::Label, text: &str, ok: bool) {
    label.set_label(text);
    label.remove_css_class("lc-ok");
    label.remove_css_class("lc-error");
    if ok {
        label.add_css_class("lc-ok");
    } else {
        label.add_css_class("lc-error");
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn form_card(title: &str, parent: &gtk::Box) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 9);
    card.add_css_class("lc-form-card");
    let label = gtk::Label::new(Some(title));
    label.add_css_class("lc-form-label");
    label.set_xalign(0.0);
    card.append(&label);
    parent.append(&card);
    card
}

fn form_row(label: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let text = gtk::Label::new(Some(label));
    text.add_css_class("lc-tiny");
    text.add_css_class("lc-dim");
    text.set_xalign(0.0);
    text.set_width_chars(18);
    row.append(&text);
    widget.set_hexpand(true);
    row.append(widget);
    row
}

fn entry(placeholder: &str) -> gtk::Entry {
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(placeholder));
    entry.add_css_class("lc-input");
    entry
}

fn spin(min: f64, max: f64, step: f64, value: f64) -> gtk::SpinButton {
    let spin = gtk::SpinButton::with_range(min, max, step);
    spin.set_value(value);
    spin.set_digits(if step < 1.0 { 2 } else { 0 });
    spin.add_css_class("lc-input");
    spin
}

fn paste_into(entry: &gtk::Entry) {
    let clipboard = entry.display().clipboard();
    let entry = entry.clone();
    clipboard.read_text_async(None::<&gtk::gio::Cancellable>, move |result| {
        if let Ok(Some(text)) = result {
            entry.set_text(text.trim());
        }
    });
}

fn provider_docs(provider: ProviderKind) -> Option<&'static str> {
    match provider {
        ProviderKind::Anthropic => Some("https://console.anthropic.com/settings/keys"),
        ProviderKind::Gemini => Some("https://aistudio.google.com/app/apikey"),
        ProviderKind::OpenAiCompatible => Some("https://platform.openai.com/api-keys"),
        ProviderKind::Ollama => None,
    }
}

fn short_provider_label(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Anthropic => "Anthropic",
        ProviderKind::Gemini => "Gemini",
        ProviderKind::Ollama => "Ollama",
        ProviderKind::OpenAiCompatible => "OpenAI-compatible",
    }
}

/// Update the small count label under the key list.
fn set_key_hint(form: &Form, text: &str) {
    form.key_hint.set_label(text);
}

/// Write the form's key rows into `pool_id`'s pool and delete the secrets the
/// user removed. Existing key ids are reused so an unchanged entry keeps its
/// secret and its exhausted flag.
///
/// `same_pool` is false when the provider just moved to a different base URL.
/// In that case the keys already stored for the new URL (it may be shared with
/// another model) are preserved, and the form's rows are merged in instead of
/// replacing them.
fn sync_key_pool(
    state: &UiState,
    form: &Form,
    pool_id: &str,
    same_pool: bool,
) -> Result<(), String> {
    if pool_id.is_empty() {
        return Ok(());
    }
    let existing = state.key_pool_snapshot(pool_id);
    let existing_by_id: std::collections::HashMap<&str, &ApiKeyEntry> = existing
        .keys
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect();

    let mut entries: Vec<ApiKeyEntry> = if same_pool {
        Vec::new()
    } else {
        existing.keys.clone()
    };
    let mut kept: std::collections::HashSet<String> =
        entries.iter().map(|entry| entry.id.clone()).collect();
    // Secrets already present, so merging into a shared URL does not duplicate.
    let mut known: std::collections::HashSet<String> = if same_pool {
        std::collections::HashSet::new()
    } else {
        existing
            .keys
            .iter()
            .filter_map(|entry| state.secrets.get_pool_key(pool_id, &entry.id))
            .map(|secret| sanitize_api_key(&secret))
            .filter(|secret| !secret.is_empty())
            .collect()
    };

    for item in form.keys.borrow().iter() {
        let secret = sanitize_api_key(&item.entry.text());
        if secret.is_empty() {
            continue;
        }
        if !known.insert(secret.clone()) {
            continue;
        }
        if entries.len() >= MAX_KEYS_PER_POOL {
            return Err(format!("a base URL can hold at most {MAX_KEYS_PER_POOL} keys"));
        }
        let id = if item.pool_id.is_empty() || !existing_by_id.contains_key(item.pool_id.as_str())
        {
            uuid::Uuid::new_v4().to_string()
        } else {
            item.pool_id.clone()
        };
        state.store_pool_key(pool_id, &id, &secret)?;
        let (exhausted, exhausted_at) = existing_by_id
            .get(id.as_str())
            .map(|entry| (entry.exhausted, entry.exhausted_at))
            .unwrap_or((false, None));
        entries.push(ApiKeyEntry {
            id: id.clone(),
            label: format!("Key {}", entries.len() + 1),
            exhausted,
            exhausted_at,
        });
        kept.insert(id);
    }

    if same_pool {
        for entry in &existing.keys {
            if !kept.contains(&entry.id) {
                state.delete_pool_key(pool_id, &entry.id);
            }
        }
    }

    let active = if entries.is_empty() {
        0
    } else {
        existing.active.min(entries.len() - 1)
    };
    state.set_key_pool(
        pool_id,
        ApiKeyPool {
            keys: entries,
            active,
        },
    );
    Ok(())
}

fn placeholder_row(text: &str) -> gtk::ListBoxRow {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("lc-dim");
    label.add_css_class("lc-tiny");
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_margin_top(12);
    label.set_margin_bottom(12);
    label.set_margin_start(10);
    label.set_margin_end(10);
    let row = gtk::ListBoxRow::new();
    row.set_activatable(false);
    row.set_selectable(false);
    row.set_child(Some(&label));
    row
}

/// Searchable list of model ids returned by an endpoint.
fn pick_model<F: Fn(String) + 'static>(parent: &gtk::Window, ids: &[String], on_pick: F) {
    let window = gtk::Window::builder()
        .title("Pick a model")
        .modal(true)
        .resizable(true)
        .default_width(460)
        .default_height(420)
        .build();
    window.add_css_class("lc-dialog");
    window.set_transient_for(Some(parent));
    add_escape(&window);

    let header = gtk::HeaderBar::new();
    let heading = gtk::Label::new(Some("Pick a model"));
    heading.add_css_class("lc-providers-title");
    header.set_title_widget(Some(&heading));
    window.set_titlebar(Some(&header));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("lc-root");

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search fetched models…"));
    search.add_css_class("lc-input");
    search.set_margin_start(12);
    search.set_margin_end(12);
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

    let ids = Rc::new(ids.to_vec());
    let rebuild: Rc<dyn Fn(&str)> = {
        let list = list.clone();
        let ids = ids.clone();
        Rc::new(move |query: &str| {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let query = query.trim().to_lowercase();
            let mut any = false;
            for id in ids.iter() {
                if !query.is_empty() && !id.to_lowercase().contains(&query) {
                    continue;
                }
                any = true;
                let label = gtk::Label::new(Some(id));
                label.set_xalign(0.0);
                label.set_wrap(true);
                label.add_css_class("lc-mono");
                label.set_margin_top(7);
                label.set_margin_bottom(7);
                label.set_margin_start(10);
                label.set_margin_end(10);
                let row = gtk::ListBoxRow::new();
                row.set_activatable(true);
                row.set_widget_name(id);
                row.set_child(Some(&label));
                list.append(&row);
            }
            if !any {
                list.append(&placeholder_row("No models match your search."));
            }
        })
    };
    rebuild("");
    {
        let rebuild = rebuild.clone();
        search.connect_search_changed(move |entry| rebuild(&entry.text()));
    }
    {
        let dialog = window.clone();
        let on_pick = Rc::new(on_pick);
        list.connect_row_activated(move |_, row| {
            let id = row.widget_name().to_string();
            if id.is_empty() {
                return;
            }
            on_pick(id);
            dialog.close();
        });
    }

    window.set_child(Some(&root));
    window.present();
    search.grab_focus();
}

/// Ensure the registry has a usable chat role after external edits.
pub fn ensure_chat_role(registry: &mut ModelRegistry) {
    if registry.roles.get(ModelRole::Chat.key()).is_none() {
        if let Some(model) = registry.models.first() {
            let id = model.id.clone();
            registry.roles.insert(ModelRole::Chat.key().to_string(), id);
        }
    }
}

/// A lightweight "add a model quickly" flow used by the welcome screen.
pub fn quick_add(state: &Rc<UiState>, provider: ProviderKind, model_id: &str, base_url: &str) {
    let mut config = ModelConfig::new(model_id, provider, model_id);
    config.base_url = base_url.to_string();
    let id = config.id.clone();
    state.registry.borrow_mut().add(config);
    state.registry.borrow_mut().set_role(ModelRole::Chat, &id);
    let registry = state.registry.borrow().clone();
    let _ = save_registry(&registry);
    state.refresh_model_badges();
}
