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
//! wired into daemon startup via [`super::super::serve_http`] (the main
//! `serve_http` function in daemon/mod.rs). A per-repo opt-out marker file
//! ([`HYGIENE_OPT_OUT_MARKER`]) disables the sweep for a single checkout.
//! Test: unit coverage in `inproject_hygiene_tests.rs`; `hygiene_*` integration
//! tests in `crates/trusty-mpm/tests/inproject_hygiene_test.rs` exercise real
//! temp git repos for the ahead/dirty/clean/gitignored/off-default/opt-out
//! cases.

use std::collections::HashSet;
use std::path::Path;

use tracing::{info, warn};

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
fn current_branch(base_path: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
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
fn ahead_count(base_path: &Path, branch: &str) -> Option<usize> {
    let range = format!("origin/{branch}..{branch}");
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["rev-list", "--count", &range])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
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
fn is_dirty(base_path: &Path) -> Option<bool> {
    porcelain_status(base_path).ok().map(|e| !e.is_empty())
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
/// `dirty_existing_checkout_fails_loud` (via the cold-start gate).
pub(crate) fn porcelain_status(base_path: &Path) -> Result<Vec<String>, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["status", "--porcelain"])
        .output()
        .map_err(|e| format!("git status --porcelain failed to spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git status --porcelain failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
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
fn git_z_lines(base_path: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(args)
        .output()
        .map_err(|e| format!("git {args:?} error: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {args:?} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
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
fn colliding_untracked_paths(base_path: &Path, target: &str) -> Result<Vec<String>, String> {
    let incoming = git_z_lines(base_path, &["diff", "--name-only", "-z", "HEAD", target])?;
    if incoming.is_empty() {
        return Ok(Vec::new());
    }
    let tracked: HashSet<String> = git_z_lines(base_path, &["ls-files", "-z"])?
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
fn write_recovery_ref(base_path: &Path, branch: &str) {
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["rev-parse", "HEAD"])
        .output();
    let sha = match head {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Ok(out) => {
            warn!(
                path = %base_path.display(),
                "inproject-hygiene: recovery-ref rev-parse failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
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
    let update = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["update-ref", &refname, &sha])
        .output();
    match update {
        Ok(out) if out.status.success() => {
            info!(path = %base_path.display(), refname = %refname, sha = %sha, "inproject-hygiene: recovery ref written");
        }
        Ok(out) => {
            warn!(
                path = %base_path.display(),
                "inproject-hygiene: recovery-ref update-ref failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
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
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
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
fn update_to_origin(base_path: &Path) {
    let default_branch = get_default_branch(base_path).unwrap_or_else(|| "main".to_string());
    let checked_out = current_branch(base_path);
    let ahead = checked_out
        .as_deref()
        .and_then(|b| ahead_count(base_path, b));
    let dirty = is_dirty(base_path);

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
    match colliding_untracked_paths(base_path, &target) {
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
        write_recovery_ref(base_path, branch);
    }

    let merge = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["merge", "--ff-only", &target])
        .output();
    match merge {
        Ok(out) if out.status.success() => {
            info!(path = %base_path.display(), branch = %default_branch, "inproject-hygiene: fast-forward OK");
        }
        Ok(out) => {
            warn!(
                path = %base_path.display(),
                "inproject-hygiene: fast-forward declined ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => {
            warn!(path = %base_path.display(), "inproject-hygiene: fast-forward error: {e}");
        }
    }
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
/// What: returns early when `.git` is absent, or when the
/// [`HYGIENE_OPT_OUT_MARKER`] file is present (a per-repo opt-out; every step is
/// skipped so an opted-out checkout is left entirely alone). Otherwise: (1)
/// `git -C <base_path> fetch origin`; (2) [`update_to_origin`]; (3) `git -C
/// <base_path> worktree prune`.
/// Test: `run_hygiene_skips_missing_dir` (unit — directory absent → early
/// return); `hygiene_opt_out_marker_skips_update`,
/// `hygiene_ahead_branch_is_not_reset`, `hygiene_dirty_tree_is_not_reset`,
/// `hygiene_clean_branch_is_fast_forwarded`, `hygiene_gitignored_file_is_not_clobbered`,
/// `hygiene_non_default_branch_is_not_updated`,
/// `hygiene_recovery_ref_written_before_update` (integration, real temp git repos).
pub fn run_hygiene_for_base(base_path: &Path) -> Result<(), String> {
    if !base_path.join(".git").exists() {
        return Ok(());
    }
    // #4961: per-repo opt-out for a checkout the user actively edits.
    if base_path.join(HYGIENE_OPT_OUT_MARKER).exists() {
        info!(path = %base_path.display(), marker = %HYGIENE_OPT_OUT_MARKER, "inproject-hygiene: opted out, skipping");
        return Ok(());
    }

    info!(path = %base_path.display(), "inproject-hygiene: running for base clone");

    // Step 1: fetch from origin.
    let fetch = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["fetch", "origin"])
        .output();
    match fetch {
        Ok(out) if out.status.success() => {
            info!(path = %base_path.display(), "inproject-hygiene: fetch OK");
        }
        Ok(out) => {
            warn!(
                path = %base_path.display(),
                "inproject-hygiene: fetch failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => {
            warn!(path = %base_path.display(), "inproject-hygiene: fetch error: {e}");
        }
    }

    // Step 2: non-destructive, gated fast-forward to the default branch.
    update_to_origin(base_path);

    // Step 3: prune stale worktrees.
    let prune = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["worktree", "prune"])
        .output();
    match prune {
        Ok(out) if out.status.success() => {
            info!(path = %base_path.display(), "inproject-hygiene: worktree prune OK");
        }
        Ok(out) => {
            warn!(
                path = %base_path.display(),
                "inproject-hygiene: worktree prune failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
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
    if !repos_root.is_dir() {
        return;
    }

    info!(root = %repos_root.display(), "inproject-hygiene: starting startup sweep");

    let owner_dirs = match std::fs::read_dir(repos_root) {
        Ok(d) => d,
        Err(e) => {
            warn!(root = %repos_root.display(), "inproject-hygiene: cannot read repos root: {e}");
            return;
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
            if let Err(e) = run_hygiene_for_base(&base_path) {
                warn!(path = %base_path.display(), "inproject-hygiene: error: {e}");
            }
        }
    }

    info!(root = %repos_root.display(), "inproject-hygiene: startup sweep complete");
}

#[cfg(test)]
#[path = "inproject_hygiene_tests.rs"]
mod inproject_hygiene_tests;
