//! `tm doctor` probes for the #7171 `git maintenance run --auto` storm.
//!
//! Why: 41 detached `git maintenance run --auto` repacks hit one shared 21 GB
//! `.git` from ~25 worktrees in one incident (load 141). `trusty_common::git`
//! and `core::git_maintenance` are the fix's prevention half; this module is
//! the DETECTION half — a machine that predates either, or a base clone an
//! operator re-enabled maintenance on by hand, should be visible in `tm
//! doctor` rather than silently exposed again.
//! What: [`check_maintenance_config`] warns on any base clone under
//! `repos_root` that carries more than 3 worktrees while
//! `maintenance.auto` is not pinned off; [`check_live_maintenance_processes`]
//! warns when more than one `git maintenance run` process is live on the host
//! right now. Both are read-only — `ps` and `git config --get`, never a write.
//! Test: `doctor_maintenance_storm_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Worktree count above which an un-pinned base clone is worth a warning.
///
/// Why: 1-3 worktrees is an ordinary session or two; the #7171 incident's
/// storm needed a FLEET (~25) sharing one object store for independent
/// maintenance runs to collide. `>3` catches "this could become a fleet"
/// early without warning on every single-session project.
const WORKTREE_WARN_THRESHOLD: usize = 3;

/// One base clone's maintenance-config exposure, as read from disk.
#[derive(Debug, PartialEq, Eq)]
struct BaseCloneExposure {
    base: PathBuf,
    worktree_count: usize,
}

/// Count entries under `<base>/.worktrees/`.
///
/// What: `0` when the directory is absent (a base clone with no sessions
/// yet) or unreadable.
fn count_worktrees(base: &Path) -> usize {
    std::fs::read_dir(base.join(".worktrees"))
        .map(|rd| rd.filter_map(Result::ok).count())
        .unwrap_or(0)
}

/// Read `maintenance.auto` from `base`'s OWN local git config — never the
/// effective (local → global → system) value.
///
/// Why `--local` rather than plain `--get`: this probe's remediation is
/// `git -C <base> config --local maintenance.auto false`, i.e. it asks
/// whether THIS repo has been provisioned, not whether SOME config layer
/// happens to disable maintenance right now. A machine-wide global override
/// (an operator's own incident mitigation, or a value this probe's own test
/// suite must not depend on) would otherwise make a never-provisioned repo
/// read as pinned, which is true today but silently stops being true the
/// moment that global override is ever removed.
/// What: `false` when unset, unreadable, or not exactly `"false"`.
fn maintenance_is_pinned_off(base: &Path) -> bool {
    trusty_common::git::command_in(base)
        .args(["config", "--local", "--get", "maintenance.auto"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .is_some_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "false")
}

/// Scan `repos_root` (`<repos_root>/<owner>/<repo>`, the layout
/// `daemon::managed_routes::inproject::base_clone_path` writes) for a base
/// clone that is exposed to the #7171 storm.
fn scan_exposed_base_clones(repos_root: &Path) -> Vec<BaseCloneExposure> {
    let mut exposed = Vec::new();
    let Ok(owners) = std::fs::read_dir(repos_root) else {
        return exposed;
    };
    for owner_entry in owners.filter_map(Result::ok) {
        let owner_path = owner_entry.path();
        if !owner_path.is_dir() {
            continue;
        }
        let Ok(repos) = std::fs::read_dir(&owner_path) else {
            continue;
        };
        for repo_entry in repos.filter_map(Result::ok) {
            let base = repo_entry.path();
            if !base.join(".git").exists() {
                continue;
            }
            let worktree_count = count_worktrees(&base);
            if worktree_count > WORKTREE_WARN_THRESHOLD && !maintenance_is_pinned_off(&base) {
                exposed.push(BaseCloneExposure {
                    base,
                    worktree_count,
                });
            }
        }
    }
    exposed
}

/// `tm doctor`'s `maintenance_config` probe (#7171).
///
/// Why/What: see the module doc's first bullet.
/// Test: `ok_with_no_repos_root`, `ok_when_worktree_count_is_at_or_below_threshold`,
/// `ok_when_maintenance_auto_is_pinned_off`,
/// `warns_when_maintenance_auto_is_unset_above_threshold`.
pub(super) fn check_maintenance_config(repos_root: Option<&Path>) -> DoctorCheck {
    let Some(root) = repos_root else {
        return DoctorCheck::new(
            "maintenance_config",
            CheckStatus::Ok,
            "no managed workspace root configured — maintenance-config scan skipped",
        );
    };
    if !root.is_dir() {
        return DoctorCheck::new(
            "maintenance_config",
            CheckStatus::Ok,
            "managed workspace root does not exist yet — maintenance-config scan skipped",
        );
    }
    let exposed = scan_exposed_base_clones(root);
    if exposed.is_empty() {
        return DoctorCheck::new(
            "maintenance_config",
            CheckStatus::Ok,
            "every base clone with more than 3 worktrees has git auto-maintenance disabled",
        );
    }
    let names = exposed
        .iter()
        .map(|e| format!("{} ({} worktrees)", e.base.display(), e.worktree_count))
        .collect::<Vec<_>>()
        .join(", ");
    DoctorCheck::new(
        "maintenance_config",
        CheckStatus::Warn,
        format!(
            "{} base clone(s) carry more than {WORKTREE_WARN_THRESHOLD} worktrees without \
             `maintenance.auto=false` pinned (#7171) — operator-run git in any of their \
             worktrees can still trigger a `git maintenance run --auto` repack against the \
             shared object store: {names}. Fix: `git -C <base> config --local maintenance.auto \
             false && git -C <base> config --local gc.auto 0`",
            exposed.len()
        ),
    )
}

/// Count lines in a `ps`-style listing that name a live `git maintenance run`
/// process.
///
/// Why split from [`check_live_maintenance_processes`]: the counting logic is
/// pure text processing and can be tested on fixture output without a real
/// process table.
/// What: matches `"git maintenance run"` as a substring of the command line;
/// excludes nothing else (`ps -A -o command=` output has no header line to
/// mismatch).
pub(super) fn count_maintenance_processes(ps_output: &str) -> usize {
    ps_output
        .lines()
        .filter(|line| line.contains("git maintenance run") || line.contains("git-maintenance"))
        .count()
}

/// `tm doctor`'s `maintenance_processes` probe (#7171).
///
/// Why/What: see the module doc's second bullet. Read-only: `ps -A -o
/// command=`, never a signal or a kill. A failed `ps` spawn reports
/// [`CheckStatus::Unknown`], never `Ok` — issue #4005 established that a
/// probe which could not determine state must say so rather than degrade to
/// healthy, because a sandboxed host with no `ps` would otherwise read as "0
/// processes, no storm" during an actual one. `binary_provenance` and
/// `memory` follow the same convention for their own unreadable-state cases.
/// Test: `count_maintenance_processes_*` cover the pure counter directly;
/// `live_maintenance_processes_probe_does_not_panic` covers the real `ps`
/// call; `check_live_maintenance_processes_reports_unknown_when_ps_is_unavailable`
/// covers the spawn-failure branch.
pub(super) fn check_live_maintenance_processes() -> DoctorCheck {
    check_live_maintenance_processes_with(|| {
        std::process::Command::new("ps")
            .args(["-A", "-o", "command="])
            .output()
    })
}

/// [`check_live_maintenance_processes`], parameterised over the `ps` spawn so
/// the spawn-failure branch is a normal, hermetic unit test rather than
/// something that can only be exercised by removing `ps` from `PATH`.
fn check_live_maintenance_processes_with(
    spawn_ps: impl FnOnce() -> std::io::Result<std::process::Output>,
) -> DoctorCheck {
    let output = match spawn_ps() {
        Ok(output) => output,
        Err(e) => {
            return DoctorCheck::new(
                "maintenance_processes",
                CheckStatus::Unknown,
                format!(
                    "could not enumerate host processes: {e} — maintenance-process state is \
                     unknown, not confirmed healthy"
                ),
            );
        }
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let count = count_maintenance_processes(&text);
    if count <= 1 {
        return DoctorCheck::new(
            "maintenance_processes",
            CheckStatus::Ok,
            format!("{count} `git maintenance run` process(es) live on this host"),
        );
    }
    DoctorCheck::new(
        "maintenance_processes",
        CheckStatus::Warn,
        format!(
            "{count} `git maintenance run` processes live on this host at once (#7171) — this \
             is the storm pattern (41 concurrent repacks against one shared object store in the \
             originating incident). Check which repo(s) they are running against \
             (`ps -A -o pid,command= | grep 'git maintenance'`) and whether \
             `maintenance.auto`/`gc.auto` are pinned off on the shared base clone."
        ),
    )
}

#[cfg(test)]
#[path = "doctor_maintenance_storm_tests.rs"]
mod tests;
