//! The workspace sandbox.
//!
//! **Hard rule #1 of Link Code:** every file operation is confined to the
//! workspace root the user opened. This module is the single choke point that
//! enforces it. It is deliberately paranoid:
//!
//! 1. `..` components are resolved lexically *before* touching the disk.
//! 2. The resulting path must still start with the canonical workspace root.
//! 3. Symlinks are resolved on the longest *existing* ancestor, so a symlink
//!    inside the workspace pointing outside it is rejected rather than followed.

use std::path::{Component, Path, PathBuf};

use crate::error::{LinkError, Result};

/// A canonicalised workspace root plus the operations allowed inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// Create a sandbox rooted at `root`, canonicalising it first.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let raw = root.as_ref();
        let root = std::fs::canonicalize(raw).map_err(|e| LinkError::io(raw, e))?;
        if !root.is_dir() {
            return Err(LinkError::other(format!(
                "workspace root `{}` is not a directory",
                root.display()
            )));
        }
        Ok(Self { root })
    }

    /// The canonical workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Lexically normalise a path: drop `.`, collapse `..`, keep everything else.
    fn normalize(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        out
    }

    fn ensure_inside(&self, candidate: &Path) -> Result<()> {
        if candidate == self.root || candidate.starts_with(&self.root) {
            Ok(())
        } else {
            Err(LinkError::PathEscape {
                path: candidate.to_path_buf(),
                root: self.root.clone(),
            })
        }
    }

    /// Resolve a user- or model-supplied path to an absolute, symlink-resolved
    /// path that is guaranteed to live inside the workspace.
    ///
    /// Works for paths that do not exist yet (new files): symlink resolution is
    /// performed against the deepest existing ancestor, which is the strongest
    /// guarantee possible before the file is created.
    pub fn resolve(&self, user_path: impl AsRef<Path>) -> Result<PathBuf> {
        let raw = user_path.as_ref();
        let joined = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.root.join(raw)
        };
        let lexical = Self::normalize(&joined);
        self.ensure_inside(&lexical)?;

        // Walk up to the longest existing ancestor and canonicalise it.
        let mut ancestor = lexical.as_path();
        let mut suffix = PathBuf::new();
        loop {
            if ancestor.exists() {
                let canon = std::fs::canonicalize(ancestor).map_err(|e| LinkError::io(ancestor, e))?;
                let full = if suffix.as_os_str().is_empty() {
                    canon
                } else {
                    canon.join(&suffix)
                };
                self.ensure_inside(&full)?;
                return Ok(full);
            }
            match (ancestor.parent(), ancestor.file_name()) {
                (Some(parent), Some(name)) => {
                    suffix = Path::new(name).join(&suffix);
                    ancestor = parent;
                }
                _ => break,
            }
        }
        Ok(lexical)
    }

    /// Like [`Sandbox::resolve`] but errors when the target does not exist.
    pub fn resolve_existing(&self, user_path: impl AsRef<Path>) -> Result<PathBuf> {
        let resolved = self.resolve(user_path)?;
        if resolved.exists() {
            Ok(resolved)
        } else {
            Err(LinkError::NotFound(resolved))
        }
    }

    /// Render an absolute path relative to the workspace root, for display.
    pub fn relative(&self, path: &Path) -> String {
        match path.strip_prefix(&self.root) {
            Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => path.to_string_lossy().into_owned(),
        }
    }

    /// True when `path` is inside the workspace.
    pub fn contains(&self, path: &Path) -> bool {
        path == self.root || path.starts_with(&self.root)
    }
}

/// File-name and directory patterns that are treated as secret material.
///
/// Used both to warn the user when they attach such a file *and* to redact its
/// contents from anything sent to a model when "Protect secrets" is enabled.
const SECRET_DIRS: &[&str] = &[".ssh", ".gnupg", ".aws"];

const SECRET_FILE_NAMES: &[&str] = &[
    ".env",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    "credentials",
    "credentials.json",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "secrets.json",
    "secrets.yaml",
    "secrets.yml",
];

/// Returns `true` when the path looks like a secret/credential file.
pub fn is_secret_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    if SECRET_FILE_NAMES.contains(&name.as_str()) {
        return true;
    }
    for extension in ["pem", "key", "p12", "pfx", "keystore", "jks"] {
        if name.ends_with(&format!(".{extension}")) {
            return true;
        }
    }
    // `.env.local`, `.env.production` and friends.
    if name.starts_with(".env.") {
        return true;
    }
    path.components().any(|component| {
        let segment = component.as_os_str().to_string_lossy();
        SECRET_DIRS.contains(&segment.as_ref())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn resolves_relative_paths_inside_root() {
        let dir = tmp();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let sb = Sandbox::new(dir.path()).unwrap();
        let resolved = sb.resolve("a.txt").unwrap();
        assert_eq!(
            resolved,
            std::fs::canonicalize(dir.path().join("a.txt")).unwrap()
        );
    }

    #[test]
    fn rejects_parent_dir_escape() {
        let dir = tmp();
        let sb = Sandbox::new(dir.path()).unwrap();
        let err = sb.resolve("../../etc/passwd").unwrap_err();
        assert!(matches!(err, LinkError::PathEscape { .. }), "got {err:?}");
    }

    #[test]
    fn rejects_absolute_escape() {
        let dir = tmp();
        let sb = Sandbox::new(dir.path()).unwrap();
        let err = sb.resolve("/etc/passwd").unwrap_err();
        assert!(matches!(err, LinkError::PathEscape { .. }), "got {err:?}");
    }

    #[test]
    fn rejects_symlink_escape() {
        let dir = tmp();
        let sb = Sandbox::new(dir.path()).unwrap();
        // A symlink inside the workspace pointing outside it must not be followed.
        std::os::unix::fs::symlink("/etc", dir.path().join("escape")).unwrap();
        let err = sb.resolve("escape/passwd").unwrap_err();
        assert!(matches!(err, LinkError::PathEscape { .. }), "got {err:?}");
    }

    #[test]
    fn allows_not_yet_created_file() {
        let dir = tmp();
        let sb = Sandbox::new(dir.path()).unwrap();
        let resolved = sb.resolve("src/deep/new.rs").unwrap();
        assert!(resolved.starts_with(std::fs::canonicalize(dir.path()).unwrap()));
    }

    #[test]
    fn normalises_internal_parent_dirs() {
        let dir = tmp();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        let sb = Sandbox::new(dir.path()).unwrap();
        let resolved = sb.resolve("a/b/../b").unwrap();
        assert_eq!(
            resolved,
            std::fs::canonicalize(dir.path().join("a/b")).unwrap()
        );
    }

    #[test]
    fn secret_detection() {
        assert!(is_secret_path(Path::new("/w/.env")));
        assert!(is_secret_path(Path::new("/w/.env.local")));
        assert!(is_secret_path(Path::new("/w/certs/server.pem")));
        assert!(is_secret_path(Path::new("/home/u/.ssh/id_rsa")));
        assert!(is_secret_path(Path::new("/w/keys/thing.key")));
        assert!(!is_secret_path(Path::new("/w/src/main.rs")));
        assert!(!is_secret_path(Path::new("/w/environment.rs")));
    }

    #[test]
    fn relative_rendering() {
        let dir = tmp();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let sb = Sandbox::new(dir.path()).unwrap();
        assert_eq!(sb.relative(&sb.resolve("src").unwrap()), "src");
    }
}
