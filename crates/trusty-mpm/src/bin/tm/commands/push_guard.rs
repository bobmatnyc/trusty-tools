//! `tm repair push-guard` — retrofit the #2867 cross-branch push guard (#2867).
//!
//! Why: the guard installs itself only when trusty-mpm clones a repository, so
//! a base clone provisioned before it shipped stays unprotected forever. The
//! populations that most need it are precisely the old, long-lived clones with
//! many worktrees — the shape #2867 happened in. `tm doctor`'s `push_guard`
//! check makes the gap visible; this command closes it.
//! What: a local, daemon-free operation over
//! [`trusty_mpm::core::push_guard`]. It writes into `$GIT_COMMON_DIR/hooks`,
//! which every linked and ad-hoc worktree of the clone shares, so ONE
//! invocation from ANY worktree protects all of them with no git config write.
//! Idempotent, and refuses rather than overwriting a hook it does not own.
//! Test: `crates/trusty-mpm/src/bin/tm/commands/push_guard_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_mpm::core::push_guard::{
    GuardState, HookInstall, effective_hooks_dir, inspect_pre_push_guard, install_pre_push_guard,
};

/// Run `tm repair push-guard`.
///
/// Why: `main` stays a one-line delegation (that file sits at the 500-SLOC
/// cap), and the outcome is unit-testable without capturing stdout.
/// What: resolves the target repo (defaulting to cwd), then either reports
/// (`dry_run`) or installs. `Ok(())` for installed / already-current / dry
/// run; `Err` for a refusal, an unresolvable path, or an I/O failure — a
/// refusal means the clone is still UNPROTECTED and must not read as success
/// to a script.
/// Test: `installs_into_a_fresh_repo_then_is_idempotent`, `dry_run_writes_nothing`,
/// `refusal_exits_nonzero_and_leaves_the_foreign_hook_intact`,
/// `unresolvable_path_exits_nonzero`.
pub(crate) fn repair_push_guard(path: Option<String>, dry_run: bool) -> anyhow::Result<()> {
    let target = resolve_target(path.as_deref()).map_err(|e| anyhow::anyhow!(e))?;

    println!("Repository: {}", target.display());
    match effective_hooks_dir(&target) {
        Ok(dir) => println!(
            "Hooks directory: {} (shared by {} worktree(s) of this clone)",
            dir.display(),
            worktree_count(&target)
        ),
        Err(reason) => println!("Hooks directory: unresolved — {reason}"),
    }

    if dry_run {
        report_dry_run(&target);
        return Ok(());
    }

    match install_pre_push_guard(&target).map_err(|e| anyhow::anyhow!(e))? {
        HookInstall::Installed(p) => {
            println!("INSTALLED   cross-branch push guard → {}", p.display());
            println!(
                "Every worktree of this clone is now covered. A deliberate cross-branch push \
                 still works with TM_ALLOW_CROSS_BRANCH_PUSH=1."
            );
            Ok(())
        }
        HookInstall::AlreadyCurrent(p) => {
            println!("UP TO DATE  guard already current at {}", p.display());
            Ok(())
        }
        HookInstall::Refused(reason) => Err(anyhow::anyhow!(
            "REFUSED — nothing was written: {reason}. trusty-mpm never overwrites a pre-push \
             hook it does not own; resolve the conflict by hand (e.g. chain the guard from \
             your own hook), then re-run."
        )),
    }
}

/// Print what a real run would do, without writing anything.
///
/// Why: an operator retrofitting a clone shared by live agent sessions wants
/// to see the verdict before mutating a file every one of them executes.
/// What: maps the read-only [`inspect_pre_push_guard`] states to the same
/// vocabulary the real run prints. Never fails — a dry run reports, it does
/// not adjudicate.
/// Test: `dry_run_writes_nothing`.
fn report_dry_run(target: &Path) {
    match inspect_pre_push_guard(target) {
        GuardState::Missing(p) => println!("WOULD INSTALL   {} (no hook present)", p.display()),
        GuardState::Outdated(p) => {
            println!("WOULD UPGRADE   {} (older guard revision)", p.display());
        }
        GuardState::Current(p) => println!("WOULD SKIP      {} (already current)", p.display()),
        GuardState::Foreign(reason) => println!("WOULD REFUSE    {reason}"),
    }
    println!("(dry run — nothing was written)");
}

/// Resolve the repository to operate on from an optional `--path`.
///
/// Why: `--path` may name any worktree; git's own `--show-toplevel` turns it
/// into the worktree root, and `effective_hooks_dir` then resolves the SHARED
/// common dir from there. Failing loudly on a non-repo beats silently writing
/// a hook nobody will ever execute.
/// What: `Err` with an actionable message when the path is missing or is not
/// inside a git working tree.
/// Test: `unresolvable_path_exits_nonzero`.
fn resolve_target(path: Option<&str>) -> Result<PathBuf, String> {
    let start = match path {
        Some(p) => PathBuf::from(p),
        None => {
            std::env::current_dir().map_err(|e| format!("cannot read current directory: {e}"))?
        }
    };
    if !start.exists() {
        return Err(format!("{} does not exist", start.display()));
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&start)
        .args(["rev-parse", "--path-format=absolute", "--show-toplevel"])
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{} is not inside a git working tree",
            start.display()
        ));
    }
    let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if top.is_empty() {
        return Err(format!(
            "git could not resolve a working tree for {}",
            start.display()
        ));
    }
    Ok(PathBuf::from(top))
}

/// How many worktrees share this clone's hooks directory.
///
/// Why: the single most useful fact for an operator deciding whether to
/// retrofit — "this writes one file that 95 worktrees will execute" is the
/// blast radius, and it is invisible otherwise.
/// What: counts `worktree ` records from `git worktree list --porcelain`;
/// falls back to `"?"` rather than failing the command over a cosmetic count.
/// Test: `worktree_count_counts_linked_worktrees`.
fn worktree_count(repo: &Path) -> String {
    let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
    else {
        return "?".to_string();
    };
    if !out.status.success() {
        return "?".to_string();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("worktree "))
        .count()
        .to_string()
}

#[cfg(test)]
#[path = "push_guard_tests.rs"]
mod tests;
