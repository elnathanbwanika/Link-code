//! Approval modal and the [`Approver`] implementation that shows it.
//!
//! The agent loop runs on a tokio worker thread. `GtkApprover` hops back to the
//! GTK main thread to build the modal, then waits on a `oneshot` channel for the
//! user's decision. Nothing blocks the main loop: the modal itself is fully
//! asynchronous, and the worker thread is parked, not the UI.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use link_core::agent::Approver;
use link_core::permission::{Decision, PermissionRequest, RiskLevel};
use link_core::project::WorkspaceSettings;
use link_core::tool::UserAsker;

use crate::markdown;

type Reply = Rc<RefCell<Option<tokio::sync::oneshot::Sender<Decision>>>>;

/// The UI-backed approver handed to every `AgentLoop`.
pub struct GtkApprover {
    window: glib::SendWeakRef<adw::ApplicationWindow>,
    workspace: PathBuf,
}

impl GtkApprover {
    pub fn new(window: adw::ApplicationWindow, workspace: PathBuf) -> Self {
        Self {
            window: window.downgrade().into(),
            workspace,
        }
    }
}

impl Approver for GtkApprover {
    fn approve(
        &self,
        call_id: String,
        request: PermissionRequest,
    ) -> futures_util::future::BoxFuture<'static, Decision> {
        let window = self.window.clone();
        let workspace = self.workspace.clone();
        Box::pin(async move {
            let (tx, rx) = tokio::sync::oneshot::channel::<Decision>();
            glib::MainContext::default().invoke(move || {
                let parent = window.upgrade();
                show_modal(parent.as_ref(), &workspace, &call_id, request, tx);
            });
            rx.await.unwrap_or(Decision::Deny)
        })
    }
}

fn show_modal(
    parent: Option<&adw::ApplicationWindow>,
    workspace: &std::path::Path,
    call_id: &str,
    request: PermissionRequest,
    sender: tokio::sync::oneshot::Sender<Decision>,
) {
    let window = gtk::Window::builder()
        .title("Approval required")
        .modal(true)
        .resizable(true)
        .default_width(480)
        .default_height(360)
        .build();
    window.add_css_class("lc-dialog");
    if let Some(parent) = parent {
        window.set_transient_for(Some(parent));
    }

    let reply: Reply = Rc::new(RefCell::new(Some(sender)));

    let header = gtk::HeaderBar::new();
    let title = gtk::Label::new(Some("APPROVAL REQUIRED"));
    title.add_css_class("lc-section");
    title.add_css_class("lc-warn");
    header.set_title_widget(Some(&title));
    window.set_titlebar(Some(&header));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class("lc-root");
    root.set_margin_start(16);
    root.set_margin_end(16);
    root.set_margin_top(14);
    root.set_margin_bottom(14);

    // ---- risk + tool ----------------------------------------------------
    let meta = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let risk = gtk::Label::new(Some(request.risk.label()));
    risk.add_css_class("lc-chip");
    risk.add_css_class(&format!("lc-{}", request.risk.css_class()));
    meta.append(&risk);
    let tool = gtk::Label::new(Some(&request.tool));
    tool.add_css_class("lc-mono");
    tool.add_css_class("lc-tiny");
    tool.add_css_class("lc-accent");
    meta.append(&tool);
    if request.mandatory {
        let locked = gtk::Label::new(Some("\u{1f512} mandatory"));
        locked.add_css_class("lc-tiny");
        locked.add_css_class("lc-error");
        meta.append(&locked);
    }
    root.append(&meta);

    let summary = gtk::Label::new(Some(&request.summary));
    summary.add_css_class("lc-heading");
    summary.set_xalign(0.0);
    summary.set_wrap(true);
    summary.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    summary.set_selectable(true);
    root.append(&summary);

    if let Some(detail) = request.detail.as_deref().filter(|text| !text.trim().is_empty()) {
        let label = gtk::Label::new(Some(detail));
        label.add_css_class("lc-mono");
        label.add_css_class("lc-tiny");
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::Char);
        label.set_selectable(true);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(70)
            .max_content_height(180)
            .propagate_natural_height(true)
            .child(&label)
            .build();
        scroll.add_css_class("lc-card");
        root.append(&scroll);
    }

    if !request.reasons.is_empty() {
        let reasons = gtk::Box::new(gtk::Orientation::Vertical, 2);
        for reason in &request.reasons {
            let label = gtk::Label::new(Some(&format!("\u{2022} {reason}")));
            label.add_css_class("lc-tiny");
            label.add_css_class("lc-warn");
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            reasons.append(&label);
        }
        root.append(&reasons);
    }

    if let Some(diff) = request.diff.as_deref().filter(|diff| !diff.trim().is_empty()) {
        let preview = gtk::Label::new(Some(&markdown::summarise(diff, 2_000)));
        preview.add_css_class("lc-mono");
        preview.add_css_class("lc-tiny");
        preview.set_xalign(0.0);
        preview.set_wrap(true);
        preview.set_wrap_mode(gtk::pango::WrapMode::Char);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .max_content_height(180)
            .propagate_natural_height(true)
            .child(&preview)
            .build();
        let expander = gtk::Expander::new(Some("Proposed diff"));
        expander.add_css_class("lc-tiny");
        expander.set_child(Some(&scroll));
        root.append(&expander);
    }

    let feedback = gtk::Entry::new();
    feedback.set_placeholder_text(Some("Optional reason if you deny \u{00b7} Enter denies\u{2026}"));
    feedback.add_css_class("lc-input");
    root.append(&feedback);

    // ---- actions --------------------------------------------------------
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_halign(gtk::Align::End);
    buttons.set_margin_top(4);

    let deny = gtk::Button::with_label("Deny");
    deny.add_css_class("lc-ghost");
    deny.add_css_class("lc-danger");
    buttons.append(&deny);

    let allow_once = gtk::Button::with_label("Allow once");
    allow_once.add_css_class("lc-ghost");
    buttons.append(&allow_once);

    let allow_session = gtk::Button::with_label("Allow for session");
    allow_session.add_css_class("lc-primary");
    buttons.append(&allow_session);

    if let Some(pattern) = request.pattern.clone() {
        let always = gtk::Button::with_label("Always allow");
        always.add_css_class("lc-ghost");
        always.set_tooltip_text(Some(&format!("Persist `{pattern}` in .link/settings.json")));
        buttons.append(&always);

        let workspace = workspace.to_path_buf();
        let reply_always = reply.clone();
        let window_always = window.clone();
        let feedback_always = feedback.clone();
        always.connect_clicked(move |_| {
            // Persist the rule before answering, so the next session remembers.
            let mut settings = WorkspaceSettings::load(&workspace);
            settings.permissions.allow_always.insert(pattern.clone());
            if let Err(error) = settings.save(&workspace) {
                tracing::warn!(%error, "could not persist the always-allow rule");
            }
            let _ = &feedback_always;
            resolve(&reply_always, Decision::AllowAlways);
            window_always.close();
        });
    }

    root.append(&buttons);

    // `GtkEntry` consumes Return for its own `activate` signal, so the window
    // key controller never sees it while the user is typing a denial reason.
    // Enter here means "deny with this reason".
    {
        let reply = reply.clone();
        let window = window.clone();
        let feedback_entry = feedback.clone();
        feedback.connect_activate(move |_| {
            let reason = feedback_entry.text().trim().to_string();
            let decision = if reason.is_empty() {
                Decision::Deny
            } else {
                Decision::DenyWithFeedback { reason }
            };
            resolve(&reply, decision);
            window.close();
        });
    }

    {
        let reply = reply.clone();
        let window = window.clone();
        let feedback = feedback.clone();
        deny.connect_clicked(move |_| {
            let reason = feedback.text().trim().to_string();
            let decision = if reason.is_empty() {
                Decision::Deny
            } else {
                Decision::DenyWithFeedback { reason }
            };
            resolve(&reply, decision);
            window.close();
        });
    }
    {
        let reply = reply.clone();
        let window = window.clone();
        allow_once.connect_clicked(move |_| {
            resolve(&reply, Decision::AllowOnce);
            window.close();
        });
    }
    {
        let reply = reply.clone();
        let window = window.clone();
        allow_session.connect_clicked(move |_| {
            resolve(&reply, Decision::AllowSession);
            window.close();
        });
    }
    {
        let reply = reply.clone();
        window.connect_close_request(move |_| {
            resolve(&reply, Decision::Deny);
            glib::Propagation::Proceed
        });
    }

    // Enter accepts once; Escape denies. Keep this local to the modal.
    let key = gtk::EventControllerKey::new();
    let reply_key = reply.clone();
    let window_key = window.clone();
    let allow_once_key = allow_once.clone();
    let feedback_key = feedback.clone();
    key.connect_key_pressed(move |_, keyval, _, _| {
        match keyval {
            gtk::gdk::Key::Escape => {
                resolve(&reply_key, Decision::Deny);
                window_key.close();
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => {
                // While the user is typing a denial reason, Enter must not
                // silently allow the tool: deny with the reason instead.
                if feedback_key.has_focus() {
                    let reason = feedback_key.text().trim().to_string();
                    let decision = if reason.is_empty() {
                        Decision::Deny
                    } else {
                        Decision::DenyWithFeedback { reason }
                    };
                    resolve(&reply_key, decision);
                    window_key.close();
                    return glib::Propagation::Stop;
                }
                let _ = &allow_once_key;
                resolve(&reply_key, Decision::AllowOnce);
                window_key.close();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    window.add_controller(key);

    window.set_child(Some(&root));
    window.present();
    let _ = call_id;
    let _ = RiskLevel::Low;
}

fn resolve(reply: &Reply, decision: Decision) {
    if let Some(sender) = reply.borrow_mut().take() {
        let _ = sender.send(decision);
    }
}

// ---------------------------------------------------------------------------
// ask_user
// ---------------------------------------------------------------------------

/// UI-backed [`UserAsker`]: shows the model's question with tappable options
/// and a free-text field, then returns the answer to the waiting agent turn.
pub struct GtkAsker {
    window: glib::SendWeakRef<adw::ApplicationWindow>,
}

impl GtkAsker {
    pub fn new(window: adw::ApplicationWindow) -> Self {
        Self {
            window: window.downgrade().into(),
        }
    }
}

impl UserAsker for GtkAsker {
    fn ask(
        &self,
        call_id: String,
        question: String,
        options: Vec<String>,
    ) -> futures_util::future::BoxFuture<'static, Option<String>> {
        let window = self.window.clone();
        Box::pin(async move {
            let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
            glib::MainContext::default().invoke(move || {
                show_question(window.upgrade().as_ref(), &call_id, &question, &options, tx);
            });
            rx.await.unwrap_or(None)
        })
    }
}

fn show_question(
    parent: Option<&adw::ApplicationWindow>,
    call_id: &str,
    question: &str,
    options: &[String],
    sender: tokio::sync::oneshot::Sender<Option<String>>,
) {
    let window = gtk::Window::builder()
        .title("The agent has a question")
        .modal(true)
        .resizable(true)
        .default_width(440)
        .default_height(220)
        .build();
    window.add_css_class("lc-dialog");
    if let Some(parent) = parent {
        window.set_transient_for(Some(parent));
    }

    let reply = Rc::new(RefCell::new(Some(sender)));

    let header = gtk::HeaderBar::new();
    let title = gtk::Label::new(Some("QUESTION FROM THE AGENT"));
    title.add_css_class("lc-section");
    title.add_css_class("lc-accent");
    header.set_title_widget(Some(&title));
    window.set_titlebar(Some(&header));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class("lc-root");
    root.set_margin_start(16);
    root.set_margin_end(16);
    root.set_margin_top(14);
    root.set_margin_bottom(14);

    let label = gtk::Label::new(Some(question));
    label.add_css_class("lc-heading");
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_selectable(true);
    // A selectable label grabs focus on open and renders with its text
    // highlighted, which reads as a prefilled entry. Keep it copyable without
    // stealing focus from the answer field.
    label.set_focusable(false);
    root.append(&label);

    let choices = gtk::Box::new(gtk::Orientation::Vertical, 6);
    for option in options.iter().filter(|option| !option.trim().is_empty()) {
        let button = gtk::Button::new();
        button.add_css_class("lc-ghost");
        button.set_halign(gtk::Align::Start);
        let option_label = gtk::Label::new(Some(option));
        option_label.set_xalign(0.0);
        option_label.set_wrap(true);
        option_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        option_label.set_max_width_chars(48);
        button.set_child(Some(&option_label));
        let reply = reply.clone();
        let window = window.clone();
        let answer = option.clone();
        button.connect_clicked(move |_| {
            resolve_question(&reply, Some(answer.clone()));
            window.close();
        });
        choices.append(&button);
    }
    root.append(&choices);

    let custom = gtk::Entry::new();
    custom.set_placeholder_text(Some("Type an answer\u{2026}"));
    custom.add_css_class("lc-input");
    root.append(&custom);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_halign(gtk::Align::End);
    let skip = gtk::Button::with_label("Skip");
    skip.add_css_class("lc-ghost");
    skip.add_css_class("lc-dim");
    buttons.append(&skip);
    let answer = gtk::Button::with_label("Answer");
    answer.add_css_class("lc-primary");
    buttons.append(&answer);
    root.append(&buttons);

    {
        let reply = reply.clone();
        let window = window.clone();
        skip.connect_clicked(move |_| {
            resolve_question(&reply, None);
            window.close();
        });
    }
    {
        let reply = reply.clone();
        let window = window.clone();
        let custom = custom.clone();
        answer.connect_clicked(move |_| {
            let text = custom.text().trim().to_string();
            resolve_question(&reply, Some(text).filter(|text| !text.is_empty()));
            window.close();
        });
    }
    {
        let reply = reply.clone();
        let custom = custom.clone();
        custom.connect_activate(move |entry| {
            let text = entry.text().trim().to_string();
            let answer = Some(text).filter(|text| !text.is_empty());
            if let Some(sender) = reply.borrow_mut().take() {
                let _ = sender.send(answer);
            }
        });
    }
    {
        let reply = reply.clone();
        window.connect_close_request(move |_| {
            resolve_question(&reply, None);
            glib::Propagation::Proceed
        });
    }

    window.set_child(Some(&root));
    window.present();
    // Ready to type without a click; Enter submits.
    custom.grab_focus();
    let _ = call_id;
}

fn resolve_question(
    reply: &Rc<RefCell<Option<tokio::sync::oneshot::Sender<Option<String>>>>>,
    answer: Option<String>,
) {
    if let Some(sender) = reply.borrow_mut().take() {
        let _ = sender.send(answer);
    }
}
