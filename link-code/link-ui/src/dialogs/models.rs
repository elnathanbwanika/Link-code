//! The Models dialog: list, add, edit, test and remove model endpoints.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use link_models::registry::{normalize_base_url, ModelConfig, ModelRegistry, ModelRole, ProviderKind};
use link_models::provider::provider_for;
use link_models::sanitize_api_key;

use crate::state::{runtime, save_registry, UiState};

pub fn present(state: &Rc<UiState>) {
    let window = gtk::Window::builder()
        .title("Models")
        .modal(true)
        .resizable(true)
        .default_width(860)
        .default_height(600)
        .build();
    window.add_css_class("lc-dialog");
    if let Some(parent) = state.widgets().map(|widgets| widgets.window.clone()) {
        window.set_transient_for(Some(&parent));
    }
    {
        // A plain `GtkWindow` has no built-in Escape handling; capture the key
        // so it closes even while a text field has focus.
        let dialog = window.clone();
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Escape {
                dialog.close();
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            }
        });
        window.add_controller(key);
    }

    let header = gtk::HeaderBar::new();
    let title = gtk::Label::new(Some("MODELS"));
    title.add_css_class("lc-section");
    title.add_css_class("lc-accent");
    header.set_title_widget(Some(&title));
    let add_button = gtk::Button::with_label("Add model");
    add_button.add_css_class("lc-primary");
    header.pack_start(&add_button);
    window.set_titlebar(Some(&header));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("lc-root");

    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.set_position(320);
    paned.set_vexpand(true);

    // ---- left: searchable list ------------------------------------------
    let list = gtk::ListBox::new();
    list.add_css_class("lc-treeview");
    list.set_selection_mode(gtk::SelectionMode::Single);
    let list_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&list)
        .build();
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search models\u{2026}"));
    search.add_css_class("lc-input");
    search.set_margin_start(8);
    search.set_margin_end(8);
    search.set_margin_top(8);
    search.set_margin_bottom(6);
    let left = gtk::Box::new(gtk::Orientation::Vertical, 0);
    left.append(&search);
    left.append(&list_scroll);
    paned.set_start_child(Some(&left));

    // ---- right: form ----------------------------------------------------
    let form = Form::new();
    let form_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&form.root)
        .build();
    paned.set_end_child(Some(&form_scroll));
    root.append(&paned);

    let ctx = Rc::new(EditorCtx {
        editing: RefCell::new(None),
    });

    // Ids currently shown in the filtered list, in row order. Selection is
    // resolved through this rather than the row index so searching is safe.
    let visible_ids: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    // Selection changes load the model into the form.
    {
        let form = form.clone();
        let ctx = ctx.clone();
        let state_for_keys = state.clone();
        let visible_ids = visible_ids.clone();
        list.connect_row_selected(move |_, row| {
            let Some(row) = row else {
                return;
            };
            let index = row.index() as usize;
            let Some(id) = visible_ids.borrow().get(index).cloned() else {
                return;
            };
            let models = state_for_keys.registry.borrow().models.clone();
            if let Some(model) = models.iter().find(|model| model.id == id) {
                form.load(model, state_for_keys.api_key_for(model).unwrap_or_default());
                *ctx.editing.borrow_mut() = Some(model.id.clone());
            }
        });
    }

    let refresh_list = {
        let list = list.clone();
        let state = state.clone();
        let visible_ids = visible_ids.clone();
        Rc::new(move |query: &str| {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let query = query.trim().to_lowercase();
            let models = state.registry.borrow().models.clone();
            let chat_id = state
                .registry
                .borrow()
                .roles
                .get(ModelRole::Chat.key())
                .cloned();
            let mut shown = Vec::new();
            for model in &models {
                if !query.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        model.display_name,
                        model.model_id,
                        model.provider.label()
                    )
                    .to_lowercase();
                    if !haystack.contains(&query) {
                        continue;
                    }
                }
                shown.push(model.id.clone());
                let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
                row.set_margin_top(5);
                row.set_margin_bottom(5);
                row.set_margin_start(8);
                row.set_margin_end(8);
                let name_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                let name = gtk::Label::new(Some(&model.display_name));
                name.set_xalign(0.0);
                name.set_hexpand(true);
                name.set_ellipsize(gtk::pango::EllipsizeMode::End);
                name_row.append(&name);
                if chat_id.as_deref() == Some(model.id.as_str()) {
                    let chip = gtk::Label::new(Some("chat"));
                    chip.add_css_class("lc-badge");
                    chip.add_css_class("lc-badge-accent");
                    name_row.append(&chip);
                }
                row.append(&name_row);
                let meta = gtk::Label::new(Some(&format!(
                    "{} \u{00b7} {}",
                    model.provider.label(),
                    model.model_id
                )));
                meta.add_css_class("lc-tiny");
                meta.add_css_class("lc-dim");
                meta.set_xalign(0.0);
                meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
                row.append(&meta);
                list.append(&row);
            }
            *visible_ids.borrow_mut() = shown;
            if models.is_empty() {
                list.append(&placeholder_row("No models configured yet. Add one on the right."));
            } else if visible_ids.borrow().is_empty() {
                list.append(&placeholder_row("No models match your search."));
            }
        })
    };
    refresh_list(&search.text());
    {
        let refresh_list = refresh_list.clone();
        search.connect_search_changed(move |entry| refresh_list(&entry.text()));
    }

    // ---- toolbar under the form ----------------------------------------
    let status = gtk::Label::new(Some(""));
    status.add_css_class("lc-tiny");
    status.add_css_class("lc-dim");
    status.set_xalign(0.0);
    status.set_hexpand(true);

    let apply = gtk::Button::with_label("Save model");
    apply.add_css_class("lc-primary");

    {
        let state = state.clone();
        let form = form.clone();
        let ctx = ctx.clone();
        let refresh_list = refresh_list.clone();
        let search = search.clone();
        let status = status.clone();
        apply.connect_clicked(move |_| {
            let config = form.read(&ctx);
            let problems = config.validate();
            if !problems.is_empty() {
                status.set_label(&problems.join(" \u{00b7} "));
                status.remove_css_class("lc-ok");
                status.add_css_class("lc-error");
                return;
            }
            let key = sanitize_api_key(&form.api_key.text());
            let was_editing = ctx.editing.borrow().clone();
            let model_id = {
                let mut registry = state.registry.borrow_mut();
                registry.add(config.clone())
            };

            let mut key_problem = None;
            let mut has_key = false;
            if !key.is_empty() {
                match store_key(&state, &model_id, &key) {
                    Ok(()) => has_key = true,
                    Err(error) => key_problem = Some(error),
                }
            } else if was_editing.as_deref() == Some(model_id.as_str()) {
                // The editor showed the stored key; an empty field means the
                // user cleared it.
                let _ = state.secrets.delete_model_key(&model_id);
            }
            if let Some(model) = state.registry.borrow_mut().get_mut(&model_id) {
                model.has_api_key = has_key;
            }

            // Remember which row was just saved, so pressing Save again
            // updates it instead of appending a duplicate model.
            *ctx.editing.borrow_mut() = Some(model_id);

            let registry = state.registry.borrow().clone();
            if let Err(error) = save_registry(&registry) {
                status.set_label(&format!("Could not save models.json: {error}"));
                status.remove_css_class("lc-ok");
                status.add_css_class("lc-error");
                return;
            }
            match key_problem {
                Some(error) => {
                    status.set_label(&format!("Saved {}, but {error}", config.display_name));
                    status.remove_css_class("lc-ok");
                    status.add_css_class("lc-error");
                }
                None => {
                    status.set_label(&format!("Saved {}", config.display_name));
                    status.remove_css_class("lc-error");
                    status.add_css_class("lc-ok");
                }
            }
            state.refresh_model_badges();
            refresh_list(&search.text());
        });
    }

    let delete = gtk::Button::with_label("Delete");
    delete.add_css_class("lc-ghost");
    delete.add_css_class("lc-danger");
    {
        let state = state.clone();
        let ctx = ctx.clone();
        let form = form.clone();
        let refresh_list = refresh_list.clone();
        let search = search.clone();
        let status = status.clone();
        delete.connect_clicked(move |_| {
            let Some(id) = ctx.editing.borrow().clone() else {
                status.set_label("Select a model first");
                return;
            };
            let name = state
                .registry
                .borrow()
                .get(&id)
                .map(|model| model.display_name.clone())
                .unwrap_or_else(|| "model".to_string());
            {
                let mut registry = state.registry.borrow_mut();
                registry.remove(&id);
            }
            let _ = state.secrets.delete_model_key(&id);
            let registry = state.registry.borrow().clone();
            let _ = save_registry(&registry);
            state.refresh_model_badges();
            refresh_list(&search.text());
            form.clear();
            *ctx.editing.borrow_mut() = None;
            status.set_label(&format!("Deleted {name}"));
        });
    }

    // ---- fetching models ------------------------------------------------
    // One pipeline shared by the button, the Browse button and the debounced
    // auto-fetch that runs after a base URL or key is pasted.
    let fetch_seq = Rc::new(Cell::new(0u64));
    let perform_fetch: Rc<dyn Fn(bool)> = {
        let state = state.clone();
        let form = form.clone();
        let ctx = ctx.clone();
        let status = status.clone();
        let fetch_seq = fetch_seq.clone();
        let window = window.clone();
        Rc::new(move |open_picker: bool| {
            let config = form.read(&ctx);
            if config.base_url.trim().is_empty() {
                if open_picker {
                    status.remove_css_class("lc-ok");
                    status.add_css_class("lc-error");
                    status.set_label("Set a base URL first");
                }
                return;
            }
            let typed = sanitize_api_key(&form.api_key.text());
            let key = if typed.is_empty() {
                state.api_key_for(&config)
            } else {
                Some(typed)
            };
            let seq = fetch_seq.get() + 1;
            fetch_seq.set(seq);
            status.remove_css_class("lc-ok");
            status.remove_css_class("lc-error");
            status.set_label("Fetching models\u{2026}");
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime().spawn(async move {
                let provider = provider_for(&config, key);
                let result = provider.list_models().await;
                let _ = tx.send(result);
            });
            let form = form.clone();
            let status = status.clone();
            let fetch_seq = fetch_seq.clone();
            let window = window.clone();
            glib::MainContext::default().spawn_local(async move {
                let result = rx.await;
                // A newer keystroke or fetch superseded this response.
                if fetch_seq.get() != seq {
                    return;
                }
                match result {
                    Ok(Ok(models)) => {
                        let ids: Vec<String> = models.into_iter().map(|model| model.id).collect();
                        form.set_fetched(&ids);
                        if ids.is_empty() {
                            status.remove_css_class("lc-ok");
                            status.add_css_class("lc-error");
                            status.set_label("The endpoint returned no models");
                            return;
                        }
                        let id_empty = form.model_id.text().trim().is_empty();
                        if id_empty && ids.len() == 1 {
                            form.model_id.set_text(&ids[0]);
                            if form.display.text().trim().is_empty() {
                                form.display.set_text(&ids[0]);
                            }
                        }
                        status.remove_css_class("lc-error");
                        status.add_css_class("lc-ok");
                        if open_picker || !id_empty || ids.len() == 1 {
                            status.set_label(&format!("Found {} models", ids.len()));
                        } else {
                            status.set_label(&format!(
                                "Found {} models \u{2014} press Browse to pick",
                                ids.len()
                            ));
                        }
                        if open_picker {
                            let form = form.clone();
                            pick_model(&window, &ids, move |id| {
                                form.model_id.set_text(&id);
                                if form.display.text().trim().is_empty() {
                                    form.display.set_text(&id);
                                }
                            });
                        } else if id_empty && ids.len() == 1 {
                            // Nothing to choose; fill it in silently.
                            status.set_label(&format!("Found {} model", ids.len()));
                        }
                    }
                    Ok(Err(error)) => {
                        status.remove_css_class("lc-ok");
                        status.add_css_class("lc-error");
                        status.set_label(&format!("Fetch failed: {error}"));
                    }
                    Err(_) => status.set_label("Fetch cancelled"),
                }
            });
        })
    };

    let fetch = gtk::Button::with_label("Fetch models");
    fetch.add_css_class("lc-ghost");
    {
        let perform_fetch = perform_fetch.clone();
        fetch.connect_clicked(move |_| perform_fetch(true));
    }

    // Browse shows the models already fetched, or fetches them first.
    {
        let form = form.clone();
        let browse = form.browse.clone();
        let window = window.clone();
        let perform_fetch = perform_fetch.clone();
        browse.connect_clicked(move |_| {
            let ids = form.fetched.borrow().clone();
            if ids.is_empty() {
                perform_fetch(true);
            } else {
                let form = form.clone();
                pick_model(&window, &ids, move |id| {
                    form.model_id.set_text(&id);
                    if form.display.text().trim().is_empty() {
                        form.display.set_text(&id);
                    }
                });
            }
        });
    }

    // Auto-fetch once typing settles. Only runs while adding a new model so
    // selecting an existing row never hits the network.
    let schedule_fetch: Rc<dyn Fn()> = {
        let perform_fetch = perform_fetch.clone();
        let form = form.clone();
        let ctx = ctx.clone();
        let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        Rc::new(move || {
            if form.suppress_fetch.get() || ctx.editing.borrow().is_some() {
                return;
            }
            if form.base_url.text().trim().is_empty() {
                return;
            }
            if let Some(source) = debounce.borrow_mut().take() {
                source.remove();
            }
            let perform_fetch = perform_fetch.clone();
            let debounce_for_timeout = debounce.clone();
            let source = glib::timeout_add_local_once(Duration::from_millis(250), move || {
                *debounce_for_timeout.borrow_mut() = None;
                perform_fetch(false);
            });
            *debounce.borrow_mut() = Some(source);
        })
    };
    {
        let schedule_fetch = schedule_fetch.clone();
        form.base_url.connect_changed(move |_| schedule_fetch());
    }
    {
        let schedule_fetch = schedule_fetch.clone();
        form.api_key.connect_changed(move |_| schedule_fetch());
    }

    // ---- test connection -------------------------------------------------
    let test = gtk::Button::with_label("Test connection");
    test.add_css_class("lc-ghost");
    {
        let state = state.clone();
        let form = form.clone();
        let ctx = ctx.clone();
        let status = status.clone();
        test.connect_clicked(move |_| {
            let config = form.read(&ctx);
            let typed = sanitize_api_key(&form.api_key.text());
            let key = if typed.is_empty() {
                state.api_key_for(&config)
            } else {
                Some(typed.clone())
            };
            let key_sent = key.is_some();
            status.remove_css_class("lc-ok");
            status.remove_css_class("lc-error");
            status.set_label("Testing\u{2026}");
            let status = status.clone();
            let state = state.clone();
            let ctx = ctx.clone();
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime().spawn(async move {
                let provider = provider_for(&config, key);
                let result = provider.test_connection().await;
                let _ = tx.send(result);
            });
            glib::MainContext::default().spawn_local(async move {
                match rx.await {
                    Ok(Ok(report)) => {
                        let mut suffix = if key_sent {
                            "key sent".to_string()
                        } else {
                            "no key sent".to_string()
                        };
                        let mut key_problem = None;
                        if !typed.is_empty() {
                            match ctx.editing.borrow().clone() {
                                Some(id) => match store_key(&state, &id, &typed) {
                                    Ok(()) => {
                                        if let Some(model) =
                                            state.registry.borrow_mut().get_mut(&id)
                                        {
                                            model.has_api_key = true;
                                        }
                                        let registry = state.registry.borrow().clone();
                                        let _ = save_registry(&registry);
                                        suffix = "key stored".to_string();
                                    }
                                    Err(error) => key_problem = Some(error),
                                },
                                // The model has not been saved yet; typing the
                                // key already marks it configured.
                                None => suffix = "press Save model to keep the key".to_string(),
                            }
                        }
                        match key_problem {
                            Some(error) => {
                                status.remove_css_class("lc-ok");
                                status.add_css_class("lc-error");
                                status.set_label(&format!("Test passed, but {error}"));
                            }
                            None => {
                                status.remove_css_class("lc-error");
                                status.add_css_class("lc-ok");
                                status.set_label(&format!(
                                    "{} \u{00b7} {suffix}",
                                    report.summary()
                                ));
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        status.remove_css_class("lc-ok");
                        status.add_css_class("lc-error");
                        let key_state = if key_sent { "key sent" } else { "no key sent" };
                        status.set_label(&format!("Test failed ({key_state}): {error}"));
                    }
                    Err(_) => status.set_label("Test cancelled"),
                }
            });
        });
    }

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_margin_start(14);
    actions.set_margin_end(14);
    actions.set_margin_top(8);
    actions.set_margin_bottom(10);
    actions.append(&fetch);
    actions.append(&test);
    actions.append(&status);
    actions.append(&delete);
    actions.append(&apply);
    root.append(&actions);

    // Add model resets the form to a blank config.
    {
        let form = form.clone();
        let ctx = ctx.clone();
        let list = list.clone();
        add_button.connect_clicked(move |_| {
            form.load(&ModelConfig::default(), String::new());
            *ctx.editing.borrow_mut() = None;
            list.unselect_all();
        });
    }

    window.set_child(Some(&root));
    window.present();

    // Select the chat model on open.
    let models = state.registry.borrow().models.clone();
    if let Some(active) = state.chat_model() {
        if let Some(index) = models.iter().position(|model| model.id == active.id) {
            if let Some(row) = list.row_at_index(index as i32) {
                list.select_row(Some(&row));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Form
// ---------------------------------------------------------------------------

struct EditorCtx {
    editing: RefCell<Option<String>>,
}

#[derive(Clone)]
struct Form {
    pub root: gtk::Box,
    display: gtk::Entry,
    provider: gtk::DropDown,
    base_url: gtk::Entry,
    model_id: gtk::Entry,
    browse: gtk::Button,
    api_key: gtk::Entry,
    context: gtk::SpinButton,
    max_output: gtk::SpinButton,
    temperature: gtk::SpinButton,
    tools: gtk::Switch,
    system_prompt: gtk::TextView,
    original: Rc<RefCell<Option<ModelConfig>>>,
    fetched: Rc<RefCell<Vec<String>>>,
    /// Set while loading a row so the changed handlers do not auto-fetch.
    suppress_fetch: Rc<Cell<bool>>,
}

impl Form {
    fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.set_margin_start(16);
        root.set_margin_end(16);
        root.set_margin_top(14);
        root.set_margin_bottom(14);

        let display = entry("Display name");
        field(&root, "Display name", &display);

        let provider_items: Vec<&str> = ProviderKind::ALL.iter().map(|kind| kind.label()).collect();
        let provider = gtk::DropDown::from_strings(&provider_items);
        field(&root, "Provider", &provider);

        let base_url = entry("https://api.example.com/v1");
        field(&root, "Base URL", &base_url);

        let model_id = entry("model-id");
        let browse = gtk::Button::with_label("Browse\u{2026}");
        browse.add_css_class("lc-ghost");
        browse.add_css_class("lc-tiny");
        let model_label = gtk::Label::new(Some("Model ID"));
        model_label.add_css_class("lc-tiny");
        model_label.add_css_class("lc-dim");
        model_label.set_width_chars(16);
        model_label.set_xalign(0.0);
        let model_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        model_row.append(&model_label);
        model_id.set_hexpand(true);
        model_row.append(&model_id);
        model_row.append(&browse);
        root.append(&model_row);

        let api_key = entry("API key (blank = none)");
        api_key.set_visibility(false);
        api_key.set_input_purpose(gtk::InputPurpose::Password);
        field(&root, "API key", &api_key);

        let context = spin(512.0, 2_000_000.0, 1024.0, 32_768.0);
        field(&root, "Context window", &context);

        let max_output = spin(256.0, 262_144.0, 256.0, 4_096.0);
        field(&root, "Max output tokens", &max_output);

        let temperature = spin(0.0, 2.0, 0.05, 0.2);
        field(&root, "Temperature", &temperature);

        // Native tool calling can be switched off for pure-chat endpoints.
        // Link Code always falls back to the text protocol, so leaving this on
        // is safe even when an endpoint ignores the `tools` field.
        let tools = gtk::Switch::new();
        tools.set_active(true);
        tools.set_halign(gtk::Align::Start);
        tools.set_tooltip_text(Some(
            "Offer file, search and shell tools to this model. Turn off only \
             for endpoints that fail whenever a `tools` field is present.",
        ));
        field(&root, "Tools", &tools);

        let system_prompt = gtk::TextView::new();
        system_prompt.set_wrap_mode(gtk::WrapMode::WordChar);
        system_prompt.set_top_margin(6);
        system_prompt.set_bottom_margin(6);
        system_prompt.set_left_margin(8);
        system_prompt.set_right_margin(8);
        system_prompt.add_css_class("lc-code");
        let prompt_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(80)
            .max_content_height(160)
            .propagate_natural_height(true)
            .child(&system_prompt)
            .build();
        prompt_scroll.add_css_class("lc-card");
        let prompt_label = gtk::Label::new(Some("System prompt"));
        prompt_label.add_css_class("lc-tiny");
        prompt_label.add_css_class("lc-dim");
        prompt_label.set_xalign(0.0);
        root.append(&prompt_label);
        root.append(&prompt_scroll);

        let hint = gtk::Label::new(Some(
            "Local endpoints (Ollama, LM Studio, llama.cpp) need no API key. \
             Keys go to the OS keyring, with a mode-0600 local fallback when \
             the keyring is unavailable.",
        ));
        hint.add_css_class("lc-tiny");
        hint.add_css_class("lc-disabled");
        hint.set_xalign(0.0);
        hint.set_wrap(true);
        root.append(&hint);

        let form = Form {
            root,
            display,
            provider,
            base_url,
            model_id,
            browse,
            api_key,
            context,
            max_output,
            temperature,
            tools,
            system_prompt,
            original: Rc::new(RefCell::new(None)),
            fetched: Rc::new(RefCell::new(Vec::new())),
            suppress_fetch: Rc::new(Cell::new(false)),
        };

        // Choosing a provider applies its default base URL.
        {
            let form = form.clone();
            form.provider.connect_selected_notify(move |dropdown| {
                if let Some(kind) = ProviderKind::ALL.get(dropdown.selected() as usize) {
                    let current = form.base_url.text().to_string();
                    if current.trim().is_empty() || ProviderKind::ALL.iter().any(|other| other.default_base_url() == current) {
                        form.base_url.set_text(kind.default_base_url());
                    }
                }
            });
        }

        form
    }

    fn load(&self, model: &ModelConfig, api_key: String) {
        self.suppress_fetch.set(true);
        self.display.set_text(&model.display_name);
        let index = ProviderKind::ALL
            .iter()
            .position(|kind| *kind == model.provider)
            .unwrap_or(0) as u32;
        self.provider.set_selected(index);
        self.base_url.set_text(&model.base_url);
        self.model_id.set_text(&model.model_id);
        self.api_key.set_text(&api_key);
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
        self.load(&ModelConfig::default(), String::new());
    }

    fn set_fetched(&self, ids: &[String]) {
        *self.fetched.borrow_mut() = ids.to_vec();
    }

    fn read(&self, ctx: &EditorCtx) -> ModelConfig {
        let provider = ProviderKind::ALL
            .get(self.provider.selected() as usize)
            .copied()
            .unwrap_or(ProviderKind::OpenAiCompatible);
        let mut config = self.original.borrow().clone().unwrap_or_default();
        config.provider = provider;
        config.display_name = self.display.text().trim().to_string();
        config.base_url = normalize_base_url(provider, &self.base_url.text());
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
        config.has_api_key = !self.api_key.text().trim().is_empty() || config.has_api_key;
        config
    }
}

fn store_key(state: &UiState, model_id: &str, key: &str) -> Result<(), String> {
    state
        .secrets
        .set_model_key(model_id, key)
        .map_err(|error| format!("could not store the API key: {error}"))?;
    match state.secrets.get_model_key(model_id) {
        Some(stored) if sanitize_api_key(&stored) == key => Ok(()),
        _ => Err("the API key could not be read back after saving".to_string()),
    }
}

fn field(parent: &gtk::Box, label: &str, widget: &impl IsA<gtk::Widget>) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let text = gtk::Label::new(Some(label));
    text.add_css_class("lc-tiny");
    text.add_css_class("lc-dim");
    text.set_xalign(0.0);
    text.set_width_chars(16);
    row.append(&text);
    widget.set_hexpand(true);
    row.append(widget);
    parent.append(&row);
}

fn entry(placeholder: &str) -> gtk::Entry {
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(placeholder));
    entry.add_css_class("lc-input");
    entry
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
        .default_width(540)
        .default_height(520)
        .build();
    window.add_css_class("lc-dialog");
    window.set_transient_for(Some(parent));
    {
        let dialog = window.clone();
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Escape {
                dialog.close();
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            }
        });
        window.add_controller(key);
    }

    let header = gtk::HeaderBar::new();
    let heading = gtk::Label::new(Some("PICK A MODEL"));
    heading.add_css_class("lc-section");
    heading.add_css_class("lc-accent");
    header.set_title_widget(Some(&heading));
    window.set_titlebar(Some(&header));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("lc-root");

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search fetched models\u{2026}"));
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

fn spin(min: f64, max: f64, step: f64, value: f64) -> gtk::SpinButton {
    let spin = gtk::SpinButton::with_range(min, max, step);
    spin.set_value(value);
    spin.set_digits(if step < 1.0 { 2 } else { 0 });
    spin.add_css_class("lc-input");
    spin
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
