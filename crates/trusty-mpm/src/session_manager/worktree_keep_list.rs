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
//!
//! # A list that could not be READ keeps everything
//!
//! A protective list has one intolerable failure mode: reading it wrong and
//! concluding nothing is protected. `TrustyToolsConfig::load` collapses any
//! YAML error to defaults so a bad config can never abort startup, and under
//! that rule a typo anywhere in the file — in a section nothing here reads —
//! left `disk` at `None`, the keep-list empty, and the sweep free to delete a
//! worktree the operator had vetoed. So the list carries a third state:
//! [`KeepList::unreadable`], which
//! [`load_disk_keep_list`](crate::core::trusty_tools_config::load_disk_keep_list)
//! builds when the config will not parse. Every path matches it, which turns
//! gate 0 into a refusal of the whole pass and names the config error in the
//! reason. Startup is still never aborted; only DELETION is.
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

/// Why a worktree is kept: an operator entry, or an unreadable keep-list.
///
/// Why: the two are not interchangeable to the operator reading the refusal.
/// One says "you asked for this"; the other says "your config is broken and
/// nothing will be reclaimed until you fix it". Carrying the distinction as
/// DATA lets every surface word it identically and keeps none of them matching
/// on a message string.
/// Test: `an_unreadable_keep_list_keeps_every_path`,
/// `a_literal_path_entry_keeps_the_directory_and_its_children`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeptBy<'a> {
    /// The operator's own entry, as they spelled it.
    Entry(&'a str),
    /// The keep-list could not be read, so every worktree is kept.
    Unreadable(&'a str),
}

impl KeptBy<'_> {
    /// The operator-facing sentence for this decision.
    ///
    /// Test: `an_unreadable_keep_list_keeps_every_path`,
    /// `classify_blocks_a_keep_listed_worktree`.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Entry(raw) => format!("kept by the owner keep-list entry `{raw}`"),
            Self::Unreadable(error) => format!(
                "the owner keep-list could not be read, so nothing may be reclaimed \
                 until it is fixed: {error}"
            ),
        }
    }
}

/// The operator's standing "never touch these worktrees" list (#6927).
///
/// Why: see the module docs. This is a protective list, so the only failure
/// mode that matters is an entry that silently protects nothing — an operator
/// who wrote a bad glob believes their tree is safe. Hence [`invalid`], which
/// every surface that reports the keep-list also reports.
/// What: compiled entries plus the patterns that would not compile. An empty
/// list matches nothing, which is the default and is a no-op gate; a list built
/// by [`Self::unreadable`] matches EVERYTHING — see the module docs.
/// Test: `worktree_keep_list_tests`.
#[derive(Debug, Default)]
pub(crate) struct KeepList {
    entries: Vec<KeepEntry>,
    /// Every pattern as configured, in order, for the surfaces that render it.
    patterns: Vec<String>,
    /// Patterns that failed to compile, as `"<pattern>: <error>"`.
    invalid: Vec<String>,
    /// Why the operator's config could not be read, when it could not be.
    unreadable: Option<String>,
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
            out.patterns.push(raw.clone());
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

    /// A keep-list standing in for one that could not be READ (#6927).
    ///
    /// Why: see the module docs. This is the fail-closed state — an operator's
    /// protective list that could not be parsed must never be mistaken for one
    /// that protects nothing.
    /// What: a list with no entries whose [`Self::keeps`] answers
    /// [`KeptBy::Unreadable`] for every path, and whose [`Self::error`] carries
    /// the parse failure for the surfaces that render it.
    /// Test: `an_unreadable_keep_list_keeps_every_path`.
    pub(crate) fn unreadable(error: impl Into<String>) -> Self {
        Self {
            unreadable: Some(error.into()),
            ..Self::default()
        }
    }

    /// Every pattern as the operator configured it, in order.
    pub(crate) fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// Why the operator's config could not be read, when it could not be.
    ///
    /// Why: rendered beside [`Self::invalid`] by every surface that shows the
    /// keep-list. A `Some` here means the list protects everything and the
    /// operator has to fix their config before any reclaim can run.
    /// Test: `an_unreadable_keep_list_keeps_every_path`.
    pub(crate) fn error(&self) -> Option<&str> {
        self.unreadable.as_deref()
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
    /// What: an UNREADABLE list keeps every path, before any entry is
    /// consulted — see the module docs. Otherwise a path entry matches when the
    /// candidate IS it or sits under it, compared on resolved forms AND on the
    /// raw spellings (a path that will not canonicalize — a worktree whose
    /// directory is already gone — still matches by its literal spelling). A
    /// glob is matched against both forms of the candidate for the same reason.
    ///
    /// One consequence of that literal fallback: an entry naming a directory
    /// that does not exist YET cannot be canonicalized, so it is compared as
    /// written. On a case-insensitive filesystem `~/work/HotStats` and
    /// `~/work/hotstats` are the same directory but different strings, and only
    /// the spelling the worktree is eventually created with will match. Write
    /// the entry in the case the directory will have.
    /// Test: `a_literal_path_entry_keeps_the_directory_and_its_children`,
    /// `a_glob_entry_keeps_every_matching_worktree`,
    /// `a_missing_directory_is_still_kept_by_its_literal_spelling`,
    /// `an_unreadable_keep_list_keeps_every_path`.
    pub(crate) fn keeps(&self, path: &Path) -> Option<KeptBy<'_>> {
        // Fail CLOSED, ahead of every entry: a list we could not read protects
        // everything, whatever `entries` happens to hold (#6927).
        if let Some(error) = &self.unreadable {
            return Some(KeptBy::Unreadable(error));
        }
        let resolved = resolve(path);
        for entry in &self.entries {
            match entry {
                KeepEntry::Path { raw, resolved: on } => {
                    if resolved.starts_with(on) || path.starts_with(on) {
                        return Some(KeptBy::Entry(raw));
                    }
                }
                KeepEntry::Pattern { raw, matcher } => {
                    if matcher.is_match(path) || matcher.is_match(&resolved) {
                        return Some(KeptBy::Entry(raw));
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
///
/// #7504: shared with
/// [`worktree_reclaim_launch`](super::worktree_reclaim_launch), whose gate
/// compares the same two forms of the same candidate paths. One resolver, so the
/// two gates cannot disagree about whether a symlinked workspace root contains a
/// worktree.
pub(in crate::session_manager) fn resolve(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
#[path = "worktree_keep_list_tests.rs"]
mod worktree_keep_list_tests;
