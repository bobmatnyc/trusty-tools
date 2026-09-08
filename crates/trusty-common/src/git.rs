//! The workspace's single entry point for constructing a `git` subprocess — #7171.
//!
//! Why: 41 detached `git maintenance run --auto` repacks hit the shared 21 GB
//! `.git` from ~25 worktrees in one incident (load 141) because every one of
//! the ~90 production `Command::new("git")` sites across trusty-common and
//! trusty-mpm ran with git's own auto-maintenance heuristics live. Git decides
//! whether to run background maintenance/gc **after any command that touches
//! the object store** (`fetch`, `commit`, `checkout`, …), so a fleet of
//! worktrees each running ordinary git commands independently triggers
//! independent maintenance runs against the ONE shared object store they all
//! point at (`.git/worktrees/*` share one `objects/` directory). This module
//! is the common-entry-point rule (`CLAUDE.md`, "Common entry point, clean
//! domain demarcation") applied to git spawns: every caller that wants a `git`
//! subprocess gets one through here, so the fix lands once instead of at each
//! of the ~90 sites.
//! What: [`command`] and [`command_in`] build a `std::process::Command`
//! (async callers: [`tokio_command`] / [`tokio_command_in`] build a
//! `tokio::process::Command`) that always carries
//! [`MAINTENANCE_DISABLE_ARGS`] as the FIRST argv entries. `-c key=value` is
//! passed on argv rather than written to a config file, so it outranks repo
//! config, global config, AND system config — a worktree cannot re-enable
//! maintenance for itself even if something writes `maintenance.auto = true`
//! into the shared `.git/config`, and no per-repo provisioning step is needed
//! for a git command that already runs through this module. Provisioning
//! (writing `maintenance.auto=false` / `gc.auto=0` into a managed checkout's
//! `.git/config`) is the separate, complementary fix in
//! `trusty-mpm::core::git_maintenance` for the ambient case — an operator
//! running git directly in a worktree, outside anything that calls this
//! module.
//! Test: `command_disables_maintenance_and_gc`,
//! `command_in_adds_dash_c_before_the_directory`,
//! `tokio_command_disables_maintenance_and_gc` (feature `unconditional-only`).

use std::path::Path;
use std::process::Command;

/// `-c` argv pairs that disable git's automatic background maintenance and
/// gc for the single invocation they are attached to (#7171).
///
/// Why: `git -c key=value` wins over every config file (repo, global,
/// system), so passing these on argv is the only form that cannot be
/// silently overridden by config a worktree does not control.
/// What: `maintenance.auto=false` stops git from scheduling a background
/// `git maintenance run --auto` after this command; `gc.auto=0` stops the
/// older, still-live `git gc --auto` heuristic some git subcommands check
/// independently of the maintenance scheduler. Both are pinned — one alone
/// leaves the other heuristic live.
/// Test: `command_disables_maintenance_and_gc`.
pub const MAINTENANCE_DISABLE_ARGS: &[&str] = &["-c", "maintenance.auto=false", "-c", "gc.auto=0"];

/// Build a bare `git` [`std::process::Command`] with automatic
/// maintenance/gc disabled.
///
/// Why: the single place every synchronous git spawn in the workspace
/// should originate from (#7171) — see the module doc.
/// What: `git -c maintenance.auto=false -c gc.auto=0`, with no subcommand or
/// `-C` yet — callers append their own args (and `-C`/`current_dir` where a
/// target directory applies) exactly as they would on a plain
/// `Command::new("git")`.
/// Test: `command_disables_maintenance_and_gc`.
pub fn command() -> Command {
    let mut cmd = Command::new("git");
    cmd.args(MAINTENANCE_DISABLE_ARGS);
    cmd
}

/// [`command`] plus `-C <dir>`, for the common case of a git invocation
/// scoped to one repository or worktree.
///
/// Why: nearly every call site immediately does `.arg("-C").arg(dir)`;
/// folding it in here removes one more place a future site could copy a bare
/// `Command::new("git")` instead of this module's entry point.
/// What: `git -c maintenance.auto=false -c gc.auto=0 -C <dir>`. `-C` is a
/// top-level git option, so its position relative to the `-c` pairs above
/// does not matter — both must (and here do) precede the subcommand a caller
/// appends next.
/// Test: `command_in_adds_dash_c_before_the_directory`.
pub fn command_in(dir: &Path) -> Command {
    let mut cmd = command();
    cmd.arg("-C").arg(dir);
    cmd
}

/// [`command`], as a `tokio::process::Command` for an async call site.
///
/// Why: some daemon call sites already run git under `tokio::process` (the
/// blocking cost of `std::process::Command::output()` inside an async
/// handler is what they avoid); those sites need the same maintenance-free
/// argv without going through `spawn_blocking` just to reach [`command`].
/// What: `git -c maintenance.auto=false -c gc.auto=0`, built on
/// `tokio::process::Command` — `tokio`'s `process` feature is already an
/// unconditional dependency of this crate.
/// Test: `tokio_command_disables_maintenance_and_gc`.
pub fn tokio_command() -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(MAINTENANCE_DISABLE_ARGS);
    cmd
}

/// [`tokio_command`] plus `-C <dir>` — the async counterpart to [`command_in`].
///
/// Test: `tokio_command_in_adds_dash_c_before_the_directory`.
pub fn tokio_command_in(dir: &Path) -> tokio::process::Command {
    let mut cmd = tokio_command();
    cmd.arg("-C").arg(dir);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn tokio_argv(cmd: &tokio::process::Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn command_disables_maintenance_and_gc() {
        let cmd = command();
        assert_eq!(cmd.get_program(), "git");
        let args = argv(&cmd);
        assert_eq!(
            args,
            vec!["-c", "maintenance.auto=false", "-c", "gc.auto=0"]
        );
    }

    #[test]
    fn command_in_adds_dash_c_before_the_directory() {
        let cmd = command_in(Path::new("/tmp/example-repo"));
        let args = argv(&cmd);
        assert_eq!(
            args,
            vec![
                "-c",
                "maintenance.auto=false",
                "-c",
                "gc.auto=0",
                "-C",
                "/tmp/example-repo",
            ]
        );
    }

    #[test]
    fn tokio_command_disables_maintenance_and_gc() {
        let cmd = tokio_command();
        assert_eq!(cmd.as_std().get_program(), "git");
        let args = tokio_argv(&cmd);
        assert_eq!(
            args,
            vec!["-c", "maintenance.auto=false", "-c", "gc.auto=0"]
        );
    }

    #[test]
    fn tokio_command_in_adds_dash_c_before_the_directory() {
        let cmd = tokio_command_in(Path::new("/tmp/example-repo"));
        let args = tokio_argv(&cmd);
        assert_eq!(
            args,
            vec![
                "-c",
                "maintenance.auto=false",
                "-c",
                "gc.auto=0",
                "-C",
                "/tmp/example-repo",
            ]
        );
    }

    #[test]
    fn callers_can_append_args_after_command_in() {
        let mut cmd = command_in(Path::new("/tmp/example-repo"));
        cmd.args(["status", "--porcelain"]);
        let args = argv(&cmd);
        assert_eq!(
            args,
            vec![
                "-c",
                "maintenance.auto=false",
                "-c",
                "gc.auto=0",
                "-C",
                "/tmp/example-repo",
                "status",
                "--porcelain",
            ]
        );
    }
}
