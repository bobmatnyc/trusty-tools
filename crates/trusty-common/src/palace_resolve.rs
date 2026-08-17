//! The single entry point for "which trusty-memory palace does this project
//! use?" — all four precedence levels, including the pin file and the git
//! probes (#5811).
//!
//! Why: [`crate::palace_id::derive_palace_id`] is a PURE core that implements
//! three of the four precedence levels; its own docs say the fourth — the
//! committed `.trusty-tools/trusty-memory.yaml` pin — "is handled above this
//! function in trusty-memory's `cwd_palace_slug_at`". That split left the pin
//! readable by exactly one caller. Four other callers
//! (`trusty-mpm`'s session launch, `trusty-common`'s catch-up, `trusty-code`'s
//! memory sink, `trusty-agents`' workstream endpoint) each re-implemented the
//! git probe and called the pure core directly, so each answered the question
//! WITHOUT the pin. trusty-mpm's answer then became the `TRUSTY_MEMORY_PALACE`
//! variable exported into every managed session — which is precedence level 1,
//! so a machine-derived slug that never saw the pin outranked the committed pin
//! it was supposed to lose to. This module hoists the whole rule, I/O included,
//! so no caller can answer the question a different way.
//!
//! What: [`resolve_palace`] applies, in order, the env override, the pin file,
//! the git `owner/repo` slug, and the `parent/dir` slug of the MAIN worktree
//! root. It reports which level decided ([`PalaceSource`]) and fails closed on
//! a pin file that exists but cannot be trusted ([`PalaceResolveError`]).
//!
//! Two properties this module exists to guarantee:
//!
//! - **A worktree and its main checkout resolve to the same palace.** A palace
//!   slug is per-project and "shared across all worktrees and branches of the
//!   same repo" — ADR-0012 §1, restated in that ADR's Context design anchors.
//!   Level 4 therefore keys on the main worktree root (`git rev-parse --git-common-dir`)
//!   rather than `--show-toplevel`, which names the worktree's own directory and
//!   made `.claude/worktrees/agent-x` resolve to `worktrees-agent-x`.
//! - **A pin file that exists but does not parse is an error, never a
//!   fallthrough.** Falling through hands the caller a plausible-looking derived
//!   name and its writes land in a palace nobody chose.
//!
//! Test: `cargo test -p trusty-common --features palace-resolve --
//! palace_resolve` — see `palace_resolve_tests.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::palace_id::{derive_palace_id, palace_override_from_env};
use crate::slug::slugify_string;

/// The `.trusty-tools/` directory name, used as a project-root marker.
pub const TRUSTY_TOOLS_DIR: &str = ".trusty-tools";

/// Relative path of the palace pin file within a project root.
pub const PIN_FILE_REL: &str = ".trusty-tools/trusty-memory.yaml";

/// Pin-file schema version. Always `1`.
pub const PIN_SCHEMA_VERSION: u32 = 1;

/// File names that mark a directory as a project root.
///
/// Why: project detection must agree across every crate that asks where a
/// project begins, so the list lives once. `.git` comes first because it is the
/// most universal signal; `.trusty-tools` is included so a directory carrying
/// only a pin file is still recognised.
/// What: an ordered slice checked by [`find_project_root`]; a directory is a
/// project root when it contains ANY entry.
/// Test: `finds_git_root_from_nested_dir`, `trusty_tools_dir_is_a_marker`.
pub const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "pyproject.toml",
    "package.json",
    "go.mod",
    ".project-root",
    TRUSTY_TOOLS_DIR,
];

/// Serialisable schema for `.trusty-tools/trusty-memory.yaml`.
///
/// Why: the pin is the committed, human-authored answer to "which palace is
/// this project's", so it needs a typed schema rather than ad-hoc string
/// scraping — a field that silently deserialises to the wrong type would
/// redirect a project's memory.
/// What: `schema_version` (always [`PIN_SCHEMA_VERSION`]), `palace` (the pinned
/// slug, stored verbatim and never re-slugified), and an optional human `note`.
/// Test: `reads_a_valid_pin`, plus trusty-memory's `write_and_read_pin_round_trips`.
///
/// `#[non_exhaustive]` because a future schema field must not be a breaking
/// change for the crates.io consumers of this crate. That closes struct-literal
/// construction outside `trusty-common`, so build one through [`Self::new`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProjectPin {
    /// Pin-file format version.
    pub schema_version: u32,
    /// The pinned palace slug — verbatim, never re-slugified.
    pub palace: String,
    /// Optional human note (e.g. "pinned before drive reorg 2026-06").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ProjectPin {
    /// A pin naming `palace`, at the current [`PIN_SCHEMA_VERSION`], with no note.
    ///
    /// Why: [`ProjectPin`] is `#[non_exhaustive]`, so no other crate can write
    /// the struct literal. Stamping the version here also stops a caller pinning
    /// an older one by copying an old literal.
    /// What: the two required fields; chain [`Self::with_note`] to add the note.
    /// Test: `new_stamps_the_current_schema_version`.
    pub fn new(palace: impl Into<String>) -> Self {
        Self {
            schema_version: PIN_SCHEMA_VERSION,
            palace: palace.into(),
            note: None,
        }
    }

    /// Attach the optional human note.
    /// Test: `with_note_round_trips_through_yaml`.
    #[must_use]
    pub fn with_note(mut self, note: Option<String>) -> Self {
        self.note = note;
        self
    }
}

/// Which precedence level produced a resolution.
///
/// Why: callers and tests need to assert not just WHICH palace was chosen but
/// WHY. The defect this module fixes was invisible precisely because every
/// level returned a bare `String` — a derived name and a pinned name were
/// indistinguishable once produced, which is how a derived name ended up in the
/// operator-override slot.
/// What: one variant per level, in precedence order.
/// Test: `env_override_reports_its_source`, `pin_reports_its_source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PalaceSource {
    /// Level 1 — the `TRUSTY_MEMORY_PALACE` environment variable.
    EnvOverride,
    /// Level 2 — a committed `.trusty-tools/trusty-memory.yaml`.
    PinFile,
    /// Level 3 — the git `owner/repo` slug from `remote.origin.url`.
    GitOwnerRepo,
    /// Level 4 — the `parent/dir` slug of the main worktree root.
    ParentDir,
}

/// A resolved palace identity plus the level that decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PalaceResolution {
    /// The palace id — a single storage-safe, kebab-case segment.
    pub id: String,
    /// Which precedence level produced [`Self::id`].
    pub source: PalaceSource,
    /// The detected project root, when one was found.
    pub project_root: Option<PathBuf>,
}

/// Why resolution could not produce a trustworthy palace id.
///
/// Why: every variant here is a case where the OLD code returned a plausible
/// name anyway. A pin file that exists but does not parse used to log a warning
/// and fall through to git derivation, so a typo in a committed pin silently
/// redirected a project's memory to a different palace. Returning an error
/// instead makes the caller decide, and makes the failure testable.
/// What: three pin-trust failures plus the exhausted-derivation case.
/// Test: `malformed_pin_is_an_error_not_a_fallthrough`,
/// `empty_pin_palace_is_an_error`, `unreadable_pin_is_an_error`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PalaceResolveError {
    /// The pin file exists but could not be read (permissions, I/O).
    #[error("read palace pin {}: {detail}", path.display())]
    PinUnreadable {
        /// Path of the pin file that could not be read.
        path: PathBuf,
        /// Underlying I/O error text.
        detail: String,
    },

    /// The pin file exists but is not valid pin YAML.
    #[error("parse palace pin {}: {detail}", path.display())]
    PinMalformed {
        /// Path of the pin file that could not be parsed.
        path: PathBuf,
        /// Underlying deserialisation error text.
        detail: String,
    },

    /// The pin file parses but its `palace` field is empty.
    #[error("palace pin {} has an empty `palace` field", path.display())]
    PinEmpty {
        /// Path of the pin file carrying the empty field.
        path: PathBuf,
    },

    /// No precedence level yielded a usable id.
    #[error(
        "could not derive a palace id from {} — set TRUSTY_MEMORY_PALACE, commit a {PIN_FILE_REL}, or pass an explicit palace",
        start.display()
    )]
    NoIdentity {
        /// The directory resolution started from.
        start: PathBuf,
    },
}

/// Walk upward from `start` to the first directory carrying a project marker.
///
/// Why: the pin file is anchored at a project root, so every caller that wants
/// to read it first needs to agree on where the project begins.
/// What: canonicalises `start` (best-effort), then ascends checking
/// [`PROJECT_MARKERS`] at each level. Returns `None` at the filesystem root.
/// Test: `finds_git_root_from_nested_dir`, `no_markers_returns_none`.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    if let Ok(canonical) = std::fs::canonicalize(&current) {
        current = canonical;
    }
    loop {
        for marker in PROJECT_MARKERS {
            if current.join(marker).exists() {
                return Some(current);
            }
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => return None,
        }
    }
}

/// Read the palace pin at `root`, distinguishing absent from untrustworthy.
///
/// Why: "no pin" and "a pin I cannot read" are different answers. The first is
/// the normal case and falls through to derivation; the second must stop the
/// caller, because deriving past a pin the operator committed is what sends
/// writes to the wrong palace.
/// What: reads `root/.trusty-tools/trusty-memory.yaml`. `Ok(None)` when the
/// file does not exist; `Ok(Some(pin))` when it parses; `Err` on any other I/O
/// failure or on a parse failure.
/// Test: `reads_a_valid_pin`, `absent_pin_is_ok_none`,
/// `malformed_pin_is_an_error_not_a_fallthrough`.
pub fn read_project_pin(root: &Path) -> Result<Option<ProjectPin>, PalaceResolveError> {
    let pin_path = root.join(PIN_FILE_REL);
    match std::fs::read_to_string(&pin_path) {
        Ok(raw) => match serde_yaml::from_str::<ProjectPin>(&raw) {
            Ok(pin) => Ok(Some(pin)),
            Err(e) => Err(PalaceResolveError::PinMalformed {
                path: pin_path,
                detail: e.to_string(),
            }),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(PalaceResolveError::PinUnreadable {
            path: pin_path,
            detail: e.to_string(),
        }),
    }
}

/// Resolve the palace for the project containing `start`.
///
/// Why: the one place the question is answered. See the module docs for why the
/// rule cannot live partly here and partly in a caller.
/// What: delegates to [`resolve_palace_with_remote`] with no explicit remote,
/// so the remote is probed from `start`.
/// Test: `worktree_and_main_checkout_agree`, `pin_beats_git_derivation`.
pub fn resolve_palace(start: &Path) -> Result<PalaceResolution, PalaceResolveError> {
    resolve_palace_with_remote(start, None)
}

/// Resolve the palace, optionally supplying the git remote directly.
///
/// Why: a trusty-mpm session cloned from a `repo_url` knows its origin remote
/// before any checkout exists on disk, so it must be able to supply the remote
/// rather than have it probed. Every other caller passes `None`.
///
/// Precedence, highest first:
///
/// 1. `TRUSTY_MEMORY_PALACE`, slugified — the operator escape hatch.
/// 2. The committed pin file at the nearest project root, verbatim.
/// 3. The git `owner/repo` slug from `remote.origin.url`.
/// 4. The `parent/dir` slug of the MAIN worktree root.
///
/// Levels 1 and 2 disagreeing is legitimate — a human set the variable — so the
/// variable wins and the disagreement is logged at WARN naming both values. A
/// pin file that exists but cannot be parsed is never skipped; it is an error.
///
/// What: returns the id, the deciding [`PalaceSource`], and the detected
/// project root.
/// Test: `env_override_wins_over_pin_and_warns`, `pin_beats_git_derivation`,
/// `worktree_and_main_checkout_agree`, `malformed_pin_is_an_error_not_a_fallthrough`.
pub fn resolve_palace_with_remote(
    start: &Path,
    explicit_remote: Option<&str>,
) -> Result<PalaceResolution, PalaceResolveError> {
    let project_root = find_project_root(start);

    // Level 2 is read FIRST even though level 1 outranks it: an unparseable pin
    // is an error regardless of who wins, and reading it here also gives the
    // override path a concrete value to report in the disagreement warning.
    let pinned = match project_root.as_deref() {
        Some(root) => read_project_pin(root)?.map(|pin| (pin.palace, root.join(PIN_FILE_REL))),
        None => None,
    };
    if let Some((palace, path)) = pinned.as_ref()
        && palace.trim().is_empty()
    {
        return Err(PalaceResolveError::PinEmpty { path: path.clone() });
    }

    // Level 1: the operator escape hatch.
    if let Some(raw) = palace_override_from_env() {
        let slug = slugify_string(&raw);
        if !slug.is_empty() {
            // #5811: the disagreement that hid the original defect. A human
            // setting the variable is legitimate; a producer laundering a
            // derived name through it is not, and this is the line that makes
            // the difference visible.
            if let Some((pinned_palace, path)) = pinned.as_ref()
                && pinned_palace != &slug
            {
                tracing::warn!(
                    env_value = %slug,
                    pinned_value = %pinned_palace,
                    pin_file = %path.display(),
                    "TRUSTY_MEMORY_PALACE disagrees with the committed pin; the environment wins"
                );
            }
            return Ok(PalaceResolution {
                id: slug,
                source: PalaceSource::EnvOverride,
                project_root,
            });
        }
    }

    // Level 2: the committed pin.
    if let Some((palace, _)) = pinned {
        return Ok(PalaceResolution {
            id: palace,
            source: PalaceSource::PinFile,
            project_root,
        });
    }

    // Levels 3 and 4: derive. The `parent/dir` fallback keys on the MAIN
    // worktree root so every worktree of a repo lands in one palace.
    let derivation_root = main_worktree_root(start)
        .or_else(|| project_root.clone())
        .unwrap_or_else(|| start.to_path_buf());
    let probed;
    let remote = match explicit_remote {
        Some(r) => Some(r),
        None => {
            probed = git_remote_origin(start);
            probed.as_deref()
        }
    };

    let had_remote = remote
        .and_then(crate::palace_id::owner_repo_from_git_remote)
        .is_some();
    match derive_palace_id(&derivation_root, remote, None) {
        Some(id) if !id.is_empty() => Ok(PalaceResolution {
            id,
            source: if had_remote {
                PalaceSource::GitOwnerRepo
            } else {
                PalaceSource::ParentDir
            },
            project_root,
        }),
        _ => Err(PalaceResolveError::NoIdentity {
            start: start.to_path_buf(),
        }),
    }
}

/// Resolve the MAIN worktree's root directory for the repo containing `start`.
///
/// Why: `git rev-parse --show-toplevel` names the CURRENT worktree, so the
/// `parent/dir` fallback derived `worktrees-agent-x` inside a worktree and
/// `projects-trusty-tools` in the main checkout — two palaces for one project,
/// against ADR-0012 §1. `--git-common-dir` names the main repo's `.git` from
/// any worktree, so its parent is the one directory every worktree agrees on.
/// What: runs `git rev-parse --path-format=absolute --git-common-dir` under
/// `start` and returns that path's parent. Returns `None` when git is absent,
/// `start` is outside a repo, or the flag is unsupported (git < 2.31), in which
/// case the caller falls back to the detected project root.
/// Test: `worktree_and_main_checkout_agree`, `main_worktree_root_outside_repo_is_none`.
pub fn main_worktree_root(start: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let git_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if git_dir.is_empty() {
        return None;
    }
    PathBuf::from(git_dir).parent().map(Path::to_path_buf)
}

/// Read `remote.origin.url` for the repo containing `start` (best-effort).
///
/// Why: the canonical identity of a hosted project is its origin remote, and
/// four crates each had their own copy of this three-line shell-out. One copy
/// means one answer.
/// What: runs `git -C <start> config --get remote.origin.url`. Returns `None`
/// when there is no origin, git is absent, or `start` is outside a repo. Works
/// unchanged inside a worktree, which shares the main repo's config. No network.
/// Test: `git_remote_origin_outside_repo_is_none`, plus every `*_git_remote` test.
pub fn git_remote_origin(start: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() { None } else { Some(url) }
}

#[cfg(test)]
#[path = "palace_resolve_tests.rs"]
mod tests;
