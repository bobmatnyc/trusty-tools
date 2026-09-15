//! Startup hygiene for managed base clones (#1709).
//!
//! Why: the protected base clone used by the in-project spawn path (#1706) must
//! always be on the default branch and up to date with the remote when the
//! daemon starts. Without hygiene, a stale or diverged base clone silently
//! yields sessions on an outdated branch, confusing agents. Running a short
//! fetch + fast-forward sequence at daemon startup keeps every base clone
//! current. Dead worktrees from previous sessions are also pruned to prevent
//! the base clone's worktree list from growing indefinitely.
//!
//! CRITICAL SAFETY INVARIANT (#2177, #4961): the hygiene sweep must NEVER
//! discard local work of any kind. The update step is a NON-DESTRUCTIVE
//! fast-forward (`git merge --ff-only`), never `git reset --hard`, and it is
//! preceded by three gates: the checked-out branch must be the default branch
//! (so the safety checks validate the very ref being moved), it must have zero
//! unpushed commits and a clean working tree, and no path the update would
//! write may already exist untracked on disk.
//!
//! That last gate is what #4961 added, and it is not redundant with the dirty
//! check. `git status --porcelain` does not report gitignored paths at all, so
//! a gitignored file holding real user content reads as "clean" — and both
//! `git reset --hard` AND `git merge --ff-only` silently overwrite it when the
//! target commit tracks the same path. A user pointing Obsidian, Cursor, or
//! any editor at a managed checkout creates exactly that shape. Adding
//! `--ignored` to the dirty check is NOT the fix: `target/` is gitignored and
//! present in any built checkout, so the gate would always see dirt and
//! hygiene would become a permanent no-op.
//!
//! What: [`get_default_branch`] reads the origin/HEAD symref;
//! [`run_hygiene_for_base`] runs fetch, a gated fast-forward, and worktree
//! prune for one base clone directory; [`run_hygiene_for_all_bases`] walks
//! `<repos_root>/<owner>/<repo>/` and calls the per-base function for each;
//! wired into daemon startup via
//! [`super::inproject_hygiene_sweep::spawn_if_enabled`]. A per-repo opt-out
//! marker file ([`HYGIENE_OPT_OUT_MARKER`]) disables the sweep for a single
//! checkout.
//!
//! # Bounded, throttled and skippable (#7965)
//!
//! Every git spawn goes through [`git_bounded`], which is the ONLY place this
//! module builds a `Command`: it carries `GIT_TERMINAL_PROMPT=0` and a
//! [`GIT_TIMEOUT`] ceiling, because a `git fetch` waiting on a credential prompt
//! nobody can answer is what `sample` caught pinning a blocking-pool thread. On
//! top of that, [`fetch_is_due`] skips the expensive step for a base fetched
//! within [`FETCH_MIN_INTERVAL`], [`PASS_BUDGET`] bounds the whole walk, and a
//! short pause between bases keeps the sweep from occupying every core
//! continuously. The daemon's request path and this sweep share one resource —
//! this process's CPU and its share of the host's IO — and on a host with 72 base
//! clones an unbounded pass held it for over nine minutes.
//! Test: unit coverage in `inproject_hygiene_tests.rs`; `hygiene_*` integration
//! tests in `crates/trusty-mpm/tests/inproject_hygiene_test.rs` exercise real
//! temp git repos for the ahead/dirty/clean/gitignored/off-default/opt-out
//! cases.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use tracing::{info, warn};

// #7965: `GIT_TIMEOUT` moved to `bounded_proc` so the orphan-GC worktree sweep
// shares this sweep's ceiling. A base that hits it is abandoned and logged.
use crate::core::bounded_proc::{BoundedError, BoundedOutput, GIT_TIMEOUT, run_bounded};

/// Wall-clock ceiling for one whole sweep over every base clone (#7965).
///
/// Why: the per-command ceiling bounds one child, not the pass. The reporting
/// host has 72 base clones; at ~10 commands each, a pathological pass could still
/// occupy the maintenance lane for over an hour. Hygiene is a freshness chore —
/// skipping the tail of a pass leaves some base clones one boot staler and
/// discards nothing — so bounding the pass is free, and it is what keeps the
/// sweep from becoming a permanent tax on the request path.
/// Test: `hygiene_pass_budget_stops_the_sweep`.
pub(crate) const PASS_BUDGET: Duration = Duration::from_secs(180);

// The pointer above and the one on `run_hygiene_for_all_bases_within` name the
// same test deliberately: the constant is only meaningful through the walk that
// spends it.

/// Skip a base clone's fetch when it was already fetched this recently (#7965).
///
/// Why: the fetch is the expensive step, and the sweep runs once per daemon
/// boot. On a host that restarts the daemon a few times an hour, 72 network
/// fetches per restart is the bulk of the load this issue is about, and almost
/// all of it re-fetches what the previous pass already has.
/// What: read from `.git/FETCH_HEAD`'s mtime — git writes it on every fetch, so
/// no new state is persisted for this. An unreadable mtime FETCHES, which is the
/// pre-#7965 behaviour.
/// Test: `hygiene_skips_a_recently_fetched_base`, `fetch_is_due_without_a_fetch_head`.
pub(crate) const FETCH_MIN_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Pause between base clones so the sweep leaves the machine some air (#7965).
///
/// Why: the sweep is a serial loop, but `git fetch` spawns its own multi-threaded
/// `index-pack`, so back-to-back bases keep every core busy continuously. A short
/// gap per base costs ~14 s over 72 bases and gives the request path a scheduling
/// window it did not have.
const BASE_PAUSE: Duration = Duration::from_millis(200);

/// Run one `git` command against a base clone, bounded and non-interactive.
///
/// Why: the single place this sweep spawns git, so the #7965 bound and the
/// credential-prompt refusal cannot be forgotten by a call site. `git fetch` with
/// no terminal-prompt guard blocks forever on a repository whose credentials have
/// expired — git waits on a prompt nobody can answer, which is exactly the
/// unbounded stdout read `sample` caught.
/// What: `trusty_common::git::command()` (so #7171's maintenance/gc suppression
/// still applies) plus `-C <base_path>`, `GIT_TERMINAL_PROMPT=0` and an ssh
/// transport in batch mode, run through [`run_bounded`] with [`GIT_TIMEOUT`].
/// `Err` carries a one-line reason for a spawn failure, a timeout, or a wait
/// failure; a non-zero EXIT is `Ok` and left for the caller to judge.
/// Test: `a_wedged_hygiene_fetch_neither_hangs_the_sweep_nor_delays_health`.
fn git_bounded(base_path: &Path, args: &[&str], budget: Duration) -> Result<BoundedOutput, String> {
    let mut cmd = trusty_common::git::command();
    cmd.arg("-C")
        .arg(base_path)
        .args(args)
        // #7965: never wait on a human. Both forms are needed — the first stops
        // git's own prompt, the second stops ssh's.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    run_bounded(cmd, budget).map_err(|e| match e {
        BoundedError::TimedOut => format!(
            "git {args:?} did not answer within {}s and its process group was killed (#7965)",
            budget.as_secs()
        ),
        other => format!("git {args:?} {other}"),
    })
}

/// Marker file that opts a single base clone out of the startup hygiene sweep.
///
/// Why: before #4961 the only control was the process-wide
/// `TRUSTY_MPM_INPROJECT_HYGIENE` env var — all-or-nothing across every managed
/// checkout. A user who actively edits one checkout in an external editor had
/// no way to exempt just that one without disabling hygiene everywhere.
/// What: an empty file named `.trusty-mpm-no-hygiene` in the base clone root.
/// Its mere presence skips every step of the sweep for that checkout.
/// Test: `hygiene_opt_out_marker_skips_update` (integration),
/// `hygiene_opt_out_marker_detected` (unit).
pub const HYGIENE_OPT_OUT_MARKER: &str = ".trusty-mpm-no-hygiene";

/// Decision on whether the fast-forward update step may proceed.
///
/// Why: the update decision has several independent inputs (checked-out branch
/// identity, ahead-count, working-tree cleanliness, detached/unknown states)
/// that are easiest to reason about — and unit-test — as a small pure function
/// separated from the git-shelling plumbing that gathers those inputs.
/// What: two variants — `Update` (safe to fast-forward the base clone to
/// origin) and `Skip(reason)` (refuse, carrying a human-readable reason for
/// the warning log).
/// Test: `decide_update_*` unit tests in `inproject_hygiene_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum UpdateDecision {
    Update,
    Skip(String),
}

/// Decide whether the fast-forward may proceed, given branch, ahead and dirty state.
///
/// Why: centralizes the data-loss-prevention rule in one pure, testable place
/// rather than scattering conditionals through the git-shelling code. Any
/// input that could not be determined (detached HEAD, no upstream, a failed
/// `git` invocation) is treated conservatively as "do not update".
///
/// The branch gate closes #4961's second finding: the ahead-count is measured
/// as `origin/<checked-out>..<checked-out>`, but the update targets
/// `origin/<default>`. Off the default branch those are different refs, so the
/// safety check could pass against one ref while the operation moved the local
/// branch to another. Requiring `checked_out == default_branch` makes the
/// check validate exactly the ref being moved.
/// What: `checked_out: None` (detached HEAD) yields `Skip`. A `checked_out`
/// that differs from `default_branch` yields `Skip`. `ahead_count: None`
/// (unknown — no upstream, or a `rev-list` failure) yields `Skip`. `dirty:
/// None` (a `status` failure) yields `Skip`. `ahead_count: Some(n) if n > 0`
/// yields `Skip` (unpushed commits). `dirty: Some(true)` yields `Skip`. Only a
/// checked-out default branch with `Some(0)` ahead and `Some(false)` dirty
/// yields `Update`.
/// Test: `decide_update_ahead_skips`, `decide_update_dirty_skips`,
/// `decide_update_unknown_ahead_skips`, `decide_update_unknown_dirty_skips`,
/// `decide_update_detached_head_skips`, `decide_update_non_default_branch_skips`,
/// `decide_update_clean_and_even_updates`.
fn decide_update(
    checked_out: Option<&str>,
    default_branch: &str,
    ahead_count: Option<usize>,
    dirty: Option<bool>,
) -> UpdateDecision {
    let Some(branch) = checked_out else {
        return UpdateDecision::Skip(
            "HEAD is detached (no branch checked out); refusing to move any ref".to_string(),
        );
    };
    // #4961: the ahead-count below is measured against `origin/<branch>`, so it
    // only proves anything when `branch` is the ref the update actually moves.
    if branch != default_branch {
        return UpdateDecision::Skip(format!(
            "checked-out branch `{branch}` is not the default branch `{default_branch}`; \
             refusing to move it to a different ref"
        ));
    }
    let Some(ahead) = ahead_count else {
        return UpdateDecision::Skip(
            "branch ahead-count unknown (no upstream, or git error); \
             refusing to discard local work"
                .to_string(),
        );
    };
    let Some(is_dirty) = dirty else {
        return UpdateDecision::Skip(
            "working tree status unknown (git error); refusing to update".to_string(),
        );
    };
    if ahead > 0 {
        return UpdateDecision::Skip(format!(
            "{ahead} commit(s) ahead of origin (unpushed); refusing to discard local work"
        ));
    }
    if is_dirty {
        return UpdateDecision::Skip("uncommitted changes present; refusing to update".to_string());
    }
    UpdateDecision::Update
}

/// Read the short name of the currently checked-out branch, if any.
///
/// Why: the ahead-count and branch-identity checks need to know which branch is
/// actually checked out in the base clone (not merely the repo's configured
/// default branch) — updating a detached HEAD or misidentifying the branch
/// could silently move the wrong ref.
/// What: runs `git -C <base_path> symbolic-ref --short HEAD`; returns `None`
/// on any failure, including detached HEAD (where `symbolic-ref` exits
/// non-zero because `HEAD` does not point at a branch).
/// Test: `hygiene_non_default_branch_is_not_updated` (integration) drives a
/// real non-default checkout through this function; the `None` case is modelled
/// by `decide_update_detached_head_skips`.
fn current_branch(base_path: &Path, budget: Duration) -> Option<String> {
    let out = git_bounded(base_path, &["symbolic-ref", "--short", "HEAD"], budget).ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = out.stdout.trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

/// Count commits on `branch` that are not yet on `origin/<branch>`.
///
/// Why: this is the direct measure of "would an update to origin discard
/// committed work" — the data-loss bug #2177 fixed.
/// What: runs `git -C <base_path> rev-list --count origin/<branch>..<branch>`
/// and parses the count. Returns `None` if the command fails (e.g. no
/// `origin/<branch>` upstream exists) or the output does not parse as a
/// number — both are treated as "unknown" by [`decide_update`], which refuses
/// to proceed on unknown input.
/// Test: `hygiene_ahead_branch_is_not_reset` integration test (via
/// [`run_hygiene_for_base`]).
fn ahead_count(base_path: &Path, branch: &str, budget: Duration) -> Option<usize> {
    let range = format!("origin/{branch}..{branch}");
    let out = git_bounded(base_path, &["rev-list", "--count", &range], budget).ok()?;
    if !out.status.success() {
        return None;
    }
    out.stdout.trim().parse().ok()
}

/// Determine whether the working tree has uncommitted changes to TRACKED files.
///
/// Why: an update must not clobber uncommitted modifications; this is one half
/// of the data-loss guard. It is deliberately NOT the whole guard — see
/// [`colliding_untracked_paths`] for the gitignored-content half (#4961).
/// What: runs `git -C <base_path> status --porcelain`; `Some(true)` if any
/// output is produced (dirty), `Some(false)` if output is empty (clean),
/// `None` if the command itself fails. Note that `--porcelain` without
/// `--ignored` reports nothing for gitignored paths, which is why it cannot be
/// the only guard.
/// Test: `hygiene_dirty_tree_is_not_reset` integration test (via
/// [`run_hygiene_for_base`]).
fn is_dirty(base_path: &Path, budget: Duration) -> Option<bool> {
    porcelain_status_within(base_path, budget)
        .ok()
        .map(|e| !e.is_empty())
}

/// Read `git status --porcelain` for a base clone as a list of entries.
///
/// Why: two callers need the same answer at different resolutions. [`is_dirty`]
/// wants the verdict; the cold-start reuse gate
/// ([`super::inproject_cold_start`]) wants the entries themselves, because its
/// error message names what is dirty. Sharing one reader keeps them from
/// drifting into two different definitions of "dirty" for the same directory.
/// What: runs `git -C <base_path> status --porcelain` and returns the non-empty
/// stdout lines verbatim. `Err` carries the spawn or non-zero-exit failure —
/// callers decide whether that is fail-safe (`is_dirty` → `None` → refuse the
/// update) or fatal (cold start → stop). Note that `--porcelain` without
/// `--ignored` reports nothing for gitignored paths, which is why this cannot
/// be the only data-loss guard; see [`colliding_untracked_paths`].
/// Test: `hygiene_dirty_tree_is_not_reset` (via [`run_hygiene_for_base`]);
/// `dirty_existing_checkout_warns_and_proceeds` (via the cold-start gate).
pub(crate) fn porcelain_status(base_path: &Path) -> Result<Vec<String>, String> {
    porcelain_status_within(base_path, GIT_TIMEOUT)
}

/// [`porcelain_status`] with the per-command ceiling as a parameter (#7965).
///
/// Why: see [`run_hygiene_for_base_within`] — a test proving the bound fires
/// must not wait out [`GIT_TIMEOUT`].
/// Test: as [`porcelain_status`].
pub(crate) fn porcelain_status_within(
    base_path: &Path,
    budget: Duration,
) -> Result<Vec<String>, String> {
    let out = git_bounded(base_path, &["status", "--porcelain"], budget)?;
    if !out.status.success() {
        return Err(format!(
            "git status --porcelain failed ({}): {}",
            out.status,
            out.stderr.trim()
        ));
    }
    Ok(out
        .stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect())
}

/// Run `git -C <base_path> <args>` and return NUL-separated stdout entries.
///
/// Why: both halves of the collision check parse `-z` git output the same way;
/// a shared helper keeps the parsing (and the "any failure is fatal to the
/// check" contract) in one place.
/// What: spawns git, returns `Err` with a description on spawn failure or a
/// non-zero exit, otherwise splits stdout on NUL and drops empty entries.
/// Test: exercised through `colliding_untracked_paths` by
/// `hygiene_gitignored_file_is_not_clobbered`.
fn git_z_lines(base_path: &Path, args: &[&str], budget: Duration) -> Result<Vec<String>, String> {
    let out = git_bounded(base_path, args, budget)?;
    if !out.status.success() {
        return Err(format!(
            "git {args:?} failed ({}): {}",
            out.status,
            out.stderr.trim()
        ));
    }
    Ok(out
        .stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect())
}

/// Paths the pending update would write that already exist untracked on disk.
///
/// Why: this is the #4961 fix. Git itself will NOT protect these files. A
/// gitignored path is invisible to `git status --porcelain`, and both `git
/// reset --hard` and `git merge --ff-only` silently overwrite an ignored file
/// when the target commit tracks the same path (`--overwrite-ignore` is git's
/// default and `merge` offers no way to turn it off). Reproduced with a
/// gitignored `notes.md` holding real content: destroyed, no warning, no
/// recovery path. Detecting the collision ourselves and refusing is the only
/// way to make the update genuinely non-destructive.
/// What: takes the paths the update would touch (`git diff --name-only HEAD
/// <target>`), subtracts everything currently tracked (`git ls-files`), and
/// returns those of the remainder that exist on disk. A path deleted by the
/// target is tracked at HEAD, so it is never flagged. `symlink_metadata` is
/// used so a broken symlink still counts as present. Any git failure returns
/// `Err`, which the caller treats as "refuse".
/// Test: `hygiene_gitignored_file_is_not_clobbered` (integration).
fn colliding_untracked_paths(
    base_path: &Path,
    target: &str,
    budget: Duration,
) -> Result<Vec<String>, String> {
    let incoming = git_z_lines(
        base_path,
        &["diff", "--name-only", "-z", "HEAD", target],
        budget,
    )?;
    if incoming.is_empty() {
        return Ok(Vec::new());
    }
    let tracked: HashSet<String> = git_z_lines(base_path, &["ls-files", "-z"], budget)?
        .into_iter()
        .collect();

    Ok(incoming
        .into_iter()
        .filter(|p| !tracked.contains(p) && base_path.join(p).symlink_metadata().is_ok())
        .collect())
}

/// Best-effort write of a recovery ref pointing at the pre-update HEAD.
///
/// Why: defense-in-depth — even when every gate correctly clears an update to
/// proceed, leaving a cheap breadcrumb to the prior HEAD costs nothing and
/// gives a manual recovery path (`git reset --hard
/// refs/trusty-mpm/pre-hygiene/<branch>`) if some unanticipated case still
/// loses committed work. It is structurally incapable of restoring
/// never-committed working-tree content, which is why the gates — not this ref
/// — are the actual guarantee. This must never abort the sweep: any failure
/// here is logged and swallowed, matching the file's existing "every step
/// logged, no step is fatal" pattern.
/// What: resolves the current HEAD sha via `git rev-parse HEAD`, then runs
/// `git update-ref refs/trusty-mpm/pre-hygiene/<branch> <sha>`. Both steps are
/// best-effort; failures are logged via `warn!` and otherwise ignored.
/// Test: `hygiene_recovery_ref_written_before_update` integration test (via
/// [`run_hygiene_for_base`]).
fn write_recovery_ref(base_path: &Path, branch: &str, budget: Duration) {
    let sha = match git_bounded(base_path, &["rev-parse", "HEAD"], budget) {
        Ok(out) if out.status.success() => out.stdout.trim().to_string(),
        Ok(out) => {
            warn!(
                path = %base_path.display(),
                "inproject-hygiene: recovery-ref rev-parse failed ({}): {}",
                out.status,
                out.stderr.trim()
            );
            return;
        }
        Err(e) => {
            warn!(path = %base_path.display(), "inproject-hygiene: recovery-ref rev-parse error: {e}");
            return;
        }
    };
    if sha.is_empty() {
        return;
    }

    let refname = format!("refs/trusty-mpm/pre-hygiene/{branch}");
    match git_bounded(base_path, &["update-ref", &refname, &sha], budget) {
        Ok(out) if out.status.success() => {
            info!(path = %base_path.display(), refname = %refname, sha = %sha, "inproject-hygiene: recovery ref written");
        }
        Ok(out) => {
            warn!(
                path = %base_path.display(),
                "inproject-hygiene: recovery-ref update-ref failed ({}): {}",
                out.status,
                out.stderr.trim()
            );
        }
        Err(e) => {
            warn!(path = %base_path.display(), "inproject-hygiene: recovery-ref update-ref error: {e}");
        }
    }
}

/// Read the default branch for a base clone by inspecting `origin/HEAD`.
///
/// Why: the update step needs the default branch name; reading the symref
/// is more reliable than hardcoding `main` across diverse repositories.
/// What: runs `git -C <base_path> symbolic-ref --short refs/remotes/origin/HEAD`
/// and returns the short branch name (e.g. `main`) on success, or `None` if git
/// fails or there is no `origin/HEAD` symref (the caller falls back to `main`).
/// Test: `get_default_branch_returns_none_for_non_git` (unit).
pub fn get_default_branch(base_path: &Path) -> Option<String> {
    get_default_branch_within(base_path, GIT_TIMEOUT)
}

/// [`get_default_branch`] with the per-command ceiling as a parameter (#7965).
///
/// Test: as [`get_default_branch`].
fn get_default_branch_within(base_path: &Path, budget: Duration) -> Option<String> {
    let out = git_bounded(
        base_path,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        budget,
    )
    .ok()?;

    if !out.status.success() {
        return None;
    }
    let branch = out.stdout.trim().to_string();
    // symbolic-ref returns e.g. "origin/main"; strip the "origin/" prefix.
    let branch = branch
        .strip_prefix("origin/")
        .unwrap_or(&branch)
        .to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

/// Fetch, then fast-forward the base clone to origin if every gate clears.
///
/// Why: split out of [`run_hygiene_for_base`] so the update step's four gates
/// read as one sequence rather than being buried between the fetch and prune
/// steps. Returns nothing: like every other step in the sweep, a refusal or a
/// git failure is logged and never propagated.
/// What: resolves the default branch, the checked-out branch, its ahead-count
/// and dirty state, and feeds them to [`decide_update`]. On `Update` it then
/// runs the [`colliding_untracked_paths`] check and refuses if any path the
/// update would write already exists untracked on disk (#4961). Only when that
/// too is clear does it write a recovery ref and run `git merge --ff-only
/// origin/<default>` — which is itself non-destructive and fails cleanly
/// rather than overwriting when it cannot fast-forward.
/// Test: `hygiene_gitignored_file_is_not_clobbered`,
/// `hygiene_non_default_branch_is_not_updated`, `hygiene_ahead_branch_is_not_reset`,
/// `hygiene_dirty_tree_is_not_reset`, `hygiene_clean_branch_is_fast_forwarded`.
fn update_to_origin(base_path: &Path, budget: Duration) {
    let default_branch =
        get_default_branch_within(base_path, budget).unwrap_or_else(|| "main".to_string());
    let checked_out = current_branch(base_path, budget);
    let ahead = checked_out
        .as_deref()
        .and_then(|b| ahead_count(base_path, b, budget));
    let dirty = is_dirty(base_path, budget);

    if let UpdateDecision::Skip(reason) =
        decide_update(checked_out.as_deref(), &default_branch, ahead, dirty)
    {
        warn!(
            path = %base_path.display(),
            branch = %checked_out.as_deref().unwrap_or("<unknown/detached>"),
            "inproject-hygiene: SKIP update — {reason}"
        );
        return;
    }

    let target = format!("origin/{default_branch}");

    // #4961: gitignored working-tree content is invisible to `git status
    // --porcelain`, and git overwrites it without complaint. Refuse instead.
    match colliding_untracked_paths(base_path, &target, budget) {
        Ok(collisions) if !collisions.is_empty() => {
            warn!(
                path = %base_path.display(),
                paths = %collisions.join(", "),
                "inproject-hygiene: SKIP update — {} untracked path(s) on disk would be \
                 overwritten by the update; refusing to discard them",
                collisions.len()
            );
            return;
        }
        Ok(_) => {}
        Err(e) => {
            warn!(path = %base_path.display(), "inproject-hygiene: SKIP update — collision check failed: {e}");
            return;
        }
    }

    if let Some(branch) = checked_out.as_deref() {
        write_recovery_ref(base_path, branch, budget);
    }

    match git_bounded(base_path, &["merge", "--ff-only", &target], budget) {
        Ok(out) if out.status.success() => {
            info!(path = %base_path.display(), branch = %default_branch, "inproject-hygiene: fast-forward OK");
        }
        Ok(out) => {
            warn!(
                path = %base_path.display(),
                "inproject-hygiene: fast-forward declined ({}): {}",
                out.status,
                out.stderr.trim()
            );
        }
        Err(e) => {
            warn!(path = %base_path.display(), "inproject-hygiene: fast-forward error: {e}");
        }
    }
}

/// Whether this base clone is due a fetch, given how long ago the last one was.
///
/// Why: see [`FETCH_MIN_INTERVAL`] — the fetch is the expensive step and the
/// sweep runs once per boot, so on a host that restarts the daemon often the same
/// 72 network fetches repeat for nothing.
/// What: `.git/FETCH_HEAD`'s mtime, which git rewrites on every fetch. Due when
/// that file is missing, its mtime is unreadable, or it is older than
/// `min_interval`. Every failure path is DUE — the pre-#7965 behaviour — so an
/// unreadable clock can only cost a redundant fetch, never a stale base clone.
/// Test: `fetch_is_due_without_a_fetch_head`,
/// `fetch_is_due_on_both_sides_of_the_interval`,
/// `fetch_is_due_when_the_mtime_is_in_the_future`,
/// `hygiene_skips_a_recently_fetched_base`.
fn fetch_is_due(base_path: &Path, min_interval: Duration) -> bool {
    let Ok(meta) = std::fs::metadata(base_path.join(".git").join("FETCH_HEAD")) else {
        return true;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age >= min_interval)
        // #7965: `duration_since` errors when `modified` is in the FUTURE — a
        // clock skewed backwards, or an NFS mtime from a faster host. Fetch.
        .unwrap_or(true)
}

/// Run hygiene for a single base clone: fetch, gated fast-forward, prune worktrees.
///
/// Why: each step targets a distinct failure mode — fetch syncs the remote object
/// store; the fast-forward (when safe) brings the checkout up to the remote
/// default branch; worktree prune cleans up working-tree entries for worktrees
/// whose paths no longer exist (left behind by decommissioned sessions). All
/// steps are non-fatal: a failure is logged as a warning and the next step
/// still runs, so a transient git error does not prevent the other steps from
/// running. Critically (#2177, #4961), the update step can never discard local
/// work — see [`update_to_origin`].
/// What: delegates to [`run_hygiene_for_base_within`] with the production
/// [`GIT_TIMEOUT`]; it is the only caller that passes that constant.
/// Test: `run_hygiene_skips_missing_dir` (unit — directory absent → early
/// return); `hygiene_opt_out_marker_skips_update`,
/// `hygiene_ahead_branch_is_not_reset`, `hygiene_dirty_tree_is_not_reset`,
/// `hygiene_clean_branch_is_fast_forwarded`, `hygiene_gitignored_file_is_not_clobbered`,
/// `hygiene_non_default_branch_is_not_updated`,
/// `hygiene_recovery_ref_written_before_update` (integration, real temp git repos).
pub fn run_hygiene_for_base(base_path: &Path) -> Result<(), String> {
    run_hygiene_for_base_within(base_path, GIT_TIMEOUT)
}

/// [`run_hygiene_for_base`] with the per-command ceiling as a parameter (#7965).
///
/// Why a parameter: the regression test for the bound drives a `git` that never
/// answers, and at [`GIT_TIMEOUT`] one base would cost minutes of test time.
/// What: returns early when `.git` is absent, or when the
/// [`HYGIENE_OPT_OUT_MARKER`] file is present (a per-repo opt-out; every step is
/// skipped so an opted-out checkout is left entirely alone). Otherwise: (1) `git
/// fetch origin`, unless [`fetch_is_due`] says a recent enough one already
/// happened; (2) [`update_to_origin`]; (3) `git worktree prune`. Every git spawn
/// goes through [`git_bounded`], so a child that never answers is killed at
/// `budget` rather than waited on.
/// Test: `a_wedged_hygiene_fetch_neither_hangs_the_sweep_nor_delays_health`
/// (the bound), `hygiene_skips_a_recently_fetched_base` (the fetch gate).
pub(crate) fn run_hygiene_for_base_within(
    base_path: &Path,
    budget: Duration,
) -> Result<(), String> {
    if !base_path.join(".git").exists() {
        return Ok(());
    }
    // #4961: per-repo opt-out for a checkout the user actively edits.
    if base_path.join(HYGIENE_OPT_OUT_MARKER).exists() {
        info!(path = %base_path.display(), marker = %HYGIENE_OPT_OUT_MARKER, "inproject-hygiene: opted out, skipping");
        return Ok(());
    }

    info!(path = %base_path.display(), "inproject-hygiene: running for base clone");

    // Step 1: fetch from origin, unless a recent enough one already happened.
    if !fetch_is_due(base_path, FETCH_MIN_INTERVAL) {
        info!(path = %base_path.display(), "inproject-hygiene: fetch skipped — fetched recently (#7965)");
    } else {
        match git_bounded(base_path, &["fetch", "origin"], budget) {
            Ok(out) if out.status.success() => {
                info!(path = %base_path.display(), "inproject-hygiene: fetch OK");
            }
            Ok(out) => {
                warn!(
                    path = %base_path.display(),
                    "inproject-hygiene: fetch failed ({}): {}",
                    out.status,
                    out.stderr.trim()
                );
            }
            Err(e) => {
                warn!(path = %base_path.display(), "inproject-hygiene: fetch error: {e}");
            }
        }
    }

    // Step 2: non-destructive, gated fast-forward to the default branch.
    update_to_origin(base_path, budget);

    // Step 3: prune stale worktrees.
    match git_bounded(base_path, &["worktree", "prune"], budget) {
        Ok(out) if out.status.success() => {
            info!(path = %base_path.display(), "inproject-hygiene: worktree prune OK");
        }
        Ok(out) => {
            warn!(
                path = %base_path.display(),
                "inproject-hygiene: worktree prune failed ({}): {}",
                out.status,
                out.stderr.trim()
            );
        }
        Err(e) => {
            warn!(path = %base_path.display(), "inproject-hygiene: worktree prune error: {e}");
        }
    }

    Ok(())
}

/// Run hygiene for every base clone under `repos_root`.
///
/// Why: at daemon startup all managed base clones should be freshened in one
/// pass so sessions spawned shortly after startup always see a current default
/// branch. Walking the two-level `<owner>/<repo>` layout avoids hardcoding
/// specific project paths.
/// What: enumerates `<repos_root>/<owner>/` directories, then for each
/// `<owner>/<repo>` subdirectory calls [`run_hygiene_for_base`]. Non-git
/// directories are silently skipped. All errors are logged as warnings; no
/// single repo failure prevents the rest from being processed.
/// Test: `run_hygiene_for_all_bases_skips_missing_root` (unit).
pub fn run_hygiene_for_all_bases(repos_root: &Path) {
    let _skipped = run_hygiene_for_all_bases_within(repos_root, PASS_BUDGET, GIT_TIMEOUT);
}

/// [`run_hygiene_for_all_bases`] with the pass budget as a parameter (#7965).
///
/// Why a parameter: a test must be able to prove the budget STOPS the walk
/// without waiting out [`PASS_BUDGET`], and resolving it internally would make
/// that test three minutes long.
/// What: identical to the public entry point except the deadline. Bases reached
/// after it are skipped; nothing is removed or modified for them, so an abandoned
/// tail is only staleness. Returns how many were skipped — the same number the
/// `warn!` line carries, so a test can assert the abandonment itself rather than
/// infer it from wall-clock time.
/// Test: `hygiene_pass_budget_stops_the_sweep`.
pub(crate) fn run_hygiene_for_all_bases_within(
    repos_root: &Path,
    budget: Duration,
    git_timeout: Duration,
) -> usize {
    if !repos_root.is_dir() {
        return 0;
    }

    info!(root = %repos_root.display(), "inproject-hygiene: starting startup sweep");
    let deadline = Instant::now() + budget;
    let mut skipped = 0usize;

    let owner_dirs = match std::fs::read_dir(repos_root) {
        Ok(d) => d,
        Err(e) => {
            warn!(root = %repos_root.display(), "inproject-hygiene: cannot read repos root: {e}");
            return 0;
        }
    };

    for owner_entry in owner_dirs.flatten() {
        let owner_path = owner_entry.path();
        if !owner_path.is_dir() {
            continue;
        }
        let repo_dirs = match std::fs::read_dir(&owner_path) {
            Ok(d) => d,
            Err(e) => {
                warn!(path = %owner_path.display(), "inproject-hygiene: cannot read owner dir: {e}");
                continue;
            }
        };
        for repo_entry in repo_dirs.flatten() {
            let base_path = repo_entry.path();
            if !base_path.is_dir() {
                continue;
            }
            // #7965: the pass budget, checked per base rather than per command —
            // a base that has started is finished, so the sweep never leaves one
            // half-updated.
            if Instant::now() >= deadline {
                skipped += 1;
                continue;
            }
            if let Err(e) = run_hygiene_for_base_within(&base_path, git_timeout) {
                warn!(path = %base_path.display(), "inproject-hygiene: error: {e}");
            }
            // #7965: see `BASE_PAUSE` — a scheduling window for the request path.
            std::thread::sleep(BASE_PAUSE);
        }
    }

    if skipped > 0 {
        warn!(
            root = %repos_root.display(),
            skipped,
            "inproject-hygiene: pass budget of {}s expired; {skipped} base clone(s) left for the \
             next boot (#7965)",
            budget.as_secs()
        );
    }
    info!(root = %repos_root.display(), "inproject-hygiene: startup sweep complete");
    skipped
}

#[cfg(test)]
#[path = "inproject_hygiene_tests.rs"]
mod inproject_hygiene_tests;
