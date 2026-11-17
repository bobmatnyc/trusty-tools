//! Whether a recorded `claude` pid still runs under a live tmux pane (#10753).
//!
//! Why: the PID-file orphan sweep ([`crate::core::pid_registry`]) decided
//! "orphan" from the live session-id set alone. That set is empty right after
//! a daemon restart and refills only on a hook event, so an idle, healthy
//! `claude` was SIGTERMed on the second sweep. A pid file records only
//! `<legacy-session-id> → pid`, with no tmux name or socket, so the evidence
//! has to come from the pid itself: a `claude` whose pane still exists has a
//! tmux server among its ancestors; a true orphan (its pane's shell died) has
//! been reparented to init or a subreaper. The walk needs no session-id
//! mapping and works for every tmux server, named socket or default (#10101).
//! What: [`HostEvidence`] is the three-way verdict; [`tmux_ancestry`] is the
//! pure walk over a process-table snapshot; [`os_host_evidence`] takes one
//! `sysinfo` snapshot and runs the walk. Any gap in the table answers
//! [`HostEvidence::Unknown`], which the sweep treats as "do not signal".
//! Test: the `tests` submodule covers every [`tmux_ancestry`] arm; the sweep
//! wiring is covered in `tests/pid_registry_sweep.rs`.

use std::collections::HashMap;

use tracing::warn;

/// Upper bound on parent hops; a pane's `claude` sits 2-3 hops under tmux.
const MAX_ANCESTOR_HOPS: usize = 32;

/// What the process table says about whether a pid still has a host pane.
///
/// Why: a protecting check must fail closed, so "could not tell" is its own
/// answer rather than being folded into "no host".
/// What: `Hosted` (a tmux server is an ancestor), `Unhosted` (the chain ends
/// at init with no tmux on it), `Unknown` (the table could not answer).
/// Test: `ancestry_*` tests in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostEvidence {
    /// A tmux server is among the pid's ancestors: its pane still exists.
    Hosted,
    /// The ancestor chain reaches init (pid 0 or 1) with no tmux server on it.
    Unhosted,
    /// A row or parent link is missing, or the chain is longer than the bound.
    Unknown,
}

/// One process-table row: the parent pid (when readable) and the short name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcRow {
    /// Parent pid; `None` when the OS did not report one.
    pub parent: Option<u32>,
    /// Short process name (`comm` on Linux, the executable name on macOS).
    pub name: String,
}

/// Walk `pid`'s ancestors in `table` and report whether tmux hosts it.
///
/// Why: this is the evidence that a pid file's session still exists (#10753).
/// Keeping it a pure function over a snapshot makes every arm testable without
/// spawning tmux or signalling anything.
/// What: from `pid`'s row, follow parent links. A parent of 0 or 1 →
/// `Unhosted`. A parent whose name is a tmux binary → `Hosted`. A missing row,
/// a missing parent link, or more than `MAX_ANCESTOR_HOPS` hops (a cycle or a
/// corrupt table) → `Unknown`.
/// Test: `ancestry_pane_shell_under_tmux_is_hosted`,
/// `ancestry_reparented_to_init_is_unhosted`,
/// `ancestry_reparented_to_subreaper_is_unhosted`,
/// `ancestry_missing_row_is_unknown`, `ancestry_missing_parent_is_unknown`,
/// `ancestry_cycle_is_unknown`.
pub fn tmux_ancestry(pid: u32, table: &HashMap<u32, ProcRow>) -> HostEvidence {
    let Some(mut row) = table.get(&pid) else {
        return HostEvidence::Unknown;
    };
    for _ in 0..MAX_ANCESTOR_HOPS {
        let parent = match row.parent {
            None => return HostEvidence::Unknown,
            Some(0 | 1) => return HostEvidence::Unhosted,
            Some(p) => p,
        };
        let Some(parent_row) = table.get(&parent) else {
            return HostEvidence::Unknown;
        };
        if is_tmux_name(&parent_row.name) {
            return HostEvidence::Hosted;
        }
        row = parent_row;
    }
    HostEvidence::Unknown
}

/// True when a process name is a tmux binary (`tmux`, `tmux: server`, a path).
fn is_tmux_name(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    base.to_ascii_lowercase().starts_with("tmux")
}

/// [`tmux_ancestry`] over one live `sysinfo` snapshot of the process table.
///
/// Why: the production [`crate::core::pid_registry::OsProcessProbe`] needs
/// the real answer; one snapshot keeps every parent link from the same moment.
/// What: refreshes every process (parent and name only); an empty table is
/// logged and answers `Unknown`, since this process itself runs.
/// Test: the decision is covered by the `ancestry_*` tests; the snapshot is a
/// thin OS read.
pub fn os_host_evidence(pid: u32) -> HostEvidence {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    if sys.processes().is_empty() {
        warn!(
            pid,
            "pid-registry: the process table listed no process; host unknown"
        );
        return HostEvidence::Unknown;
    }
    let table: HashMap<u32, ProcRow> = sys
        .processes()
        .iter()
        .map(|(p, proc_)| {
            let row = ProcRow {
                parent: proc_.parent().map(|pp| pp.as_u32()),
                name: proc_.name().to_string_lossy().into_owned(),
            };
            (p.as_u32(), row)
        })
        .collect();
    tmux_ancestry(pid, &table)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &[(u32, Option<u32>, &str)]) -> HashMap<u32, ProcRow> {
        rows.iter()
            .map(|&(pid, parent, name)| {
                let row = ProcRow {
                    parent,
                    name: name.to_string(),
                };
                (pid, row)
            })
            .collect()
    }

    #[test]
    fn ancestry_pane_shell_under_tmux_is_hosted() {
        // claude → zsh (pane) → tmux server → launchd, macOS and Linux names.
        for server in ["tmux", "tmux: server", "/opt/homebrew/bin/tmux"] {
            let t = table(&[
                (500, Some(400), "claude"),
                (400, Some(300), "zsh"),
                (300, Some(1), server),
            ]);
            assert_eq!(tmux_ancestry(500, &t), HostEvidence::Hosted, "{server}");
        }
    }

    #[test]
    fn ancestry_reparented_to_init_is_unhosted() {
        let t = table(&[(500, Some(1), "claude")]);
        assert_eq!(tmux_ancestry(500, &t), HostEvidence::Unhosted);
    }

    #[test]
    fn ancestry_reparented_to_subreaper_is_unhosted() {
        // Linux: an orphan lands on `systemd --user`, not pid 1.
        let t = table(&[(500, Some(80), "claude"), (80, Some(1), "systemd")]);
        assert_eq!(tmux_ancestry(500, &t), HostEvidence::Unhosted);
    }

    #[test]
    fn ancestry_missing_row_is_unknown() {
        // The pid itself, or a parent mid-chain, is absent from the snapshot.
        assert_eq!(tmux_ancestry(500, &table(&[])), HostEvidence::Unknown);
        let t = table(&[(500, Some(400), "claude")]);
        assert_eq!(tmux_ancestry(500, &t), HostEvidence::Unknown);
    }

    #[test]
    fn ancestry_missing_parent_is_unknown() {
        let t = table(&[(500, None, "claude")]);
        assert_eq!(tmux_ancestry(500, &t), HostEvidence::Unknown);
    }

    #[test]
    fn ancestry_cycle_is_unknown() {
        let t = table(&[(500, Some(400), "claude"), (400, Some(500), "zsh")]);
        assert_eq!(tmux_ancestry(500, &t), HostEvidence::Unknown);
    }
}
