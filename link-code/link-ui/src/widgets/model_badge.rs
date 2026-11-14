//! The model selector popover (header bar + status bar).

use std::rc::Rc;

use gtk::prelude::*;

use link_models::registry::ModelRole;

use crate::state::UiState;

pub fn show_picker(state: &Rc<UiState>, anchor: &gtk::Widget) {
    let popover = gtk::Popover::new();
    popover.add_css_class("lc-popover");
    popover.set_parent(anchor);
    popover.set_has_arrow(true);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_width_request(320);

    let heading = gtk::Label::new(Some("MODEL"));
    heading.add_css_class("lc-section");
    heading.set_xalign(0.0);
    heading.set_margin_start(6);
    content.append(&heading);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search models\u{2026}"));
    search.add_css_class("lc-input");
    search.set_margin_start(6);
    search.set_margin_end(6);
    search.set_margin_bottom(2);
    content.append(&search);

    let list = gtk::ListBox::new();
    list.add_css_class("lc-treeview");
    list.set_selection_mode(gtk::SelectionMode::None);

    let active = state.chat_model().map(|model| model.id);
    let models = Rc::new(state.registry.borrow().models.clone());
    let chat_role = state
        .registry
        .borrow()
        .roles
        .get(ModelRole::Chat.key())
        .cloned();

    let rebuild: Rc<dyn Fn(&str)> = {
        let list = list.clone();
        let models = models.clone();
        let active = active.clone();
        let chat_role = chat_role.clone();
        let state = state.clone();
        let popover = popover.clone();
        Rc::new(move |query: &str| {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            if models.is_empty() {
                let empty = gtk::Label::new(Some("No models configured yet."));
                empty.add_css_class("lc-dim");
                empty.set_margin_all_like(10);
                list.append(&empty);
                return;
            }
            let query = query.trim().to_lowercase();
            let mut any = false;
            for model in models.iter() {
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
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row.set_margin_top(4);
                row.set_margin_bottom(4);
                row.set_margin_start(6);
                row.set_margin_end(6);

                let mark = gtk::Label::new(Some(if active.as_deref() == Some(model.id.as_str()) {
                    "\u{2713}"
                } else {
                    " "
                }));
                mark.add_css_class("lc-accent");
                mark.set_width_chars(2);
                row.append(&mark);

                let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
                let title = gtk::Label::new(Some(&model.display_name));
                title.set_xalign(0.0);
                let subtitle = gtk::Label::new(Some(&format!(
                    "{} \u{00b7} {}",
                    model.provider.label(),
                    model.model_id
                )));
                subtitle.add_css_class("lc-tiny");
                subtitle.add_css_class("lc-dim");
                subtitle.set_xalign(0.0);
                text.append(&title);
                text.append(&subtitle);
                row.append(&text);
                let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                spacer.set_hexpand(true);
                row.append(&spacer);

                if chat_role.as_deref() == Some(model.id.as_str()) {
                    let chip = gtk::Label::new(Some("chat"));
                    chip.add_css_class("lc-badge");
                    chip.add_css_class("lc-badge-accent");
                    row.append(&chip);
                }

                let button = gtk::Button::builder()
                    .child(&row)
                    .has_frame(false)
                    .build();
                button.add_css_class("lc-recent-row");
                {
                    let state = state.clone();
                    let id = model.id.clone();
                    let popover = popover.clone();
                    button.connect_clicked(move |_| {
                        state.set_chat_model(&id);
                        state.toast(&format!("Chat model: {}", state.chat_model_label()));
                        popover.popdown();
                    });
                }
                list.append(&button);
            }
            if !any {
                let empty = gtk::Label::new(Some("No models match your search."));
                empty.add_css_class("lc-dim");
                empty.add_css_class("lc-tiny");
                empty.set_margin_all_like(10);
                list.append(&empty);
            }
        })
    };
    rebuild("");
    {
        let rebuild = rebuild.clone();
        search.connect_search_changed(move |entry| rebuild(&entry.text()));
    }

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .max_content_height(360)
        .propagate_natural_height(true)
        .child(&list)
        .build();
    content.append(&scroll);

    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    content.append(&separator);

    let manage = gtk::Button::with_label("Manage models\u{2026}");
    manage.add_css_class("lc-ghost");
    {
        let state = state.clone();
        let popover = popover.clone();
        manage.connect_clicked(move |_| {
            popover.popdown();
            crate::dialogs::models::present(&state);
        });
    }
    content.append(&manage);

    popover.set_child(Some(&content));
    // Popovers are parented manually; drop them when dismissed so repeated
    // clicks do not accumulate orphaned widgets.
    popover.connect_closed(|popover| popover.unparent());
    popover.popup();
}

/// Small helper so the picker and dialogs keep margins consistent without
/// depending on `set_margin_all` (not available in this gtk-rs release).
trait MarginAll {
    fn set_margin_all_like(&self, margin: i32);
}

impl<T: IsA<gtk::Widget>> MarginAll for T {
    fn set_margin_all_like(&self, margin: i32) {
        self.set_margin_start(margin);
        self.set_margin_end(margin);
        self.set_margin_top(margin);
        self.set_margin_bottom(margin);
    }
}
