//! Git integration through the system `git` CLI.
//!
//! Link Code does not link libgit2: the command line is already installed on
//! every machine that has a repository, handles every protocol and credential
//! helper, and keeps the binary small. These helpers cover what the Git panel
//! needs — status, clone, commit and push — and never write a token into
//! `.git/config`: authentication goes through a short-lived `GIT_ASKPASS`
//! script fed by environment variables instead.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::error::{LinkError, Result};

/// One entry from `git status --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitChange {
    /// Two-character porcelain code, e.g. `" M"`, `"A "` or `"??"`.
    pub code: String,
    /// Workspace-relative path, already unquoted.
    pub path: String,
}

impl GitChange {
    /// True when the change is already staged for the next commit.
    pub fn staged(&self) -> bool {
        let mut chars = self.code.chars();
        match (chars.next(), chars.next()) {
            (Some(index), Some(_)) => index != ' ' && index != '?',
            _ => false,
        }
    }

    /// Short human label for the change, e.g. `modified` or `untracked`.
    pub fn label(&self) -> &'static str {
        let mut chars = self.code.chars();
        let index = chars.next().unwrap_or(' ');
        let worktree = chars.next().unwrap_or(' ');
        let code = if index == '?' || worktree == '?' {
            '?'
        } else if index != ' ' {
            index
        } else {
            worktree
        };
        match code {
            'M' => "modified",
            'A' => "added",
            'D' => "deleted",
            'R' => "renamed",
            'C' => "copied",
            'T' => "type changed",
            'U' => "conflict",
            '?' => "untracked",
            '!' => "ignored",
            _ => "changed",
        }
    }

    /// Single-glyph marker for the compact list.
    pub fn glyph(&self) -> &'static str {
        match self.label() {
            "modified" => "M",
            "added" => "A",
            "deleted" => "D",
            "renamed" => "R",
            "copied" => "C",
            "type changed" => "T",
            "conflict" => "!",
            "untracked" => "?",
            _ => "\u{2022}",
        }
    }
}

/// Branch and working-tree state for one repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStatus {
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub changes: Vec<GitChange>,
}

impl GitStatus {
    /// Number of files with uncommitted changes.
    pub fn change_count(&self) -> usize {
        self.changes.len()
    }

    pub fn is_clean(&self) -> bool {
        self.changes.is_empty()
    }
}

/// True when `dir` is inside a Git working tree.
pub fn is_repo(dir: &Path) -> bool {
    repo_root(dir).is_some()
}

/// The top level of the working tree containing `dir`.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let root = text.trim();
    if root.is_empty() {
        None
    } else {
        Some(PathBuf::from(root))
    }
}

/// Read `git status --porcelain=v1 --branch` synchronously.
///
/// Status is cheap (milliseconds even in large trees) and the panel refreshes
/// it on the GTK thread, so keeping it synchronous avoids a race where a stale
/// async result overwrites a newer one.
pub fn status(dir: &Path) -> Result<GitStatus> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain=v1", "--branch"])
        .stdin(Stdio::null())
        .output()
        .map_err(LinkError::IoPlain)?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(LinkError::other(if message.is_empty() {
            "git status failed".to_string()
        } else {
            message
        }));
    }
    Ok(parse_status(&String::from_utf8_lossy(&output.stdout)))
}

/// Parse the output of `git status --porcelain=v1 --branch`.
pub fn parse_status(text: &str) -> GitStatus {
    let mut status = GitStatus::default();
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            parse_branch_header(header, &mut status);
            continue;
        }
        if line.len() < 3 {
            continue;
        }
        let code = line[..2].to_string();
        let mut path = line[3..].to_string();
        // Renames and copies print `old -> new`; the visible path is the new
        // one, and the compact list only has room for a single name.
        if let Some((_, new)) = path.rsplit_once(" -> ") {
            path = new.to_string();
        }
        status.changes.push(GitChange {
            code,
            path: unquote(&path),
        });
    }
    status
}

fn parse_branch_header(header: &str, status: &mut GitStatus) {
    // `main...origin/main [ahead 1, behind 2]`, `No commits yet on main`,
    // `HEAD (no branch)`.
    let (names, tracking) = match header.split_once(" [") {
        Some((names, rest)) => (names, Some(rest.trim_end_matches(']'))),
        None => (header, None),
    };
    if let Some(rest) = names.strip_prefix("No commits yet on ") {
        status.branch = rest.trim().to_string();
    } else {
        let branch = names.split("...").next().unwrap_or(names).trim();
        status.branch = if branch == "HEAD (no branch)" {
            "(detached)".to_string()
        } else {
            branch.to_string()
        };
        if let Some(upstream) = names.split_once("...").map(|(_, upstream)| upstream.trim()) {
            if !upstream.is_empty() {
                status.upstream = Some(upstream.to_string());
            }
        }
    }
    if let Some(tracking) = tracking {
        for part in tracking.split(',') {
            let part = part.trim();
            if let Some(value) = part.strip_prefix("ahead ") {
                status.ahead = value.trim().parse().unwrap_or(0);
            } else if let Some(value) = part.strip_prefix("behind ") {
                status.behind = value.trim().parse().unwrap_or(0);
            }
        }
    }
}

/// Remove the C-style quoting Git adds to paths with unusual characters.
fn unquote(path: &str) -> String {
    let path = path.trim();
    let Some(inner) = path
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return path.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

/// Normalise a GitHub remote URL to `owner/repo`.
pub fn github_slug(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches(".git");
    if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        return normalise_slug(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("ssh://git@github.com/") {
        return normalise_slug(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        return normalise_slug(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("http://github.com/") {
        return normalise_slug(rest);
    }
    None
}

fn normalise_slug(rest: &str) -> Option<String> {
    let rest = rest.trim_matches('/');
    let parts: Vec<&str> = rest.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() < 2 {
        return None;
    }
    Some(format!("{}/{}", parts[0], parts[1]))
}

/// The configured `origin` URL, if the directory is a repository.
pub fn remote_url(dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["remote", "get-url", "origin"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() {
        None
    } else {
        Some(url)
    }
}

/// `owner/repo` for the repository's `origin`, when it points at GitHub.
pub fn github_repo_slug(dir: &Path) -> Option<String> {
    remote_url(dir).and_then(|url| github_slug(&url))
}

/// Directory name GitHub would use for a clone of `repo`.
pub fn suggested_folder(repo: &str) -> String {
    repo.rsplit('/').next().unwrap_or(repo).to_string()
}

/// Clone `url` into `dest`, authenticating with `token` when it is private.
pub async fn clone_repo(
    url: &str,
    dest: &Path,
    token: Option<&str>,
    login: Option<&str>,
) -> Result<()> {
    if dest.exists() {
        let empty = std::fs::read_dir(dest)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false);
        if !empty {
            return Err(LinkError::other(format!(
                "{} already exists and is not empty",
                dest.display()
            )));
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|error| LinkError::io(parent, error))?;
    }
    let dest_arg = dest.as_os_str().to_owned();
    let output = run_git_authenticated(
        None,
        &["clone".into(), "--progress".into(), url.into(), dest_arg],
        token,
        login,
    )
    .await?;
    if !output.status.success() {
        return Err(git_error("git clone", &output.stderr));
    }
    Ok(())
}

/// Stage every change and commit it with `message`.
///
/// Returns the number of files committed. Committing an unchanged tree is an
/// error the caller surfaces as "nothing to commit" rather than a silent no-op.
pub async fn commit_all(dir: &Path, message: &str, name: &str, email: &str) -> Result<usize> {
    let message = message.trim();
    if message.is_empty() {
        return Err(LinkError::other("a commit message is required"));
    }
    let add = run_plain(dir, &["add", "-A"]).await?;
    if !add.status.success() {
        return Err(git_error("git add", &add.stderr));
    }
    let staged = status(dir)?;
    if staged.is_clean() {
        return Err(LinkError::other("nothing to commit"));
    }
    let count = staged.change_count();
    let name = if name.trim().is_empty() {
        "Link Code"
    } else {
        name.trim()
    };
    let email = if email.trim().is_empty() {
        "link-code@users.noreply.github.com"
    } else {
        email.trim()
    };
    let name_arg = format!("user.name={name}");
    let email_arg = format!("user.email={email}");
    let output = run_plain(
        dir,
        &["-c", &name_arg, "-c", &email_arg, "commit", "-m", message],
    )
    .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("nothing to commit") || stderr.contains("no changes added") {
            return Err(LinkError::other("nothing to commit"));
        }
        return Err(git_error("git commit", &output.stderr));
    }
    Ok(count)
}

/// Push the current branch, setting its upstream on the first push.
pub async fn push(dir: &Path, token: Option<&str>, login: Option<&str>) -> Result<()> {
    let has_upstream = run_plain(
        dir,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .await
    .map(|output| output.status.success())
    .unwrap_or(false);
    let args: Vec<std::ffi::OsString> = if has_upstream {
        ["push"].iter().map(|arg| arg.into()).collect()
    } else {
        ["push", "-u", "origin", "HEAD"]
            .iter()
            .map(|arg| arg.into())
            .collect()
    };
    let output = run_git_authenticated(Some(dir), &args, token, login).await?;
    if !output.status.success() {
        return Err(git_error("git push", &output.stderr));
    }
    Ok(())
}

async fn run_plain(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(LinkError::IoPlain)
}

/// Run Git with an askpass helper that reads the token from the environment.
///
/// The helper is a 0700 temp script removed as soon as Git exits. Neither the
/// token nor the login ever appears in argv, the remote URL or `.git/config`.
async fn run_git_authenticated(
    dir: Option<&Path>,
    args: &[std::ffi::OsString],
    token: Option<&str>,
    login: Option<&str>,
) -> Result<std::process::Output> {
    let mut command = Command::new("git");
    if let Some(dir) = dir {
        command.arg("-C").arg(dir);
    }
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A GUI process has no terminal, so Git must fail instead of waiting
        // for credentials when no token is available.
        .env("GIT_TERMINAL_PROMPT", "0");
    let Some(token) = token.filter(|token| !token.trim().is_empty()) else {
        return command.output().await.map_err(LinkError::IoPlain);
    };

    let askpass = AskpassScript::create()?;
    command
        .env("GIT_ASKPASS", askpass.path())
        .env("LINK_GIT_TOKEN", token.trim())
        .env("LINK_GIT_LOGIN", login.unwrap_or("x-access-token").trim());
    let output = command.output().await;
    drop(askpass);
    output.map_err(LinkError::IoPlain)
}

/// A temporary executable that answers Git's username/password prompts.
struct AskpassScript {
    path: PathBuf,
}

impl AskpassScript {
    fn create() -> Result<Self> {
        let path =
            std::env::temp_dir().join(format!("link-code-askpass-{}.sh", uuid::Uuid::new_v4()));
        let script = "#!/bin/sh\n\
            case \"$1\" in\n\
            *sername*) printf '%s\\n' \"$LINK_GIT_LOGIN\" ;;\n\
            *) printf '%s\\n' \"$LINK_GIT_TOKEN\" ;;\n\
            esac\n";
        std::fs::write(&path, script).map_err(|error| LinkError::io(&path, error))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(&path, permissions)
                .map_err(|error| LinkError::io(&path, error))?;
        }
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for AskpassScript {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn git_error(command: &str, stderr: &[u8]) -> LinkError {
    let message = String::from_utf8_lossy(stderr).trim().to_string();
    if message.is_empty() {
        LinkError::other(format!("{command} failed"))
    } else {
        LinkError::other(format!("{command} failed: {message}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_branch_header_with_tracking() {
        let status = parse_status("## main...origin/main [ahead 2, behind 1]\n");
        assert_eq!(status.branch, "main");
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!(status.ahead, 2);
        assert_eq!(status.behind, 1);
        assert!(status.is_clean());
    }

    #[test]
    fn parses_a_new_repository_header() {
        let status = parse_status("## No commits yet on main\n?? hello.txt\n");
        assert_eq!(status.branch, "main");
        assert_eq!(status.upstream, None);
        assert_eq!(status.changes.len(), 1);
        assert_eq!(status.changes[0].code, "??");
        assert_eq!(status.changes[0].label(), "untracked");
        assert!(!status.changes[0].staged());
    }

    #[test]
    fn parses_staged_unstaged_and_renamed_entries() {
        let status =
            parse_status("## dev\nM  src/staged.rs\n M src/dirty.rs\nR  old.rs -> new.rs\n");
        assert_eq!(status.branch, "dev");
        assert_eq!(status.changes.len(), 3);
        assert!(status.changes[0].staged());
        assert!(!status.changes[1].staged());
        assert_eq!(status.changes[1].label(), "modified");
        assert_eq!(status.changes[2].path, "new.rs");
        assert_eq!(status.changes[2].label(), "renamed");
    }

    #[test]
    fn recognises_github_remotes() {
        assert_eq!(
            github_slug("git@github.com:owner/repo.git").as_deref(),
            Some("owner/repo")
        );
        assert_eq!(
            github_slug("https://github.com/owner/repo.git").as_deref(),
            Some("owner/repo")
        );
        assert_eq!(
            github_slug("https://github.com/owner/repo").as_deref(),
            Some("owner/repo")
        );
        assert_eq!(github_slug("https://gitlab.com/owner/repo.git"), None);
        assert_eq!(suggested_folder("owner/my-repo"), "my-repo");
    }

    #[test]
    fn unquotes_escaped_paths() {
        assert_eq!(unquote("\"a\\tb\""), "a\tb");
        assert_eq!(unquote("plain.rs"), "plain.rs");
    }

    #[tokio::test]
    async fn clones_commits_and_pushes_a_local_repository() {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        let init = std::process::Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(&origin)
            .status()
            .unwrap();
        assert!(init.success());

        let checkout = dir.path().join("checkout");
        clone_repo(origin.to_str().unwrap(), &checkout, None, None)
            .await
            .unwrap();
        assert!(is_repo(&checkout));

        std::fs::write(checkout.join("hello.txt"), "hello\n").unwrap();
        let committed = commit_all(&checkout, "Add hello", "Test User", "test@example.com")
            .await
            .unwrap();
        assert_eq!(committed, 1);
        assert!(status(&checkout).unwrap().is_clean());

        push(&checkout, None, None).await.unwrap();
        let head = std::process::Command::new("git")
            .arg("-C")
            .arg(&origin)
            .args(["rev-parse", "--verify", "HEAD"])
            .output()
            .unwrap();
        assert!(head.status.success());
    }

    #[tokio::test]
    async fn committing_an_unchanged_tree_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(repo)
            .status()
            .unwrap();
        let error = commit_all(repo, "nothing", "Test", "test@example.com")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("nothing to commit"));
    }
}
