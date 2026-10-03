//! The census broken down per build group, for `tm build-lease --census`
//! (#8261 round 4).
//!
//! Why: under the owner's ruling (a) every build running without a lease
//! counts against the ceiling, so a count that reaches it refuses every leased
//! build. Round 3 found no record of which processes an earlier count of 16
//! was. The live check logs this view while builds run, so the next count can
//! be attributed to a process group, a driver and whoever started it.
//!
//! What: [`describe`] turns census groups and the process table into
//! [`GroupDetail`]s; [`live_breakdown`] reads this machine. Read-only. An argv
//! is shown only for a build driver, and only through
//! [`summarize_command`] (program, subcommand, `-p` packages); any other
//! program is shown by its basename alone, so no argument value is printed.
//! Test: the unit suite below; `the_census_view_lists_holders_without_argument_values`
//! in `tests/tm_build_lease.rs`.

use std::collections::HashMap;

use super::census::{foreign_groups, host_name};
use super::slots::{HolderRecord, summarize_command};
use crate::core::build_probe::{
    BuildGroup, ProcessSampler, ProcessSnapshot, SysinfoSampler, is_build_driver,
};

/// Process names that say who started a build; the parent chain stops at the
/// first one it reaches.
const OWNER_NAMES: &[&str] = &[
    "claude",
    "tm",
    "trusty-mpm",
    "tmux",
    "screen",
    "sshd",
    "login",
    "launchd",
    "systemd",
    "init",
    "cron",
    "Terminal",
    "iTerm2",
    "Code Helper",
    "Cursor",
    "zed",
    "Runner.Worker",
    "Runner.Listener",
];

/// The longest argv summary shown, in characters.
const SUMMARY_MAX_CHARS: usize = 60;

/// One census group, attributed.
///
/// Test: `a_group_is_attributed_to_its_owner`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct GroupDetail {
    /// The process group of the group's root, when the OS will say.
    pub pgid: Option<u32>,
    /// The process-group leader's redacted argv.
    pub leader: String,
    /// The driver: name, pid and redacted argv.
    pub driver: String,
    /// The compilers under it, as [`BuildGroup::render`] shows them.
    pub compilers: String,
    /// `name(pid)` from the root upward, ending at the first owner.
    pub chain: Vec<String>,
}

impl GroupDetail {
    /// `pgid 4410 | leader `cargo test` | driver … | rustc ×2 under … | chain a(1) < b(2)`.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "pgid {} | leader `{}` | driver {} | {} | chain {}",
            self.pgid.map_or_else(|| "?".to_string(), |p| p.to_string()),
            self.leader,
            self.driver,
            self.compilers,
            self.chain.join(" < ")
        )
    }
}

/// Attribute each census group.
///
/// What: `pgid_of` names the root's process group; the leader is that group's
/// leader when it is in `rows`, else the root. `argv_of` supplies argv for the
/// leader and the root only; both pass through [`redact`].
/// Test: `a_group_is_attributed_to_its_owner`,
/// `a_non_driver_argv_is_shown_by_basename_only`.
#[must_use]
pub fn describe(
    groups: &[BuildGroup],
    rows: &[ProcessSnapshot],
    argv_of: &dyn Fn(u32) -> Option<Vec<String>>,
    pgid_of: &dyn Fn(u32) -> Option<u32>,
) -> Vec<GroupDetail> {
    let by_pid: HashMap<u32, &ProcessSnapshot> = rows.iter().map(|r| (r.pid, r)).collect();
    groups
        .iter()
        .map(|group| {
            let pgid = pgid_of(group.root_pid);
            let leader_pid = pgid
                .filter(|p| by_pid.contains_key(p))
                .unwrap_or(group.root_pid);
            let shown = |pid: u32| argv_of(pid).map_or_else(|| "?".to_string(), |a| redact(&a));
            GroupDetail {
                pgid,
                leader: shown(leader_pid),
                driver: format!(
                    "{} (pid {}) `{}`",
                    group.root_name,
                    group.root_pid,
                    shown(group.root_pid)
                ),
                compilers: group.render(),
                chain: owner_chain(&group.ancestry, &by_pid),
            }
        })
        .collect()
}

/// A build driver's [`summarize_command`], else the program's basename.
fn redact(argv: &[String]) -> String {
    let Some(program) = argv.first() else {
        return "?".to_string();
    };
    let base = program.rsplit('/').next().unwrap_or(program);
    let shown = if is_build_driver(base) {
        summarize_command(argv)
    } else {
        base.to_string()
    };
    if shown.chars().count() > SUMMARY_MAX_CHARS {
        let cut: String = shown.chars().take(SUMMARY_MAX_CHARS - 1).collect();
        format!("{cut}…")
    } else {
        shown
    }
}

/// `name(pid)` up `ancestry`, stopping after the first [`OWNER_NAMES`] entry.
fn owner_chain(ancestry: &[u32], by_pid: &HashMap<u32, &ProcessSnapshot>) -> Vec<String> {
    let mut chain = Vec::new();
    for pid in ancestry {
        let name = by_pid.get(pid).map_or("?", |p| p.name.as_str());
        chain.push(format!("{name}({pid})"));
        if OWNER_NAMES.contains(&name) {
            break;
        }
    }
    chain
}

/// This machine's census, attributed, excluding `holders`' own builds.
///
/// # Errors
///
/// The process table cannot be read.
///
/// Test: `live_breakdown_reads_this_host`.
pub fn live_breakdown(holders: &[HolderRecord]) -> Result<Vec<GroupDetail>, String> {
    let sampler = SysinfoSampler::new();
    // CPU is a delta between two refreshes.
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    let rows = sampler.sample().map_err(|err| err.to_string())?;
    let groups = foreign_groups(&rows, holders);
    let mut wanted: Vec<u32> = groups.iter().map(|g| g.root_pid).collect();
    wanted.extend(groups.iter().filter_map(|g| pgid_of(g.root_pid)));
    let argv = argv_of_pids(&wanted);
    Ok(describe(
        &groups,
        &rows,
        &|pid| argv.get(&pid).cloned(),
        &pgid_of,
    ))
}

/// The census header line for `tm build-lease --census`.
#[must_use]
pub fn header(groups: usize, holders: usize, ceiling: u32) -> String {
    format!(
        "tm build-lease census on {}: {groups} build group(s) without a lease, {holders} \
         lease(s) held, ceiling {ceiling}",
        host_name()
    )
}

/// argv of `pids` only, read fresh.
fn argv_of_pids(pids: &[u32]) -> HashMap<u32, Vec<String>> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let pids: Vec<Pid> = pids.iter().map(|p| Pid::from_u32(*p)).collect();
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    sys.processes()
        .iter()
        .map(|(pid, proc_)| {
            let argv = proc_
                .cmd()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            (pid.as_u32(), argv)
        })
        .collect()
}

/// The process group of `pid`, or `None` when it has exited.
fn pgid_of(pid: u32) -> Option<u32> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    // SAFETY: getpgid takes a plain integer and touches no memory of ours; it
    // returns -1 (rejected by the conversion below) for a pid that is gone.
    let pgid = unsafe { libc::getpgid(pid) };
    u32::try_from(pgid).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::build_probe::build_groups;

    fn row(pid: u32, parent: Option<u32>, name: &str) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            parent,
            name: name.to_string(),
            exe: None,
            cpu_pct: 10.0,
        }
    }

    fn argv(words: &[&str]) -> Option<Vec<String>> {
        Some(words.iter().map(ToString::to_string).collect())
    }

    #[test]
    fn a_group_is_attributed_to_its_owner() {
        let rows = vec![
            row(1, None, "launchd"),
            row(50, Some(1), "tmux"),
            row(60, Some(50), "zsh"),
            row(200, Some(60), "cargo"),
            row(300, Some(200), "rustc"),
            row(301, Some(200), "clippy-driver"),
        ];
        let groups = build_groups(&rows);
        let details = describe(
            &groups,
            &rows,
            &|pid| match pid {
                200 => argv(&["/x/cargo", "clippy", "-p", "tm", "--token", "s3cr3t"]),
                _ => None,
            },
            &|_| Some(200),
        );
        assert_eq!(details.len(), 1);
        let line = details[0].render();
        assert!(
            line.starts_with("pgid 200 | leader `cargo clippy -p tm`"),
            "{line}"
        );
        assert!(
            line.contains("driver cargo (pid 200) `cargo clippy -p tm`"),
            "{line}"
        );
        assert!(
            line.contains("chain cargo(200) < zsh(60) < tmux(50)"),
            "{line}"
        );
        assert!(
            !line.contains("launchd"),
            "stops at the first owner: {line}"
        );
        assert!(!line.contains("s3cr3t"), "{line}");
    }

    /// A leader that is not a build driver is shown by basename only.
    #[test]
    fn a_non_driver_argv_is_shown_by_basename_only() {
        let rows = vec![row(10, None, "bash"), row(11, Some(10), "rustc")];
        let groups = build_groups(&rows);
        let details = describe(
            &groups,
            &rows,
            &|pid| match pid {
                10 => argv(&["/bin/bash", "-c", "mysql -phunter2 && cargo build"]),
                11 => argv(&["rustc", "--crate-name", "secret", "-Cpasswd=hunter2"]),
                _ => None,
            },
            &|_| Some(10),
        );
        let line = details[0].render();
        assert!(line.starts_with("pgid 10 | leader `bash`"), "{line}");
        assert!(line.contains("driver rustc (pid 11) `rustc`"), "{line}");
        assert!(!line.contains("hunter2"), "{line}");
        assert!(!line.contains("secret"), "{line}");
        let long: Vec<String> = std::iter::once("cargo".to_string())
            .chain((0..40).map(|i| format!("-p crate-number-{i}")))
            .collect();
        assert!(redact(&long).chars().count() <= SUMMARY_MAX_CHARS);
    }

    #[test]
    fn live_breakdown_reads_this_host() {
        assert!(live_breakdown(&[]).is_ok());
        assert_eq!(pgid_of(u32::MAX), None, "no such process");
        assert!(pgid_of(std::process::id()).is_some());
        assert!(
            header(2, 1, 4)
                .ends_with("2 build group(s) without a lease, 1 lease(s) held, ceiling 4")
        );
    }
}
