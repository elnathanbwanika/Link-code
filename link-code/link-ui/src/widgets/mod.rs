//! Reusable widgets used by the panels and dialogs.

use gtk::prelude::*;

pub mod chat_message;
pub mod edit_chip;
pub mod model_badge;
pub mod status_bar;
pub mod tool_chip;

/// A label that shrinks instead of forcing its container to grow.
///
/// Buttons in the header and the status bar can display model names, workspace
/// paths or file names that are arbitrarily long. A plain `Button::with_label`
/// makes that text part of the window's minimum width, so a long model name can
/// silently widen the whole window. These labels ellipsize at `max_chars` and
/// never report a minimum width of their own.
pub fn ellipsizing_label(text: &str, max_chars: i32) -> gtk::Label {
    ellipsizing_label_for(text, max_chars, gtk::pango::EllipsizeMode::Middle)
}

/// Like [`ellipsizing_label`], with an explicit ellipsis mode.
pub fn ellipsizing_label_for(
    text: &str,
    max_chars: i32,
    mode: gtk::pango::EllipsizeMode,
) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_ellipsize(mode);
    label.set_max_width_chars(max_chars.max(1));
    label.set_xalign(0.0);
    label
}

/// Replace a button's text while keeping it bounded.
///
/// `gtk::Button::set_label` swaps the child for a fresh non-ellipsizing label,
/// which reintroduces the auto-expansion problem. This helper updates the
/// existing ellipsizing label when there is one, and installs a new one
/// otherwise.
pub fn set_button_text(button: &gtk::Button, text: &str, max_chars: i32) {
    set_button_text_for(
        button,
        text,
        max_chars,
        gtk::pango::EllipsizeMode::Middle,
    );
}

/// Like [`set_button_text`], with an explicit ellipsis mode.
pub fn set_button_text_for(
    button: &gtk::Button,
    text: &str,
    max_chars: i32,
    mode: gtk::pango::EllipsizeMode,
) {
    if let Some(label) = button
        .child()
        .and_then(|child| child.downcast::<gtk::Label>().ok())
    {
        label.set_text(text);
        label.set_ellipsize(mode);
        label.set_max_width_chars(max_chars.max(1));
        return;
    }
    let label = ellipsizing_label_for(text, max_chars, mode);
    button.set_child(Some(&label));
}
