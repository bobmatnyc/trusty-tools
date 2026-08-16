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
//! Toward IN USE. `lsof` missing, unspawnable, or returning output this cannot
//! parse all resolve to "something may be in there", never to "free" — the
//! [ADR-0045](../../../../docs/adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md)
//! rule that an empty observation on a destructive path is UNDETERMINABLE and
//! not ABSENT. On a machine with no `lsof` this refuses every reap and says so,
//! which is the correct trade for a `git worktree remove --force`.
//!
//! # Stated gap: cwd only, not open descriptors
//!
//! `lsof -d cwd` costs one invocation and about a second regardless of how big
//! the tree is. The recursive form (`lsof +D <path>`) also catches a process
//! whose cwd is elsewhere but which holds a descriptor inside the tree, at a
//! cost that scales with the tree — an agent worktree here carries a 1.5 GiB
//! `target/`. The cwd form catches the observed incident exactly; a process
//! holding only a descriptor is not covered and is left as a known gap rather
//! than paid for on every reap.
//! Test: the `#[cfg(test)]` suite in `worktree_liveness_tests.rs`.

use std::path::Path;

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
    match run_cwd_probe(LSOF) {
        Ok(output) => scan_probe(&output, &canonical),
        Err(reason) => Some(reason),
    }
}

/// The probe binary. One literal so the doc, the error text and the call agree.
const LSOF: &str = "lsof";

/// Ask `bin` for every process's current working directory.
///
/// Why: `-d cwd` restricts the descriptor set to the one entry per process this
/// check cares about, which is what keeps a system-wide listing to roughly a
/// thousand lines and about a second. `-w` suppresses the warnings `lsof` emits
/// for processes it may not examine — those are expected for another user's
/// processes and are not a probe failure.
///
/// `bin` is a parameter rather than a constant so the fail-toward-in-use arms
/// can be exercised against an absent binary. It is NOT configuration: the one
/// production call site passes [`LSOF`], and an environment override would be
/// process-global state this crate does not use.
/// What: `Ok(stdout)` for a zero exit; `Err(reason)` for a spawn failure or a
/// non-zero exit, each carrying the text the caller reports as its refusal.
/// Test: `liveness_treats_a_missing_lsof_as_in_use`.
fn run_cwd_probe(bin: &str) -> Result<String, String> {
    let out = std::process::Command::new(bin)
        .args(["-w", "-d", "cwd", "-F", "pcn"])
        .output()
        .map_err(|e| format!("could not run `{bin}` to check for live processes: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`{bin}` exited {} while checking for live processes: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Find the first process in an `lsof -F pcn` listing whose cwd is under `root`.
///
/// Why: split from the spawn so the parser is testable against fixed text,
/// including the shapes that must NOT be read as "free".
/// What: `lsof`'s field output repeats `p<pid>`, `c<command>`, then `n<path>`;
/// the `p`/`c` values carry forward until the next process set. Returns
/// `Some(reason)` for the first match, for an empty listing (nothing was
/// observed, so nothing was ruled out), and for a listing that yielded no `n`
/// field at all (the probe answered in a shape this cannot read).
/// Test: `liveness_reports_a_process_standing_in_the_directory`,
/// `liveness_ignores_a_sibling_directory`,
/// `liveness_treats_an_unparsable_probe_as_in_use`,
/// `liveness_treats_an_empty_probe_as_in_use`.
fn scan_probe(output: &str, root: &Path) -> Option<String> {
    let (mut pid, mut command, mut saw_a_path) = ("?", "?", false);
    for line in output.lines() {
        let Some((tag, value)) = line.split_at_checked(1) else {
            continue;
        };
        match tag {
            "p" => pid = value,
            "c" => command = value,
            "n" => {
                saw_a_path = true;
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
    if !saw_a_path {
        return Some(format!(
            "the live-process probe returned no working directories at all, so nothing rules \
             out a process inside {} — treating it as in use (ADR-0045)",
            root.display()
        ));
    }
    None
}

#[cfg(test)]
#[path = "worktree_liveness_tests.rs"]
mod tests;
