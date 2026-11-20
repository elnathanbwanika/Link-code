//! The agent chat panel: message log, live event rendering and composer.
//!
//! The panel owns no business logic: it builds widgets, then translates
//! [`AgentEvent`]s into them (see [`handle_event`]) and puts user input on the
//! tokio runtime through [`link_core::agent::AgentLoop`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use link_core::agent::AgentOptions;
use link_core::event::{AgentEvent, DoneReason, ErrorKind, TodoItem, TodoStatus};
use link_core::paths::Sandbox;
use link_core::permission::{PermissionEngine, PermissionMode, PermissionRules};
use link_core::project::{ProjectMemory, WorkspaceSettings};
use link_core::session::{Session, StoredRole};
use link_core::tool::ToolContext;
use link_models::types::ChatMessage;

use link_tools::builtin_registry;

use crate::dialogs::approval::{GtkApprover, GtkAsker};
use crate::state::{runtime, UiState};
use crate::widgets::chat_message::{self, BlockView};
use crate::widgets::edit_chip::EditChip;
use crate::widgets::tool_chip::ToolChip;

/// Tools whose arguments are streamed into the editor instead of the chat.
fn is_file_edit_tool(tool: &str) -> bool {
    matches!(tool, "write_file" | "edit_file" | "create_file" | "multi_edit")
}

pub struct AgentPanel {
    pub root: gtk::Box,
    pub chat_list: gtk::Box,
    pub chat_scroll: gtk::ScrolledWindow,
    pub activity: gtk::Box,
    pub activity_spinner: gtk::Spinner,
    pub activity_label: gtk::Label,
    pub input: gtk::TextView,
    pub send_button: gtk::Button,
    pub stop_button: gtk::Button,
    pub mode_button: gtk::Button,
    /// Model chip inside the composer; opens the model selector.
    pub composer_model_button: gtk::Button,
    pub context_bar: gtk::ProgressBar,
    /// Right-aligned token count beside the context meter.
    pub context_label: gtk::Label,
    /// Compact task summary above the composer, with the full checklist in a
    /// popover. The checklist never takes height away from the conversation.
    pub tasks: Rc<TaskPanel>,
    pub suggestions_box: gtk::Box,
    pub attachments_box: gtk::Box,
    pub attachments_scroll: gtk::ScrolledWindow,
}

/// Compact task summary docked directly above the composer.
///
/// The bar is one line tall and always present while a plan exists. The full
/// checklist lives in a popover anchored above it, so opening it overlays the
/// conversation instead of stealing its height.
pub struct TaskPanel {
    /// Clickable bar; also the popover anchor.
    pub menu: gtk::MenuButton,
    panel: gtk::Box,
    popover: gtk::Popover,
    label: gtk::Label,
    progress: gtk::ProgressBar,
    list: gtk::Box,
    scroll: gtk::ScrolledWindow,
    items: RefCell<Vec<TodoItem>>,
    focus_row: Rc<RefCell<Option<gtk::Widget>>>,
    /// Bumped on every update so a stale "hide after 5s" timer does nothing.
    generation: Rc<Cell<u64>>,
}

impl TaskPanel {
    fn new(panel: &gtk::Box) -> Self {
        let menu = gtk::MenuButton::new();
        menu.add_css_class("lc-task-bar");
        menu.set_always_show_arrow(false);
        menu.set_direction(gtk::ArrowType::Up);
        menu.set_visible(false);
        menu.set_tooltip_text(Some("Show the task list"));

        let inner = gtk::Box::new(gtk::Orientation::Vertical, 3);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.add_css_class("lc-tiny");
        row.append(&label);
        let chevron = gtk::Label::new(Some("\u{25be}"));
        chevron.add_css_class("lc-tiny");
        chevron.add_css_class("lc-dim");
        row.append(&chevron);
        inner.append(&row);

        let progress = gtk::ProgressBar::new();
        progress.add_css_class("lc-task-progress");
        progress.set_show_text(false);
        progress.set_fraction(0.0);
        inner.append(&progress);
        menu.set_child(Some(&inner));

        let popover = gtk::Popover::new();
        popover.add_css_class("lc-task-popover");
        popover.set_autohide(true);
        popover.set_has_arrow(false);
        popover.set_position(gtk::PositionType::Top);
        menu.set_popover(Some(&popover));

        let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        list.set_margin_start(10);
        list.set_margin_end(10);
        list.set_margin_top(8);
        list.set_margin_bottom(8);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .child(&list)
            .build();
        popover.set_child(Some(&scroll));

        // Esc closes the checklist; the conversation underneath is untouched.
        {
            let popover_for_key = popover.clone();
            let key = gtk::EventControllerKey::new();
            key.connect_key_pressed(move |_, keyval, _, _| {
                if keyval == gtk::gdk::Key::Escape {
                    popover_for_key.popdown();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            popover.add_controller(key);
        }

        let focus_row: Rc<RefCell<Option<gtk::Widget>>> = Rc::new(RefCell::new(None));
        let generation = Rc::new(Cell::new(0u64));

        // Size the popover to the panel: at most half the panel height and the
        // same width as the bar. `scroll_to_row` centres the in-progress item.
        {
            let panel = panel.clone();
            let scroll = scroll.clone();
            let popover_for_show = popover.clone();
            let focus = focus_row.clone();
            popover.connect_show(move |_| {
                let max_height = (panel.height() / 2).max(120);
                scroll.set_max_content_height(max_height);
                popover_for_show.set_size_request((panel.width() - 12).max(220), -1);
                if let Some(row) = focus.borrow().clone() {
                    scroll_to_row(&scroll, &row);
                }
            });
        }

        // Keep the half-panel height cap while the window is resized.
        {
            let scroll = scroll.clone();
            panel.connect_notify_local(Some("height"), move |panel, _| {
                scroll.set_max_content_height((panel.height() / 2).max(120));
            });
        }

        Self {
            menu,
            panel: panel.clone(),
            popover,
            label,
            progress,
            list,
            scroll,
            items: RefCell::new(Vec::new()),
            focus_row,
            generation,
        }
    }

    /// Rebuild the bar and checklist from the current plan.
    fn render(&self, items: &[TodoItem]) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        *self.items.borrow_mut() = items.to_vec();
        chat_message::clear(&self.list);
        self.focus_row.replace(None);

        if items.is_empty() {
            self.popover.popdown();
            self.menu.set_visible(false);
            return;
        }

        let total = items.len();
        let done = items
            .iter()
            .filter(|item| item.status == TodoStatus::Done)
            .count();
        let all_done = done == total;
        let active = items
            .iter()
            .find(|item| item.status == TodoStatus::InProgress);

        let summary = if all_done {
            format!("\u{2713} All tasks complete \u{00b7} {done}/{total}")
        } else if let Some(item) = active {
            format!("\u{25d0} {} \u{00b7} {done}/{total}", item.text)
        } else {
            format!("{done}/{total} tasks")
        };
        self.label.set_text(&summary);
        self.label.set_tooltip_text(active.map(|item| item.text.clone()).as_deref());
        if all_done {
            self.menu.add_css_class("lc-task-bar-complete");
        } else {
            self.menu.remove_css_class("lc-task-bar-complete");
        }
        self.progress.set_fraction(done as f64 / total as f64);
        self.menu.set_visible(true);

        for item in items {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.add_css_class("lc-task-row");

            let (icon, icon_class) = match item.status {
                TodoStatus::Done => ("\u{2713}", "lc-task-icon-done"),
                TodoStatus::InProgress => ("\u{25d0}", "lc-task-icon-active"),
                TodoStatus::Pending => ("\u{25cb}", "lc-task-icon-pending"),
            };
            let marker = gtk::Label::new(Some(icon));
            marker.add_css_class("lc-task-icon");
            marker.add_css_class(icon_class);
            marker.set_xalign(0.0);
            marker.set_width_chars(1);
            row.append(&marker);

            let text = gtk::Label::new(Some(&item.text));
            text.set_xalign(0.0);
            text.set_wrap(true);
            text.set_wrap_mode(gtk::pango::WrapMode::Char);
            text.set_hexpand(true);
            // Keep the popover to the panel's width so a long task wraps
            // instead of widening the checklist across the editor.
            text.set_max_width_chars(
                (self.panel.width().saturating_sub(56) / 7).clamp(20, 80),
            );
            text.add_css_class("lc-tiny");
            match item.status {
                TodoStatus::Done => text.add_css_class("lc-todo-done"),
                TodoStatus::InProgress => text.add_css_class("lc-todo-active"),
                TodoStatus::Pending => text.add_css_class("lc-dim"),
            }
            row.append(&text);
            self.list.append(&row);

            if item.status == TodoStatus::InProgress && self.focus_row.borrow().is_none() {
                self.focus_row
                    .replace(Some(row.clone().upcast::<gtk::Widget>()));
            }
        }

        // The checklist may already be open when a new plan arrives; keep the
        // in-progress row in view.
        if self.popover.is_visible() {
            if let Some(row) = self.focus_row.borrow().clone() {
                scroll_to_row(&self.scroll, &row);
            }
        }

        // Completed plans tidy themselves away, but a later update re-shows the
        // bar because the generation check fails the stale timer.
        if all_done {
            let menu = self.menu.clone();
            let popover = self.popover.clone();
            let generation_cell = self.generation.clone();
            glib::timeout_add_local_once(Duration::from_secs(5), move || {
                if generation_cell.get() != generation {
                    return;
                }
                popover.popdown();
                menu.set_visible(false);
            });
        }
    }

    /// Drop the plan for a new or cleared conversation.
    pub fn clear(&self) {
        self.generation.set(self.generation.get() + 1);
        self.items.borrow_mut().clear();
        chat_message::clear(&self.list);
        self.focus_row.replace(None);
        self.popover.popdown();
        self.menu.set_visible(false);
    }
}

/// Scroll a checklist so `row` sits near the middle of the viewport.
fn scroll_to_row(scroll: &gtk::ScrolledWindow, row: &gtk::Widget) {
    let scroll = scroll.clone();
    let row = row.clone();
    glib::idle_add_local_once(move || {
        let adjustment = scroll.vadjustment();
        let allocation = row.allocation();
        let page = adjustment.page_size();
        let target = (allocation.y() as f64 + allocation.height() as f64 - page / 2.0)
            .clamp(adjustment.lower(), (adjustment.upper() - page).max(adjustment.lower()));
        adjustment.set_value(target);
    });
}

/// Forwards live tool output (shell progress, large writes) into the UI event
/// stream so the tool card updates while the command is still running.
struct EventStream {
    tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
}

impl link_core::tool::ToolStream for EventStream {
    fn chunk(&self, call_id: &str, text: &str) {
        let _ = self.tx.send(AgentEvent::ToolOutputDelta {
            call_id: call_id.to_string(),
            chunk: text.to_string(),
        });
    }
}

pub fn build(state: &Rc<UiState>) -> AgentPanel {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("lc-agent");

    // ---- floating top row ----------------------------------------------
    // A translucent strip over the conversation: sidebar toggle on the left,
    // Browser + Terminal pill on the right. It floats so the chat itself gets
    // the full height of the column.
    let top_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    top_row.add_css_class("lc-agent-top");
    top_row.set_valign(gtk::Align::Start);
    top_row.set_halign(gtk::Align::Fill);
    top_row.set_margin_start(8);
    top_row.set_margin_end(8);
    top_row.set_margin_top(6);
    top_row.set_can_target(false);

    let sidebar_toggle = gtk::Button::from_icon_name("sidebar-show-symbolic");
    sidebar_toggle.add_css_class("lc-agent-toggle");
    sidebar_toggle.set_tooltip_text(Some("Toggle sidebar (Ctrl+B)"));
    {
        let state = state.clone();
        sidebar_toggle.connect_clicked(move |_| crate::window::toggle_sidebar(&state));
    }
    top_row.append(&sidebar_toggle);

    let top_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    top_spacer.set_hexpand(true);
    top_spacer.set_can_target(false);
    top_row.append(&top_spacer);

    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    tools.add_css_class("lc-agent-tools");
    let browser = tool_pill(
        "applications-internet-symbolic",
        "Browser",
        "Open http://localhost:3000 in your browser",
    );
    {
        let state = state.clone();
        browser.connect_clicked(move |_| {
            let result = gtk::gio::AppInfo::launch_default_for_uri(
                "http://localhost:3000",
                None::<&gtk::gio::AppLaunchContext>,
            );
            if result.is_err() {
                state.toast("Could not launch a browser");
            }
        });
    }
    tools.append(&browser);
    let terminal_btn = tool_pill(
        "utilities-terminal-symbolic",
        "Terminal",
        "Show the terminal (Ctrl+J)",
    );
    {
        let state = state.clone();
        terminal_btn.connect_clicked(move |_| {
            if let Some(widgets) = state.widgets() {
                widgets.bottom_root.set_visible(true);
                widgets.bottom.focus_terminal();
            }
        });
    }
    tools.append(&terminal_btn);
    top_row.append(&tools);

    // ---- message log ----------------------------------------------------
    let chat_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
    chat_list.set_margin_start(12);
    chat_list.set_margin_end(12);
    chat_list.set_margin_top(46);
    chat_list.set_margin_bottom(8);

    let chat_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&chat_list)
        .build();
    chat_scroll.add_css_class("lc-conversation");

    // ---- live activity --------------------------------------------------
    // One line that says what the agent is doing right now. It replaces
    // silence: every tool start, approval wait and failure is narrated here.
    let activity = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    activity.set_margin_start(12);
    activity.set_margin_end(12);
    activity.set_margin_bottom(4);
    activity.set_visible(false);
    let activity_spinner = gtk::Spinner::new();
    activity_spinner.set_valign(gtk::Align::Center);
    activity.append(&activity_spinner);
    let activity_label = gtk::Label::new(None);
    activity_label.add_css_class("lc-tiny");
    activity_label.add_css_class("lc-dim");
    activity_label.set_xalign(0.0);
    activity_label.set_hexpand(true);
    // Bounded and ellipsized so a long command line cannot widen the window.
    activity_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    activity_label.set_max_width_chars(32);
    activity.append(&activity_label);

    // ---- suggestion chips ----------------------------------------------
    // Vertical, full-width rows: a horizontal row of variable-length chips
    // used to set the window's *minimum* width, so three long suggestions
    // could stretch the whole window from 960px to over 1600px.
    let suggestions_box = gtk::Box::new(gtk::Orientation::Vertical, 3);
    suggestions_box.set_margin_start(12);
    suggestions_box.set_margin_end(12);
    suggestions_box.set_margin_bottom(6);
    suggestions_box.set_visible(false);

    // ---- composer -------------------------------------------------------
    let composer_area = gtk::Box::new(gtk::Orientation::Vertical, 4);
    composer_area.add_css_class("lc-input-bar");

    let attachments_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    // Attachment chips contain full file paths. Keep them in a horizontal
    // scroller so a deep path scrolls instead of widening the window.
    let attachments_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&attachments_box)
        .build();
    attachments_scroll.set_visible(false);
    composer_area.append(&attachments_scroll);

    // The compact task bar sits directly above the input. The full checklist
    // is a popover, so it never changes the conversation's height.
    let tasks = Rc::new(TaskPanel::new(&root));
    composer_area.append(&tasks.menu);

    // Context usage: a hairline meter with a right-aligned token count.
    let context_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    context_row.add_css_class("lc-context-row");
    let context_bar = gtk::ProgressBar::new();
    context_bar.add_css_class("lc-context-meter");
    context_bar.set_show_text(false);
    context_bar.set_fraction(0.0);
    context_bar.set_hexpand(true);
    context_bar.set_valign(gtk::Align::Center);
    context_bar.set_tooltip_text(Some("Context window usage"));
    context_row.append(&context_bar);
    let context_label = gtk::Label::new(Some("0 / 0"));
    context_label.add_css_class("lc-context-label");
    context_label.add_css_class("lc-mono");
    context_label.set_xalign(1.0);
    context_row.append(&context_label);
    composer_area.append(&context_row);

    let composer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    composer.add_css_class("lc-composer");

    let input = gtk::TextView::new();
    input.set_wrap_mode(gtk::WrapMode::WordChar);
    input.set_accepts_tab(false);
    input.set_top_margin(2);
    input.set_bottom_margin(2);
    input.set_left_margin(0);
    input.set_right_margin(0);
    input.add_css_class("lc-input");
    input.set_tooltip_text(Some(
        "Enter to send \u{00b7} Shift+Enter for a new line \u{00b7} @ attaches a file",
    ));

    let input_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(44)
        .max_content_height(160)
        .propagate_natural_height(true)
        .child(&input)
        .build();

    let input_overlay = gtk::Overlay::new();
    input_overlay.set_child(Some(&input_scroll));
    let placeholder = gtk::Label::new(Some("Ask AI or enter a command..."));
    placeholder.add_css_class("lc-composer-placeholder");
    placeholder.set_halign(gtk::Align::Start);
    placeholder.set_valign(gtk::Align::Start);
    placeholder.set_margin_start(10);
    placeholder.set_margin_top(10);
    placeholder.set_can_target(false);
    input_overlay.add_overlay(&placeholder);
    composer.append(&input_overlay);

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("lc-composer-bar");

    let attach = gtk::Button::from_icon_name("list-add-symbolic");
    attach.add_css_class("lc-plus");
    attach.set_tooltip_text(Some("Attach a workspace file (@)"));
    {
        let state = state.clone();
        attach.connect_clicked(move |_| attach_file(&state));
    }
    bar.append(&attach);

    let composer_model_button = gtk::Button::new();
    crate::widgets::set_button_text_for(
        &composer_model_button,
        &state.chat_model_label(),
        18,
        gtk::pango::EllipsizeMode::End,
    );
    composer_model_button.add_css_class("lc-model-chip");
    composer_model_button.set_tooltip_text(Some("Select model"));
    {
        let state = state.clone();
        composer_model_button.connect_clicked(move |button| {
            crate::widgets::model_badge::show_picker(&state, button.upcast_ref());
        });
    }
    bar.append(&composer_model_button);

    let bar_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bar_spacer.set_hexpand(true);
    bar.append(&bar_spacer);

    let mode_button = gtk::Button::with_label(state.mode().label());
    mode_button.add_css_class("lc-mode-pill");
    mode_button.set_tooltip_text(Some(state.mode().description()));
    if state.mode() == PermissionMode::FullAuto {
        mode_button.add_css_class("full-auto");
    }
    {
        let state = state.clone();
        mode_button.connect_clicked(move |_| state.cycle_mode());
    }
    bar.append(&mode_button);

    let stop_button = gtk::Button::from_icon_name("media-playback-stop-symbolic");
    stop_button.add_css_class("lc-stop");
    stop_button.set_tooltip_text(Some("Stop the agent"));
    stop_button.set_visible(false);
    {
        let state = state.clone();
        stop_button.connect_clicked(move |_| stop(&state));
    }
    bar.append(&stop_button);

    let send_button = gtk::Button::from_icon_name("go-next-symbolic");
    send_button.add_css_class("lc-send");
    send_button.set_tooltip_text(Some("Send (Enter)"));
    {
        let state = state.clone();
        send_button.connect_clicked(move |_| send_message(&state));
    }
    bar.append(&send_button);

    composer.append(&bar);
    composer_area.append(&composer);

    // Keep the placeholder and the send state in step with the buffer.
    {
        let placeholder = placeholder.clone();
        let send_button = send_button.clone();
        input.buffer().connect_changed(move |buffer| {
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
            placeholder.set_visible(text.is_empty());
            if text.trim().is_empty() {
                send_button.remove_css_class("lc-send-active");
            } else {
                send_button.add_css_class("lc-send-active");
            }
        });
    }

    // Enter sends, Shift+Enter inserts a newline; the configured `agent.send`
    // accelerator (Ctrl+Enter by default) sends too.
    let (send_key, send_mods) =
        gtk::accelerator_parse(&crate::app::send_shortcut(&state.config.borrow()))
            .unwrap_or((gtk::gdk::Key::VoidSymbol, gtk::gdk::ModifierType::empty()));
    let default_mods = gtk::accelerator_get_default_mod_mask();
    let send_mods = send_mods & default_mods;
    let key = gtk::EventControllerKey::new();
    {
        let state = state.clone();
        key.connect_key_pressed(move |_, keyval, _, modifier| {
            let mods = modifier & default_mods;
            let enter = keyval == gtk::gdk::Key::Return || keyval == gtk::gdk::Key::KP_Enter;
            let plain_enter = enter
                && !mods.intersects(
                    gtk::gdk::ModifierType::SHIFT_MASK
                        | gtk::gdk::ModifierType::CONTROL_MASK
                        | gtk::gdk::ModifierType::ALT_MASK,
                );
            let configured =
                send_key != gtk::gdk::Key::VoidSymbol && keyval == send_key && mods == send_mods;
            if plain_enter || configured {
                send_message(&state);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    input.add_controller(key);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&chat_scroll);
    body.append(&activity);
    body.append(&suggestions_box);
    body.append(&composer_area);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&body));
    overlay.add_overlay(&top_row);
    overlay.set_vexpand(true);
    root.append(&overlay);

    AgentPanel {
        root,
        chat_list,
        chat_scroll,
        activity,
        activity_spinner,
        activity_label,
        input,
        send_button,
        stop_button,
        mode_button,
        composer_model_button,
        context_bar,
        context_label,
        tasks,
        suggestions_box,
        attachments_box,
        attachments_scroll,
    }
}

/// Small icon + text button used inside the floating Browser/Terminal pill.
fn tool_pill(icon: &str, label: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.set_tooltip_text(Some(tooltip));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    row.set_margin_start(9);
    row.set_margin_end(10);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(15);
    row.append(&image);
    let text = gtk::Label::new(Some(label));
    text.add_css_class("lc-tiny");
    row.append(&text);
    button.set_child(Some(&row));
    button
}

// ---------------------------------------------------------------------------
// Composer helpers
// ---------------------------------------------------------------------------

pub fn input_text(input: &gtk::TextView) -> String {
    let buffer = input.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string()
}

pub fn clear_input(input: &gtk::TextView) {
    input.buffer().set_text("");
}

pub fn refresh_composer(state: &Rc<UiState>) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    let busy = state.busy.get();
    widgets.send_button.set_visible(!busy);
    widgets.stop_button.set_visible(busy);
    let mode = state.mode();
    widgets.mode_button.set_label(mode.label());
    widgets
        .mode_button
        .set_tooltip_text(Some(mode.description()));
    if mode == PermissionMode::FullAuto {
        widgets.mode_button.add_css_class("full-auto");
    } else {
        widgets.mode_button.remove_css_class("full-auto");
    }
}

pub fn stop(state: &Rc<UiState>) {
    if !state.busy.get() {
        return;
    }
    if let Some(handle) = state.agent_handle.borrow().as_ref() {
        handle.stop();
    }
    state.toast("Stopping the agent\u{2026}");
}

pub fn scroll_to_bottom(widgets: &crate::state::Widgets) {
    thread_local! {
        static PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    // Streaming emits deltas far faster than the screen refreshes. One queued
    // scroll per frame is enough; the callback recomputes the target, so the
    // skipped calls lose nothing.
    if PENDING.with(|pending| pending.replace(true)) {
        return;
    }
    let adjustment = widgets.chat_scroll.vadjustment();
    glib::timeout_add_local_once(Duration::from_millis(30), move || {
        PENDING.with(|pending| pending.set(false));
        let target = adjustment.upper() - adjustment.page_size();
        adjustment.set_value(target.max(0.0));
    });
}

/// Show what the agent is doing right now. Cheap to call every event: the
/// label text is compared first, so repeated deltas do no widget work.
fn set_activity(widgets: &crate::state::Widgets, text: &str, css: &str) {
    widgets.activity.set_visible(true);
    if !widgets.activity_spinner.is_spinning() {
        widgets.activity_spinner.start();
    }
    if widgets.activity_label.text().as_str() == text {
        return;
    }
    for class in ["lc-dim", "lc-warn", "lc-error", "lc-ok"] {
        widgets.activity_label.remove_css_class(class);
    }
    widgets.activity_label.add_css_class(css);
    widgets.activity_label.set_text(text);
    widgets.activity_label.set_tooltip_text(Some(text));
}

fn clear_activity(widgets: &crate::state::Widgets) {
    widgets.activity_spinner.stop();
    widgets.activity.set_visible(false);
}

/// Compact token counts for the context label: `90.3k / 1.0M`.
fn human_tokens(value: usize) -> String {
    let value = value as f64;
    if value >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value >= 1_000.0 {
        format!("{:.1}k", value / 1_000.0)
    } else {
        format!("{}", value as usize)
    }
}

fn attach_file(state: &Rc<UiState>) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Attach a file")
        .modal(true)
        .build();
    let state = state.clone();
    dialog.open(
        Some(&widgets.window),
        None::<&gtk::gio::Cancellable>,
        move |result| {
            if let Ok(file) = result {
                if let Some(path) = file.path() {
                    state.push_attachment(&path);
                    refresh_attachments(&state);
                    state.toast(&format!("Attached {}", state.relative_to_workspace(&path)));
                }
            }
        },
    );
}

pub fn refresh_attachments(state: &Rc<UiState>) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    let attachments = state.attachments.borrow().clone();
    chat_message::clear(&widgets.attachments_box);
    widgets.attachments_scroll.set_visible(!attachments.is_empty());
    for attachment in attachments {
        let chip = gtk::Button::new();
        crate::widgets::set_button_text(&chip, &format!("\u{1f4ce} {attachment}"), 40);
        chip.add_css_class("lc-chip");
        chip.set_tooltip_text(Some(&format!("Remove {attachment}")));
        let state = state.clone();
        chip.connect_clicked(move |_| {
            state.attachments.borrow_mut().retain(|item| item != &attachment);
            refresh_attachments(&state);
        });
        widgets.attachments_box.append(&chip);
    }
}

// ---------------------------------------------------------------------------
// Sending a turn
// ---------------------------------------------------------------------------

pub fn send_message(state: &Rc<UiState>) {
    if state.busy.get() {
        state.toast("The agent is still working \u{2014} press Stop first");
        return;
    }
    let Some(widgets) = state.widgets() else {
        return;
    };
    let text = input_text(&widgets.input);
    if text.trim().is_empty() {
        return;
    }
    let Some(workspace) = state.workspace_path() else {
        state.toast("Open a folder first (Ctrl+O)");
        return;
    };
    let Some(model) = state.chat_model() else {
        state.toast("Add a model first (Ctrl+M)");
        crate::dialogs::models::present(state);
        return;
    };
    let Some(session_id) = state.ensure_session() else {
        return;
    };

    // Conversation history as it stood before this message; `send()` appends
    // the new user turn itself.
    let history = {
        let slot = state.session.borrow();
        slot.as_ref().map(build_history).unwrap_or_default()
    };

    clear_input(&widgets.input);
    let attachments = state.take_attachments();
    refresh_attachments(state);

    let bubble = chat_message::user_bubble(text.trim());
    widgets.chat_list.append(&bubble);
    scroll_to_bottom(&widgets);

    state.push_user_message(text.trim());
    *state.turn_text.borrow_mut() = String::new();
    *state.turn_thinking.borrow_mut() = String::new();
    *state.turn_model.borrow_mut() = model.badge();
    state.blocks.borrow_mut().clear();
    state.block_order.borrow_mut().clear();
    state.tools.borrow_mut().clear();
    state.edits.borrow_mut().clear();

    state.busy.set(true);
    refresh_composer(state);

    // ---- build the loop -------------------------------------------------
    let config = state.config.borrow().clone();
    let sandbox = match Sandbox::new(&workspace) {
        Ok(sandbox) => sandbox,
        Err(error) => {
            state.toast(&format!("Cannot sandbox the workspace: {error}"));
            state.busy.set(false);
            refresh_composer(state);
            return;
        }
    };

    let mut tool_ctx = ToolContext::new(sandbox)
        .with_shell_timeout(config.shell_timeout())
        .with_output_cap(config.agent.tool_output_cap);
    tool_ctx.protect_secrets = config.agent.protect_secrets;

    // The event channel is created before the tool context so `bash` output
    // can stream into the tool card while the command runs.
    let (event_tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tool_ctx = tool_ctx
        .with_stream(Arc::new(EventStream {
            tx: event_tx.clone(),
        }))
        .with_asker(Arc::new(GtkAsker::new(widgets.window.clone())));

    let tools = builtin_registry(config.agent.enable_web_tools);
    let memory = ProjectMemory::load(&workspace);
    let mut rules = merge_rules(&config.permissions, &workspace);
    rules.deny_always.extend(config.permissions.deny_always.iter().cloned());
    let engine = PermissionEngine::new(state.mode(), rules);

    let provider = state.rotating_provider(&model);

    // Build the effective system prompt: user's override (if any) + built-in
    // skills that give the agent web, debugging, and full-stack coding abilities.
    const SKILLS_ADDENDUM: &str = "\
## Developer Skills

You are an expert full-stack developer and AI coding assistant. You have deep \
knowledge in:

- **Web development**: HTML5, CSS3, JavaScript/TypeScript, React, Vue, Svelte, \
  Next.js, Nuxt, Tailwind CSS, responsive design, CSS animations, WCAG accessibility
- **Backend**: Node.js, Python (FastAPI/Django/Flask), Rust, Go, Java, PHP, REST \
  and GraphQL APIs
- **Databases**: PostgreSQL, MySQL, SQLite, MongoDB, Redis
- **DevOps**: Docker, CI/CD pipelines, Linux administration, shell scripting
- **Debugging**: Reading stack traces, profiling, systematic root-cause analysis, \
  reproducing minimal test cases
- **Code quality**: Unit, integration, and e2e testing; linting; type safety; \
  performance optimisation
- **Design**: Colour theory, typography, modern UI patterns, design systems

### How you work
1. Read existing code *before* writing new code — never guess at structure.
2. Narrate every significant step: what you found, what you are about to do, and why.
3. Run tests after changes when a test suite exists.
4. For web projects, verify cross-browser compatibility and mobile layout.
5. When debugging, state your hypothesis before trying a fix.
6. Write production-quality code — not demos or prototypes.
7. If a task is complex, split it into clear sub-tasks and tackle them one by one.
8. Never stop mid-task. If you reach an iteration limit, continue automatically.";

    let effective_system_prompt: Option<String> = {
        let mut parts: Vec<&str> = Vec::new();
        if let Some(ref user_override) = model.system_prompt {
            let trimmed = user_override.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed);
            }
        }
        parts.push(SKILLS_ADDENDUM);
        Some(parts.join("\n\n"))
    };

    let options = AgentOptions {
        // The provider expects the endpoint model name here, not the local
        // configuration id (which carries a random suffix).
        model_name: model.model_id.clone(),
        model_label: model.badge(),
        system_prompt_override: effective_system_prompt,
        project_memory: memory,
        max_iterations: config.agent.max_tool_iterations.max(1),
        max_output_tokens: config.agent.max_output_tokens.max(256),
        temperature: model.temperature,
        auto_compact_threshold: config.agent.auto_compact_threshold.clamp(0.1, 0.98),
        generate_suggestions: config.agent.generate_suggestions,
        tools_enabled: provider.capabilities().tools,
        protect_secrets: config.agent.protect_secrets,
        context_window: provider.context_window().max(1024),
    };

    let sink = link_core::event::ChannelSink::from_sender(event_tx);
    let approver = Arc::new(GtkApprover::new(widgets.window.clone(), workspace.clone()));
    let handle = state
        .agent_handle
        .borrow()
        .clone()
        .unwrap_or_default();
    *state.agent_handle.borrow_mut() = Some(handle.clone());

    let mut runner = link_core::AgentLoop::new(
        provider,
        tools,
        tool_ctx,
        Arc::new(sink),
        approver,
        engine,
        options,
    )
    .with_history(history)
    .with_handle(handle);

    let text = text.trim().to_string();
    let _ = session_id;
    runtime().spawn(async move {
        if let Err(error) = runner.send(text, attachments).await {
            tracing::warn!(?error, "agent turn failed");
        }
    });

    // ---- drain events on the GTK thread --------------------------------
    let state_for_events = state.clone();
    glib::MainContext::default().spawn_local(async move {
        while let Some(event) = rx.recv().await {
            handle_event(&state_for_events, event);
        }
        // Safety net: if the sink was dropped without a terminal `Done`
        // event, the composer would stay locked forever.
        if state_for_events.busy.get() {
            if let Some(widgets) = state_for_events.widgets() {
                clear_activity(&widgets);
            }
            finish_blocks(&state_for_events);
            state_for_events.busy.set(false);
            refresh_composer(&state_for_events);
            state_for_events.commit_turn();
            *state_for_events.turn_text.borrow_mut() = String::new();
            *state_for_events.turn_thinking.borrow_mut() = String::new();
            state_for_events.toast("The agent stopped unexpectedly");
            if let Some(widgets) = state_for_events.widgets() {
                crate::panels::sidebar::refresh_history(&state_for_events, &widgets.sidebar);
            }
        }
    });
}

fn build_history(session: &Session) -> Vec<ChatMessage> {
    session
        .messages
        .iter()
        .filter_map(|message| match message.role {
            StoredRole::User => Some(ChatMessage::user(message.content.clone())),
            StoredRole::Assistant => Some(ChatMessage::assistant(message.content.clone())),
            _ => None,
        })
        .collect()
}

fn merge_rules(base: &PermissionRules, workspace: &std::path::Path) -> PermissionRules {
    let mut rules = base.clone();
    let project = WorkspaceSettings::load(workspace);
    rules.allow_always.extend(project.permissions.allow_always);
    rules.deny_always.extend(project.permissions.deny_always);
    rules
}

// ---------------------------------------------------------------------------
// Event rendering
// ---------------------------------------------------------------------------

pub fn handle_event(state: &Rc<UiState>, event: AgentEvent) {
    let Some(widgets) = state.widgets() else {
        return;
    };
    match event {
        AgentEvent::TurnStarted { model, .. } => {
            *state.turn_model.borrow_mut() = model.clone();
            set_activity(&widgets, &format!("▶ Starting with {} — reading context…", model), "lc-dim");
        }
        AgentEvent::TurnFinished { reason, .. } => {
            if reason != DoneReason::Cancelled {
                finish_blocks(state);
            }
        }
        AgentEvent::BlockStarted { block_id } => {
            // Tool call ids repeat across model rounds (some providers use
            // `call_0`, `call_1` every time), so start each round with a fresh
            // chip map. Everything from the previous round has already been
            // finalised by its ToolResult.
            state.edits.borrow_mut().clear();
            state.tools.borrow_mut().clear();
            ensure_block(state, &widgets, block_id);
        }
        AgentEvent::TokenDelta { block_id, text } => {
            if let Some(block) = ensure_block(state, &widgets, block_id) {
                block.append_text(&text);
                state.turn_text.borrow_mut().push_str(&text);
                set_activity(&widgets, "✍ Writing reply…", "lc-dim");
                scroll_to_bottom(&widgets);
            }
        }
        AgentEvent::ThinkingDelta { block_id, text } => {
            if let Some(block) = ensure_block(state, &widgets, block_id) {
                block.append_thinking(&text);
                state.turn_thinking.borrow_mut().push_str(&text);
                set_activity(&widgets, "💭 Reasoning through the problem…", "lc-dim");
                scroll_to_bottom(&widgets);
            }
        }
        AgentEvent::TextComplete { block_id, text } => {
            if let Some(block) = ensure_block(state, &widgets, block_id) {
                block.complete(&text);
                rebuild_turn_text(state);
                // Push every code block in this response to the editor.
                push_code_to_editor(state, &block_id.to_string(), &text);
            }
        }
        AgentEvent::EditorStreamStarted {
            call_id,
            tool,
            path,
            new_file,
        } => {
            // Create the chip up front so the conversation shows one compact
            // line while the editor is being typed into live.
            let existing = state.edits.borrow().get(&call_id).cloned();
            let chip = match existing {
                Some(chip) => chip,
                None => {
                    let chip = EditChip::new(state, &call_id, &tool, &path);
                    state
                        .edits
                        .borrow_mut()
                        .insert(call_id.clone(), chip.clone());
                    widgets.chat_list.append(&chip.root);
                    chip
                }
            };
            chip.set_path(&path);
            chip.set_status(link_core::event::ToolStatus::Running);
            if new_file {
                crate::panels::sidebar::refresh_files_deferred(state, &widgets.sidebar);
            }
            crate::panels::editor::begin_ai_edit(state, &call_id, &tool, &path, new_file);
            set_activity(&widgets, &format!("✎ Editing {path}…"), "lc-dim");
            scroll_to_bottom(&widgets);
        }
        AgentEvent::EditorStreamDelta {
            call_id,
            field,
            text,
        } => {
            crate::panels::editor::push_ai_delta(state, &call_id, field, &text);
        }
        AgentEvent::EditorStreamEnded {
            call_id,
            path,
            completed,
        } => {
            crate::panels::editor::end_ai_edit(state, &call_id);
            if !completed {
                // The stream was interrupted: drop the half-typed buffer and
                // show what is actually on disk.
                crate::panels::editor::reload_force(state, std::path::Path::new(&path));
                set_activity(&widgets, &format!("⚠ Interrupted while editing {path}"), "lc-error");
            } else {
                set_activity(&widgets, &format!("✎ Wrote {path}"), "lc-dim");
            }
        }
        AgentEvent::ToolRequested {
            block_id,
            call_id,
            tool,
            summary,
            args,
        } => {
            // `update_tasks` has no chip: its payload lands in the pinned
            // TASKS checklist through `TodosUpdated`.
            if tool == "update_tasks" {
                return;
            }
            if is_file_edit_tool(&tool) {
                let path = args
                    .get("path")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let existing = state.edits.borrow().get(&call_id).cloned();
                let chip = match existing {
                    Some(chip) => {
                        if !path.is_empty() {
                            chip.set_path(&path);
                        }
                        chip
                    }
                    None => {
                        let chip = EditChip::new(state, &call_id, &tool, &path);
                        state
                            .edits
                            .borrow_mut()
                            .insert(call_id.clone(), chip.clone());
                        match ensure_block(state, &widgets, block_id) {
                            Some(block) => block.add_widget(&chip.root),
                            None => widgets.chat_list.append(&chip.root),
                        }
                        chip
                    }
                };
                chip.set_status(link_core::event::ToolStatus::Running);
                let _ = summary;
                scroll_to_bottom(&widgets);
                return;
            }
            let chip = ToolChip::new(state, &call_id, &tool, &summary, &args);
            state
                .tools
                .borrow_mut()
                .insert(call_id.clone(), chip.clone());
            match ensure_block(state, &widgets, block_id) {
                Some(block) => block.add_widget(&chip.root),
                None => widgets.chat_list.append(&chip.root),
            }
            set_activity(&widgets, &format!("🔧 {}", summary), "lc-dim");
            scroll_to_bottom(&widgets);
        }
        AgentEvent::ToolStatusChanged { call_id, status } => {
            if let Some(chip) = state.edits.borrow().get(&call_id).cloned() {
                chip.set_status(status);
                // A denied or failed write never reaches disk: drop the
                // live-preview text and restore the file the user already had.
                if matches!(
                    status,
                    link_core::event::ToolStatus::Denied | link_core::event::ToolStatus::Failed
                ) && !chip.path().is_empty()
                {
                    crate::panels::editor::reload_force(
                        state,
                        std::path::Path::new(&chip.path()),
                    );
                }
            }
            if let Some(chip) = state.tools.borrow().get(&call_id).cloned() {
                chip.set_status(status);
            }
        }
        AgentEvent::ToolOutputDelta { call_id, chunk } => {
            if let Some(chip) = state.tools.borrow().get(&call_id).cloned() {
                chip.append_output(&chunk);
                scroll_to_bottom(&widgets);
            }
        }
        AgentEvent::ToolResult {
            call_id,
            tool,
            ok,
            content,
            duration_ms,
            truncated: _,
        } => {
            let chip = state.edits.borrow().get(&call_id).cloned();
            if let Some(chip) = &chip {
                chip.set_status(if ok {
                    link_core::event::ToolStatus::Done
                } else {
                    link_core::event::ToolStatus::Failed
                });
            }
            if let Some(chip) = state.tools.borrow().get(&call_id).cloned() {
                chip.set_status(if ok {
                    link_core::event::ToolStatus::Done
                } else {
                    link_core::event::ToolStatus::Failed
                });
                if ok {
                    chip.set_output(&content);
                } else if !content.trim().is_empty() {
                    // Failures stay out of the conversation: the chip keeps
                    // its one-line shape and the error becomes a tooltip.
                    chip.set_detail_text(&crate::markdown::summarise(&content, 400));
                }
            }
            if ok {
                set_activity(
                    &widgets,
                    &format!("✓ {tool} done in {duration_ms}ms"),
                    "lc-ok",
                );
            } else {
                // Say plainly that something went wrong, and where to look.
                set_activity(
                    &widgets,
                    &format!("✗ {tool} failed — see the error card above"),
                    "lc-error",
                );
            }
            // A failed stream left the buffer half-typed; pull the real bytes
            // back from disk so the editor never shows a phantom edit.
            if !ok {
                if let Some(chip) = &chip {
                    if !chip.path().is_empty() {
                        crate::panels::editor::reload_force(
                            state,
                            std::path::Path::new(&chip.path()),
                        );
                    }
                }
            }
            // A tool may have created, removed or renamed files; keep the
            // explorer in sync so the user sees the work land.
            if ok
                && (is_file_edit_tool(&tool)
                    || matches!(tool.as_str(), "write_file" | "edit_file" | "multi_edit" | "bash"))
            {
                crate::panels::sidebar::refresh_files_deferred(state, &widgets.sidebar);
            }
        }
        AgentEvent::DiffProposed {
            call_id,
            diff,
            applied,
            ..
        } => {
            let chip = state.edits.borrow().get(&call_id).cloned();
            if let Some(chip) = &chip {
                chip.set_counts(diff.added, diff.removed);
            }
            // Show the new contents in the editor as soon as the write lands,
            // so the user watches the file change instead of waiting a turn.
            if applied {
                // Show an inline red/green review of the change with
                // Accept/Revert controls. This also works for diffs that did
                // not stream into the buffer (it falls back to reloading the
                // file and re-inserting the removed lines).
                crate::panels::editor::show_diff_review(
                    state,
                    std::path::Path::new(&diff.path),
                    &diff,
                );
                crate::panels::sidebar::refresh_files_deferred(state, &widgets.sidebar);
                set_activity(
                    &widgets,
                    &format!("📝 Applied changes to {}", diff.path),
                    "lc-ok",
                );
            } else {
                set_activity(
                    &widgets,
                    &format!("👁 Previewing diff for {}", diff.path),
                    "lc-dim",
                );
            }
        }
        AgentEvent::ApprovalRequested { call_id, request, .. } => {
            if let Some(chip) = state.tools.borrow().get(&call_id).cloned() {
                chip.add_note(
                    &format!("\u{23f3} Waiting for approval: {}", request.summary),
                    "lc-warn",
                );
            }
            set_activity(
                &widgets,
                &format!("Waiting for your approval \u{2014} {}", request.summary),
                "lc-warn",
            );
            scroll_to_bottom(&widgets);
        }
        AgentEvent::ApprovalResolved { call_id, allowed, .. } => {
            if let Some(chip) = state.tools.borrow().get(&call_id).cloned() {
                chip.clear_notes();
                if allowed {
                    chip.add_note("\u{2713} Approved", "lc-ok");
                } else {
                    chip.add_note("\u{2715} Denied by you", "lc-error");
                }
            }
            set_activity(
                &widgets,
                if allowed {
                    "Approved \u{2014} continuing\u{2026}"
                } else {
                    "Denied \u{2014} the agent will adapt\u{2026}"
                },
                if allowed { "lc-dim" } else { "lc-warn" },
            );
        }
        AgentEvent::PlanProposed { block_id, markdown } => {
            let card = chat_message::plan_card(&markdown);
            match ensure_block(state, &widgets, block_id) {
                Some(block) => block.add_widget(&card),
                None => widgets.chat_list.append(&card),
            }
            scroll_to_bottom(&widgets);
        }
        AgentEvent::TodosUpdated { items } => {
            widgets.tasks.render(&items);
        }
        AgentEvent::SuggestionShown { .. } => {}
        AgentEvent::SuggestionsReady { items, .. } => {
            render_suggestions(state, &widgets, &items);
        }
        AgentEvent::SuggestionAccepted { .. } => {}
        AgentEvent::ContextUpdated {
            used_tokens,
            max_tokens,
        } => {
            state.context_used.set(used_tokens);
            state.context_max.set(max_tokens);
            let fraction = if max_tokens > 0 {
                (used_tokens as f64 / max_tokens as f64).clamp(0.0, 1.0)
            } else {
                0.0
            };
            widgets.context_bar.set_fraction(fraction);
            widgets.context_label.set_text(&format!(
                "{} / {}",
                human_tokens(used_tokens),
                human_tokens(max_tokens)
            ));
            widgets
                .context_label
                .set_tooltip_text(Some(&format!("{used_tokens} / {max_tokens} tokens")));
            widgets.context_bar.remove_css_class("lc-meter-warn");
            widgets.context_bar.remove_css_class("lc-meter-full");
            if fraction > 0.9 {
                widgets.context_bar.add_css_class("lc-meter-full");
            } else if fraction > 0.7 {
                widgets.context_bar.add_css_class("lc-meter-warn");
            }
            crate::widgets::status_bar::refresh(state);
        }
        AgentEvent::Usage { cost_usd, .. } => {
            if cost_usd > 0.0 {
                state.cost.set(state.cost.get() + cost_usd);
                crate::widgets::status_bar::refresh(state);
            }
        }
        AgentEvent::Notice { message } => {
            widgets
                .chat_list
                .append(&chat_message::notice(&message));
            scroll_to_bottom(&widgets);
        }
        AgentEvent::ModelChanged { model } => {
            *state.turn_model.borrow_mut() = model;
            state.refresh_model_badges();
        }
        AgentEvent::Error {
            block_id,
            kind,
            message,
            detail,
        } => {
            let card = chat_message::error_card(kind, &message, detail.as_deref());
            match block_id.and_then(|id| ensure_block(state, &widgets, id)) {
                Some(block) => block.add_widget(&card),
                None => widgets.chat_list.append(&card),
            }
            if kind == ErrorKind::NoModel
                || kind == ErrorKind::Auth
                || kind == ErrorKind::MissingApiKey
            {
                state.toast(&message);
            }
            set_activity(&widgets, &format!("\u{2715} {message}"), "lc-error");
            scroll_to_bottom(&widgets);
        }
        AgentEvent::Done { reason } => {
            finish_blocks(state);
            state.busy.set(false);
            refresh_composer(state);
            state.commit_turn();
            *state.turn_text.borrow_mut() = String::new();
            *state.turn_thinking.borrow_mut() = String::new();
            clear_activity(&widgets);

            match reason {
                DoneReason::IterationLimit => {
                    // Task not done — automatically continue instead of stopping silently.
                    // Show a notice so the user knows what happened.
                    widgets.chat_list.append(&chat_message::notice(
                        "⟳ Iteration limit reached — continuing automatically…",
                    ));
                    scroll_to_bottom(&widgets);
                    // Re-send a continuation prompt on the next GTK idle tick so
                    // widgets finish rendering before the next turn starts.
                    let state = state.clone();
                    glib::timeout_add_local_once(Duration::from_millis(80), move || {
                        // Only continue if the user has not already typed something new.
                        if let Some(widgets) = state.widgets() {
                            let current_input = crate::panels::agent::input_text(&widgets.input);
                            if current_input.trim().is_empty() {
                                // Inject a silent continuation message
                                widgets.input.buffer().set_text(
                                    "Continue where you left off. Do not repeat what you already did. Keep working until the task is fully complete.",
                                );
                                send_message(&state);
                            }
                        }
                    });
                    return; // Don't refresh history yet — another turn is starting
                }
                DoneReason::Errored => {
                    state.toast("The turn ended with an error — check the chat for details");
                }
                DoneReason::Cancelled => {
                    // User explicitly stopped; nothing to say
                }
                DoneReason::Completed => {
                    // Normal finish; nothing extra needed
                }
            }

            if let Some(widgets) = state.widgets() {
                crate::panels::sidebar::refresh_history(&state, &widgets.sidebar);
            }
        }
    }
}

fn ensure_block(
    state: &Rc<UiState>,
    widgets: &crate::state::Widgets,
    block_id: uuid::Uuid,
) -> Option<Rc<BlockView>> {
    if let Some(block) = state.blocks.borrow().get(&block_id).cloned() {
        return Some(block);
    }
    let block = Rc::new(BlockView::new());
    // Only the first block of a turn carries the "LINK CODE" header.
    if !state.block_order.borrow().is_empty() {
        block.set_header_visible(false);
    }
    BlockView::wire_copy_btn(&block);
    widgets.chat_list.append(&block.root);
    state.blocks.borrow_mut().insert(block_id, block.clone());
    state.block_order.borrow_mut().push(block_id);
    Some(block)
}

/// Rebuild the turn's persisted text from every block, in creation order.
/// Several text blocks can stream in one turn; each must keep its position.
fn rebuild_turn_text(state: &Rc<UiState>) {
    let order = state.block_order.borrow().clone();
    let blocks = state.blocks.borrow();
    let mut text = String::new();
    for id in order {
        let Some(block) = blocks.get(&id) else {
            continue;
        };
        let part = block.text();
        if part.trim().is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(part.trim_end());
    }
    *state.turn_text.borrow_mut() = text;
}

fn finish_blocks(state: &Rc<UiState>) {
    let ids: Vec<uuid::Uuid> = state.blocks.borrow().keys().copied().collect();
    let mut keep = Vec::new();
    for id in ids {
        let block = state.blocks.borrow().get(&id).cloned();
        let Some(block) = block else {
            continue;
        };
        block.finish();
        if block.is_empty() {
            block.detach();
        } else {
            keep.push(id);
        }
    }
    state.blocks.borrow_mut().retain(|id, _| keep.contains(id));
    // If the first block was empty and got detached, the next one becomes the
    // head of the reply and takes over the "LINK CODE" header.
    let order = state.block_order.borrow().clone();
    let blocks = state.blocks.borrow();
    if let Some(first) = order.iter().find_map(|id| blocks.get(id)) {
        first.set_header_visible(true);
    }
    drop(blocks);
    rebuild_turn_text(state);
}

fn render_suggestions(
    state: &Rc<UiState>,
    widgets: &crate::state::Widgets,
    items: &[String],
) {
    chat_message::clear(&widgets.suggestions_box);
    if items.is_empty() {
        widgets.suggestions_box.set_visible(false);
        return;
    }
    widgets.suggestions_box.set_visible(true);

    // Header row: "Suggestions" label + dismiss (×) button
    let hdr = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let hdr_label = gtk::Label::new(Some("SUGGESTIONS"));
    hdr_label.add_css_class("lc-section");
    hdr_label.set_xalign(0.0);
    hdr_label.set_hexpand(true);
    hdr.append(&hdr_label);

    let dismiss = gtk::Button::from_icon_name("window-close-symbolic");
    dismiss.add_css_class("lc-copy-btn");
    dismiss.set_tooltip_text(Some("Dismiss suggestions"));
    {
        let suggestions_box = widgets.suggestions_box.clone();
        dismiss.connect_clicked(move |_| {
            chat_message::clear(&suggestions_box);
            suggestions_box.set_visible(false);
        });
    }
    hdr.append(&dismiss);
    widgets.suggestions_box.append(&hdr);

    for item in items.iter().take(3) {
        let chip = gtk::Button::new();
        chip.add_css_class("lc-suggestion");
        chip.set_halign(gtk::Align::Fill);
        chip.set_hexpand(true);
        chip.set_tooltip_text(Some(item));
        let label = gtk::Label::new(Some(item));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_max_width_chars(52);
        chip.set_child(Some(&label));
        let state = state.clone();
        let text = item.clone();
        chip.connect_clicked(move |_| {
            if let Some(widgets) = state.widgets() {
                widgets.input.buffer().set_text(&text);
                widgets.input.grab_focus();
                // Dismiss suggestions after selecting one
                chat_message::clear(&widgets.suggestions_box);
                widgets.suggestions_box.set_visible(false);
            }
        });
        widgets.suggestions_box.append(&chip);
    }
}

/// Refresh the local mode chip when it changes outside the panel.
pub fn set_mode_badge(state: &Rc<UiState>) {
    if let Some(widgets) = state.widgets() {
        widgets.mode_button.set_label(state.mode().label());
    }
}

/// Exposed for the command palette and the status bar.
pub fn mode_label(mode: PermissionMode) -> &'static str {
    mode.label()
}

/// Extract every fenced code block from an AI response and open each one in
/// the editor as a preview tab.  Called from the TextComplete handler where
/// `state` is always a live strong reference.
fn push_code_to_editor(state: &Rc<UiState>, block_id: &str, text: &str) {
    let segs = crate::markdown::segments(text);
    let mut seq: u32 = 0;
    for seg in segs {
        if let crate::markdown::Segment::Code { language, code } = seg {
            if code.trim().is_empty() {
                continue;
            }
            let tab_key = format!("{block_id}-{seq}");
            let label = code_tab_label(&language, seq);
            crate::panels::editor::open_code_preview(state, &tab_key, &label, &code);
            seq += 1;
        }
    }
}

/// Derive a human-readable tab label from a language tag.
fn code_tab_label(language: &str, seq: u32) -> String {
    let ext = match language.to_lowercase().as_str() {
        "rust" | "rs"             => "rs",
        "python" | "py"           => "py",
        "javascript" | "js"       => "js",
        "typescript" | "ts"       => "ts",
        "tsx"                     => "tsx",
        "jsx"                     => "jsx",
        "html"                    => "html",
        "css"                     => "css",
        "scss" | "sass"           => "scss",
        "json"                    => "json",
        "toml"                    => "toml",
        "yaml" | "yml"            => "yaml",
        "bash" | "sh" | "shell"   => "sh",
        "sql"                     => "sql",
        "go"                      => "go",
        "java"                    => "java",
        "c"                       => "c",
        "cpp" | "c++"             => "cpp",
        "markdown" | "md"         => "md",
        "xml"                     => "xml",
        "dockerfile"              => "Dockerfile",
        _                         => "txt",
    };
    if seq == 0 { format!("snippet.{ext}") } else { format!("snippet-{seq}.{ext}") }
}
