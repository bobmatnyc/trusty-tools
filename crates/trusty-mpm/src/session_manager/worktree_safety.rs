//! Pre-deletion safety checks for an orphaned-worktree reclaim candidate
//! (#3649 `git worktree list` cross-check, extended #4091 dirty-tree guard).
//!
//! Why: before #4091 the reclaim path's ENTIRE safety model was "who owned
//! this directory" — the #3649 ownership sentinel plus the
//! [`git_worktree_list_agrees`] cross-check. Neither asks the one question an
//! operator actually cares about: *is there unsaved work in here*. A worktree
//! with a KNOWN owner that passed the owner-terminal + grace-window gates was
//! force-deleted (`git worktree remove --force`, falling back to
//! `fs::remove_dir_all`) with uncommitted edits still in it, silently. Worse,
//! `decommission::remove_session_worktree` also runs
//! `git branch -D session/<leaf>` on success, so commits made on the
//! worktree's own branch and never pushed lose their last reachable ref too —
//! committed work is genuinely destroyed by a reclaim, not merely detached.
//! Ownership answers "may I delete this"; it must not also have to answer "is
//! this safe to delete".
//! What: [`DirtyWorktreePolicy`] (skip vs. explicit force-discard),
//! [`DirtyWorktree`] (a reportable skip record), [`inspect_dirt`] (the check
//! itself), and [`git_worktree_list_agrees`] (moved here verbatim from
//! `prune.rs`, which is at its 500-SLOC cap — these are the same category of
//! check and belong together).
//!
//! FAIL-SAFE DIRECTION: every error path in this module resolves to DIRTY.
//! A git subprocess that cannot be spawned, exits non-zero, or emits an
//! unparsable count, and a directory that cannot be read, all mean "this
//! check could not prove the directory is safe to delete" — which must never
//! become a green light to delete. That inverts the whole point.
//!
//! # The rule, stated once (#4118 review round)
//!
//! `git status` answers "what does git see", and the two most valuable things
//! in a session worktree are precisely the things it does NOT see: work hidden
//! behind `.gitignore`, and work hidden behind this guard's own carve-outs.
//! Both were live on this machine on 2026-07-27, on DIFFERENT branches of the
//! same repo — `main` gitignores `.claude/worktrees/` and `.trusty-mpm/*`
//! (`.gitignore:40,49`), while the older branch checked out in
//! `.base/.worktrees/2eb72dca-…` gitignores neither, so the same content
//! arrives as `??` lines there and is invisible here. A guard that closed only
//! one of those would be sound on one branch and useless on the other, so this
//! module never reasons from "is it gitignored". It asks five questions:
//!
//! 1. **Does git report anything?** — [`STATUS_ARGS`], with every
//!    config knob that could silence it pinned on the command line.
//! 2. **Is anything committed but not on a remote?** — on `HEAD` *and* on the
//!    `session/<leaf>` branch that `decommission` force-deletes.
//! 3. **Is there a nested repository holding unsaved work?** — registered
//!    worktrees from `git worktree list --porcelain` (exact), plus a bounded
//!    walk of gitignored subtrees for repositories git has never heard of,
//!    bare and non-bare alike. Each nested root is then put through questions
//!    1, 2 and 4 in its own right.
//! 4. **Is there anything under `.trusty-mpm/` that is not one of two
//!    disposable artefacts?** — accounted per-file by pathspec, covering the
//!    gitignored and untracked spellings alike.
//! 5. **Is there a high-value gitignored file?** (#4166) — a short list of
//!    names that are unrecoverable if deleted (`.env*`, `*.bak`, `*.orig`),
//!    found by the same walk as question 3. See
//!    `super::worktree_nested::is_high_value_ignored`.
//!
//! # What this rule still CANNOT see (residual risk)
//!
//! - **Gitignored loose files outside `.trusty-mpm/` whose names are not on the
//!   #4166 list** — a scratch note in an ignored directory, a `.mcp.json.local`.
//!   Deliberately not counted: on this repo's own 31 session worktrees,
//!   treating every non-disposable ignored entry as dirt would flag `.claude/`
//!   in 30 of 31 and disable reclamation outright, which is the failure mode of
//!   a guard nobody can run. The named list buys back the unrecoverable cases
//!   without that flag rate; widening it is a measurement, not a judgement
//!   call.
//! - **Anything under a disposable build directory** (`target/`,
//!   `node_modules/`, …), including a nested repository inside one — see
//!   `super::worktree_nested::DISPOSABLE_DIR_NAMES`.
//! - **A repository whose root this walk cannot recognise.** Question 3 knows
//!   two shapes: a `.git` entry, and the `HEAD` + `objects/` + `refs/` triple a
//!   bare repository has instead (#4166 — before that, a bare repo was invisible
//!   and a candidate holding one read CLEAN). A repository laid out as neither —
//!   a `.git` FILE pointing somewhere unusual, a bare repo missing a marker — is
//!   still not seen.
//! - **`.git/info/exclude`**, which is per-repo and on-disk and cannot be
//!   overridden from the command line the way `core.excludesFile` can.
//! - **The stash — but only for the CANDIDATE ITSELF, and this distinction
//!   matters.** A session worktree's stash lives in the shared `.base/.git`
//!   *outside* the candidate, so it survives removal and is genuinely out of
//!   scope here. A **self-contained nested clone** is the opposite case: its
//!   `refs/stash` and reflog sit inside the directory being deleted and die
//!   with it, so [`super::worktree_nested`] counts them. The earlier blanket
//!   claim that "the stash is repo-level, not per-worktree" was true of the
//!   top-level candidate and false of a nested clone — the same conflation
//!   that produced the round-3 CRITICAL.
//! - **Symlinked subtrees** — not followed, because `remove_dir_all` deletes
//!   the link and not its target, so no work is at risk there.
//! - **A repository nested inside a gitignored subtree OF a nested repository**
//!   — the scan stops descending at the first repository root it finds.
//! - **The bare-branch population, partially.** `count_session_branch_unpushed`
//!   covers both `session/<leaf>` and the bare `<leaf>` spelling. Since #4165
//!   every producer emits `session/<leaf>`; the bare arm covers worktrees
//!   provisioned by an older build, whose branch `decommission` still misses.
//! - **`DirtyWorktreePolicy::ForceDiscard`**, which destroys all of the above
//!   by explicit operator opt-in.
//!
//! Test: `worktree_safety_tests`.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

/// The bookkeeping directory trusty-mpm writes into every managed workspace.
const TRUSTY_MPM_DIR: &str = ".trusty-mpm";

/// The ONLY two artefacts under [`TRUSTY_MPM_DIR`] this guard treats as
/// disposable (#4118 gap review).
///
/// Why: the first cut of this guard excused the whole `.trusty-mpm/` subtree on
/// the grounds that it is "the scrollback-snapshot directory". It is not. That
/// subtree ALSO holds `sessions/` pause snapshots (written by
/// `trusty-common`'s `catchup::pause`, read back by `/tm-session-resume`),
/// config checkpoints, and — observed live on 2026-07-27 in
/// `.base/.worktrees/2eb72dca-…/.trusty-mpm/wip-backup-20260727-jira/` — twelve
/// files of hand-rescued, uncommitted Rust source. A prefix match excused all
/// of it, so `dirty_files` stayed 0 and the worktree read CLEAN.
/// What: an ALLOWLIST of exact repo-relative paths, not a prefix. `scrollback`
/// is a pane dump the daemon rewrites on every snapshot;
/// `last-instructions.md` is a regenerated copy of the PM brief. Everything
/// else under `.trusty-mpm/` — named or not, tracked, untracked, or gitignored
/// — counts as work.
/// Test: `inspect_dirt_counts_wip_backup_under_trusty_mpm`,
/// `inspect_dirt_counts_pause_snapshots_under_trusty_mpm`,
/// `inspect_dirt_excludes_untracked_trusty_mpm_dir`.
const DISPOSABLE_TRUSTY_MPM_FILES: &[&str] = &[
    ".trusty-mpm/scrollback.txt",
    ".trusty-mpm/last-instructions.md",
];

/// Git global options pinned on EVERY invocation this module makes (#4118).
///
/// Why: `git status --porcelain` does not report "all work". It reports all
/// work git has been CONFIGURED to show, and three of those config keys can
/// silence it outright. Proven on git 2.54.0 against throwaway repos:
/// `status.showUntrackedFiles=no` makes `git status --porcelain` exit 0 with
/// EMPTY output while untracked work sits on disk; a `core.excludesFile`
/// naming that work hides it the same way. The natural home for either is the
/// SHARED `.base/.git/config` that all ~95 worktrees in the sweep read, so one
/// line there would blind the guard fleet-wide. A work-destroying gate must not
/// be steerable by configuration it does not control.
/// What: `--no-optional-locks` (never write to a candidate's index just to
/// inspect it), plus `-c` pins for every key that changes what status reports
/// or how it spells a path. `-c` beats every config file, so this is
/// deterministic regardless of system/global/repo config. NOT pinned: aliases
/// (git refuses to let an alias shadow a built-in command — verified) and
/// `status.branch` (verified to have no effect on `--porcelain` v1 output).
/// Test: `inspect_dirt_survives_hostile_show_untracked_files_config`,
/// `inspect_dirt_survives_hostile_global_excludes_file`.
const GIT_PINNED_GLOBAL_ARGS: &[&str] = &[
    "--no-optional-locks",
    "-c",
    "core.quotePath=false",
    "-c",
    "core.excludesFile=",
    "-c",
    "status.relativePaths=true",
    "-c",
    "status.showUntrackedFiles=normal",
];

/// Environment variables that point git at a DIFFERENT repository (#4118).
///
/// Why: `git -C <candidate> status` obeys `GIT_DIR`/`GIT_WORK_TREE` over its
/// own `-C`. The daemon inherits its environment from whatever launched it, and
/// a `GIT_DIR` + `GIT_WORK_TREE` pair naming a CLEAN repository makes every
/// candidate report clean — verified: with both set, status on a repo holding
/// an untracked file exits 0 with empty output. Most other combinations fail
/// toward DIRTY, but "most" is not a safety property.
/// What: removed from the child environment so git always resolves the
/// repository from `-C <candidate>` alone. `GIT_CONFIG_COUNT` is stripped too
/// because env-injected config would otherwise compete with
/// [`GIT_PINNED_GLOBAL_ARGS`].
/// Test: `git_command_strips_repository_redirecting_env`.
///
/// `pub(crate)` since #6391 so a failing worktree removal can name which of
/// these the ambient environment carried, instead of reporting only that the
/// directory is still there.
pub(crate) const GIT_ENV_REDIRECTS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CONFIG_COUNT",
];

/// Working-tree status for the candidate itself.
const STATUS_ARGS: &[&str] = &[
    "status",
    "--porcelain",
    "--untracked-files=normal",
    "--ignore-submodules=none",
];

/// Per-file accounting of `.trusty-mpm/`, scoped by pathspec (#4118).
///
/// Why: `--untracked-files=normal` COLLAPSES an untracked directory to a single
/// `?? .trusty-mpm/` line, which makes scrollback indistinguishable from
/// rescued source — the exact discrimination the allowlist needs. `-uall` plus
/// `--ignored=matching` restores per-path detail AND covers the case where the
/// project gitignores `.trusty-mpm/*` (this repo's `main`) so the subtree is
/// invisible to plain status altogether. Both spellings are live in the fleet
/// right now, so the guard must not depend on either.
/// What: scoped by the `-- .trusty-mpm` pathspec so the expensive `-uall`
/// recursion is confined to one small directory instead of the whole checkout.
const TRUSTY_MPM_STATUS_ARGS: &[&str] = &[
    "status",
    "--porcelain",
    "--untracked-files=all",
    "--ignored=matching",
    "--ignore-submodules=none",
    "--",
    TRUSTY_MPM_DIR,
];

/// What the reclaim path does when a candidate holds unsaved work (#4091).
///
/// Why: the default must be impossible to trigger accidentally — an ordinary
/// `/tm-session-pause` (which prunes by DEFAULT, `prune_worktrees` unwrapping
/// to `true`) must never be able to discard uncommitted work. Modelling the
/// override as its own two-variant enum rather than a `force: bool` keeps it
/// from being positionally confused with the `dry_run: bool` that already sits
/// next to it in [`super::prune::SessionManager::prune_orphaned_worktrees`](crate::session_manager::SessionManager::prune_orphaned_worktrees)'s
/// signature, where a swapped argument would mean "really delete, and discard
/// dirty work" instead of "preview".
/// What: [`Skip`](Self::Skip) (the [`Default`]) refuses to remove a dirty
/// candidate and reports it; [`ForceDiscard`](Self::ForceDiscard) removes it
/// anyway, and is reachable ONLY from the explicit `discard_dirty` flag on the
/// `prune-worktrees` HTTP route / `tm session prune-worktrees --discard-dirty`.
/// Test: `dirty_policy_defaults_to_skip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirtyWorktreePolicy {
    /// Refuse to remove a candidate holding unsaved work; report it instead.
    #[default]
    Skip,
    /// Explicit opt-in: remove the candidate even though work will be lost.
    ForceDiscard,
}

/// A reclaim candidate that was skipped because it holds unsaved work (#4091).
///
/// Why: "never silently" is half the fix. A skipped worktree that only shows
/// up as a log line is invisible to the MCP caller, the HTTP client, and the
/// `/tm-session-pause` skill — so the operator sweeping ~95 worktrees would
/// have no idea which ones still hold work. This is the structured, wire-
/// serializable form that every surface returns.
/// What: the candidate `path`, a human-readable `reason`, and the two counts
/// behind that reason — `dirty_files` (working-tree entries reported by
/// `git status --porcelain`, i.e. modified/staged tracked files PLUS
/// untracked-but-not-ignored files) and `unpushed_commits`.
/// Test: `inspect_dirt_reports_modified_tracked_file`,
/// `inspect_dirt_reports_untracked_file`, `inspect_dirt_reports_unpushed_commit`.
#[derive(Debug, Clone, Serialize)]
pub struct DirtyWorktree {
    /// The candidate directory that was NOT removed.
    pub path: PathBuf,
    /// Human-readable explanation of what was found (or why it is unknown).
    pub reason: String,
    /// Working-tree entries from `git status --porcelain` (0 when unknown).
    pub dirty_files: usize,
    /// Commits on `HEAD` not present on the upstream / any remote (0 when unknown).
    pub unpushed_commits: usize,
}

impl DirtyWorktree {
    /// Build a skip record for `path` with an explicit reason and counts.
    pub(super) fn new(
        path: &Path,
        reason: impl Into<String>,
        dirty_files: usize,
        unpushed: usize,
    ) -> Self {
        Self {
            path: path.to_path_buf(),
            reason: reason.into(),
            dirty_files,
            unpushed_commits: unpushed,
        }
    }
}

/// Does this reclaim candidate hold work that removal would destroy (#4091)?
///
/// Why: this is the check the reclaim path never had. It is deliberately the
/// LAST gate — it runs only on candidates the #3649 ownership gate has already
/// approved, so it is additive and cannot weaken that guard.
/// What: returns `None` only when the directory is PROVABLY free of unsaved
/// work; `Some(DirtyWorktree)` in every other case, including every error.
/// "Dirty" means any of:
/// 1. [`count_dirty_files`] finds a working-tree entry — modified or staged
///    tracked files, untracked-but-not-ignored files, and anything under
///    `.trusty-mpm/` that is not one of two disposable artefacts;
/// 2. [`count_unpushed`] finds a commit that is on no remote, on `HEAD` or on
///    the `session/<leaf>` branch `remove_session_worktree` force-deletes;
/// 3. [`super::worktree_nested::nested_dirt`] finds a nested repository
///    holding unsaved work, or a high-value gitignored file (#4166), either
///    way regardless of gitignore;
/// 4. the directory is not a git worktree ROOT at all and is not empty — see
///    [`non_git_dirt`];
/// 5. any check errored — see the module-level FAIL-SAFE note.
///
/// The worktree-root identity check is not incidental: running
/// `git status` inside a plain directory that happens to sit INSIDE a checkout
/// would report the ENCLOSING repository's status, which is both wrong and
/// misleading, so the toplevel must canonicalize to the candidate itself.
/// Test: `inspect_dirt_clean_pushed_worktree_is_none`,
/// `inspect_dirt_reports_modified_tracked_file`,
/// `inspect_dirt_reports_untracked_file`,
/// `inspect_dirt_reports_unpushed_commit`,
/// `inspect_dirt_treats_missing_path_as_dirty`,
/// `inspect_dirt_treats_non_worktree_with_files_as_dirty`,
/// `inspect_dirt_allows_empty_non_git_leftover`.
pub(crate) fn inspect_dirt(path: &Path) -> Option<DirtyWorktree> {
    inspect_dirt_at(path, true)
}

/// [`inspect_dirt`], with the nested-repository scan switchable off.
///
/// Why: the nested scan inspects each nested root by calling back into this
/// function. Running the scan again from inside a nested root would recurse
/// without bound, and buys nothing — [`nested_repo_roots`] already enumerates
/// registered worktrees at EVERY depth in one flat pass.
/// What: `scan_nested = false` answers only questions 1, 2 and 4 for `path`.
/// Test: `inspect_dirt_reports_nested_gitignored_worktree`.
pub(super) fn inspect_dirt_at(path: &Path, scan_nested: bool) -> Option<DirtyWorktree> {
    match is_worktree_root(path) {
        Ok(true) => {}
        // Not a git worktree root (or git cannot tell us) — fall back to the
        // "is it even empty" question, the only one answerable without git.
        Ok(false) | Err(_) => return non_git_dirt(path),
    }

    let dirty_files = match count_dirty_files(path) {
        Ok(n) => n,
        Err(e) => {
            return Some(DirtyWorktree::new(
                path,
                format!("dirty-check failed: {e}"),
                0,
                0,
            ));
        }
    };

    let unpushed = match count_unpushed(path) {
        Ok(n) => n,
        Err(e) => {
            return Some(DirtyWorktree::new(
                path,
                format!("dirty-check failed: {e}"),
                dirty_files,
                0,
            ));
        }
    };

    if dirty_files > 0 || unpushed > 0 {
        let reason =
            format!("{dirty_files} uncommitted/untracked file(s), {unpushed} unpushed commit(s)");
        return Some(DirtyWorktree::new(path, reason, dirty_files, unpushed));
    }
    if scan_nested {
        return super::worktree_nested::nested_dirt(path);
    }
    None
}

/// Count working-tree entries that represent WORK, in two passes (#4118).
///
/// Why: one `git status` cannot both stay cheap on a whole checkout and stay
/// per-file precise inside `.trusty-mpm/`. `-unormal` collapses untracked
/// directories (cheap, but `?? .trusty-mpm/` hides what is underneath) and
/// `-uall` does not (precise, but recurses the entire tree). Splitting by
/// pathspec buys the precision exactly where the allowlist needs it.
/// What: pass one counts every `-unormal` entry except the ownership sentinel
/// and anything rooted at `.trusty-mpm/`; pass two hands `.trusty-mpm/` to
/// [`trusty_mpm_dirt`], which sees ignored and untracked content alike.
/// Test: `inspect_dirt_counts_wip_backup_under_trusty_mpm`,
/// `inspect_dirt_excludes_own_sentinel`.
pub(super) fn count_dirty_files(path: &Path) -> Result<usize, String> {
    let status = git_stdout(path, STATUS_ARGS)?;
    let mut count = 0usize;
    for line in status.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let entry = porcelain_path(line);
        if line.starts_with("?? ") && entry == super::decommission::WORKTREE_SENTINEL_FILE {
            continue;
        }
        if is_trusty_mpm_scoped(entry) {
            continue; // owned by the per-file pass below
        }
        count += 1;
    }
    Ok(count + trusty_mpm_dirt(path)?)
}

/// The repo-relative path of a `git status --porcelain` v1 line.
///
/// Why: every line is `XY <path>`, so the path starts at byte 3. A rename is
/// `R  <old> -> <new>`; this returns the whole `<old> -> <new>` span, which no
/// caller matches against an allowlist — a rename INTO an excused path
/// therefore fails the match and COUNTS, which is the fail-safe direction.
/// What: the trimmed remainder of the line, or `""` for a malformed one (which
/// then matches no allowlist entry and is counted).
fn porcelain_path(line: &str) -> &str {
    line.get(3..).unwrap_or("").trim()
}

/// Is this repo-relative path rooted at the `.trusty-mpm/` bookkeeping dir?
///
/// Why: the ownership sentinel is `.trusty-mpm-worktree`, a SIBLING whose name
/// shares the prefix. Matching on a bare `.trusty-mpm` prefix would route the
/// sentinel into the per-file pass, where it is not on the allowlist and would
/// be counted — marking every managed worktree dirty forever.
/// What: true for `.trusty-mpm`, `.trusty-mpm/` and anything beneath it; false
/// for `.trusty-mpm-worktree`.
/// Test: `inspect_dirt_excludes_own_sentinel`.
fn is_trusty_mpm_scoped(path: &str) -> bool {
    path == TRUSTY_MPM_DIR || path.starts_with(".trusty-mpm/")
}

/// Count entries under `.trusty-mpm/` that are not disposable bookkeeping.
///
/// Why: see [`DISPOSABLE_TRUSTY_MPM_FILES`]. This is the leg that closes the
/// live `wip-backup-20260727-jira/` hole, and it must work whether the subtree
/// is gitignored (invisible to plain status) or untracked (collapsed by plain
/// status) — hence [`TRUSTY_MPM_STATUS_ARGS`].
/// What: one count per reported entry that is not on the allowlist. A quoted
/// path is an ERROR, not a silently-unmatched one: if the guard cannot spell
/// the path it cannot decide the path is disposable.
/// Test: `inspect_dirt_counts_wip_backup_under_trusty_mpm`,
/// `inspect_dirt_counts_pause_snapshots_under_trusty_mpm`,
/// `inspect_dirt_counts_a_deleted_tracked_file_under_trusty_mpm`.
fn trusty_mpm_dirt(root: &Path) -> Result<usize, String> {
    // NOT `Path::exists()`: that collapses every I/O error to `false`, and
    // pass 1 has already skipped every `.trusty-mpm/`-scoped entry on the
    // promise that this pass owns them — so a permission error here would
    // become CLEAN, inverting the module's own fail-safe invariant.
    let dir = root.join(TRUSTY_MPM_DIR);
    match std::fs::symlink_metadata(&dir) {
        Ok(_) => {}
        // #4166: absence must NOT short-circuit. A tracked file DELETED from
        // `.trusty-mpm/` leaves no directory behind, and returning 0 here
        // meant pass 1 skipped that status line and pass 2 never ran — so the
        // deletion was counted by neither. The pathspec below is correct on
        // its own: verified to exit 0 with empty output in a repo where
        // `.trusty-mpm` never existed, so nothing is gained by asking first.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("`{}` is unreadable: {e}", dir.display())),
    }
    let out = git_stdout(root, TRUSTY_MPM_STATUS_ARGS)?;
    let mut count = 0usize;
    for line in out.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let entry = porcelain_path(line);
        if entry.starts_with('"') {
            return Err(format!(
                "`{entry}` under {TRUSTY_MPM_DIR}/ has a quoted path this guard cannot classify"
            ));
        }
        if DISPOSABLE_TRUSTY_MPM_FILES.contains(&entry) {
            continue;
        }
        count += 1;
    }
    Ok(count)
}

/// Classify a candidate that is NOT a git worktree root (#4091).
///
/// Why: the orphan sweep's original purpose (#1838) is reclaiming the leftover
/// `.worktrees/<id>` SHELLS that accumulate when `git worktree remove` never
/// ran or half-ran — one project grew 94 of them. Those shells carry no git
/// metadata, so no git-based dirty check can speak to them, and treating every
/// one as dirty would neuter reclamation entirely (the guard must not become a
/// permanent leak). But a non-git directory that still holds FILES is exactly
/// the case we cannot assess, and the fail-safe direction is to keep it.
/// What: `None` when the directory holds nothing but the trusty-mpm ownership
/// sentinel (an empty shell — provably no work); `Some` otherwise, including
/// when the directory cannot be read at all.
/// Test: `inspect_dirt_allows_empty_non_git_leftover`,
/// `inspect_dirt_treats_non_worktree_with_files_as_dirty`,
/// `inspect_dirt_treats_missing_path_as_dirty`.
fn non_git_dirt(path: &Path) -> Option<DirtyWorktree> {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            return Some(DirtyWorktree::new(
                path,
                format!("not a git worktree and the directory is unreadable: {e}"),
                0,
                0,
            ));
        }
    };
    let mut count = 0usize;
    for entry in entries {
        let Ok(entry) = entry else {
            return Some(DirtyWorktree::new(
                path,
                "not a git worktree and a directory entry is unreadable",
                count,
                0,
            ));
        };
        if entry.file_name() == super::decommission::WORKTREE_SENTINEL_FILE {
            continue;
        }
        count += 1;
    }
    if count == 0 {
        return None;
    }
    Some(DirtyWorktree::new(
        path,
        format!(
            "not a git worktree root, but holds {count} entr(y/ies) whose contents \
             cannot be verified as saved"
        ),
        count,
        0,
    ))
}

/// Does `path` canonicalize to its own git worktree toplevel?
///
/// Why: `git -C <dir> status` happily answers for an ENCLOSING repository when
/// `dir` is merely nested inside one, so an identity check is required before
/// any status output can be attributed to the candidate itself.
/// What: `Ok(true)` when `git rev-parse --show-toplevel` canonicalizes to the
/// same path as `path`; `Ok(false)` when it resolves elsewhere; `Err` when git
/// cannot answer (not a repository, git missing, spawn failure).
/// Test: `inspect_dirt_treats_non_worktree_with_files_as_dirty` (the nested
/// plain-directory case), `inspect_dirt_clean_pushed_worktree_is_none`.
fn is_worktree_root(path: &Path) -> Result<bool, String> {
    let top = git_stdout(path, &["rev-parse", "--show-toplevel"])?;
    let top = PathBuf::from(top.trim());
    if top.as_os_str().is_empty() {
        return Ok(false);
    }
    let resolved_top = std::fs::canonicalize(&top).unwrap_or(top);
    let resolved_self =
        std::fs::canonicalize(path).map_err(|e| format!("candidate is unreadable: {e}"))?;
    Ok(resolved_top == resolved_self)
}

/// Count commits on `HEAD` that are not yet safely on a remote (#4091).
///
/// Why: `decommission::remove_session_worktree` deletes the worktree's
/// `session/<leaf>` branch after a successful removal, so a commit that exists
/// only on that branch loses its last reachable ref and becomes garbage-
/// collectable. "Committed" is therefore NOT the same as "safe".
/// What: when the checked-out branch has an upstream, counts
/// `<upstream>..HEAD`. With NO upstream configured, counts commits reachable
/// from `HEAD` but from no remote-tracking ref at all
/// (`rev-list --count HEAD --not --remotes`) — which subsumes the
/// "not on `origin/main`" rule (a commit absent from `origin/main` AND every
/// other remote ref is counted) while not falsely flagging a branch that WAS
/// pushed under its own name but never had upstream tracking configured. In a
/// repository with no remotes at all, `--remotes` expands to nothing and every
/// commit counts as unpushed — the correct, conservative answer.
/// Reachability alone is not the question a SQUASH-merge repository can answer
/// (#6507). A squash merge replays the branch's changes as ONE NEW commit on
/// `main`, so the branch tip is never an ancestor of it, and
/// `gh pr merge --squash --delete-branch` then removes the remote branch that
/// made those commits reachable. `rev-list --count HEAD --not --remotes` counts
/// them all as unpushed, gate 6 of the merged-PR reclaim reports "holds unsaved
/// work", and `prune-worktrees --merged-prs` reclaims nothing on a repository
/// that squash-merges every PR. [`count_unlanded`] subtracts the commits whose
/// PATCH is provably already on a remote landing branch — per commit for a
/// one-commit branch, and per whole divergence for the two-or-more-commit
/// branch a squash collapses into a patch none of them matches
/// ([`squashed_divergence`]).
/// Test: `inspect_dirt_reports_unpushed_commit`,
/// `inspect_dirt_clean_pushed_worktree_is_none`,
/// `inspect_dirt_clears_a_squash_merged_branch_whose_upstream_was_pruned`,
/// `inspect_dirt_clears_a_two_commit_squash_merge`.
fn count_unpushed(path: &Path) -> Result<usize, String> {
    let upstream = git_stdout(
        path,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());

    // #6507: the count and the listing must be the SAME query — the listing is
    // what the patch-equivalence filter runs over.
    let range;
    let (count_args, list_args): (Vec<&str>, Vec<&str>) = match &upstream {
        Some(up) => {
            range = format!("{up}..HEAD");
            (
                vec!["rev-list", "--count", &range],
                vec!["rev-list", &range],
            )
        }
        None => (
            vec!["rev-list", "--count", "HEAD", "--not", "--remotes"],
            vec!["rev-list", "HEAD", "--not", "--remotes"],
        ),
    };
    let head = count_unlanded(path, &count_args, &list_args, &["HEAD"])?;
    Ok(head + count_session_branch_unpushed(path)?)
}

/// The most unpushed-looking commits this guard will patch-compare (#6507).
///
/// Why: the no-upstream arm of [`count_unpushed`] is
/// `rev-list HEAD --not --remotes`, which in a repository with no remote refs
/// at all expands to the WHOLE history — tens of thousands of commits here. The
/// patch comparison is worth running over a branch's divergence and never over
/// a history, so past this many candidates the raw count is returned unchanged.
/// That is the fail-closed direction: more unpushed commits, never fewer.
/// What: 200, comfortably above any real branch's divergence.
/// Test: `inspect_dirt_clears_a_squash_merged_branch_whose_upstream_was_pruned`
/// exercises the comparing path; the cap only ever restores today's behaviour.
const LANDED_PROBE_MAX_CANDIDATES: usize = 200;

/// Ref patterns naming a branch a merge can LAND on (#6507).
///
/// Why: `git cherry` takes exactly one upstream, so the comparison needs a
/// concrete ref rather than `--remotes`. A squash merge lands on a remote's
/// default branch, and these three patterns name it: `HEAD` when the remote's
/// symbolic default is known locally (every `git clone` sets it), and the two
/// conventional spellings for a repository built by `git init` + `git remote
/// add`, which sets no `HEAD`. A pattern that matches nothing contributes
/// nothing, so an unusual default branch simply leaves the raw count in place.
/// Test: `inspect_dirt_clears_a_squash_merged_branch_whose_upstream_was_pruned`.
const LANDING_BASE_PATTERNS: &[&str] = &[
    "refs/remotes/*/HEAD",
    "refs/remotes/*/main",
    "refs/remotes/*/master",
];

/// How many distinct landing bases one call will compare against (#6507).
///
/// Why: each base costs one `git cherry` per tip, and a machine with several
/// remotes would otherwise pay for all of them on every dirty check.
const LANDING_BASE_LIMIT: usize = 4;

/// Count the commits in one rev-list query that are NOT already on a remote,
/// by patch rather than by reachability (#6507).
///
/// Why: see [`count_unpushed`]. The subtraction is per-COMMIT and never a
/// difference of counts: `git cherry`'s output range is `<base>..<tip>`, which
/// can hold commits this query never counted, so subtracting its `-` total
/// would discount a genuinely unsaved commit against an unrelated match.
/// What: runs `count_args` for the number and `list_args` for the same
/// commits' SHAs, then drops those [`landed_on_a_remote`] proved equivalent to
/// a commit already on a landing branch. Every failure arm keeps the raw count:
/// an unanswerable patch comparison must never lower it.
/// Test: `inspect_dirt_clears_a_squash_merged_branch_whose_upstream_was_pruned`,
/// `inspect_dirt_still_reports_a_genuinely_unpushed_commit_beside_a_squashed_one`.
fn count_unlanded(
    path: &Path,
    count_args: &[&str],
    list_args: &[&str],
    tips: &[&str],
) -> Result<usize, String> {
    let raw = git_stdout(path, count_args)?;
    let counted = raw
        .trim()
        .parse::<usize>()
        .map_err(|e| format!("unparsable rev-list count `{}`: {e}", raw.trim()))?;
    if counted == 0 || counted > LANDED_PROBE_MAX_CANDIDATES {
        return Ok(counted);
    }
    let listed = git_stdout(path, list_args)?;
    let landed = landed_on_a_remote(path, tips);
    Ok(listed
        .split_whitespace()
        .filter(|sha| !landed.contains(*sha))
        .count())
}

/// The SHAs reachable from `tips` whose patch is already on a landing branch
/// (#6507).
///
/// Why: this is the only evidence that turns "unreachable from every remote"
/// into "already landed". `git cherry <base> <tip>` marks a commit `-` when its
/// patch id matches one on `base` — which is exactly what a squash merge
/// produces — and `+` otherwise.
/// What: the `-` SHAs, unioned over every [`landing_bases`] base and every tip,
/// plus whatever [`squashed_divergence`] proves landed as ONE squash commit —
/// which is the only thing that clears a branch of two or more commits.
/// An empty set on every failure path, which leaves the caller's count intact.
///
/// Residual, stated rather than hidden: an EMPTY commit has an empty patch id,
/// so two unrelated empty commits read as equivalent and one could be
/// discounted. An empty commit carries no file content, so nothing recoverable
/// is at risk. `git cherry` also skips merge commits, which therefore never
/// appear here and stay counted — the conservative direction.
/// Test: `inspect_dirt_clears_a_squash_merged_branch_whose_upstream_was_pruned`,
/// `inspect_dirt_clears_a_two_commit_squash_merge`.
fn landed_on_a_remote(path: &Path, tips: &[&str]) -> HashSet<String> {
    let mut landed = HashSet::new();
    for base in landing_bases(path) {
        for tip in tips {
            let Ok(out) = git_stdout(path, &["cherry", &base, tip]) else {
                continue;
            };
            for line in out.lines() {
                if let Some(sha) = line.strip_prefix("- ") {
                    landed.insert(sha.trim().to_string());
                }
            }
            // #6507: `git cherry` compares one commit's patch at a time, so a
            // squash of TWO OR MORE commits matches none of them. Ask the
            // whole-divergence question too.
            landed.extend(squashed_divergence(path, &base, tip));
        }
    }
    landed
}

/// How many landing-branch commits one squash probe patch-compares (#6507).
///
/// Why: the probe narrows candidates by a path the branch actually touched, so
/// the true squash commit is always in the list; this only bounds how far back
/// a match is looked for when many commits touch that path. A miss keeps the
/// raw count, so the cap can only ever over-report.
const SQUASH_CANDIDATE_MAX: usize = 25;

/// The commits in `<base>..<tip>` when the branch's WHOLE divergence is already
/// on `base` as ONE squash commit (#6507).
///
/// Why: `git cherry` — the #6528 fix — compares commits pairwise, so it clears
/// a branch that squash-merged from a SINGLE commit and nothing else. Live on
/// 2026-09-03: PR #6705 squashed two commits (`37540d9cb`, `4c8e11de8`) into
/// `7933d406d`, neither commit's patch id matched, and gate 6 of the merged-PR
/// reclaim reported "0 uncommitted/untracked file(s), 2 unpushed commit(s)" for
/// a worktree whose work had fully landed. A squash's patch is the UNION of the
/// branch's commits, so the branch's aggregate diff is the only thing that can
/// match it.
/// What: `git diff <merge-base> <tip>` reduced to one patch id, compared
/// against the patch ids of up to [`SQUASH_CANDIDATE_MAX`] commits on `base`
/// that touch a path the branch touched. An exact match means every commit in
/// `<merge-base>..<tip>` landed, and those SHAs are returned; anything else —
/// no merge base, an empty diff, no candidate, a git failure — returns nothing
/// and leaves the caller's count untouched.
///
/// The match is exact and aggregate, so a commit added AFTER the squash changes
/// the aggregate diff and nothing is discounted: the fail-closed direction.
/// Test: `inspect_dirt_clears_a_two_commit_squash_merge`,
/// `inspect_dirt_reports_a_commit_added_after_a_two_commit_squash_merge`.
fn squashed_divergence(path: &Path, base: &str, tip: &str) -> Vec<String> {
    let nothing = Vec::new();
    let Ok(merge_base) = git_stdout(path, &["merge-base", base, tip]) else {
        return nothing;
    };
    let merge_base = merge_base.trim().to_string();
    if merge_base.is_empty() {
        return nothing;
    }
    let Some((aggregate, _)) = patch_ids(path, &["diff", &merge_base, tip])
        .into_iter()
        .next()
    else {
        return nothing;
    };
    let Ok(names) = git_stdout(path, &["diff", "--name-only", &merge_base, tip]) else {
        return nothing;
    };
    let Some(probe) = names.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return nothing;
    };
    let limit = SQUASH_CANDIDATE_MAX.to_string();
    let range = format!("{merge_base}..{base}");
    let Ok(listing) = git_stdout(
        path,
        &["log", "--format=%H", "-n", &limit, &range, "--", probe],
    ) else {
        return nothing;
    };
    let mut show: Vec<&str> = vec!["show"];
    show.extend(listing.split_whitespace());
    if show.len() == 1
        || !patch_ids(path, &show)
            .iter()
            .any(|(id, _)| *id == aggregate)
    {
        return nothing;
    }
    let Ok(landed) = git_stdout(path, &["rev-list", &format!("{merge_base}..{tip}")]) else {
        return nothing;
    };
    landed.split_whitespace().map(str::to_string).collect()
}

/// `git patch-id --stable` over the patch a git command prints (#6507).
///
/// Why: patch equivalence is the only signal that survives a squash, and
/// `git patch-id` is the only thing that computes it. It reads a patch STREAM
/// and emits one `<patch-id> <commit-id>` line per patch in it, so a single
/// pair of subprocesses covers both `git diff` (one line, no commit) and
/// `git show <sha>…` (one line per commit).
/// What: runs `git <args>`, feeds its stdout to `git patch-id --stable`, and
/// parses the reply. Every failure — the first command, the spawn, a non-zero
/// exit — yields an empty vector, which discounts nothing.
/// Test: `inspect_dirt_clears_a_two_commit_squash_merge`.
fn patch_ids(path: &Path, args: &[&str]) -> Vec<(String, String)> {
    let Ok(patch) = git_stdout(path, args) else {
        return Vec::new();
    };
    if patch.trim().is_empty() {
        return Vec::new();
    }
    let Ok(mut child) = git_command(path, &["patch-id", "--stable"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(patch.as_bytes());
    }
    let Ok(out) = child.wait_with_output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((
                fields.next()?.to_string(),
                fields.next().unwrap_or_default().to_string(),
            ))
        })
        .collect()
}

/// Remote refs a squash merge could have landed on, deduplicated by commit
/// (#6507).
///
/// Why: `refs/remotes/origin/HEAD` and `refs/remotes/origin/main` usually name
/// the SAME commit, and comparing against both would double the subprocess cost
/// for one answer.
/// What: one `for-each-ref` over [`LANDING_BASE_PATTERNS`], keeping the first
/// ref for each distinct object and at most [`LANDING_BASE_LIMIT`] of them. An
/// empty vector when git cannot be asked, which discounts nothing.
/// Test: `inspect_dirt_clears_a_squash_merged_branch_whose_upstream_was_pruned`.
fn landing_bases(path: &Path) -> Vec<String> {
    let mut args = vec!["for-each-ref", "--format=%(objectname) %(refname)"];
    args.extend_from_slice(LANDING_BASE_PATTERNS);
    let Ok(out) = git_stdout(path, &args) else {
        return Vec::new();
    };
    let mut seen: HashSet<&str> = HashSet::new();
    let mut bases = Vec::new();
    for line in out.lines() {
        let Some((object, refname)) = line.trim().split_once(' ') else {
            continue;
        };
        if !seen.insert(object) {
            continue;
        }
        bases.push(refname.to_string());
        if bases.len() >= LANDING_BASE_LIMIT {
            break;
        }
    }
    bases
}

/// Count unpushed commits on the `session/<leaf>` branch `decommission` deletes.
///
/// Why: [`count_unpushed`] inspects `HEAD`, but
/// `decommission::remove_session_worktree` step 3 force-deletes
/// `session/<leaf>` derived from the DIRECTORY NAME, not from `HEAD`. Switching
/// a session worktree off its own branch is routine here — the live
/// `.base/.worktrees/2eb72dca-…` sits on `fix/4061-…`, not `session/2eb72dca-…`
/// — and in that state HEAD is fully pushed while the session branch is not.
/// Reproduced end-to-end on git 2.54.0: after committing on `session/sess` and
/// switching to `main`, `rev-list --count HEAD --not --remotes` returns 0 while
/// the branch still carries the commit. HEAD-only counting therefore returned a
/// false CLEAN for the one branch about to be force-deleted.
/// What: BOTH spellings of "the branch named after this directory" are checked
/// — `refs/heads/session/<leaf>` and the bare `refs/heads/<leaf>`. Every
/// producer now emits the prefixed spelling: `worktree_branch_for` backs
/// `inproject::create_session_worktree`, `remove_session_worktree`'s
/// `branch -D`, and — since #4165 — `provisioner::workspace::provision_in`,
/// which used to name the branch bare and so leaked one branch per session.
///
/// The bare check stays for the population already on disk. Worktrees
/// provisioned before #4165 still carry a bare branch, `branch -D
/// session/<uuid>` still misses it, and it still survives removal — so
/// counting it here over-reports rather than under-reports, which is the
/// direction this module errs in deliberately. Drop the bare arm only once
/// no pre-#4165 worktree remains; a guard that goes quietly inert because
/// somebody repaired a naming bug is the failure mode this module exists to
/// prevent.
///
/// 0 when neither ref exists (`for-each-ref` exits 0 with empty output, so
/// absence is distinguishable from a spawn failure, which stays an `Err`).
/// Otherwise the commits on those branches reachable from no remote AND not
/// already counted via `HEAD` — excluding `HEAD` is what stops the common
/// "branch IS the checked-out branch" case from being counted twice, and
/// passing both refs to ONE `rev-list` stops a commit shared by both spellings
/// from being counted twice either.
/// Test: `inspect_dirt_reports_unpushed_session_branch_when_head_moved`,
/// `inspect_dirt_reports_unpushed_bare_leaf_branch_when_head_moved`,
/// `inspect_dirt_does_not_double_count_the_checked_out_session_branch`.
fn count_session_branch_unpushed(path: &Path) -> Result<usize, String> {
    let Some(leaf) = path.file_name().and_then(|n| n.to_str()) else {
        return Ok(0);
    };
    let mut present = Vec::new();
    for refname in [
        format!("refs/heads/session/{leaf}"),
        format!("refs/heads/{leaf}"),
    ] {
        let listed = git_stdout(path, &["for-each-ref", "--format=%(refname)", &refname])?;
        if !listed.trim().is_empty() {
            present.push(refname);
        }
    }
    if present.is_empty() {
        return Ok(0);
    }
    let mut count_args: Vec<&str> = vec!["rev-list", "--count"];
    count_args.extend(present.iter().map(String::as_str));
    count_args.extend(["--not", "--remotes", "HEAD"]);
    let mut list_args: Vec<&str> = vec!["rev-list"];
    list_args.extend(present.iter().map(String::as_str));
    list_args.extend(["--not", "--remotes", "HEAD"]);
    // #6507: a session branch is squash-merged like any other, so the same
    // patch-equivalence subtraction applies here. The tips are the branch refs
    // themselves — `HEAD` is excluded from this query, so comparing against it
    // would name commits this arm never counted.
    let tips: Vec<&str> = present.iter().map(String::as_str).collect();
    count_unlanded(path, &count_args, &list_args, &tips)
}

/// Run `git -C <dir> <args>` and return stdout, or an error string.
///
/// Why: every check in this module needs the same "ran, exited zero, gave me
/// stdout" contract, and every deviation from it must surface as an `Err` the
/// caller turns into DIRTY rather than being swallowed.
/// What: non-zero exit and spawn failure both become `Err` carrying the
/// command and git's own stderr; stdout is lossily decoded.
/// Test: `inspect_dirt_treats_missing_path_as_dirty`.
pub(crate) fn git_stdout(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git_command(dir, args)
        .output()
        .map_err(|e| format!("`git {}` could not be run: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "`git {}` failed ({}): {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Build a `git` invocation that cannot be steered by ambient config or
/// environment (#4118).
///
/// Why: EVERY git call in this module has to be immune to the two hijacks that
/// turn a dirty worktree into a clean report — config keys that suppress
/// output ([`GIT_PINNED_GLOBAL_ARGS`]) and environment variables that point git
/// at a different repository ([`GIT_ENV_REDIRECTS`]). Centralising it here is
/// what makes "every git call" true rather than aspirational; a future call
/// site cannot forget.
/// What: `git <pinned globals> -C <dir> <args>`, with the redirect variables
/// removed from the child environment.
/// Test: `git_command_strips_repository_redirecting_env`,
/// `git_command_pins_untracked_and_excludes_config`.
pub(crate) fn git_command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(GIT_PINNED_GLOBAL_ARGS)
        .arg("-C")
        .arg(dir)
        .args(args);
    for key in GIT_ENV_REDIRECTS {
        cmd.env_remove(key);
    }
    cmd
}

/// Build the one `git worktree remove --force` invocation this crate runs
/// (#6391).
///
/// Why: `git worktree` resolves its repository from `GIT_DIR` ahead of its own
/// `-C`, so an ambient redirect aims the removal at a DIFFERENT repository than
/// the one every gate before it just examined. Those gates all route through
/// [`git_command`], which strips [`GIT_ENV_REDIRECTS`]; the removal did not,
/// because each of its two call sites spelt the command out with a bare
/// `Command::new("git")`. That left the one call that actually deletes as the
/// only unhardened one. What an operator sees is a reap in which every gate
/// passes, `git` then exits 128 with `is not a working tree` (the text #6099
/// recorded), the refusal reaches nothing but a `warn!`, and the directory is
/// still on disk. A git hook exports `GIT_DIR` and `GIT_INDEX_FILE` to
/// everything it launches, so a daemon or test run descended from one inherits
/// exactly that redirect.
/// What: [`git_command`] plus `worktree remove --force <path>`. `path` is passed
/// as a `Path` rather than a `&str` so a non-UTF-8 name is not coerced lossily
/// (#1840).
/// Test: `worktree_remove_command_strips_repository_redirecting_env`,
/// `worktree_remove_command_ends_in_the_target_path`.
pub(crate) fn worktree_remove_command(repo_root: &Path, path: &Path) -> Command {
    let mut cmd = git_command(repo_root, &["worktree", "remove", "--force"]);
    cmd.arg(path);
    cmd
}

/// Decide whether a reclaim candidate may be removed, and say so out loud.
///
/// Why: the reclaim path has to make this decision TWICE — once while
/// classifying candidates and again immediately before each removal (#4118
/// TOCTOU) — and `prune.rs` sits against its 500-SLOC cap. Both call sites also
/// have to log identically, or a reader of the daemon log cannot tell which
/// pass spared a worktree.
/// What: `Some(dirt)` means "do not remove this, and record it"; `None` means
/// "proceed". Under [`DirtyWorktreePolicy::ForceDiscard`] a dirty candidate
/// returns `None` after a `warn!` naming exactly what is about to be destroyed.
/// Test: `prune_orphaned_worktrees_rechecks_dirt_immediately_before_removal`.
pub(crate) fn dirt_blocks_removal(
    candidate: &Path,
    policy: DirtyWorktreePolicy,
    phase: &'static str,
) -> Option<DirtyWorktree> {
    let dirt = inspect_dirt(candidate)?;
    if policy == DirtyWorktreePolicy::Skip {
        tracing::warn!(
            path = %candidate.display(), reason = %dirt.reason, phase,
            "prune-worktrees: candidate holds unsaved work — refusing to remove it; \
             re-run with an explicit discard opt-in only if the work is genuinely \
             disposable (#4091)"
        );
        return Some(dirt);
    }
    tracing::warn!(
        path = %candidate.display(), reason = %dirt.reason, phase,
        "prune-worktrees: DISCARDING unsaved work — explicit force-discard opt-in \
         was supplied (#4091)"
    );
    None
}

/// Best-effort cross-check: does `git worktree list` on the checkout owning
/// `candidate` agree that `candidate` is a real, currently-registered git
/// worktree (#3649)?
///
/// Why: the sentinel + store-ownerless checks establish WHO owned this
/// directory and whether that owner is provably gone, but neither confirms
/// git's OWN bookkeeping still recognises the path as a worktree at all — a
/// belt-and-suspenders safety net against deleting a directory that merely
/// LOOKS like a worktree (e.g. its git worktree entry was already pruned by
/// something else, or the shape matched by coincidence). A disagreement is
/// treated conservatively: skip rather than delete.
/// What (rebuilt git-native, #4207): asks the CANDIDATE'S OWN repository —
/// `git -C <candidate> worktree list --porcelain` — rather than a repository
/// guessed from the candidate's grandparent directory. The grandparent rule
/// was an inference, not a fact, and it was wrong in both directions: for
/// `<repo>/.claude/worktrees/<name>` it named `<repo>/.claude`, which is not a
/// repository at all, and for the fourteen worktrees living under
/// `<repo>/.base/.worktrees/` but REGISTERED to the parent repo `<repo>` it
/// named `.base`, which correctly disowned them — so this check returned
/// `false` for every one of them and the sweep skipped them conservatively,
/// forever. Git resolves the owning repository from the candidate itself, so
/// the answer no longer depends on where the worktree is parked.
///
/// Returns `true` (agree — deletion may proceed, subject to the caller's other
/// checks) when the probe cannot be answered at all: this check is an
/// ADDITIONAL safety net on top of the sentinel/store checks, not a
/// replacement for them, so a missing `git` binary or a transient failure
/// never blocks a deletion those checks already approved. It is emphatically
/// NOT the fail-safe gate; that role belongs to [`inspect_dirt`], which fails
/// toward DIRTY. Returns `true` only when `candidate`'s canonicalized path
/// appears among the registered worktrees.
/// Test: `git_worktree_list_agrees_true_for_real_worktree`,
///       `git_worktree_list_agrees_false_for_untracked_dir`,
///       `git_worktree_list_agrees_true_for_worktree_registered_to_parent_repo`
///       (#4207 — fails against the grandparent rule).
pub(crate) fn git_worktree_list_agrees(candidate: &Path) -> bool {
    let Some(worktrees) = super::worktree_registry::list_registered_worktrees(candidate) else {
        return true; // best-effort: an unanswerable probe must never block a delete
    };
    let canonical_candidate =
        std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    worktrees
        .into_iter()
        .any(|wt| std::fs::canonicalize(&wt.path).unwrap_or(wt.path) == canonical_candidate)
}

#[cfg(test)]
#[path = "worktree_safety_tests.rs"]
mod worktree_safety_tests;
