//! Is a live process standing in this directory? (#4311, DOC-66 §1.3)
//!
//! Why: every gate guarding a worktree removal today asks a REGISTRY — the
//! session store's `workspace_path`s, and the delegation tracker's registered
//! trees (`agent_worktree_reap::paths_in_use`). A process nothing registered is
//! invisible to all of them. On 2026-08-15 a `trusty-memory serve --foreground`
//! started by hand inside an agent worktree ran for a day; session_manager and
//! delegation tracking both had no record of it, so `git worktree remove
//! --force` would have deleted the directory out from under a running process
//! holding open descriptors in it. That gap was dormant while the reaper reaped
//! nothing; making agent worktrees attributable is what wakes it up, so the
//! check lands in the same change.
//!
//! What: [`process_holding`] asks the OS, not a registry — one `lsof` call
//! listing every process's current working directory, prefix-matched against
//! the candidate.
//!
//! # Fail direction
//!
//! Toward IN USE. `lsof` missing, unspawnable, stalled past its ceiling (#7540),
//! or returning output this cannot parse all resolve to "something may be in there", never to "free" — the
//! [ADR-0045](../../../../docs/adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md)
//! rule that an empty observation on a destructive path is UNDETERMINABLE and
//! not ABSENT. On a machine with no `lsof` this refuses every reap and says so,
//! which is the correct trade for a `git worktree remove --force`.
//!
//! # Stated gap: this misses the agent process itself
//!
//! State the limit at its real width, not its flattering one. This catches a
//! process that `cd`-ed into the tree. It does NOT reliably catch the agent the
//! worktree was granted to: measured on this machine, every running `claude`
//! process has its cwd at a project root and none inside `.claude/worktrees/`.
//! Coverage of a live agent is therefore incidental — it exists only where that
//! agent happened to `cd`. What this gate reliably catches is the class of
//! long-running command started BY an agent from inside its tree, which is the
//! 2026-08-15 `trusty-memory serve --foreground` shape.
//!
//! The agent process itself is covered by the registry instead:
//! `agent_worktree_reap::paths_in_use` reads every non-terminal delegation in
//! every session. The two are complementary, and neither alone is sufficient.
//!
//! `lsof +D <path>` would additionally catch a process holding a descriptor
//! inside the tree with its cwd elsewhere, at a cost that scales with the tree —
//! an agent worktree here carries a 1.5 GiB `target/`. Not paid for on every
//! reap, and not the fix for the gap above.
//! Test: the `#[cfg(test)]` suite in `worktree_liveness_tests.rs`.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

// #7540: the crate's kill-on-timeout runner; `git_ceiling` wraps it for git only.
use crate::core::bounded_proc::{BoundedError, run_bounded};

/// Name a live process whose working directory is inside `path`, or explain why
/// the question could not be answered.
///
/// Why: the one gate in the removal chain that consults the operating system
/// rather than a record trusty-mpm wrote itself, so a process nobody registered
/// still protects its directory.
/// What: `Some(reason)` means DO NOT REMOVE — either a process was found (the
/// reason names its pid, command and cwd) or the probe could not complete (the
/// reason says which step failed). `None` means the probe ran and found
/// nothing, which is the only result that permits removal.
///
/// `path` is canonicalized first so a symlinked candidate still matches the
/// resolved cwd `lsof` reports; a canonicalize failure is itself an
/// undeterminable answer, not a pass.
/// Test: `liveness_reports_a_process_standing_in_the_directory`,
/// `liveness_ignores_a_sibling_directory`,
/// `liveness_treats_a_missing_lsof_as_in_use`,
/// `liveness_kills_a_probe_that_outlives_its_ceiling`,
/// `liveness_treats_an_unparsable_probe_as_in_use`.
pub(crate) fn process_holding(path: &Path) -> Option<String> {
    let canonical = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) => {
            return Some(format!(
                "could not canonicalize {} to compare against live process cwds: {e}",
                path.display()
            ));
        }
    };
    match run_cwd_probe(lsof_command(), LSOF_TIMEOUT) {
        Ok(output) => scan_probe(&output, &canonical),
        Err(reason) => Some(reason),
    }
}

/// The probe binary. One literal so the doc, the error text and the call agree.
const LSOF: &str = "lsof";

/// The ceiling on one `lsof` listing (#7540 critic round).
///
/// Why: a healthy listing takes about a second; a stalled one (a hung NFS or
/// SMB mount) would otherwise hold the reclaim tick, and every later sweep,
/// forever. Fifteen healthy listings' worth is past any live answer.
const LSOF_TIMEOUT: Duration = Duration::from_secs(15);

/// The `lsof` invocation listing every process's current working directory.
///
/// Why: `-d cwd` restricts the descriptor set to the one entry per process this
/// check cares about, which is what keeps a system-wide listing to roughly a
/// thousand lines and about a second. `-w` suppresses the warnings `lsof` emits
/// for processes it may not examine — those are expected for another user's
/// processes and are not a probe failure. `-b` (#7540) avoids the kernel calls
/// that block on a hung mount; measured on macOS and on Linux `lsof` 4.99.4, it
/// still reports every cwd, this process's own included.
fn lsof_command() -> Command {
    let mut cmd = Command::new(LSOF);
    cmd.args(["-w", "-b", "-d", "cwd", "-F", "pcn"]);
    cmd
}

/// Run a cwd listing, killing it past `budget`.
///
/// Why: the command and the ceiling are parameters so the fail-toward-in-use
/// arms can be exercised against an absent binary and a hung one. Neither is
/// configuration: the one production call site passes [`lsof_command`] and
/// [`LSOF_TIMEOUT`].
/// What: runs through [`run_bounded`], which kills and reaps the child's whole
/// process group at the deadline (#7540). `Ok(stdout)` for a zero exit;
/// `Err(reason)` for a spawn failure, a timeout or a non-zero exit, each
/// carrying the text the caller reports as its refusal.
/// Test: `liveness_treats_a_missing_lsof_as_in_use`,
/// `liveness_kills_a_probe_that_outlives_its_ceiling`.
fn run_cwd_probe(cmd: Command, budget: Duration) -> Result<String, String> {
    let bin = cmd.get_program().to_string_lossy().into_owned();
    let out = run_bounded(cmd, budget).map_err(|e| match e {
        BoundedError::Spawn(e) => {
            format!("could not run `{bin}` to check for live processes: {e}")
        }
        BoundedError::TimedOut => format!(
            "`{bin}` timed out after {budget:?} while checking for live processes; its \
             process group was killed (#7540)"
        ),
        other => format!("`{bin}` failed while checking for live processes: {other}"),
    })?;
    if !out.status.success() {
        return Err(format!(
            "`{bin}` exited {} while checking for live processes: {}",
            out.status,
            out.stderr.trim()
        ));
    }
    Ok(out.stdout)
}

/// Find the first process in an `lsof -F pcn` listing whose cwd is under `root`.
///
/// Why: split from the spawn so the parser is testable against fixed text,
/// including the shapes that must NOT be read as "free".
/// What: `lsof`'s field output repeats `p<pid>`, `c<command>`, then `n<path>`;
/// the `p`/`c` values carry forward until the next process set. Returns
/// `Some(reason)` for the first match, and for any listing that fails the
/// SELF-VISIBILITY proof below.
///
/// # Why "the listing was non-empty" is not proof it saw anything
///
/// Without root, `lsof` reports only the caller's own processes and exits 0
/// regardless — measured on this machine, `lsof -w -d cwd -F pcn` returned 429
/// of 667 processes and ZERO of the 229 owned by other users. A flag that only
/// asks "did ANY `n` line appear" is satisfied by that partial listing, so
/// blindness to the very process that matters reads as a clean negative, which
/// is a PERMIT on a `git worktree remove --force`. That is #4470's
/// empty-`lsof`-is-not-`Free` defect in a second subsystem.
///
/// So the proof is positive and specific: the listing must contain THIS
/// process's own pid with a working directory. If the probe cannot see the
/// process that ran it, its silence about every other process carries no
/// information at all.
/// Test: `liveness_reports_a_process_standing_in_the_directory`,
/// `liveness_ignores_a_sibling_directory`,
/// `liveness_treats_an_unparsable_probe_as_in_use`,
/// `liveness_treats_an_empty_probe_as_in_use`,
/// `liveness_treats_a_listing_that_cannot_see_this_process_as_in_use`.
fn scan_probe(output: &str, root: &Path) -> Option<String> {
    let self_pid = std::process::id().to_string();
    let (mut pid, mut command, mut saw_self) = ("?", "?", false);
    for line in output.lines() {
        let Some((tag, value)) = line.split_at_checked(1) else {
            continue;
        };
        match tag {
            "p" => pid = value,
            "c" => command = value,
            "n" => {
                if pid == self_pid {
                    saw_self = true;
                }
                if Path::new(value).starts_with(root) {
                    return Some(format!(
                        "pid {pid} ({command}) is standing in {value} — a live process nothing \
                         registered, found by asking the OS rather than a record (#4311)"
                    ));
                }
            }
            _ => {}
        }
    }
    if !saw_self {
        return Some(format!(
            "the live-process probe never reported this process (pid {self_pid}) — it cannot \
             see the process that ran it, so its silence about {} proves nothing (ADR-0045)",
            root.display()
        ));
    }
    None
}

#[cfg(test)]
#[path = "worktree_liveness_tests.rs"]
mod tests;
