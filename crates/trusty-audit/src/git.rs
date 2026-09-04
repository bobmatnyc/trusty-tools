//! The one `git` child this crate spawns (#6079).
//!
//! Why: [`crate::local_repo`] already spelled a hardened `git` constructor —
//! ambient repository variables removed, terminal prompting off — privately, for
//! the clone and validation half. #6079's churn collector needs a `git log` run
//! under the identical hygiene, and CLAUDE.md's common-entry-point rule makes a
//! second `Command::new("git")` carrying its own copy of that environment list a
//! defect. One list, two constructors, both callers routed through them.
//!
//! An inherited `GIT_DIR` or `GIT_WORK_TREE` points a child at whatever the
//! parent shell was pointed at, which for an agent-run or hook-run invocation is
//! somebody else's repository — a churn measurement taken there is silently
//! about the wrong codebase. `GIT_TERMINAL_PROMPT=0` keeps a source that
//! unexpectedly wants credentials from hanging an unattended sweep.
//!
//! Nothing here decides WHAT to run. Every subcommand this crate passes through
//! it is read-only against the repository named (`rev-parse`, `log`) or writes
//! only under the working directory (`clone`), and that invariant stays with the
//! caller that knows its own subcommand.
//!
//! Test: `crate::grounding::churn::churn_tests::a_fixture_repository_ranks_its_hotspots`,
//! `crate::local_repo::local_repo_tests::the_source_checkout_is_byte_identical_afterwards`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The executable every constructor here runs.
pub const BINARY: &str = "git";

/// Repository-selecting variables an inherited environment must not supply.
///
/// Stated once because a copy of this list is exactly what drifts: a caller that
/// clears three of the four still runs against the parent's repository whenever
/// the fourth is set.
const AMBIENT: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// Where `git` is on this machine, or the one line saying it is not.
///
/// # Errors
/// One line, safe to show the recipient, when no `git` is on the resolver's
/// search path. The caller turns it into a gap of its own.
pub fn resolve() -> Result<PathBuf, String> {
    trusty_common::bin_resolve::resolve_binary(BINARY)
        .ok_or_else(|| format!("`{BINARY}` is not installed"))
}

/// A hardened synchronous `git -C <repo> <args…>`, ready to `output()`.
///
/// `-C` rather than `current_dir`: it is what git documents for running against
/// another checkout, and it keeps this process's own working directory out of
/// the child's resolution entirely.
#[must_use]
pub fn at(binary: &Path, repo: &Path, args: &[&str]) -> std::process::Command {
    let mut command = std::process::Command::new(binary);
    command.arg("-C").arg(repo).args(args);
    harden_sync(&mut command);
    command
}

/// A hardened asynchronous `git <argv…>`.
///
/// The argv is taken whole rather than as `(repo, args)` because the clone half
/// runs `git clone <source> <staging>`, which names two paths and no `-C`.
#[must_use]
pub fn spawn(argv: Vec<OsString>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(BINARY);
    command.args(argv);
    for var in AMBIENT {
        command.env_remove(var);
    }
    command.env("GIT_TERMINAL_PROMPT", "0");
    command
}

/// [`AMBIENT`] cleared and prompting disabled on a synchronous child.
fn harden_sync(command: &mut std::process::Command) {
    for var in AMBIENT {
        command.env_remove(var);
    }
    command.env("GIT_TERMINAL_PROMPT", "0");
}
