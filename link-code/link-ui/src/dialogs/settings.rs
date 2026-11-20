//! The Preferences window.
//!
//! Every control applies immediately: it mutates [`UiState::config`] and writes
//! `config.json`, so there is no "OK / Cancel" state to lose.

use std::rc::Rc;

use adw::prelude::*;

use link_core::permission::{PermissionMode, PermissionRules};
use link_core::ProxyProtocol;

use crate::state::UiState;

pub fn present(state: &Rc<UiState>) {
    let window = adw::PreferencesWindow::builder()
        .title("Settings")
        .modal(true)
        .search_enabled(true)
        .default_width(620)
        .default_height(560)
        .build();
    window.add_css_class("lc-dialog");
    if let Some(parent) = state.widgets().map(|widgets| widgets.window.clone()) {
        window.set_transient_for(Some(&parent));
    }
    {
        // Escape closes the preferences window even while a row is focused.
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

    window.add(&general_page(state));
    window.add(&appearance_page(state));
    window.add(&agent_page(state));
    window.add(&editor_page(state));
    window.add(&terminal_page(state));
    window.add(&proxy_page(state));
    window.add(&permissions_page(state));

    window.present();
}

fn save(state: &Rc<UiState>) {
    if let Err(error) = state.save_config() {
        tracing::warn!(%error, "could not save the configuration");
    }
}

fn page(title: &str, icon: &str) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title(title);
    page.set_icon_name(Some(icon));
    page
}

fn group(title: &str) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(title);
    group
}

fn switch(
    title: &str,
    subtitle: &str,
    active: bool,
    on_change: impl Fn(bool) + 'static,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::new();
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_active(active);
    row.connect_active_notify(move |row| on_change(row.is_active()));
    row
}

fn spin(
    title: &str,
    subtitle: &str,
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    digits: u32,
    on_change: impl Fn(f64) + 'static,
) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(min, max, step);
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_digits(digits);
    row.set_value(value);
    row.connect_value_notify(move |row| on_change(row.value()));
    row
}

fn combo(
    title: &str,
    subtitle: &str,
    options: &[&str],
    selected: u32,
    on_change: impl Fn(u32) + 'static,
) -> adw::ComboRow {
    let row = adw::ComboRow::new();
    row.set_title(title);
    row.set_subtitle(subtitle);
    let list = gtk::StringList::new(options);
    row.set_model(Some(&list));
    row.set_selected(selected);
    row.connect_selected_notify(move |row| on_change(row.selected()));
    row
}

fn entry(
    title: &str,
    text: &str,
    on_change: impl Fn(String) + 'static,
) -> adw::EntryRow {
    let row = adw::EntryRow::new();
    row.set_title(title);
    row.set_text(text);
    row.connect_changed(move |row| on_change(row.text().to_string()));
    row
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

fn general_page(state: &Rc<UiState>) -> adw::PreferencesPage {
    let page = page("General", "preferences-system-symbolic");
    let basics = group("Workspace");

    {
        let state = state.clone();
        let row = switch(
            "Reopen the last workspace",
            "When no folder is passed on the command line",
            { let value = state.config.borrow().general.reopen_last_workspace; value },
            move |active| {
                state.config.borrow_mut().general.reopen_last_workspace = active;
                save(&state);
            },
        );
        basics.add(&row);
    }

    {
        let state = state.clone();
        let row = spin(
            "Recent workspaces",
            "How many folders to remember",
            { let value = state.config.borrow().general.recent_limit as f64; value },
            1.0,
            50.0,
            1.0,
            0,
            move |value| {
                state.config.borrow_mut().general.recent_limit = value as usize;
                save(&state);
            },
        );
        basics.add(&row);
    }

    {
        let state = state.clone();
        let language = state.config.borrow().general.language.clone();
        let row = entry("Interface language", &language, move |text| {
            state.config.borrow_mut().general.language = text;
            save(&state);
        });
        basics.add(&row);
    }

    let clone = group("Welcome screen");
    {
        let state = state.clone();
        let row = switch(
            "Show \u{201c}Clone repository\u{201d}",
            "Adds a clone action to the welcome screen",
            { let value = state.config.borrow().general.show_clone_action; value },
            move |active| {
                state.config.borrow_mut().general.show_clone_action = active;
                save(&state);
            },
        );
        clone.add(&row);
    }

    page.add(&basics);
    page.add(&clone);
    page
}

fn appearance_page(state: &Rc<UiState>) -> adw::PreferencesPage {
    let page = page("Appearance", "applications-graphics-symbolic");
    let text = group("Editor text");

    {
        let state = state.clone();
        let font = state.config.borrow().appearance.editor_font.clone();
        let row = entry("Editor font", &font, move |value| {
            state.config.borrow_mut().appearance.editor_font = value;
            save(&state);
        });
        text.add(&row);
    }

    {
        let state = state.clone();
        let row = spin(
            "Editor font size",
            "In points",
            { let value = state.config.borrow().appearance.editor_font_size; value },
            8.0,
            32.0,
            0.5,
            1,
            move |value| {
                state.config.borrow_mut().appearance.editor_font_size = value;
                save(&state);
            },
        );
        text.add(&row);
    }

    let display = group("Window");
    {
        let state = state.clone();
        let row = spin(
            "Interface scale",
            "0.8 \u{2013} 1.6",
            { let value = state.config.borrow().appearance.ui_scale; value },
            0.8,
            1.6,
            0.05,
            2,
            move |value| {
                state.config.borrow_mut().appearance.ui_scale = value;
                save(&state);
            },
        );
        display.add(&row);
    }
    {
        let state = state.clone();
        let row = switch(
            "High contrast",
            "Stronger borders and brighter dim text",
            { let value = state.config.borrow().appearance.high_contrast; value },
            move |active| {
                state.config.borrow_mut().appearance.high_contrast = active;
                save(&state);
            },
        );
        display.add(&row);
    }
    {
        let state = state.clone();
        let row = switch(
            "Respect reduced motion",
            "Disable non-essential animations",
            { let value = state.config.borrow().appearance.respect_reduced_motion; value },
            move |active| {
                state.config.borrow_mut().appearance.respect_reduced_motion = active;
                save(&state);
            },
        );
        display.add(&row);
    }

    page.add(&text);
    page.add(&display);
    page
}

fn agent_page(state: &Rc<UiState>) -> adw::PreferencesPage {
    let page = page("Agent", "utilities-terminal-symbolic");
    let modes: Vec<&str> = PermissionMode::ALL.iter().map(|mode| mode.label()).collect();
    let default_mode = state.config.borrow().agent.default_mode;
    let selected = PermissionMode::ALL
        .iter()
        .position(|mode| *mode == default_mode)
        .unwrap_or(0) as u32;

    let behaviour = group("Behaviour");
    {
        let state = state.clone();
        let row = combo(
            "Default permission mode",
            "Also adjustable from the composer chip",
            &modes,
            selected,
            move |index| {
                if let Some(mode) = PermissionMode::ALL.get(index as usize) {
                    state.set_mode(*mode);
                    save(&state);
                }
            },
        );
        behaviour.add(&row);
    }
    {
        let state = state.clone();
        let row = spin(
            "Maximum tool iterations",
            "Stops a runaway loop",
            { let value = state.config.borrow().agent.max_tool_iterations as f64; value },
            1.0,
            200.0,
            1.0,
            0,
            move |value| {
                state.config.borrow_mut().agent.max_tool_iterations = value as u32;
                save(&state);
            },
        );
        behaviour.add(&row);
    }
    {
        let state = state.clone();
        let row = spin(
            "Maximum output tokens",
            "Per model response",
            { let value = state.config.borrow().agent.max_output_tokens as f64; value },
            256.0,
            131_072.0,
            256.0,
            0,
            move |value| {
                state.config.borrow_mut().agent.max_output_tokens = value as u32;
                save(&state);
            },
        );
        behaviour.add(&row);
    }
    {
        let state = state.clone();
        let row = spin(
            "Shell timeout",
            "Seconds before a command is killed",
            { let value = state.config.borrow().agent.shell_timeout_secs as f64; value },
            5.0,
            3_600.0,
            5.0,
            0,
            move |value| {
                state.config.borrow_mut().agent.shell_timeout_secs = value as u64;
                save(&state);
            },
        );
        behaviour.add(&row);
    }
    {
        let state = state.clone();
        let row = spin(
            "Auto-compact threshold",
            "Share of the context window before compaction",
            { let value = state.config.borrow().agent.auto_compact_threshold; value },
            0.1,
            0.98,
            0.01,
            2,
            move |value| {
                state.config.borrow_mut().agent.auto_compact_threshold = value;
                save(&state);
            },
        );
        behaviour.add(&row);
    }

    let safety = group("Safety and extras");
    {
        let state = state.clone();
        let row = switch(
            "Protect secrets",
            "Redact secret-looking values before they reach a model",
            { let value = state.config.borrow().agent.protect_secrets; value },
            move |active| {
                state.config.borrow_mut().agent.protect_secrets = active;
                save(&state);
            },
        );
        safety.add(&row);
    }
    {
        let state = state.clone();
        let row = switch(
            "Next-step suggestions",
            "Ask the model for follow-up chips after each answer",
            { let value = state.config.borrow().agent.generate_suggestions; value },
            move |active| {
                state.config.borrow_mut().agent.generate_suggestions = active;
                save(&state);
            },
        );
        safety.add(&row);
    }
    {
        let state = state.clone();
        let row = switch(
            "Web tools",
            "Adds web_fetch (sends requests off the machine)",
            { let value = state.config.borrow().agent.enable_web_tools; value },
            move |active| {
                state.config.borrow_mut().agent.enable_web_tools = active;
                save(&state);
            },
        );
        safety.add(&row);
    }
    {
        let state = state.clone();
        let row = switch(
            "Inline completion",
            "Ghost text in the editor (uses the completion role)",
            { let value = state.config.borrow().agent.inline_completion; value },
            move |active| {
                state.config.borrow_mut().agent.inline_completion = active;
                save(&state);
            },
        );
        safety.add(&row);
    }

    page.add(&behaviour);
    page.add(&safety);
    page
}

fn editor_page(state: &Rc<UiState>) -> adw::PreferencesPage {
    let page = page("Editor", "accessories-text-editor-symbolic");
    let text = group("Text");

    let add_switch = |group: &adw::PreferencesGroup,
                      title: &'static str,
                      subtitle: &'static str,
                      get: fn(&link_core::config::EditorSettings) -> bool,
                      set: fn(&mut link_core::config::EditorSettings, bool)| {
        let state = state.clone();
        let active = get(&state.config.borrow().editor);
        let row = switch(title, subtitle, active, move |value| {
            set(&mut state.config.borrow_mut().editor, value);
            save(&state);
        });
        group.add(&row);
    };

    {
        let state = state.clone();
        let row = spin(
            "Tab width",
            "Spaces per indentation level",
            { let value = state.config.borrow().editor.tab_width as f64; value },
            1.0,
            16.0,
            1.0,
            0,
            move |value| {
                state.config.borrow_mut().editor.tab_width = value as u32;
                save(&state);
            },
        );
        text.add(&row);
    }

    add_switch(
        &text,
        "Insert spaces",
        "Use spaces instead of tab characters",
        |editor| editor.insert_spaces,
        |editor, value| editor.insert_spaces = value,
    );
    add_switch(
        &text,
        "Word wrap",
        "Wrap long lines instead of scrolling",
        |editor| editor.word_wrap,
        |editor, value| editor.word_wrap = value,
    );
    add_switch(
        &text,
        "Trim trailing whitespace",
        "Strip spaces at the end of lines when saving",
        |editor| editor.trim_trailing_whitespace,
        |editor, value| editor.trim_trailing_whitespace = value,
    );

    let behaviour = group("Behaviour");
    let add_switch = |group: &adw::PreferencesGroup,
                      title: &'static str,
                      subtitle: &'static str,
                      get: fn(&link_core::config::EditorSettings) -> bool,
                      set: fn(&mut link_core::config::EditorSettings, bool)| {
        let state = state.clone();
        let active = get(&state.config.borrow().editor);
        let row = switch(title, subtitle, active, move |value| {
            set(&mut state.config.borrow_mut().editor, value);
            save(&state);
        });
        group.add(&row);
    };
    add_switch(
        &behaviour,
        "Auto-indent",
        "Match the previous line's indentation",
        |editor| editor.auto_indent,
        |editor, value| editor.auto_indent = value,
    );
    add_switch(
        &behaviour,
        "Format on save",
        "Run the configured formatter before writing",
        |editor| editor.format_on_save,
        |editor, value| editor.format_on_save = value,
    );

    page.add(&text);
    page.add(&behaviour);
    page
}

fn terminal_page(state: &Rc<UiState>) -> adw::PreferencesPage {
    let page = page("Terminal", "utilities-terminal-symbolic");
    let group = group("Shell");

    {
        let state = state.clone();
        let shell = state.config.borrow().terminal.shell.clone();
        let row = entry("Shell", &shell, move |value| {
            state.config.borrow_mut().terminal.shell = value;
            save(&state);
        });
        group.add(&row);
    }
    {
        let state = state.clone();
        let row = spin(
            "Font size",
            "Points",
            { let value = state.config.borrow().terminal.font_size; value },
            8.0,
            32.0,
            0.5,
            1,
            move |value| {
                state.config.borrow_mut().terminal.font_size = value;
                save(&state);
            },
        );
        group.add(&row);
    }
    {
        let state = state.clone();
        let row = spin(
            "Scrollback",
            "Lines kept in memory",
            { let value = state.config.borrow().terminal.scrollback_lines as f64; value },
            100.0,
            200_000.0,
            100.0,
            0,
            move |value| {
                state.config.borrow_mut().terminal.scrollback_lines = value as u32;
                save(&state);
            },
        );
        group.add(&row);
    }
    {
        let state = state.clone();
        let row = switch(
            "Blinking cursor",
            "Show a blinking caret in embedded terminals",
            { let value = state.config.borrow().terminal.cursor_blink; value },
            move |active| {
                state.config.borrow_mut().terminal.cursor_blink = active;
                save(&state);
            },
        );
        group.add(&row);
    }

    page.add(&group);
    page
}

fn proxy_page(state: &Rc<UiState>) -> adw::PreferencesPage {
    let page = page("Proxy", "network-workgroup-symbolic");
    let server = group("Proxy server");
    server.set_description(Some(
        "Every outbound request \u{2014} model calls, endpoint discovery and web tools \u{2014} goes \
         through this proxy. Choose \u{201c}Direct\u{201d} to disable it.",
    ));

    let labels: Vec<&str> = ProxyProtocol::ALL.iter().map(|protocol| protocol.label()).collect();
    let current = state.config.borrow().proxy.clone();
    let selected = ProxyProtocol::ALL
        .iter()
        .position(|protocol| *protocol == current.protocol)
        .unwrap_or(0) as u32;
    {
        let state = state.clone();
        let row = combo(
            "Protocol",
            "HTTP, HTTPS, SOCKS4 or SOCKS5",
            &labels,
            selected,
            move |index| {
                if let Some(protocol) = ProxyProtocol::ALL.get(index as usize) {
                    state.config.borrow_mut().proxy.protocol = *protocol;
                    save(&state);
                }
            },
        );
        server.add(&row);
    }
    {
        let state = state.clone();
        let host = current.host.clone();
        let row = entry("Host", &host, move |text| {
            state.config.borrow_mut().proxy.host = text;
            save(&state);
        });
        server.add(&row);
    }
    {
        let state = state.clone();
        let row = spin(
            "Port",
            "The proxy listener, for example 3128 or 1080",
            current.port as f64,
            1.0,
            65_535.0,
            1.0,
            0,
            move |value| {
                state.config.borrow_mut().proxy.port = value.round() as u16;
                save(&state);
            },
        );
        server.add(&row);
    }

    let auth = group("Authentication (optional)");
    auth.set_description(Some(
        "The password is kept in the system keyring (or the local secret file), never in config.json.",
    ));
    {
        let state = state.clone();
        let username = current.username.clone();
        let row = entry("Username", &username, move |text| {
            state.config.borrow_mut().proxy.username = text;
            save(&state);
        });
        auth.add(&row);
    }
    {
        let state = state.clone();
        let row = adw::PasswordEntryRow::new();
        row.set_title("Password");
        row.set_text(&current.password);
        row.connect_changed(move |row| {
            state.config.borrow_mut().proxy.password = row.text().to_string();
            save(&state);
        });
        auth.add(&row);
    }

    page.add(&server);
    page.add(&auth);
    page
}

fn permissions_page(state: &Rc<UiState>) -> adw::PreferencesPage {
    let page = page("Permissions", "security-high-symbolic");
    let allow = group("Always allow");
    allow.set_description(Some(
        "Patterns such as `bash:cargo test*`. One per line \u{2014} saved in .link/settings.json.",
    ));
    let deny = group("Always deny");
    deny.set_description(Some("Always refused, even in Full auto. One per line."));

    {
        let state = state.clone();
        let rules = state.config.borrow().permissions.clone();
        let row = entry(
            "Allow patterns",
            &rules.allow_always.iter().cloned().collect::<Vec<_>>().join(", "),
            move |text| {
                let rules = PermissionRules {
                    allow_always: text
                        .split(',')
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .map(str::to_string)
                        .collect(),
                    deny_always: state.config.borrow().permissions.deny_always.clone(),
                };
                state.config.borrow_mut().permissions = rules;
                save(&state);
            },
        );
        allow.add(&row);
    }
    {
        let state = state.clone();
        let rules = state.config.borrow().permissions.clone();
        let row = entry(
            "Deny patterns",
            &rules.deny_always.iter().cloned().collect::<Vec<_>>().join(", "),
            move |text| {
                let rules = PermissionRules {
                    allow_always: state.config.borrow().permissions.allow_always.clone(),
                    deny_always: text
                        .split(',')
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .map(str::to_string)
                        .collect(),
                };
                state.config.borrow_mut().permissions = rules;
                save(&state);
            },
        );
        deny.add(&row);
    }

    page.add(&allow);
    page.add(&deny);
    page
}
