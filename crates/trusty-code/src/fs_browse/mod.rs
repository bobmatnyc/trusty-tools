//! Daemon-side directory inspection for the UI's project picker (UI Phase-1).
//!
//! Why: the UI is a THIN CLIENT — "the UI communicates with the daemon; all UI
//! services talk to the daemon, the daemon provides all functionality". No UI
//! target touches the local filesystem directly, **including Tauri** (which
//! technically could, but must not: doing so would make the Tauri build capable
//! of something the web build isn't, forking the UI into two behaviours). So
//! "browse my disk to pick a project" has to be a daemon API, and this module
//! is it. One call serves both jobs the picker needs: rendering the folder list
//! (screen 7a — breadcrumb + a `git` badge per entry) AND feeding tcode's
//! three-state project binding model (projectless → non-git dir → git repo),
//! because [`DirEntryInfo::is_git_repo`] is exactly the discriminator that model
//! keys on. A non-git directory is a FIRST-CLASS, supported state here, never an
//! error — #2728 removed the git-repo short-circuit precisely so tcode indexes
//! non-git working dirs, and #2747 extended that to temp dirs.
//!
//! What: [`list_dir`] resolves a (possibly `~`-relative) path, then returns a
//! [`DirListing`] — the resolved path, a tilde-collapsed `display_path` for the
//! breadcrumb, the `parent` for up-navigation, and the sorted `entries`, each
//! carrying `name`/`path`/`is_dir`/`is_git_repo`. Failures are the typed
//! [`ListDirError`], distinguishable rather than stringly.
//!
//! ## No path guard here, deliberately — do not "fix" this
//!
//! There is no denylist, allowlist, sandbox root, or permission layer on the
//! path argument, and that is a decision, not an oversight. tcode is a LOCAL
//! app: the daemon runs as the user, with the user's own OS entitlements, so a
//! directory listing discloses nothing the user could not already get from `ls`
//! in their own shell. Decisively: the same API already exposes `task.run`,
//! which executes ARBITRARY CODE as the user. `list_dir` is strictly less
//! powerful than a capability sitting right beside it — guarding browse while
//! `task.run` is one method away is ceremony, not security. There is likewise no
//! macOS TCC permission-state machine: the app inherits whatever entitlements it
//! is granted, and an OS-level refusal surfaces as an ordinary typed error
//! ([`ListDirError::PermissionDenied`]) rather than a bespoke permission model.
//!
//! Note for anyone reaching for a precedent: trusty-search's
//! `allow_sensitive_path` (#2747) is NOT one. That guards INDEXING — file
//! CONTENT leaving the machine for an embedder — which is a categorically
//! different act from listing path names locally.
//!
//! Test: `fs_browse::tests::*` (resolution, git-ness incl. the linked-worktree
//! `.git`-as-file shape, error distinguishability, 7a response shape).

pub mod protocol;
pub mod roster;

use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// A single entry in a [`DirListing`] — one row of screen 7a's picker.
///
/// Why: carries exactly the four fields 7a renders and the binding model
/// consumes: the label (`name`), the value to navigate/bind to (`path`),
/// whether it can be descended into (`is_dir`), and whether it draws the `git`
/// badge vs. the `—` placeholder (`is_git_repo`).
/// What: `path` is absolute and already resolved; `is_git_repo` is always
/// `false` for a non-directory (a file cannot be a repo).
/// Test: `tests::lists_entries_with_names_and_paths`,
/// `tests::non_git_dir_is_reported_as_non_git`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DirEntryInfo {
    /// The entry's file name — 7a's row label (e.g. `acme-api`).
    pub name: String,
    /// Absolute path to the entry; what the client passes back to navigate
    /// into it or to bind it as the project root.
    pub path: String,
    /// Whether the entry is a directory (symlinks are followed).
    pub is_dir: bool,
    /// Whether the entry is a git repository — 7a's `git` badge, and the
    /// non-git → git discriminator of the three-state binding model. See
    /// [`is_git_repo`] for how this is determined.
    pub is_git_repo: bool,
}

/// The result of a successful [`list_dir`] — everything screen 7a needs to
/// render one view of the picker.
///
/// Why: a bare `Vec<DirEntryInfo>` would force the client to re-derive
/// navigation state it cannot compute without filesystem access it does not
/// have. The listed directory has to describe itself: where it resolved to
/// (`path` — `~`/`..` are resolved server-side), how to caption it (`display_path`),
/// and where "up" goes (`parent`).
/// What: `path` is the canonical absolute directory; `display_path` is `path`
/// with the home prefix collapsed back to `~` for the breadcrumb (7a shows
/// `~/code / acme-api /`, which the client renders by splitting this on `/`);
/// `parent` is `None` only at the filesystem root, which is precisely when the
/// client must disable its up-affordance.
/// Test: `tests::listing_shape_supports_breadcrumb_and_badges`,
/// `tests::parent_is_none_at_filesystem_root`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DirListing {
    /// Canonical absolute path of the directory that was listed.
    pub path: String,
    /// `path` with the user's home collapsed to `~` — the breadcrumb caption.
    pub display_path: String,
    /// Absolute path of the parent, or `None` at the filesystem root.
    pub parent: Option<String>,
    /// Entries, directories first, then case-insensitively by name.
    pub entries: Vec<DirEntryInfo>,
}

/// Why a [`list_dir`] call failed, in distinguishable variants.
///
/// Why: the picker reacts differently per cause — a typo'd path is a
/// recoverable inline message, a permission refusal is an OS-level prompt, an
/// unexpected IO fault is a bug report. A stringly error would force the client
/// to substring-match to tell those apart. Each variant maps to a distinct
/// JSON-RPC code in `protocol::to_rpc_error`.
/// What: `NotFound`/`NotADirectory`/`PermissionDenied` are the three
/// caller-actionable cases; `Io` is the residual OS fault; `NoHome` covers a
/// `~` path on a system with no resolvable home directory.
/// Test: `tests::nonexistent_path_is_not_found`,
/// `tests::file_path_is_not_a_directory`,
/// `tests::unreadable_dir_is_permission_denied`.
#[derive(Debug, thiserror::Error)]
pub enum ListDirError {
    /// The path does not exist.
    #[error("path not found: {0}")]
    NotFound(String),
    /// The path exists but is a file, not a directory.
    #[error("not a directory: {0}")]
    NotADirectory(String),
    /// The OS refused access. tcode inherits the user's entitlements — this is
    /// reported, never worked around (see module docs).
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// Any other OS-level IO failure.
    #[error("io error reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// A `~`-relative path was given but no home directory could be resolved.
    #[error("cannot expand `~` — no home directory for this user: {0}")]
    NoHome(String),
}

/// Expand a leading `~` against the user's home directory.
///
/// Why: the picker's breadcrumb speaks in `~/code`, and an operator typing a
/// path into it expects shell semantics. This mirrors the expansion the rest of
/// the workspace already does (`trusty-mpm`'s `managed_root::expand_tilde`,
/// `trusty-agents`' `repl::commands::connect::expand_tilde`) so tcode is
/// consistent with its siblings rather than inventing a third convention.
/// What: `~` and `~/...` expand against `dirs::home_dir`; `~user` is NOT
/// supported (no workspace precedent does it either) and is left verbatim, as
/// is every non-`~` path. Returns `NoHome` only when expansion was actually
/// required and home is unresolvable.
/// Test: `tests::tilde_expands_to_home`, `tests::tilde_bare_expands_to_home`,
/// `tests::absolute_path_is_left_alone`.
fn expand_tilde(raw: &str) -> Result<PathBuf, ListDirError> {
    if raw != "~" && !raw.starts_with("~/") {
        return Ok(PathBuf::from(raw));
    }
    let home = dirs::home_dir().ok_or_else(|| ListDirError::NoHome(raw.to_string()))?;
    Ok(match raw.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => home,
    })
}

/// Collapse a leading home-directory prefix back to `~` for display.
///
/// Why: the inverse of [`expand_tilde`], so the breadcrumb reads `~/code`
/// rather than `/Users/masa/code` — screen 7a's caption. Purely cosmetic: the
/// client always navigates with the absolute `path`, never with this string.
/// What: returns `~` / `~/rest` when `path` is under home, else `path`
/// unchanged (including when home cannot be resolved at all).
/// Test: `tests::display_path_collapses_home_to_tilde`.
fn collapse_home(path: &Path) -> String {
    let Some(home) = dirs::home_dir() else {
        return path.display().to_string();
    };
    if path == home {
        return "~".to_string();
    }
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Whether `dir` is a git repository.
///
/// Why: this is 7a's `git` badge and the binding model's non-git → git
/// discriminator, so it has to be right on the shapes this repo actually
/// contains — and this workspace lives in linked worktrees. `pub(crate)` (not
/// private) since [`roster::list_projects`] (issue #3365's project-picker
/// roster) reuses the exact same discriminator to filter candidate
/// directories down to git repos — one implementation, not a second copy.
///
/// **The subtlety:** in a linked git worktree (and in a submodule), `.git` is a
/// FILE containing a `gitdir: <path>` pointer, NOT a directory. Testing
/// `.git.is_dir()` therefore reports every worktree as non-git — exactly the bug
/// PR #2839 hit in `build.rs`, and the same latent bug still sitting in
/// `trusty_common::project_discovery::inspect_project_dir`. This function
/// handles BOTH shapes.
///
/// What: `.git` as a directory → repo. `.git` as a file → repo only if it
/// actually carries git's documented `gitdir:` pointer, so a coincidental file
/// named `.git` does not earn a badge. The pointer check reads only the first
/// 64 bytes of the file rather than the whole thing — `gitdir: <path>\n` is
/// always well under that, and this runs on the picker's hot path across every
/// entry, so an unbounded `read_to_string` on a `.git` that happens to be a
/// large coincidental file (not a real pointer) would be an avoidable stall.
/// Anything else (including a metadata error, e.g. a permission refusal on the
/// probe) → `false`.
///
/// **Why not shell out to `git rev-parse`** (as `run_task::diff::is_git_repo`
/// does): that costs one subprocess PER ENTRY, and this runs across every entry
/// of a directory on an interactive picker's hot path — a 200-entry `~/code` at
/// ~5ms a spawn is a second of latency for a badge. Two `stat`s and, only for
/// the rare `.git`-as-file, one small bounded read gets the same answer.
/// `rev-parse` stays the right tool in `diff.rs`, which asks once per run about
/// one root.
///
/// Note this deliberately answers "is `dir` a repo ROOT", not "is `dir` inside a
/// work tree" — the picker badges repos, and a plain subdirectory of a repo is
/// not itself one to bind to.
/// Test: `tests::git_dir_is_detected`, `tests::linked_worktree_gitfile_is_detected`,
/// `tests::bogus_dot_git_file_is_not_a_repo`, `tests::non_git_dir_is_reported_as_non_git`,
/// `tests::large_dot_git_file_is_not_a_repo`, `tests::gitdir_pointer_at_exactly_the_read_bound`.
pub(crate) fn is_git_repo(dir: &Path) -> bool {
    let dot_git = dir.join(".git");
    // `metadata` follows symlinks, matching how git itself resolves `.git`.
    match std::fs::metadata(&dot_git) {
        Ok(m) if m.is_dir() => true,
        // Bounded read: `gitdir: <path>` is always well under 64 bytes, and this
        // runs per-entry on the picker's hot path — never read a coincidental
        // large file in full just to check its first few bytes.
        Ok(m) if m.is_file() => std::fs::File::open(&dot_git)
            .and_then(|mut f| {
                let mut buf = [0u8; 64];
                let n = f.read(&mut buf)?;
                Ok(String::from_utf8_lossy(&buf[..n])
                    .trim_start()
                    .starts_with("gitdir:"))
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// Map an OS IO error onto the typed [`ListDirError`] for `path`.
///
/// Why: keeps the `NotFound`/`PermissionDenied`/`Io` triage in one place so
/// `list_dir`'s two fallible syscalls classify failures identically.
/// What: matches `ErrorKind::NotFound`/`PermissionDenied`, else `Io`.
/// Test: exercised via `tests::nonexistent_path_is_not_found` and
/// `tests::unreadable_dir_is_permission_denied`.
fn classify_io(err: std::io::Error, path: &str) -> ListDirError {
    match err.kind() {
        std::io::ErrorKind::NotFound => ListDirError::NotFound(path.to_string()),
        std::io::ErrorKind::PermissionDenied => ListDirError::PermissionDenied(path.to_string()),
        _ => ListDirError::Io {
            path: path.to_string(),
            source: err,
        },
    }
}

/// List `path`, returning everything the picker needs to render it.
///
/// Why: the single daemon-side primitive behind screen 7a — see module docs for
/// why this is an API at all rather than UI-local filesystem access, and for why
/// it carries no path guard.
/// What: expands `~`, canonicalizes (resolving `..`, symlinks, and relative
/// segments — so a client can navigate up by passing `parent`, or by passing
/// `<path>/..`), rejects a non-directory, then reads the entries. `include_hidden`
/// gates dot-entries: `false` (the picker default) hides them, since 7a lists
/// project folders and a user's home is otherwise mostly dotfile noise; `true`
/// is the escape hatch for reaching a genuinely hidden project directory.
/// Entries are sorted directories-first then case-insensitively by name, so the
/// picker's order is stable and does not depend on the OS's readdir order.
/// An entry whose metadata cannot be read is included with `is_dir: false`
/// rather than dropped — a listing that silently omits rows is worse than one
/// that shows an unbadged entry. Likewise, an entry that `read_dir`'s iterator
/// itself fails to produce (a per-entry OS fault mid-enumeration) is skipped
/// with a `tracing::warn!` rather than failing the whole listing — the same
/// best-effort philosophy, just one layer up. This skip path has no dedicated
/// test: triggering a per-entry `read_dir` iteration error deterministically
/// requires a TOCTOU race (e.g. another process removing an entry between
/// `readdir` returning its name and the kernel stat'ing it) that cannot be
/// reproduced portably in a unit test without faking the OS boundary; the
/// existing coverage for `list_dir`'s other per-entry fallback (metadata read
/// failure → `is_dir: false`, see `tests::*` above) exercises the same
/// best-effort code shape.
/// Test: `tests::*`.
pub fn list_dir(path: &str, include_hidden: bool) -> Result<DirListing, ListDirError> {
    let expanded = expand_tilde(path)?;
    let resolved = std::fs::canonicalize(&expanded)
        .map_err(|e| classify_io(e, &expanded.display().to_string()))?;
    let resolved_str = resolved.display().to_string();

    if !resolved.is_dir() {
        return Err(ListDirError::NotADirectory(resolved_str));
    }

    let read = std::fs::read_dir(&resolved).map_err(|e| classify_io(e, &resolved_str))?;

    let mut entries = Vec::new();
    for entry in read {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                // A per-entry enumeration fault (e.g. a file vanishing mid-scan) is
                // best-effort: skip that row rather than failing the whole listing,
                // matching the "an unbadged entry beats a missing one" philosophy
                // above.
                tracing::warn!(
                    dir = %resolved_str,
                    error = %e,
                    "skipping unreadable directory entry"
                );
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !include_hidden && name.starts_with('.') {
            continue;
        }
        let entry_path = entry.path();
        // `metadata` (not `file_type`) so a symlink to a directory lists as a
        // directory — the picker cares about the target, as `cd` would.
        let is_dir = std::fs::metadata(&entry_path)
            .map(|m| m.is_dir())
            .unwrap_or(false);
        entries.push(DirEntryInfo {
            name,
            path: entry_path.display().to_string(),
            is_dir,
            is_git_repo: is_dir && is_git_repo(&entry_path),
        });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(DirListing {
        display_path: collapse_home(&resolved),
        parent: resolved.parent().map(|p| p.display().to_string()),
        path: resolved_str,
        entries,
    })
}

#[cfg(test)]
mod tests;
