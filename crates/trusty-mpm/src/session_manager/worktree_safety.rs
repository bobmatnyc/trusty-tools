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
//! module never reasons from "is it gitignored". It asks four questions:
//!
//! 1. **Does git report anything?** — [`STATUS_ARGS`], with every
//!    config knob that could silence it pinned on the command line.
//! 2. **Is anything committed but not on a remote?** — on `HEAD` *and* on the
//!    `session/<leaf>` branch that `decommission` force-deletes.
//! 3. **Is there a nested repository holding unsaved work?** — enumerated
//!    exactly, never inferred: registered worktrees from
//!    `git worktree list --porcelain`, plus a bounded walk of gitignored
//!    subtrees for unregistered clones. Each nested root is then put through
//!    questions 1, 2 and 4 in its own right.
//! 4. **Is there anything under `.trusty-mpm/` that is not one of two
//!    disposable artefacts?** — accounted per-file by pathspec, covering the
//!    gitignored and untracked spellings alike.
//!
//! # What this rule still CANNOT see (residual risk)
//!
//! - **Gitignored loose files outside `.trusty-mpm/`** that are not part of a
//!   nested repository — a `.env.local`, a `.mcp.json.bak`, a scratch note in
//!   an ignored directory. Deliberately not counted: on this repo's own 31
//!   session worktrees, treating every non-disposable ignored entry as dirt
//!   would flag `.claude/` in 30 of 31 and disable reclamation outright, which
//!   is the failure mode of a guard nobody can run.
//! - **Anything under a [`DISPOSABLE_DIR_NAMES`] directory**, including a
//!   nested repository inside `target/` or `node_modules/`.
//! - **`.git/info/exclude`**, which is per-repo and on-disk and cannot be
//!   overridden from the command line the way `core.excludesFile` can.
//! - **`git stash` entries** — the stash is repo-level, not per-worktree.
//! - **Symlinked subtrees** — not followed, because `remove_dir_all` deletes
//!   the link and not its target, so no work is at risk there.
//! - **A repository nested inside a gitignored subtree OF a nested repository**
//!   — the scan stops descending at the first `.git` it finds.
//! - **`DirtyWorktreePolicy::ForceDiscard`**, which destroys all of the above
//!   by explicit operator opt-in.
//!
//! Test: `worktree_safety_tests`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Directory names whose contents are regenerable build/cache output (#4118).
///
/// Why: the nested-repository scan ([`nested_repo_roots`]) has to walk
/// gitignored subtrees, and those are dominated by exactly two things —
/// enormous disposable build output, and the occasional nested checkout that
/// holds real work. Walking `target/` on 95 candidates would cost minutes and
/// find nothing; skipping it by NAME is the difference between a scan that runs
/// and one that gets disabled. These names are regenerable by definition: no
/// tool writes work-of-record into them, and losing them costs only CPU.
/// What: matched against a directory's own file name at any depth. A nested
/// repository underneath one of these is NOT seen — see the module-level
/// residual-risk note.
/// Test: `inspect_dirt_does_not_scan_disposable_build_dirs`.
const DISPOSABLE_DIR_NAMES: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".parcel-cache",
    ".cache",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".venv",
    "venv",
    ".gradle",
    "coverage",
    ".terraform",
];

/// Directory entries the gitignored-subtree scan may visit per candidate.
///
/// Why: an unbounded walk of an arbitrary ignored tree is a denial-of-service
/// against the sweep. Measured against this repo's own 31 session worktrees, a
/// FULL-tree walk with [`DISPOSABLE_DIR_NAMES`] pruning visited 5.6k entries per
/// worktree on average; the ignored-only subset is far smaller, so 50k is two
/// orders of magnitude of headroom.
/// What: exceeding the budget is an ERROR, which the caller turns into DIRTY —
/// "I could not finish looking" is never "there is nothing there".
const IGNORED_SCAN_ENTRY_BUDGET: usize = 50_000;

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
const GIT_ENV_REDIRECTS: &[&str] = &[
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

/// Collapsed listing of gitignored entries, used to bound the nested-repo scan.
const IGNORED_STATUS_ARGS: &[&str] = &[
    "status",
    "--porcelain",
    "--untracked-files=normal",
    "--ignored",
    "--ignore-submodules=none",
];

/// What the reclaim path does when a candidate holds unsaved work (#4091).
///
/// Why: the default must be impossible to trigger accidentally — an ordinary
/// `/tm-session-pause` (which prunes by DEFAULT, `prune_worktrees` unwrapping
/// to `true`) must never be able to discard uncommitted work. Modelling the
/// override as its own two-variant enum rather than a `force: bool` keeps it
/// from being positionally confused with the `dry_run: bool` that already sits
/// next to it in [`super::prune::SessionManager::prune_orphaned_worktrees`]'s
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
    fn new(path: &Path, reason: impl Into<String>, dirty_files: usize, unpushed: usize) -> Self {
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
/// 3. [`nested_dirt`] finds a nested repository holding unsaved work,
///    regardless of gitignore;
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
fn inspect_dirt_at(path: &Path, scan_nested: bool) -> Option<DirtyWorktree> {
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
        return nested_dirt(path);
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
fn count_dirty_files(path: &Path) -> Result<usize, String> {
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
/// What: 0 when the directory does not exist; otherwise one count per reported
/// entry that is not on the allowlist. A quoted path is an ERROR, not a
/// silently-unmatched one: if the guard cannot spell the path it cannot decide
/// the path is disposable.
/// Test: `inspect_dirt_counts_wip_backup_under_trusty_mpm`,
/// `inspect_dirt_counts_pause_snapshots_under_trusty_mpm`.
fn trusty_mpm_dirt(root: &Path) -> Result<usize, String> {
    if !root.join(TRUSTY_MPM_DIR).exists() {
        return Ok(0);
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

/// Does a NESTED repository under this candidate hold unsaved work (#4118)?
///
/// Why: this is the hole that made the whole guard bypassable. Deleting the
/// candidate deletes everything inside it, but `git status` on the candidate
/// says nothing about a nested checkout — `.claude/worktrees/` is gitignored on
/// this repo's `main` (`.gitignore:40`), so seven registered agent worktrees
/// inside `.base/.worktrees/2eb72dca-…` produced `dirty_files = 0`. Worse, the
/// #3649 ownership gate deliberately REFUSES to delete those nested worktrees
/// directly (no sentinel ⇒ owner-unknown), so the sweep would have honoured
/// that refusal and then deleted the directory they live in — bypassing its own
/// guard by removing the parent.
/// What: every nested root from [`nested_repo_roots`] is put through
/// [`inspect_dirt_at`] with the nested scan off. A nested root that is dirty OR
/// unassessable makes the PARENT dirty; one that is provably clean and pushed
/// does not, so a session that merely once spawned an agent worktree stays
/// reclaimable. The reported counts are the nested root's own, unmodified.
/// Test: `inspect_dirt_reports_nested_gitignored_worktree`,
/// `inspect_dirt_reports_unregistered_nested_repo_in_ignored_dir`,
/// `inspect_dirt_allows_clean_nested_worktree`.
fn nested_dirt(candidate: &Path) -> Option<DirtyWorktree> {
    let roots = match nested_repo_roots(candidate) {
        Ok(r) => r,
        Err(e) => {
            return Some(DirtyWorktree::new(
                candidate,
                format!("nested-repository scan failed: {e}"),
                0,
                0,
            ));
        }
    };
    for root in roots {
        // A clean, pushed nested root does not pin the parent — keep looking.
        let Some(inner) = inspect_dirt_at(&root, false) else {
            continue;
        };
        let shown = root
            .strip_prefix(candidate)
            .unwrap_or(&root)
            .display()
            .to_string();
        return Some(DirtyWorktree::new(
            candidate,
            format!(
                "nested git worktree/repository `{shown}` holds unsaved work \
                 that `git status` on this directory cannot see: {}",
                inner.reason
            ),
            inner.dirty_files,
            inner.unpushed_commits,
        ));
    }
    None
}

/// Enumerate every nested repository root strictly beneath `candidate`.
///
/// Why: "does this directory contain a checkout" must be answered EXACTLY, not
/// heuristically, because the answer decides whether gigabytes get deleted. Two
/// disjoint populations exist and neither subsumes the other: worktrees
/// REGISTERED with the enclosing repository (the `.claude/worktrees/…` shape,
/// free to enumerate — git already knows) and independent clones that git has
/// never heard of (only findable on disk).
/// What: the union of (a) `worktree list --porcelain` entries that are strict
/// descendants of `candidate` — exact, at any depth, and immune to gitignore
/// because git's own bookkeeping is the source — and (b) a bounded walk of the
/// gitignored subtrees `git status --ignored` reports, looking for a `.git`
/// entry. Untracked-but-not-ignored nested repos need no walk at all: they
/// already surface as `??` lines in [`count_dirty_files`].
/// Test: `inspect_dirt_reports_nested_gitignored_worktree`,
/// `inspect_dirt_reports_unregistered_nested_repo_in_ignored_dir`.
fn nested_repo_roots(candidate: &Path) -> Result<Vec<PathBuf>, String> {
    let canonical =
        std::fs::canonicalize(candidate).map_err(|e| format!("candidate is unreadable: {e}"))?;
    let mut roots: BTreeSet<PathBuf> = BTreeSet::new();

    for line in git_stdout(candidate, &["worktree", "list", "--porcelain"])?.lines() {
        let Some(raw) = line.strip_prefix("worktree ") else {
            continue;
        };
        let listed = PathBuf::from(raw.trim());
        let listed = std::fs::canonicalize(&listed).unwrap_or(listed);
        if listed != canonical && listed.starts_with(&canonical) {
            roots.insert(listed);
        }
    }

    let mut budget = IGNORED_SCAN_ENTRY_BUDGET;
    for line in git_stdout(candidate, IGNORED_STATUS_ARGS)?.lines() {
        let Some(raw) = line.strip_prefix("!! ") else {
            continue;
        };
        let entry = raw.trim();
        if entry.starts_with('"') {
            return Err(format!(
                "ignored entry `{entry}` has a quoted path this scan cannot interpret"
            ));
        }
        scan_for_repos(
            &canonical.join(entry.trim_end_matches('/')),
            &mut roots,
            &mut budget,
        )?;
    }
    Ok(roots.into_iter().collect())
}

/// Walk `root`, collecting directories that contain a `.git` entry.
///
/// Why: an unregistered clone inside a gitignored directory is invisible to
/// every git question asked of the CANDIDATE, so the only way to find it is to
/// look. Iterative rather than recursive so a pathologically deep tree cannot
/// overflow the stack, and symlinks are never followed — `remove_dir_all`
/// deletes a link, not its target, so nothing beyond one is at risk.
/// What: pushes each directory holding a `.git` entry into `out` and stops
/// descending there. Skips [`DISPOSABLE_DIR_NAMES`] by name at every level.
/// Exhausting `budget`, or any unreadable entry, is an `Err` the caller turns
/// into DIRTY.
/// Test: `inspect_dirt_reports_unregistered_nested_repo_in_ignored_dir`,
/// `inspect_dirt_does_not_scan_disposable_build_dirs`.
fn scan_for_repos(
    root: &Path,
    out: &mut BTreeSet<PathBuf>,
    budget: &mut usize,
) -> Result<(), String> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        match std::fs::symlink_metadata(&dir) {
            // A plain file, or a symlink of any kind: nothing to descend into.
            Ok(meta) if !meta.is_dir() => continue,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("`{}` is unreadable: {e}", dir.display())),
        }
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if DISPOSABLE_DIR_NAMES.contains(&name) {
            continue;
        }
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("`{}` is unreadable: {e}", dir.display()))?;
        let mut children = Vec::new();
        let mut is_repo = false;
        for entry in entries {
            let entry =
                entry.map_err(|e| format!("an entry of `{}` is unreadable: {e}", dir.display()))?;
            if *budget == 0 {
                return Err(format!(
                    "gitignored-subtree scan exceeded its {IGNORED_SCAN_ENTRY_BUDGET}-entry \
                     budget at `{}`",
                    dir.display()
                ));
            }
            *budget -= 1;
            if entry.file_name() == ".git" {
                is_repo = true;
                break;
            }
            children.push(entry.path());
        }
        if is_repo {
            out.insert(dir);
            continue;
        }
        stack.extend(children);
    }
    Ok(())
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
/// Test: `inspect_dirt_reports_unpushed_commit`,
/// `inspect_dirt_clean_pushed_worktree_is_none`.
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

    let raw = match upstream {
        Some(up) => git_stdout(path, &["rev-list", "--count", &format!("{up}..HEAD")])?,
        None => git_stdout(path, &["rev-list", "--count", "HEAD", "--not", "--remotes"])?,
    };
    let head = raw
        .trim()
        .parse::<usize>()
        .map_err(|e| format!("unparsable rev-list count `{}`: {e}", raw.trim()))?;
    Ok(head + count_session_branch_unpushed(path)?)
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
/// What: 0 when no such branch exists (`for-each-ref` exits 0 with empty output,
/// so absence is distinguishable from a spawn failure, which stays an `Err`).
/// Otherwise the commits on that branch reachable from no remote AND not
/// already counted via `HEAD` — excluding `HEAD` is what stops the common
/// "branch IS the checked-out branch" case from being counted twice.
/// Test: `inspect_dirt_reports_unpushed_session_branch_when_head_moved`,
/// `inspect_dirt_does_not_double_count_the_checked_out_session_branch`.
fn count_session_branch_unpushed(path: &Path) -> Result<usize, String> {
    let Some(leaf) = path.file_name().and_then(|n| n.to_str()) else {
        return Ok(0);
    };
    let refname = format!("refs/heads/session/{leaf}");
    let listed = git_stdout(path, &["for-each-ref", "--format=%(refname)", &refname])?;
    if listed.trim().is_empty() {
        return Ok(0);
    }
    let raw = git_stdout(
        path,
        &[
            "rev-list",
            "--count",
            &refname,
            "--not",
            "--remotes",
            "HEAD",
        ],
    )?;
    raw.trim()
        .parse::<usize>()
        .map_err(|e| format!("unparsable rev-list count `{}`: {e}", raw.trim()))
}

/// Run `git -C <dir> <args>` and return stdout, or an error string.
///
/// Why: every check in this module needs the same "ran, exited zero, gave me
/// stdout" contract, and every deviation from it must surface as an `Err` the
/// caller turns into DIRTY rather than being swallowed.
/// What: non-zero exit and spawn failure both become `Err` carrying the
/// command and git's own stderr; stdout is lossily decoded.
/// Test: `inspect_dirt_treats_missing_path_as_dirty`.
fn git_stdout(dir: &Path, args: &[&str]) -> Result<String, String> {
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
fn git_command(dir: &Path, args: &[&str]) -> Command {
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
/// What: runs `git -C <repo_root> worktree list --porcelain`, where
/// `repo_root` is `candidate`'s grandparent directory — the SAME derivation
/// `decommission::remove_session_worktree` uses, which works identically for
/// both worktree-store shapes (`<repo>/.worktrees/<name>` and
/// `<repo>/.base/.worktrees/<id>`, since either way the grandparent of the
/// worktree leaf is the git checkout root). Returns `true` (agree — deletion
/// may proceed, subject to the caller's other checks) when the git command
/// cannot be run or fails outright — this check is an ADDITIONAL safety net
/// on top of the sentinel/store checks, not a replacement for them, so a
/// missing `git` binary or a transient failure never blocks a deletion those
/// checks already approved. It is emphatically NOT the fail-safe gate; that
/// role belongs to [`inspect_dirt`], which fails toward DIRTY. Returns `true`
/// only when `candidate`'s canonicalized path appears among the porcelain
/// output's `worktree <path>` lines.
/// Test: `git_worktree_list_agrees_true_for_real_worktree`,
///       `git_worktree_list_agrees_false_for_untracked_dir`.
pub(crate) fn git_worktree_list_agrees(candidate: &Path) -> bool {
    let Some(repo_root) = candidate.parent().and_then(|p| p.parent()) else {
        return true;
    };
    let out = git_command(repo_root, &["worktree", "list", "--porcelain"]).output();
    let Ok(out) = out else {
        return true; // best-effort: git unavailable must never block a delete
    };
    if !out.status.success() {
        return true;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let canonical_candidate =
        std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    stdout
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|p| {
            let pb = PathBuf::from(p);
            std::fs::canonicalize(&pb).unwrap_or(pb) == canonical_candidate
        })
}

#[cfg(test)]
#[path = "worktree_safety_tests.rs"]
mod worktree_safety_tests;
