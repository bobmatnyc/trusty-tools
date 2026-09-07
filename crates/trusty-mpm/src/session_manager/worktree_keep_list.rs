//! The owner-authored keep-list: worktrees no sweep may ever propose (#6927).
//!
//! Why: every gate in [`classify`](super::worktree_reclaim::classify) answers a
//! question ABOUT the worktree — who claims it, whether its branch landed,
//! whether it holds unsaved work. None of them can express the operator's own
//! standing decision that a particular tree is off limits whatever those
//! answers turn out to be. DOC-73 §16.2 asks for exactly that as a first-class
//! gate, so a keep-listed worktree renders `Blocked` with a reason rather than
//! being filtered out of the survey and becoming invisible.
//! What: [`KeepList`], built from the operator's `disk.keep_list` patterns
//! (`~/.trusty-tools/trusty-mpm/config.yaml`). An entry is either a PATH — which
//! protects that directory and everything under it — or a glob. A pattern that
//! will not compile is not silently dropped: it is recorded in
//! [`KeepList::invalid`] so the surfaces that render the list can say the
//! operator's intent did not take effect.
//! Test: `worktree_keep_list_tests`.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobMatcher};

/// The characters that make an entry a GLOB rather than a path.
///
/// Why: an operator writing `/Users/me/hotstats` means a directory, and a
/// directory protects its contents; one writing `**/hotstats*` means a match
/// rule. Deciding by spelling rather than by a second config key keeps one
/// list able to express both without the operator declaring which is which.
const GLOB_CHARS: [char; 5] = ['*', '?', '[', ']', '{'];

/// One compiled keep-list entry.
#[derive(Debug)]
enum KeepEntry {
    /// A directory: it and everything beneath it is kept.
    Path {
        /// The entry as the operator wrote it, for the refusal message.
        raw: String,
        /// Resolved form, compared against the candidate's resolved form.
        resolved: PathBuf,
    },
    /// A glob matched against the candidate's path.
    Pattern {
        /// The entry as the operator wrote it, for the refusal message.
        raw: String,
        /// The compiled matcher.
        matcher: GlobMatcher,
    },
}

/// The operator's standing "never touch these worktrees" list (#6927).
///
/// Why: see the module docs. This is a protective list, so the only failure
/// mode that matters is an entry that silently protects nothing — an operator
/// who wrote a bad glob believes their tree is safe. Hence [`invalid`], which
/// every surface that reports the keep-list also reports.
/// What: compiled entries plus the patterns that would not compile. An empty
/// list matches nothing, which is the default and is a no-op gate.
/// Test: `worktree_keep_list_tests`.
#[derive(Debug, Default)]
pub(crate) struct KeepList {
    entries: Vec<KeepEntry>,
    /// Patterns that failed to compile, as `"<pattern>: <error>"`.
    invalid: Vec<String>,
}

impl KeepList {
    /// Compile the operator's patterns.
    ///
    /// Why: `from_patterns` rather than `From<Vec<String>>` so the caller reads
    /// as "these are patterns", and so an empty slice is the obvious default.
    /// What: an entry containing any of [`GLOB_CHARS`] compiles as a glob;
    /// everything else is a path, with a leading `~` expanded and the result
    /// resolved once here so matching does no repeated work. A glob that will
    /// not compile lands in [`Self::invalid`] and matches nothing.
    /// Test: `a_literal_path_entry_keeps_the_directory_and_its_children`,
    /// `an_uncompilable_glob_is_reported_rather_than_silently_dropped`.
    pub(crate) fn from_patterns<S: AsRef<str>>(patterns: &[S]) -> Self {
        let mut out = Self::default();
        for pattern in patterns {
            let raw = pattern.as_ref().trim().to_string();
            if raw.is_empty() {
                continue;
            }
            if raw.contains(GLOB_CHARS) {
                match Glob::new(&raw) {
                    Ok(glob) => out.entries.push(KeepEntry::Pattern {
                        raw,
                        matcher: glob.compile_matcher(),
                    }),
                    Err(e) => out.invalid.push(format!("{raw}: {e}")),
                }
            } else {
                let resolved = resolve(&expand_home(&raw));
                out.entries.push(KeepEntry::Path { raw, resolved });
            }
        }
        out
    }

    /// The patterns that would not compile, as `"<pattern>: <error>"`.
    ///
    /// Why: reported by every surface that renders the keep-list — an entry
    /// that protects nothing must not look like one that does.
    pub(crate) fn invalid(&self) -> &[String] {
        &self.invalid
    }

    /// The entry that keeps `path`, or `None` when nothing does.
    ///
    /// Why: returning the entry rather than a bool is what lets the refusal
    /// name the operator's own spelling back to them, so they can find and edit
    /// the line that produced it.
    /// What: a path entry matches when the candidate IS it or sits under it,
    /// compared on resolved forms AND on the raw spellings (a path that will
    /// not canonicalize — a worktree whose directory is already gone — still
    /// matches by its literal spelling). A glob is matched against both forms
    /// of the candidate for the same reason.
    /// Test: `a_literal_path_entry_keeps_the_directory_and_its_children`,
    /// `a_glob_entry_keeps_every_matching_worktree`,
    /// `a_missing_directory_is_still_kept_by_its_literal_spelling`.
    pub(crate) fn keeps(&self, path: &Path) -> Option<&str> {
        let resolved = resolve(path);
        for entry in &self.entries {
            match entry {
                KeepEntry::Path { raw, resolved: on } => {
                    if resolved.starts_with(on) || path.starts_with(on) {
                        return Some(raw);
                    }
                }
                KeepEntry::Pattern { raw, matcher } => {
                    if matcher.is_match(path) || matcher.is_match(&resolved) {
                        return Some(raw);
                    }
                }
            }
        }
        None
    }
}

/// Expand a leading `~` against `$HOME`, leaving every other spelling alone.
///
/// Why: the config file is hand-written, and `~/hotstats` is how an operator
/// spells a home-relative directory. `workspace_root_template` already accepts
/// that spelling, so the keep-list accepting it too is consistency rather than
/// a new convention.
/// Test: `a_tilde_path_expands_against_home`.
fn expand_home(raw: &str) -> PathBuf {
    let Some(rest) = raw.strip_prefix('~') else {
        return PathBuf::from(raw);
    };
    let Some(home) = std::env::var_os("HOME") else {
        return PathBuf::from(raw);
    };
    PathBuf::from(home).join(rest.trim_start_matches('/'))
}

/// Canonicalize, falling back to the path as given.
///
/// Why: both sides of every comparison have to be in the same form or a
/// symlinked workspace root compares as unrelated to the worktree inside it.
/// Canonicalization fails for a directory that no longer exists — a stale
/// worktree pointer, which is precisely a case this gate must still answer for
/// — so a failure falls back rather than refusing to compare.
fn resolve(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
#[path = "worktree_keep_list_tests.rs"]
mod worktree_keep_list_tests;
