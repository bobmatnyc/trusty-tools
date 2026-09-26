//! Which builds on this host hold no lease, and the live readings (#8261).
//!
//! Why: a `cargo` an operator runs in a terminal, or one another session
//! starts through a script the hook cannot see, loads the machine exactly as a
//! leased build does. Counting only leases undercounts; counting every compiler
//! group double-counts the leased ones. The census therefore subtracts the
//! leased builds from the process table.
//!
//! What: [`foreign_groups`] drops every compiler process that descends from a
//! lease holder's `tm build-lease` pid (or its child's pid) and groups the
//! rest — every process a leased build starts descends from the `tm
//! build-lease` that holds the lock, so the holder pid in the slot file
//! identifies the whole tree. What remains is foreign. [`Sampler`] is the seam
//! the acquire loop reads through; [`LiveSampler`] reads this machine and
//! [`FixedSampler`] replays one set of readings.
//! Test: the unit suite below.

use std::collections::HashMap;

use trusty_common::memory_pressure::{MemoryPressure, PressureThresholds, read_memory_pressure};

use super::admission::Readings;
use super::config::BuildLeaseConfig;
use super::slots::HolderRecord;
use crate::core::build_probe::{
    BuildGroup, ProcessSampler, ProcessSnapshot, SysinfoSampler, build_groups, is_compiler_process,
};

/// Bound on one ancestry walk, as in `build_probe`.
const MAX_HOPS: usize = 64;

/// The compiler groups on this host that belong to no lease holder.
///
/// Why: see the module doc — a leased build is counted by its lease, so its
/// compilers must not be counted again.
/// What: every compiler row whose own ancestry (the process upward, bounded)
/// passes through a holder's `pid` or `child_pid` is removed BEFORE grouping,
/// so a driver above `tm build-lease` (`make` → `tm build-lease` → `cargo`)
/// cannot turn a leased compiler back into a foreign group. Holders with pid
/// `0` (record not yet written) exclude nothing.
/// Test: `a_leased_build_is_not_foreign`, `a_terminal_cargo_is_foreign`,
/// `a_driver_above_the_lease_does_not_make_it_foreign`,
/// `a_real_foreign_compiler_is_counted`.
#[must_use]
pub fn foreign_groups(rows: &[ProcessSnapshot], holders: &[HolderRecord]) -> Vec<BuildGroup> {
    let leased: Vec<u32> = holders
        .iter()
        .flat_map(|h| [Some(h.pid), h.child_pid])
        .flatten()
        .filter(|pid| *pid != 0)
        .collect();
    foreign_groups_excluding(rows, &leased)
}

/// [`foreign_groups`] with the excluded ancestor pids given directly.
fn foreign_groups_excluding(rows: &[ProcessSnapshot], leased: &[u32]) -> Vec<BuildGroup> {
    if leased.is_empty() {
        return build_groups(rows);
    }
    let parent_of: HashMap<u32, Option<u32>> = rows.iter().map(|r| (r.pid, r.parent)).collect();
    let under_a_lease = |pid: u32| {
        let mut cursor = Some(pid);
        for _ in 0..MAX_HOPS {
            let Some(current) = cursor else {
                return false;
            };
            if leased.contains(&current) {
                return true;
            }
            cursor = parent_of.get(&current).copied().flatten();
        }
        false
    };
    let kept: Vec<ProcessSnapshot> = rows
        .iter()
        .filter(|r| !(is_compiler_process(r) && under_a_lease(r.pid)))
        .cloned()
        .collect();
    build_groups(&kept)
}

/// Keeps [`Sampler`] implementable only inside this crate.
mod sealed {
    /// The supertrait no other crate can name.
    pub trait Sealed {}
    impl Sealed for super::FixedSampler {}
    impl Sealed for super::LiveSampler {}
}

/// Where the acquire loop gets its readings from.
///
/// Why: the loop polls; a test must be able to script what each poll sees.
/// Sealed, so a required method can be added without breaking a caller: the
/// implementations are [`LiveSampler`] and [`FixedSampler`].
/// Test: the `acquire` module's suite.
pub trait Sampler: sealed::Sealed {
    /// Take every reading now, excluding `holders`' own builds from the census.
    fn sample(&mut self, holders: &[HolderRecord]) -> Readings;

    /// The readings for a build that could take no lease (#8261).
    ///
    /// What: `foreign` must count every build running WITHOUT a lease,
    /// including other `tm build-lease` processes started earlier than this
    /// one that also hold none — a queue position, so two unleased builds
    /// started together cannot both see an empty census. Defaults to
    /// [`Self::sample`] with no holders.
    fn sample_unleased(&mut self) -> Readings {
        self.sample(&[])
    }
}

/// A sampler that returns the same readings on every poll.
///
/// Why: tests drive the lease and the `builder_cap` row with scripted
/// readings, and [`Sampler`] is sealed.
/// Test: `a_fixed_sampler_replays_its_readings`.
#[derive(Debug, Clone)]
pub struct FixedSampler {
    readings: Readings,
    unleased: Option<Readings>,
}

impl FixedSampler {
    /// A sampler that always returns `readings`.
    #[must_use]
    pub fn new(readings: Readings) -> Self {
        Self {
            readings,
            unleased: None,
        }
    }

    /// The same sampler, answering [`Sampler::sample_unleased`] with `unleased`.
    #[must_use]
    pub fn with_unleased(mut self, unleased: Readings) -> Self {
        self.unleased = Some(unleased);
        self
    }
}

impl Sampler for FixedSampler {
    fn sample(&mut self, _holders: &[HolderRecord]) -> Readings {
        self.readings.clone()
    }

    fn sample_unleased(&mut self) -> Readings {
        self.unleased
            .clone()
            .unwrap_or_else(|| self.readings.clone())
    }
}

/// This machine's readings.
///
/// Why: one `SysinfoSampler` for the life of a wait, so its CPU figures are
/// deltas rather than the zeros a cold sampler reports.
/// Test: `live_readings_are_plausible_on_this_host`.
pub struct LiveSampler {
    processes: SysinfoSampler,
    thresholds: PressureThresholds,
    host: String,
}

impl LiveSampler {
    /// A sampler configured from `config`.
    #[must_use]
    pub fn new(config: &BuildLeaseConfig) -> Self {
        Self {
            processes: SysinfoSampler::new(),
            thresholds: PressureThresholds::default()
                .with_min_available_pct(config.effective_min_available_pct()),
            host: host_name(),
        }
    }

    fn pressure_and_load(&self) -> (Result<MemoryPressure, String>, Result<f64, String>) {
        let pressure = read_memory_pressure(&self.thresholds).map_err(|err| match err.errno() {
            Some(errno) => format!("{err} (errno {errno})"),
            None => err.to_string(),
        });
        let load = trusty_common::load_average::read_load_average()
            .map(|avg| avg.one_minute)
            .map_err(|err| err.to_string());
        (pressure, load)
    }
}

impl Sampler for LiveSampler {
    // The census always runs; `builders.count_foreign_builds = false` is applied
    // in `decide`, because the unleased bound needs the census regardless.
    fn sample(&mut self, holders: &[HolderRecord]) -> Readings {
        let (pressure, load) = self.pressure_and_load();
        let foreign = self
            .processes
            .sample()
            .map(|rows| foreign_groups(&rows, holders))
            .map_err(|err| err.to_string());
        Readings::new(pressure, load, logical_cores(), foreign, self.host.clone())
    }

    fn sample_unleased(&mut self) -> Readings {
        let (pressure, load) = self.pressure_and_load();
        let foreign = self
            .processes
            .sample()
            .map_err(|err| err.to_string())
            .and_then(|rows| {
                let lease = lease_processes()?;
                let mut groups = foreign_groups_excluding(&rows, &lease.all);
                groups.extend(lease.earlier.iter().map(|pid| unleased_group(*pid)));
                Ok(groups)
            });
        Readings::new(pressure, load, logical_cores(), foreign, self.host.clone())
    }
}

fn logical_cores() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

/// Other `tm build-lease` processes on this host.
struct LeaseProcesses {
    /// Every one of them: their compilers are theirs, counted once below.
    all: Vec<u32>,
    /// Those started before this process (ties broken by pid).
    earlier: Vec<u32>,
}

/// Find every other `tm build-lease` process, and which ones started first.
///
/// Why: see [`Sampler::sample_unleased`]. A leased waiter or holder is counted
/// too — conservatively — because from here a lease cannot be told apart.
///
/// # Errors
///
/// When this process cannot be found in the table (no start time to order by).
fn lease_processes() -> Result<LeaseProcesses, String> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    let me = std::process::id();
    let my_start = sys
        .process(Pid::from_u32(me))
        .map(sysinfo::Process::start_time)
        .ok_or_else(|| "this process is missing from the process table".to_string())?;
    let mut all = Vec::new();
    let mut earlier = Vec::new();
    for (pid, proc_) in sys.processes() {
        let pid = pid.as_u32();
        if pid == me || !proc_.cmd().iter().any(|a| a == "build-lease") {
            continue;
        }
        let program = proc_
            .cmd()
            .first()
            .map(|a| a.to_string_lossy().into_owned());
        let base = program
            .as_deref()
            .and_then(|p| p.rsplit('/').next())
            .unwrap_or("");
        if base != "tm" && base != "trusty-mpm" {
            continue;
        }
        all.push(pid);
        if (proc_.start_time(), pid) < (my_start, me) {
            earlier.push(pid);
        }
    }
    Ok(LeaseProcesses { all, earlier })
}

/// A census entry standing for one earlier `tm build-lease` with no lease.
fn unleased_group(pid: u32) -> BuildGroup {
    BuildGroup {
        root_pid: pid,
        root_name: "tm build-lease".to_string(),
        compilers: vec!["queued or running without a lease".to_string()],
        cpu_pct: 0.0,
        ancestry: vec![pid],
    }
}

/// This machine's host name, or `unknown-host`.
#[must_use]
pub fn host_name() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is writable for its full length; gethostname NUL-terminates
    // on success when the name fits, and a missing NUL is handled below.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return "unknown-host".to_string();
    }
    buf.iter()
        .position(|b| *b == 0)
        .and_then(|end| String::from_utf8(buf[..end].to_vec()).ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, parent: Option<u32>, name: &str) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            parent,
            name: name.into(),
            exe: None,
            cpu_pct: 50.0,
        }
    }

    fn holder(pid: u32, child: Option<u32>) -> HolderRecord {
        HolderRecord {
            slot: 0,
            pid,
            child_pid: child,
            command: "cargo test".into(),
            cwd: "/repo".into(),
            started_at: String::new(),
            target_dir: None,
        }
    }

    /// A shell → `tm build-lease` (pid 20) → cargo → rustc tree is leased.
    #[test]
    fn a_leased_build_is_not_foreign() {
        let table = vec![
            row(1, None, "zsh"),
            row(20, Some(1), "tm"),
            row(21, Some(20), "cargo"),
            row(22, Some(21), "rustc"),
        ];
        assert!(foreign_groups(&table, &[holder(20, Some(21))]).is_empty());
        assert_eq!(
            foreign_groups(&table, &[holder(0, None)]).len(),
            1,
            "pid 0 excludes nothing"
        );
    }

    /// The brief's census case: a cargo started outside any lease is foreign.
    #[test]
    fn a_terminal_cargo_is_foreign() {
        let table = vec![
            row(1, None, "zsh"),
            row(20, Some(1), "tm"),
            row(21, Some(20), "cargo"),
            row(22, Some(21), "rustc"),
            row(30, Some(1), "cargo"),
            row(31, Some(30), "rustc"),
        ];
        let foreign = foreign_groups(&table, &[holder(20, Some(21))]);
        assert_eq!(foreign.len(), 1);
        assert_eq!(foreign[0].root_pid, 30);
    }

    #[test]
    fn a_driver_above_the_lease_does_not_make_it_foreign() {
        let table = vec![
            row(10, None, "make"),
            row(20, Some(10), "tm"),
            row(21, Some(20), "cargo"),
            row(22, Some(21), "rustc"),
        ];
        assert!(foreign_groups(&table, &[holder(20, None)]).is_empty());
    }

    /// A real process named `rustc` (a symlink to `sleep`) under this test is
    /// counted foreign, and stops being counted when a holder record names
    /// this test process as its `tm build-lease`.
    #[test]
    fn a_real_foreign_compiler_is_counted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fake = tmp.path().join("rustc");
        std::os::unix::fs::symlink("/bin/sleep", &fake).expect("symlink");
        let mut child = std::process::Command::new(&fake)
            .arg("30")
            .spawn()
            .expect("spawn the fake compiler");
        let sampler = SysinfoSampler::new();
        let rows = sampler.sample().expect("process table");
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            rows.iter()
                .any(|r| r.pid == child.id() && is_compiler_process(r)),
            "the spawned `rustc` must read as a compiler: {:?}",
            rows.iter().find(|r| r.pid == child.id())
        );
        let count = |groups: Vec<BuildGroup>| -> usize {
            groups
                .iter()
                .map(|g| g.compilers.iter().filter(|n| n.as_str() == "rustc").count())
                .sum()
        };
        let unleased = count(foreign_groups(&rows, &[]));
        let leased = count(foreign_groups(&rows, &[holder(std::process::id(), None)]));
        assert_eq!(
            unleased,
            leased + 1,
            "exactly this test's rustc is excluded by the lease"
        );
    }

    #[test]
    fn live_readings_are_plausible_on_this_host() {
        let mut sampler = LiveSampler::new(&BuildLeaseConfig::default());
        let r = sampler.sample(&[]);
        assert!(r.logical_cores >= 1);
        assert!(!r.host.is_empty());
        assert!(r.foreign.is_ok(), "{:?}", r.foreign);
        let u = sampler.sample_unleased();
        assert!(u.foreign.is_ok(), "{:?}", u.foreign);
    }

    #[test]
    fn a_fixed_sampler_replays_its_readings() {
        let readings = Readings::new(
            Err("no pressure".into()),
            Ok(1.5),
            4,
            Ok(Vec::new()),
            "fixed-host",
        );
        let mut sampler = FixedSampler::new(readings);
        for _ in 0..2 {
            let r = sampler.sample(&[]);
            assert_eq!(r.host, "fixed-host");
            assert_eq!(r.load_avg_1min, Ok(1.5));
        }
    }
}
