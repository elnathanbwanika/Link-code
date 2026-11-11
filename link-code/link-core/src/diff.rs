//! Unified diffs: generation (for the diff cards and approval modals) and
//! application (for the `edit_file` tool family).
//!
//! The diff engine is deliberately small: `similar` produces the hunks, this
//! module shapes them into the exact structure the GTK diff cards render (with
//! per-line old/new numbers, so gutters line up in both unified and
//! side-by-side layouts).

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

use crate::error::{LinkError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    Context,
    Addition,
    Removal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// 1-based line number in the *old* file, when the line exists there.
    pub old_line: Option<u32>,
    /// 1-based line number in the *new* file, when the line exists there.
    pub new_line: Option<u32>,
    /// Line text, always without the trailing newline.
    pub text: String,
}

impl DiffLine {
    pub fn marker(&self) -> char {
        match self.kind {
            DiffLineKind::Context => ' ',
            DiffLineKind::Addition => '+',
            DiffLineKind::Removal => '-',
        }
    }

    /// CSS class for the GTK diff card.
    pub fn css_class(&self) -> &'static str {
        match self.kind {
            DiffLineKind::Context => "diff-context",
            DiffLineKind::Addition => "diff-added",
            DiffLineKind::Removal => "diff-removed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<DiffLine>,
}

impl DiffHunk {
    pub fn header(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_count, self.new_start, self.new_count
        )
    }
}

/// A complete, renderable diff for one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    /// Workspace-relative path.
    pub path: String,
    pub hunks: Vec<DiffHunk>,
    pub added: usize,
    pub removed: usize,
    /// The unified diff text (used by "Open in editor diff tab" and by logs).
    pub unified: String,
    /// True when the file did not exist before (a pure creation).
    pub is_new: bool,
    /// True when the file is emptied/deleted.
    pub is_delete: bool,
}

impl FileDiff {
    /// Compute the diff between two revisions of a file.
    pub fn compute(path: impl Into<String>, old: &str, new: &str) -> Self {
        let path = path.into();
        let diff = TextDiff::from_lines(old, new);

        let mut added = 0usize;
        let mut removed = 0usize;
        for change in diff.iter_all_changes() {
            match change.tag() {
                ChangeTag::Insert => added += 1,
                ChangeTag::Delete => removed += 1,
                ChangeTag::Equal => {}
            }
        }

        let mut hunks = Vec::new();
        for group in diff.grouped_ops(3) {
            let mut lines = Vec::new();
            let (mut old_start, mut new_start) = (0u32, 0u32);
            if let Some(first) = group.first() {
                old_start = first.old_range().start as u32 + 1;
                new_start = first.new_range().start as u32 + 1;
            }
            for op in &group {
                for change in diff.iter_changes(op) {
                    let text = change.value().trim_end_matches('\n').to_string();
                    let (kind, old_line, new_line) = match change.tag() {
                        ChangeTag::Equal => (
                            DiffLineKind::Context,
                            change.old_index().map(|i| i as u32 + 1),
                            change.new_index().map(|i| i as u32 + 1),
                        ),
                        ChangeTag::Delete => (
                            DiffLineKind::Removal,
                            change.old_index().map(|i| i as u32 + 1),
                            None,
                        ),
                        ChangeTag::Insert => (
                            DiffLineKind::Addition,
                            None,
                            change.new_index().map(|i| i as u32 + 1),
                        ),
                    };
                    lines.push(DiffLine {
                        kind,
                        old_line,
                        new_line,
                        text,
                    });
                }
            }
            let old_count = lines
                .iter()
                .filter(|l| l.kind != DiffLineKind::Addition)
                .count() as u32;
            let new_count = lines
                .iter()
                .filter(|l| l.kind != DiffLineKind::Removal)
                .count() as u32;
            hunks.push(DiffHunk {
                old_start,
                old_count,
                new_start,
                new_count,
                lines,
            });
        }

        let unified = diff
            .unified_diff()
            .context_radius(3)
            .header(&format!("a/{path}"), &format!("b/{path}"))
            .to_string();

        Self {
            path,
            hunks,
            added,
            removed,
            unified,
            is_new: old.is_empty() && !new.is_empty(),
            is_delete: new.is_empty() && !old.is_empty(),
        }
    }

    /// `+12 −3` style summary used on diff cards.
    pub fn summary(&self) -> String {
        format!("+{} \u{2212}{}", self.added, self.removed)
    }

    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }

    /// A short, single-line description for tool-call cards.
    pub fn one_line(&self) -> String {
        if self.is_new {
            format!("create {} ({}+)", self.path, self.added)
        } else if self.is_delete {
            format!("delete {} (\u{2212}{})", self.path, self.removed)
        } else {
            format!("{} {}", self.path, self.summary())
        }
    }
}

/// Apply an exact-string edit, with a uniqueness check.
///
/// This is the primitive behind [`edit_file`] and is intentionally strict:
/// a model that supplies an ambiguous `old_string` gets a clear error rather
/// than a silently-corrupted file.
pub fn apply_edit(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<String> {
    if old_string.is_empty() {
        return Err(LinkError::EditFailed(
            "`old_string` must not be empty — use `write_file` to create a file".to_string(),
        ));
    }
    if old_string == new_string {
        return Err(LinkError::EditFailed(
            "`old_string` and `new_string` are identical; nothing to do".to_string(),
        ));
    }

    let occurrences = content.matches(old_string).count();
    if occurrences == 0 {
        return Err(LinkError::EditFailed(format!(
            "`old_string` was not found in the file ({} characters searched)",
            old_string.len()
        )));
    }
    if occurrences > 1 && !replace_all {
        return Err(LinkError::EditFailed(format!(
            "`old_string` matches {occurrences} locations; add more surrounding context or set `replace_all`"
        )));
    }

    Ok(if replace_all {
        content.replace(old_string, new_string)
    } else {
        content.replacen(old_string, new_string, 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_line_numbers_and_counts() {
        let old = "one\ntwo\nthree\n";
        let new = "one\nTWO\nthree\nfour\n";
        let diff = FileDiff::compute("a.txt", old, new);
        assert_eq!(diff.added, 2);
        assert_eq!(diff.removed, 1);
        assert_eq!(diff.hunks.len(), 1);
        assert!(!diff.is_empty());

        let hunk = &diff.hunks[0];
        // `two` (old line 2) is removed, `TWO` (new line 2) and `four` (new line 4) added.
        let removed: Vec<_> = hunk
            .lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Removal)
            .collect();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].old_line, Some(2));
        assert_eq!(removed[0].new_line, None);

        let added: Vec<_> = hunk
            .lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Addition)
            .collect();
        assert_eq!(added.len(), 2);
        assert_eq!(added[0].new_line, Some(2));
        assert_eq!(added[1].new_line, Some(4));

        // Context lines carry both numbers.
        let context = hunk
            .lines
            .iter()
            .find(|l| l.kind == DiffLineKind::Context && l.text == "one")
            .unwrap();
        assert_eq!(context.old_line, Some(1));
        assert_eq!(context.new_line, Some(1));
    }

    #[test]
    fn detects_creation_and_deletion() {
        let created = FileDiff::compute("new.rs", "", "fn main() {}\n");
        assert!(created.is_new);
        assert!(!created.is_delete);

        let deleted = FileDiff::compute("old.rs", "fn main() {}\n", "");
        assert!(deleted.is_delete);
        assert!(!deleted.is_new);
    }

    #[test]
    fn unified_diff_carries_readable_headers() {
        let diff = FileDiff::compute("src/main.rs", "a\nb\n", "a\nc\n");
        assert!(diff.unified.contains("--- a/src/main.rs"), "{}", diff.unified);
        assert!(diff.unified.contains("+++ b/src/main.rs"), "{}", diff.unified);
        assert!(diff.unified.contains("-b"));
        assert!(diff.unified.contains("+c"));
        assert_eq!(diff.summary(), "+1 \u{2212}1");
    }

    #[test]
    fn identical_input_produces_no_hunks() {
        let diff = FileDiff::compute("a.txt", "same\n", "same\n");
        assert!(diff.is_empty());
        assert_eq!(diff.added, 0);
        assert_eq!(diff.removed, 0);
    }

    #[test]
    fn apply_edit_replaces_unique_match() {
        let out = apply_edit("let a = 1;\nlet b = 2;\n", "let b = 2;", "let b = 3;", false).unwrap();
        assert_eq!(out, "let a = 1;\nlet b = 3;\n");
    }

    #[test]
    fn apply_edit_rejects_ambiguous_match() {
        let err = apply_edit("x\nx\n", "x", "y", false).unwrap_err();
        match err {
            LinkError::EditFailed(message) => assert!(message.contains("matches 2 locations"), "{message}"),
            other => panic!("unexpected error {other:?}"),
        }
    }

    #[test]
    fn apply_edit_replace_all_handles_ambiguity() {
        let out = apply_edit("x\nx\n", "x", "y", true).unwrap();
        assert_eq!(out, "y\ny\n");
    }

    #[test]
    fn apply_edit_rejects_missing_and_empty_strings() {
        assert!(matches!(
            apply_edit("abc", "zzz", "y", false).unwrap_err(),
            LinkError::EditFailed(_)
        ));
        assert!(matches!(
            apply_edit("abc", "", "y", false).unwrap_err(),
            LinkError::EditFailed(_)
        ));
        assert!(matches!(
            apply_edit("abc", "abc", "abc", false).unwrap_err(),
            LinkError::EditFailed(_)
        ));
    }
}
