//! The Git tab: GitHub account, repository list, local changes and commit.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use link_core::git;

use crate::github::{self, GitHubRepo, GitHubUser, GITHUB_TOKEN_KEY};
use crate::state::UiState;

/// Repositories rendered at once; the filter narrows a long list without
/// building every row.
const MAX_REPOS: usize = 120;
/// Changes rendered at once; the count label still reports the true total.
const MAX_CHANGES: usize = 300;

pub struct GitPanel {
    pub root: gtk::Box,

    account_status: gtk::Label,
    account_detail: gtk::Label,
    token_row: gtk::Box,
    token_entry: gtk::PasswordEntry,
    connect_button: gtk::Button,
    disconnect_button: gtk::Button,
    refresh_repos_button: gtk::Button,

    repo_filter: gtk::SearchEntry,
    repos_box: gtk::Box,
    repo_label: gtk::Label,
    clone_button: gtk::Button,

    branch_label: gtk::Label,
    changes_count: gtk::Label,
    changes_box: gtk::Box,
    refresh_button: gtk::Button,

    message: gtk::Entry,
    commit_button: gtk::Button,
    push_button: gtk::Button,

    connected: Cell<bool>,
    loading: Cell<bool>,
    user: RefCell<Option<GitHubUser>>,
    repos: RefCell<Vec<GitHubRepo>>,
    selected: RefCell<Option<GitHubRepo>>,
}

pub fn build(state: &Rc<UiState>) -> Rc<GitPanel> {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("lc-git");
    root.set_vexpand(true);

    let columns = gtk::Paned::new(gtk::Orientation::Horizontal);
    columns.add_css_class("lc-git-paned");
    columns.set_vexpand(true);
    columns.set_position(300);
    columns.set_resize_start_child(false);
    columns.set_shrink_start_child(false);

    // ---- left: account and repositories ---------------------------------
    let left = gtk::Box::new(gtk::Orientation::Vertical, 4);
    left.add_css_class("lc-git-side");
    left.set_margin_start(8);
    left.set_margin_end(6);
    left.set_margin_top(6);
    left.set_margin_bottom(6);
    left.set_size_request(230, -1);

    let account_head = gtk::Label::new(Some("GITHUB"));
    account_head.add_css_class("lc-section");
    account_head.set_xalign(0.0);
    left.append(&account_head);

    let account_status = gtk::Label::new(Some("Not connected"));
    account_status.add_css_class("lc-tiny");
    account_status.add_css_class("lc-dim");
    account_status.set_xalign(0.0);
    account_status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    left.append(&account_status);

    let account_detail = gtk::Label::new(None);
    account_detail.add_css_class("lc-tiny");
    account_detail.add_css_class("lc-warn");
    account_detail.set_xalign(0.0);
    account_detail.set_wrap(true);
    account_detail.set_visible(false);
    left.append(&account_detail);

    let token_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let token_entry = gtk::PasswordEntry::new();
    token_entry.set_show_peek_icon(true);
    token_entry.set_placeholder_text(Some("GitHub access token"));
    token_entry.add_css_class("lc-input");
    token_entry.set_hexpand(true);
    token_row.append(&token_entry);
    let connect_button = gtk::Button::with_label("Connect");
    connect_button.add_css_class("lc-primary");
    connect_button.add_css_class("lc-git-connect");
    token_row.append(&connect_button);
    left.append(&token_row);

    let disconnect_button = gtk::Button::with_label("Disconnect");
    disconnect_button.add_css_class("lc-ghost");
    disconnect_button.set_halign(gtk::Align::Start);
    disconnect_button.set_visible(false);
    left.append(&disconnect_button);

    let repos_head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    repos_head.add_css_class("lc-git-repos-head");
    let repos_title = gtk::Label::new(Some("REPOSITORIES"));
    repos_title.add_css_class("lc-section");
    repos_title.set_xalign(0.0);
    repos_title.set_hexpand(true);
    repos_head.append(&repos_title);
    let refresh_repos_button = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh_repos_button.add_css_class("lc-icon-btn");
    refresh_repos_button.set_tooltip_text(Some("Reload repositories"));
    repos_head.append(&refresh_repos_button);
    left.append(&repos_head);

    let repo_filter = gtk::SearchEntry::new();
    repo_filter.add_css_class("lc-filter");
    repo_filter.set_placeholder_text(Some("Filter repositories"));
    repo_filter.set_visible(false);
    left.append(&repo_filter);

    let repos_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let repos_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&repos_box)
        .build();
    repos_scroll.add_css_class("lc-bg");
    left.append(&repos_scroll);

    let clone_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    clone_row.add_css_class("lc-git-clone-row");
    let repo_label = gtk::Label::new(Some("Select a repository"));
    repo_label.add_css_class("lc-tiny");
    repo_label.add_css_class("lc-dim");
    repo_label.set_xalign(0.0);
    repo_label.set_hexpand(true);
    repo_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    clone_row.append(&repo_label);
    let clone_button = gtk::Button::with_label("Clone");
    clone_button.add_css_class("lc-accent-outline");
    clone_button.set_sensitive(false);
    clone_row.append(&clone_button);
    left.append(&clone_row);

    // ---- right: working tree and commit ---------------------------------
    let right = gtk::Box::new(gtk::Orientation::Vertical, 4);
    right.add_css_class("lc-git-main");
    right.set_margin_start(6);
    right.set_margin_end(8);
    right.set_margin_top(6);
    right.set_margin_bottom(6);

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let branch_label = gtk::Label::new(Some("No folder open"));
    branch_label.add_css_class("lc-tiny");
    branch_label.set_xalign(0.0);
    branch_label.set_hexpand(true);
    branch_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    head.append(&branch_label);
    let refresh_button = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh_button.add_css_class("lc-icon-btn");
    refresh_button.set_tooltip_text(Some("Refresh git status"));
    head.append(&refresh_button);
    right.append(&head);

    let changes_head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    changes_head.add_css_class("lc-git-changes-head");
    let changes_title = gtk::Label::new(Some("CHANGES"));
    changes_title.add_css_class("lc-section");
    changes_title.set_xalign(0.0);
    changes_head.append(&changes_title);
    let changes_count = gtk::Label::new(None);
    changes_count.add_css_class("lc-tiny");
    changes_count.add_css_class("lc-dim");
    changes_count.set_xalign(0.0);
    changes_count.set_hexpand(true);
    changes_head.append(&changes_count);
    right.append(&changes_head);

    let changes_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let changes_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&changes_box)
        .build();
    changes_scroll.add_css_class("lc-bg");
    changes_scroll.set_size_request(-1, 46);
    right.append(&changes_scroll);

    let commit_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    commit_row.add_css_class("lc-git-commit-row");
    let message = gtk::Entry::new();
    message.set_placeholder_text(Some("Commit message"));
    message.add_css_class("lc-input");
    message.set_hexpand(true);
    commit_row.append(&message);
    let commit_button = gtk::Button::with_label("Commit");
    commit_button.add_css_class("lc-primary");
    commit_button.set_sensitive(false);
    commit_row.append(&commit_button);
    let push_button = gtk::Button::with_label("Push");
    push_button.add_css_class("lc-ghost");
    push_button.set_sensitive(false);
    commit_row.append(&push_button);
    right.append(&commit_row);

    columns.set_start_child(Some(&left));
    columns.set_end_child(Some(&right));
    root.append(&columns);

    let panel = Rc::new(GitPanel {
        root,
        account_status,
        account_detail,
        token_row,
        token_entry,
        connect_button,
        disconnect_button,
        refresh_repos_button,
        repo_filter,
        repos_box,
        repo_label,
        clone_button,
        branch_label,
        changes_count,
        changes_box,
        refresh_button,
        message,
        commit_button,
        push_button,
        connected: Cell::new(false),
        loading: Cell::new(false),
        user: RefCell::new(None),
        repos: RefCell::new(Vec::new()),
        selected: RefCell::new(None),
    });
    wire(&panel, state);
    panel
}

fn wire(panel: &Rc<GitPanel>, state: &Rc<UiState>) {
    {
        let panel_for_cb = panel.clone();
        let state = state.clone();
        panel
            .connect_button
            .connect_clicked(move |_| connect(&panel_for_cb, &state));
    }
    {
        let panel_for_cb = panel.clone();
        let state = state.clone();
        panel
            .token_entry
            .connect_activate(move |_| connect(&panel_for_cb, &state));
    }
    {
        let panel_for_cb = panel.clone();
        let state = state.clone();
        panel
            .disconnect_button
            .connect_clicked(move |_| disconnect(&panel_for_cb, &state));
    }
    {
        let panel_for_cb = panel.clone();
        let state = state.clone();
        panel
            .refresh_repos_button
            .connect_clicked(move |_| reload_repos(&panel_for_cb, &state));
    }
    {
        let panel_for_cb = panel.clone();
        let state = state.clone();
        panel
            .repo_filter
            .connect_search_changed(move |_| render_repos(&panel_for_cb, &state));
    }
    {
        let panel_for_cb = panel.clone();
        let state = state.clone();
        panel
            .refresh_button
            .connect_clicked(move |_| panel_for_cb.refresh(&state));
    }
    {
        let panel_for_cb = panel.clone();
        let state = state.clone();
        panel
            .commit_button
            .connect_clicked(move |_| commit(&panel_for_cb, &state));
    }
    {
        let panel_for_cb = panel.clone();
        let state = state.clone();
        panel
            .message
            .connect_activate(move |_| commit(&panel_for_cb, &state));
    }
    {
        let panel_for_cb = panel.clone();
        let state = state.clone();
        panel
            .push_button
            .connect_clicked(move |_| push(&panel_for_cb, &state));
    }
    {
        let panel_for_cb = panel.clone();
        let state = state.clone();
        panel
            .clone_button
            .connect_clicked(move |_| clone_selected(&panel_for_cb, &state));
    }

    if token(state).is_some() {
        panel.connected.set(true);
        panel.update_account();
        reload_account(panel, state);
    } else {
        panel.update_account();
        render_repos(panel, state);
    }
    panel.refresh(state);
}

impl GitPanel {
    fn update_account(&self) {
        let connected = self.connected.get();
        let loading = self.loading.get();
        self.token_row.set_visible(!connected);
        self.disconnect_button.set_visible(connected);
        self.repo_filter.set_visible(connected);
        self.refresh_repos_button
            .set_sensitive(connected && !loading);

        self.account_status.remove_css_class("lc-ok");
        self.account_status.remove_css_class("lc-dim");
        if loading {
            self.account_status.set_text(if connected {
                "Connected \u{2014} loading repositories\u{2026}"
            } else {
                "Connecting\u{2026}"
            });
            self.account_status.add_css_class("lc-dim");
        } else if connected {
            let text = match self.user.borrow().as_ref() {
                Some(user) => format!("Connected as {}", user.display()),
                None => "Connected".to_string(),
            };
            self.account_status.set_text(&text);
            self.account_status.add_css_class("lc-ok");
        } else {
            self.account_status.set_text("Not connected");
            self.account_status.add_css_class("lc-dim");
        }
    }

    fn set_detail(&self, text: Option<&str>) {
        match text {
            Some(text) if !text.trim().is_empty() => {
                self.account_detail.set_text(text.trim());
                self.account_detail.set_visible(true);
            }
            _ => {
                self.account_detail.set_text("");
                self.account_detail.set_visible(false);
            }
        }
    }

    /// Refresh the working-tree summary and the commit button.
    pub fn refresh(&self, state: &Rc<UiState>) {
        self.update_clone_button(state);
        let Some(workspace) = state.workspace_path() else {
            self.branch_label.set_text("No folder open");
            self.changes_count.set_text("");
            clear_children(&self.changes_box);
            add_changes_hint(self, "Open a folder to work with Git");
            self.commit_button.set_sensitive(false);
            self.push_button.set_sensitive(false);
            return;
        };
        let Some(root) = git::repo_root(&workspace) else {
            self.branch_label.set_text("Not a Git repository");
            self.changes_count
                .set_text("Select a repository on the left and clone it");
            clear_children(&self.changes_box);
            add_changes_hint(self, "This folder is not a Git working tree yet.");
            self.commit_button.set_sensitive(false);
            self.push_button.set_sensitive(false);
            return;
        };

        match git::status(&root) {
            Ok(status) => {
                let mut branch = if status.branch.is_empty() {
                    "(unknown branch)".to_string()
                } else {
                    status.branch.clone()
                };
                if status.ahead > 0 {
                    branch.push_str(&format!("  \u{2191}{}", status.ahead));
                }
                if status.behind > 0 {
                    branch.push_str(&format!("  \u{2193}{}", status.behind));
                }
                self.branch_label
                    .set_text(&format!("{}  \u{00b7}  {}", branch, root.display()));
                self.branch_label
                    .set_tooltip_text(Some(&root.display().to_string()));

                let count = status.change_count();
                self.changes_count.set_text(&if count == 0 {
                    "Working tree clean".to_string()
                } else {
                    format!("{count} file{} with changes", plural(count))
                });

                clear_children(&self.changes_box);
                if count == 0 {
                    add_changes_hint(self, "Nothing to commit.");
                } else {
                    for change in status.changes.iter().take(MAX_CHANGES) {
                        self.changes_box.append(&change_row(change));
                    }
                    if count > MAX_CHANGES {
                        add_changes_hint(
                            self,
                            &format!("and {} more\u{2026}", count - MAX_CHANGES),
                        );
                    }
                }
                self.commit_button.set_sensitive(count > 0);
                let has_remote = git::remote_url(&root).is_some();
                self.push_button.set_sensitive(has_remote);
                self.push_button.set_tooltip_text(Some(if has_remote {
                    "Push the current branch to origin"
                } else {
                    "This repository has no remote named origin"
                }));
            }
            Err(error) => {
                self.branch_label.set_text("Git status unavailable");
                self.changes_count.set_text("");
                clear_children(&self.changes_box);
                add_changes_hint(self, &error.to_string());
                self.commit_button.set_sensitive(false);
                self.push_button.set_sensitive(false);
            }
        }
    }

    fn update_clone_button(&self, state: &UiState) {
        let selected = self.selected.borrow().clone();
        let Some(repo) = selected else {
            self.repo_label.set_text("Select a repository");
            self.clone_button.set_label("Clone");
            self.clone_button.set_sensitive(false);
            return;
        };
        self.repo_label.set_text(&repo.full_name);
        if let Some(workspace) = state.workspace_path() {
            let current = git::repo_root(&workspace).and_then(|root| git::github_repo_slug(&root));
            if current.as_deref() == Some(repo.full_name.as_str()) {
                self.clone_button.set_label("Already cloned");
                self.clone_button.set_sensitive(false);
                return;
            }
        }
        self.clone_button.set_label(&format!("Clone {}", repo.name));
        self.clone_button.set_sensitive(true);
    }
}

// ---------------------------------------------------------------------------
// GitHub account
// ---------------------------------------------------------------------------

fn token(state: &UiState) -> Option<String> {
    state
        .secrets
        .get(GITHUB_TOKEN_KEY)
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

fn connect(panel: &Rc<GitPanel>, state: &Rc<UiState>) {
    let panel = panel.clone();
    let state = state.clone();
    if panel.loading.get() {
        return;
    }
    let token = panel.token_entry.text().trim().to_string();
    if token.is_empty() {
        state.toast("Paste a GitHub personal access token first");
        panel.token_entry.grab_focus();
        return;
    }
    panel.loading.set(true);
    panel.update_account();
    panel.set_detail(Some("Verifying the token with GitHub\u{2026}"));

    let token_for_task = token.clone();
    run_async(
        async move {
            let user = github::get_user(&token_for_task).await?;
            let repos = github::list_repos(&token_for_task).await?;
            Ok::<(GitHubUser, Vec<GitHubRepo>), String>((user, repos))
        },
        move |result| {
            panel.loading.set(false);
            match result {
                Ok((user, repos)) => {
                    if let Err(error) = state.secrets.set(GITHUB_TOKEN_KEY, &token) {
                        tracing::warn!(%error, "could not save the GitHub token");
                        panel.set_detail(Some(
                            "Signed in, but the token could not be saved for next launch.",
                        ));
                    }
                    panel.token_entry.set_text("");
                    panel.connected.set(true);
                    *panel.user.borrow_mut() = Some(user);
                    *panel.repos.borrow_mut() = repos;
                    panel.update_account();
                    panel.set_detail(None);
                    render_repos(&panel, &state);
                    panel.update_clone_button(&state);
                    let count = panel.repos.borrow().len();
                    state.toast(&format!(
                        "GitHub connected \u{00b7} {count} repositor{}",
                        if count == 1 { "y" } else { "ies" }
                    ));
                }
                Err(error) => {
                    panel.connected.set(false);
                    *panel.user.borrow_mut() = None;
                    panel.loading.set(false);
                    panel.update_account();
                    panel.set_detail(Some(&error));
                    render_repos(&panel, &state);
                    state.toast(&error);
                }
            }
        },
    );
}

/// Re-verify a token restored from the secret store and reload repositories.
fn reload_account(panel: &Rc<GitPanel>, state: &Rc<UiState>) {
    let Some(stored) = token(state) else {
        panel.connected.set(false);
        panel.update_account();
        return;
    };
    let panel = panel.clone();
    let state = state.clone();
    panel.loading.set(true);
    panel.update_account();
    run_async(
        async move {
            let user = github::get_user(&stored).await?;
            let repos = github::list_repos(&stored).await?;
            Ok::<(GitHubUser, Vec<GitHubRepo>), String>((user, repos))
        },
        move |result| {
            panel.loading.set(false);
            match result {
                Ok((user, repos)) => {
                    panel.connected.set(true);
                    *panel.user.borrow_mut() = Some(user);
                    *panel.repos.borrow_mut() = repos;
                    panel.update_account();
                    panel.set_detail(None);
                }
                Err(error) => {
                    panel.connected.set(false);
                    *panel.user.borrow_mut() = None;
                    panel.update_account();
                    panel.set_detail(Some(&error));
                }
            }
            render_repos(&panel, &state);
            panel.update_clone_button(&state);
        },
    );
}

/// Reload just the repository list from the stored token.
fn reload_repos(panel: &Rc<GitPanel>, state: &Rc<UiState>) {
    if panel.loading.get() {
        return;
    }
    let Some(stored) = token(state) else {
        state.toast("Connect a GitHub account first");
        return;
    };
    let panel = panel.clone();
    let state = state.clone();
    panel.loading.set(true);
    panel.update_account();
    run_async(
        async move { github::list_repos(&stored).await },
        move |result| {
            panel.loading.set(false);
            panel.update_account();
            match result {
                Ok(repos) => {
                    let count = repos.len();
                    *panel.repos.borrow_mut() = repos;
                    render_repos(&panel, &state);
                    state.toast(&format!(
                        "Loaded {count} repositor{}",
                        if count == 1 { "y" } else { "ies" }
                    ));
                }
                Err(error) => {
                    panel.set_detail(Some(&error));
                    state.toast(&error);
                }
            }
        },
    );
}

fn disconnect(panel: &Rc<GitPanel>, state: &Rc<UiState>) {
    let _ = state.secrets.delete(GITHUB_TOKEN_KEY);
    panel.connected.set(false);
    panel.loading.set(false);
    *panel.user.borrow_mut() = None;
    panel.repos.borrow_mut().clear();
    *panel.selected.borrow_mut() = None;
    panel.update_account();
    panel.set_detail(None);
    render_repos(panel, state);
    panel.update_clone_button(state);
    state.toast("GitHub disconnected");
}

// ---------------------------------------------------------------------------
// Repository list
// ---------------------------------------------------------------------------

fn render_repos(panel: &Rc<GitPanel>, state: &Rc<UiState>) {
    clear_children(&panel.repos_box);
    if !panel.connected.get() {
        panel
            .repos_box
            .append(&hint_label("Connect an account to browse repositories."));
        return;
    }
    if panel.loading.get() {
        panel
            .repos_box
            .append(&hint_label("Loading repositories\u{2026}"));
        return;
    }
    let repos = panel.repos.borrow().clone();
    if repos.is_empty() {
        panel
            .repos_box
            .append(&hint_label("No repositories found for this account."));
        return;
    }

    let filter = panel.repo_filter.text().to_lowercase();
    let selected = panel
        .selected
        .borrow()
        .as_ref()
        .map(|repo| repo.full_name.clone());
    let mut shown = 0;
    for repo in repos
        .iter()
        .filter(|repo| filter.is_empty() || repo.full_name.to_lowercase().contains(&filter))
    {
        if shown >= MAX_REPOS {
            panel.repos_box.append(&hint_label(
                "More repositories match \u{2014} filter the list.",
            ));
            break;
        }
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.add_css_class("lc-git-repo-row");
        let name = gtk::Label::new(Some(&repo.full_name));
        name.add_css_class("lc-tiny");
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        row.append(&name);
        if repo.private {
            let tag = gtk::Label::new(Some("private"));
            tag.add_css_class("lc-tiny");
            tag.add_css_class("lc-dim");
            row.append(&tag);
        }
        let button = gtk::Button::builder().child(&row).has_frame(false).build();
        button.add_css_class("lc-git-repo");
        if selected.as_deref() == Some(repo.full_name.as_str()) {
            button.add_css_class("lc-git-repo-selected");
        }
        if let Some(description) = repo.description.as_deref() {
            if !description.trim().is_empty() {
                button.set_tooltip_text(Some(description));
            }
        }
        {
            let repo = repo.clone();
            let panel = panel.clone();
            let state = state.clone();
            button.connect_clicked(move |_| {
                *panel.selected.borrow_mut() = Some(repo.clone());
                panel.repo_label.set_text(&repo.full_name);
                render_repos(&panel, &state);
                panel.update_clone_button(&state);
            });
        }
        panel.repos_box.append(&button);
        shown += 1;
    }
    if shown == 0 {
        panel
            .repos_box
            .append(&hint_label("No repository matches the filter."));
    }
}

// ---------------------------------------------------------------------------
// Clone, commit and push
// ---------------------------------------------------------------------------

fn clone_selected(panel: &Rc<GitPanel>, state: &Rc<UiState>) {
    let Some(repo) = panel.selected.borrow().clone() else {
        state.toast("Select a repository first");
        return;
    };
    match state.workspace_path() {
        Some(workspace) => {
            let destination = clone_destination(&workspace, &repo);
            start_clone(panel, state, repo, destination);
        }
        None => {
            let Some(widgets) = state.widgets() else {
                return;
            };
            let dialog = gtk::FileDialog::builder()
                .title("Choose where to clone the repository")
                .modal(true)
                .build();
            let panel = panel.clone();
            let state = state.clone();
            dialog.select_folder(
                Some(&widgets.window),
                None::<&gtk::gio::Cancellable>,
                move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(parent) = file.path() else {
                        return;
                    };
                    let destination = parent.join(&repo.name);
                    start_clone(&panel, &state, repo.clone(), destination);
                },
            );
        }
    }
}

/// Where a clone should land: the workspace itself when it is an empty folder,
/// otherwise a subfolder named after the repository.
fn clone_destination(workspace: &Path, repo: &GitHubRepo) -> PathBuf {
    if !git::is_repo(workspace) && dir_is_empty(workspace) {
        workspace.to_path_buf()
    } else {
        workspace.join(&repo.name)
    }
}

fn dir_is_empty(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

fn start_clone(panel: &Rc<GitPanel>, state: &Rc<UiState>, repo: GitHubRepo, destination: PathBuf) {
    let panel = panel.clone();
    let state = state.clone();
    let token = token(&state);
    let login = panel.user.borrow().as_ref().map(|user| user.login.clone());
    let url = repo.clone_url.clone();
    let full_name = repo.full_name.clone();
    panel.set_detail(Some(&format!("Cloning {full_name}\u{2026}")));
    state.toast(&format!("Cloning {full_name}\u{2026}"));

    let destination_for_task = destination.clone();
    run_async(
        async move {
            git::clone_repo(
                &url,
                &destination_for_task,
                token.as_deref(),
                login.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())
        },
        move |result| match result {
            Ok(()) => {
                panel.set_detail(None);
                state.toast(&format!("Cloned {full_name}"));
                crate::window::open_workspace(&state, &destination);
                if let Some(widgets) = state.widgets() {
                    widgets.bottom.git.refresh(&state);
                }
            }
            Err(error) => {
                panel.set_detail(Some(&error));
                state.toast(&error);
            }
        },
    );
}

fn commit(panel: &Rc<GitPanel>, state: &Rc<UiState>) {
    let panel = panel.clone();
    let state = state.clone();
    let Some(workspace) = state.workspace_path() else {
        state.toast("Open a folder first");
        return;
    };
    let Some(root) = git::repo_root(&workspace) else {
        state.toast("This folder is not a Git repository");
        return;
    };
    let message = panel.message.text().trim().to_string();
    if message.is_empty() {
        state.toast("Type a commit message first");
        panel.message.grab_focus();
        return;
    }
    let (name, email) = match panel.user.borrow().as_ref() {
        Some(user) => (user.commit_name(), user.commit_email()),
        None => (String::new(), String::new()),
    };
    panel.commit_button.set_sensitive(false);
    let root_for_task = root.clone();
    let message_for_task = message.clone();
    run_async(
        async move {
            git::commit_all(&root_for_task, &message_for_task, &name, &email)
                .await
                .map_err(|error| error.to_string())
        },
        move |result| {
            match result {
                Ok(count) => {
                    panel.message.set_text("");
                    state.toast(&format!("Committed {count} file{}", plural(count)));
                }
                Err(error) => state.toast(&error),
            }
            panel.refresh(&state);
        },
    );
}

fn push(panel: &Rc<GitPanel>, state: &Rc<UiState>) {
    let panel = panel.clone();
    let state = state.clone();
    let Some(workspace) = state.workspace_path() else {
        state.toast("Open a folder first");
        return;
    };
    let Some(root) = git::repo_root(&workspace) else {
        state.toast("This folder is not a Git repository");
        return;
    };
    let token = token(&state);
    let login = panel.user.borrow().as_ref().map(|user| user.login.clone());
    panel.push_button.set_sensitive(false);
    let root_for_task = root.clone();
    run_async(
        async move {
            git::push(&root_for_task, token.as_deref(), login.as_deref())
                .await
                .map_err(|error| error.to_string())
        },
        move |result| {
            match result {
                Ok(()) => state.toast("Pushed to origin"),
                Err(error) => state.toast(&error),
            }
            panel.refresh(&state);
        },
    );
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

fn clear_children(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn hint_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("lc-tiny");
    label.add_css_class("lc-dim");
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_margin_start(6);
    label.set_margin_top(4);
    label.set_margin_end(6);
    label
}

fn add_changes_hint(panel: &GitPanel, text: &str) {
    panel.changes_box.append(&hint_label(text));
}

fn change_row(change: &git::GitChange) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("lc-git-change");
    let glyph = gtk::Label::new(Some(change.glyph()));
    glyph.add_css_class("lc-git-change-glyph");
    glyph.set_width_chars(1);
    row.append(&glyph);
    let path = gtk::Label::new(Some(&change.path));
    path.add_css_class("lc-tiny");
    path.set_xalign(0.0);
    path.set_hexpand(true);
    path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    path.set_tooltip_text(Some(&change.path));
    row.append(&path);
    let label = gtk::Label::new(Some(change.label()));
    label.add_css_class("lc-tiny");
    label.add_css_class("lc-dim");
    row.append(&label);
    row
}

/// Run a `Send` future on the tokio runtime and deliver its result to the GTK
/// thread. `UiState` and widgets never cross the thread boundary.
fn run_async<T, F>(future: F, done: impl FnOnce(T) + 'static)
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    crate::state::runtime().spawn(async move {
        let _ = sender.send(future.await);
    });
    glib::MainContext::default().spawn_local(async move {
        if let Ok(value) = receiver.await {
            done(value);
        }
    });
}
