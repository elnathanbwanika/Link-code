//! Turn checkpoints — the safety net behind **Undo turn** and
//! **Restore checkpoint…**.
//!
//! Before an agent turn touches any file, the affected files are copied into a
//! hidden store under `~/.local/share/link-code/checkpoints/`. The user's git
//! history is never involved: a workspace without git still gets full undo.
//!
//! A checkpoint records, for every affected path, whether the file *existed*
//! before the turn. Restoring a turn therefore also removes files the agent
//! created during it, which is what "Undo turn" has to mean to be useful.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::data_dir;
use crate::error::{LinkError, Result};
use crate::paths::Sandbox;

/// One captured file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointEntry {
    /// Workspace-relative path.
    pub path: String,
    /// Blob file name inside the checkpoint directory (`None` when the file did
    /// not exist before the turn).
    pub blob: Option<String>,
    /// Whether the file existed before the turn.
    pub existed: bool,
    /// The file was a directory (so restoring means re-creating it).
    pub was_directory: bool,
}

/// A snapshot of every file an agent turn is about to touch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: String,
    pub session_id: String,
    pub turn_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub label: String,
    /// Workspace root the snapshot was taken from.
    pub workspace: PathBuf,
    pub entries: Vec<CheckpointEntry>,
}

impl Checkpoint {
    pub fn file_count(&self) -> usize {
        self.entries.len()
    }

    /// Text shown in the "Restore checkpoint…" list.
    pub fn display_label(&self) -> String {
        let created = self.created_at.format("%H:%M:%S");
        format!(
            "{} — {} file{} at {created}",
            self.label,
            self.entries.len(),
            if self.entries.len() == 1 { "" } else { "s" }
        )
    }
}

/// Reads and writes checkpoints on disk.
#[derive(Debug, Clone)]
pub struct CheckpointStore {
    root: PathBuf,
}

impl Default for CheckpointStore {
    fn default() -> Self {
        Self::default_location()
    }
}

impl CheckpointStore {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `~/.local/share/link-code/checkpoints`
    pub fn default_location() -> Self {
        Self::at(data_dir().join("checkpoints"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn session_dir(&self, session_id: &str) -> PathBuf {
        self.root.join(sanitise(session_id))
    }

    /// Snapshot the given workspace-relative paths before a turn mutates them.
    /// Paths that do not exist yet are recorded as "created by this turn".
    pub fn snapshot(
        &self,
        session_id: &str,
        turn_id: Uuid,
        workspace: &Path,
        label: impl Into<String>,
        paths: &[PathBuf],
    ) -> Result<Checkpoint> {
        let sandbox = Sandbox::new(workspace)?;
        let id = Uuid::new_v4().to_string();
        let dir = self.session_dir(session_id).join(&id);
        let blobs = dir.join("files");
        std::fs::create_dir_all(&blobs).map_err(|e| LinkError::io(&blobs, e))?;

        let mut entries = Vec::new();
        for (index, absolute) in paths.iter().enumerate() {
            let relative = sandbox.relative(absolute);
            if absolute.is_file() {
                let blob = format!("file-{index}");
                std::fs::copy(absolute, blobs.join(&blob)).map_err(|e| LinkError::io(absolute, e))?;
                entries.push(CheckpointEntry {
                    path: relative,
                    blob: Some(blob),
                    existed: true,
                    was_directory: false,
                });
            } else if absolute.is_dir() {
                entries.push(CheckpointEntry {
                    path: relative,
                    blob: None,
                    existed: true,
                    was_directory: true,
                });
            } else {
                entries.push(CheckpointEntry {
                    path: relative,
                    blob: None,
                    existed: false,
                    was_directory: false,
                });
            }
        }

        let checkpoint = Checkpoint {
            id,
            session_id: session_id.to_string(),
            turn_id,
            created_at: Utc::now(),
            label: label.into(),
            workspace: sandbox.root().to_path_buf(),
            entries,
        };
        let manifest = serde_json::to_string_pretty(&checkpoint)
            .map_err(|e| LinkError::other(format!("checkpoint manifest: {e}")))?;
        std::fs::write(dir.join("manifest.json"), manifest).map_err(|e| LinkError::io(&dir, e))?;
        Ok(checkpoint)
    }
}

/// Make a session id safe to use as a directory name.
fn sanitise(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

impl CheckpointStore {
    /// Every checkpoint for a session, newest first.
    pub fn list(&self, session_id: &str) -> Vec<Checkpoint> {
        let dir = self.session_dir(session_id);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut checkpoints: Vec<Checkpoint> = entries
            .flatten()
            .filter_map(|entry| {
                let manifest = entry.path().join("manifest.json");
                let text = std::fs::read_to_string(manifest).ok()?;
                serde_json::from_str(&text).ok()
            })
            .collect();
        checkpoints.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        checkpoints
    }

    pub fn latest(&self, session_id: &str) -> Option<Checkpoint> {
        self.list(session_id).into_iter().next()
    }

    /// Restore every file in a checkpoint. Returns the paths that changed.
    pub fn restore(&self, checkpoint: &Checkpoint) -> Result<Vec<PathBuf>> {
        let sandbox = Sandbox::new(&checkpoint.workspace)?;
        let blobs = self
            .session_dir(&checkpoint.session_id)
            .join(&checkpoint.id)
            .join("files");
        let mut changed = Vec::new();

        for entry in &checkpoint.entries {
            let target = sandbox.resolve(&entry.path)?;
            if !entry.existed {
                // The turn created this file: undo means remove it.
                if target.is_file() {
                    std::fs::remove_file(&target).map_err(|e| LinkError::io(&target, e))?;
                    changed.push(target);
                }
                continue;
            }
            if entry.was_directory {
                if !target.exists() {
                    std::fs::create_dir_all(&target).map_err(|e| LinkError::io(&target, e))?;
                    changed.push(target);
                }
                continue;
            }
            let Some(blob) = &entry.blob else {
                continue;
            };
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| LinkError::io(parent, e))?;
            }
            std::fs::copy(blobs.join(blob), &target).map_err(|e| LinkError::io(&target, e))?;
            changed.push(target);
        }
        Ok(changed)
    }

    /// Remove a single checkpoint from disk.
    pub fn delete(&self, session_id: &str, checkpoint_id: &str) -> Result<()> {
        let dir = self.session_dir(session_id).join(sanitise(checkpoint_id));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| LinkError::io(&dir, e))?;
        }
        Ok(())
    }

    /// Drop every checkpoint for a session (called when a session is deleted).
    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        let dir = self.session_dir(session_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| LinkError::io(&dir, e))?;
        }
        Ok(())
    }

    /// Prune old checkpoints, keeping the newest `keep` per session.
    pub fn prune(&self, session_id: &str, keep: usize) -> Result<usize> {
        let checkpoints = self.list(session_id);
        let mut removed = 0;
        for checkpoint in checkpoints.into_iter().skip(keep) {
            self.delete(session_id, &checkpoint.id)?;
            removed += 1;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_and_restore_file_contents() {
        let workspace = tempfile::tempdir().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let file = workspace.path().join("main.rs");
        std::fs::write(&file, "original").unwrap();

        let store = CheckpointStore::at(store_dir.path());
        let turn = Uuid::new_v4();
        let checkpoint = store
            .snapshot("sess", turn, workspace.path(), "turn 1", &[file.clone()])
            .unwrap();
        assert_eq!(checkpoint.file_count(), 1);

        std::fs::write(&file, "modified by the agent").unwrap();
        let changed = store.restore(&checkpoint).unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
    }

    #[test]
    fn restore_removes_files_created_by_the_turn() {
        let workspace = tempfile::tempdir().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let new_file = workspace.path().join("created.rs");

        let store = CheckpointStore::at(store_dir.path());
        let checkpoint = store
            .snapshot("sess", Uuid::new_v4(), workspace.path(), "turn", &[new_file.clone()])
            .unwrap();
        assert!(!checkpoint.entries[0].existed);

        std::fs::write(&new_file, "fn f() {}").unwrap();
        assert!(new_file.exists());
        store.restore(&checkpoint).unwrap();
        assert!(!new_file.exists(), "undo should delete files the turn created");
    }

    #[test]
    fn list_is_newest_first_and_prune_keeps_newest() {
        let workspace = tempfile::tempdir().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let file = workspace.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let store = CheckpointStore::at(store_dir.path());

        for index in 0..3u32 {
            store
                .snapshot("sess", Uuid::new_v4(), workspace.path(), format!("turn {index}"), &[file.clone()])
                .unwrap();
            // Ensure distinct timestamps so ordering is deterministic.
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let listed = store.list("sess");
        assert_eq!(listed.len(), 3);
        assert!(listed[0].created_at >= listed[2].created_at);

        let removed = store.prune("sess", 1).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(store.list("sess").len(), 1);
    }

    #[test]
    fn delete_session_removes_everything() {
        let workspace = tempfile::tempdir().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let file = workspace.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let store = CheckpointStore::at(store_dir.path());
        store
            .snapshot("sess", Uuid::new_v4(), workspace.path(), "turn", &[file])
            .unwrap();
        assert_eq!(store.list("sess").len(), 1);
        store.delete_session("sess").unwrap();
        assert!(store.list("sess").is_empty());
    }
}
