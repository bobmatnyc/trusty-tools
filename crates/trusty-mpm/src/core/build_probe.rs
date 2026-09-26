//! What is actually compiling on this host, sampled from the process table
//! (#8297).
//!
//! Why: #6892 rationed builder slots by AGENT TYPE — every dispatch declaring
//! `role: engineer` took one. That counts intent, not work: on 2026-09-21 four
//! `python-engineer` agents in two unrelated sessions held every slot on this
//! host while four `rust-engineer` dispatches were refused as "builder 6 of 5",
//! and in adaptive-crm (TypeScript/Terraform/shell) a dispatch that edited a
//! shell script was refused a slot it had no use for. The owner ruling of
//! 2026-09-21 replaces the classifier outright: the cap counts live
//! compiler-class PROCESSES, so what holds a slot is a `rustc` that exists, not
//! an agent that might one day start one.
//!
//! What: [`ProcessSampler`] is the seam — one snapshot of the process table —
//! with [`SysinfoSampler`] the real implementation and any `Vec` of
//! [`ProcessSnapshot`] usable as a fake. [`build_groups`] folds that snapshot
//! into [`BuildGroup`]s: one group per BUILD, not per process, because a single
//! `cargo build` fans out sixteen `rustc` children and counting them
//! individually would refuse every dispatch on a machine running one build.
//!
//! **Sampling failure is reported, never counted as zero.** Every failure arm
//! returns [`BuildProbeError`]; `tm build-lease` treats it as "census
//! unreadable" and admits against its own lease count and the ceiling alone,
//! with a warning naming the error (#8261 increment two — see
//! `core::build_lease::admission` for why that direction, not denial).
//!
//! Reused from branch `fix/8297-builder-cap-process-detection` @ 8bfb4d1f2 (#8297,
//! absorbed into #8261); that branch's `builders.rs` grace-lease changes were left
//! behind.
//!
//! CPU is sampled and reported, never gated on: a link phase that dips below a
//! threshold is still a build holding the machine (owner ruling, point 12). The
//! number is in the census and the refusal text so an operator can see which
//! build is working and which is wedged.
//! Test: the `#[cfg(test)]` suite below.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// One live process, as much of it as the cap needs.
///
/// Why: the seam's currency. Taking a plain struct rather than a `sysinfo`
/// handle is what lets [`build_groups`] be tested against a constructed process
/// tree instead of a machine that happens to be compiling.
/// What: the four fields grouping and attribution read. `exe` is carried
/// because Go's toolchain names its binaries `compile`, `link` and `asm`, which
/// are far too generic to match on name alone — see [`is_compiler_process`].
/// Test: `a_cargo_build_collapses_to_one_group`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ProcessSnapshot {
    /// The process id.
    pub pid: u32,
    /// The parent process id, absent for a reparented or root process.
    pub parent: Option<u32>,
    /// The executable's file name, as the OS reports it.
    pub name: String,
    /// The executable's full path, when the OS will say.
    pub exe: Option<PathBuf>,
    /// CPU use, where `100.0` is one saturated core (`sysinfo`'s convention).
    pub cpu_pct: f32,
}

/// Why a process-table sample could not be taken (#8297).
///
/// Why: the guard's verdict differs between "nothing is building" and "nobody
/// could tell", and a bare `Option` collapses them into the admit direction.
/// What: two arms, both carrying text the refusal quotes verbatim so an
/// operator reads the actual failure rather than a generic "unverifiable".
/// Test: `an_empty_process_table_is_an_error`,
/// `a_poisoned_sampler_reports_the_failure`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildProbeError {
    /// The refresh returned no processes at all, which no live host can be.
    #[error("the process table sampled empty, which cannot be true of a running host")]
    EmptyProcessTable,
    /// The sampler itself could not be used — a poisoned lock, typically.
    #[error("the process sampler is unusable: {0}")]
    Unusable(String),
}

/// One snapshot of the host's process table.
///
/// Why: the cap's whole input, behind a trait so the tally is provable without
/// spawning a compiler. A test hands it a constructed tree; `tm build-lease` hands it
/// a warm `sysinfo::System`.
/// What: one method, returning every live process or the reason none could be
/// read. Implementations must be cheap enough to call on a dispatch's hot path
/// — see [`SysinfoSampler`] for what "warm" buys.
///
/// # Errors
///
/// [`BuildProbeError`] when the process table cannot be read. Never an empty
/// `Ok`: see the module doc.
/// Sealed (#8261 round 3): a required method can be added later without
/// breaking a caller. The implementations are [`SysinfoSampler`] and a `Vec` of
/// [`ProcessSnapshot`].
/// Test: `a_cargo_build_collapses_to_one_group`.
pub trait ProcessSampler: Send + Sync + sealed::Sealed {
    /// Sample every live process on this host.
    ///
    /// # Errors
    ///
    /// [`BuildProbeError`] when the table cannot be read.
    fn sample(&self) -> Result<Vec<ProcessSnapshot>, BuildProbeError>;
}

/// Keeps [`ProcessSampler`] implementable only inside this crate.
mod sealed {
    /// The supertrait no other crate can name.
    pub trait Sealed {}
    impl Sealed for Vec<super::ProcessSnapshot> {}
    impl Sealed for super::SysinfoSampler {}
}

impl ProcessSampler for Vec<ProcessSnapshot> {
    /// A constructed process tree, for tests and for nothing else.
    ///
    /// Test: `an_empty_process_table_is_an_error`.
    fn sample(&self) -> Result<Vec<ProcessSnapshot>, BuildProbeError> {
        if self.is_empty() {
            return Err(BuildProbeError::EmptyProcessTable);
        }
        Ok(self.clone())
    }
}

/// The host's real process table, sampled through `sysinfo`.
///
/// Why: CPU use is a DELTA between two refreshes — a cold `System` reports
/// `0.0` for everything on its first read — so the sampler must outlive one
/// call. One instance lives for a whole `tm build-lease` wait, so every poll
/// after the first reports a meaningful number.
/// `sysinfo` is already an unconditional dependency of this crate, so this adds
/// no crate to the lockfile.
/// What: a `Mutex<System>` refreshed in place on each [`ProcessSampler::sample`].
/// The mutex is this type's own and is held for the refresh only; a poisoned
/// one is [`BuildProbeError::Unusable`] rather than a panic, because the cap
/// must never take `tm build-lease` down.
/// Test: `the_real_sampler_sees_this_test_process`.
pub struct SysinfoSampler {
    /// The warm process table, refreshed per sample for the CPU delta.
    sys: Mutex<sysinfo::System>,
}

impl SysinfoSampler {
    /// A sampler with its CPU baseline primed.
    ///
    /// Why: the first refresh after construction is the baseline, so priming
    /// here means the first CALLER already gets a real percentage.
    /// Test: `the_real_sampler_sees_this_test_process`.
    #[must_use]
    pub fn new() -> Self {
        let mut sys = sysinfo::System::new_with_specifics(
            sysinfo::RefreshKind::nothing()
                .with_processes(sysinfo::ProcessRefreshKind::nothing().with_cpu()),
        );
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        Self {
            sys: Mutex::new(sys),
        }
    }
}

impl Default for SysinfoSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSampler for SysinfoSampler {
    fn sample(&self) -> Result<Vec<ProcessSnapshot>, BuildProbeError> {
        let mut sys = self
            .sys
            .lock()
            .map_err(|e| BuildProbeError::Unusable(e.to_string()))?;
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let rows: Vec<ProcessSnapshot> = sys
            .processes()
            .iter()
            .map(|(pid, proc_)| ProcessSnapshot {
                pid: pid.as_u32(),
                parent: proc_.parent().map(sysinfo::Pid::as_u32),
                name: proc_.name().to_string_lossy().into_owned(),
                exe: proc_.exe().map(PathBuf::from),
                cpu_pct: proc_.cpu_usage(),
            })
            .collect();
        if rows.is_empty() {
            return Err(BuildProbeError::EmptyProcessTable);
        }
        Ok(rows)
    }
}

/// Executable names that ARE a compilation while they are running.
///
/// Why: the owner ruling's "target high-CPU processes only — rustc etc." named
/// the class, and this is it. A driver (`clang`) is listed alongside its worker
/// (`cc1`) because Apple's clang compiles in-process and forks no worker at
/// all, so matching only the worker would see nothing on macOS. Over-matching a
/// driver is harmless: [`build_groups`] collapses a driver and its workers into
/// the one build they belong to.
///
/// `tsc` is deliberately absent. The 2026-09-21 ruling names typescript among
/// the stacks the cap must never hold, and a type-check is single-core work
/// that never approached the 2026-08-08 overcommit.
/// What: matched case-insensitively against [`ProcessSnapshot::name`] by
/// [`is_compiler_process`], with a `.exe` suffix tolerated for Windows.
/// Test: `compiler_processes_are_recognised`,
/// `ordinary_processes_are_not_compilers`.
// #8297: the cap counts these existing, not agents that might start one.
const COMPILER_PROCESS_NAMES: &[&str] = &[
    "rustc", "cc", "c++", "cc1", "cc1plus", "clang", "clang++", "gcc", "g++", "ld", "ld64", "lld",
    "ld.lld", "javac", "swiftc", "csc", "msbuild",
];

/// Go toolchain binaries, which are matched by PATH and never by name alone.
///
/// Why: `compile`, `link` and `asm` are the Go toolchain's real binary names
/// and are far too generic to match bare — an unrelated program called `link`
/// would hold a builder slot forever. Go installs them under a
/// `pkg/tool/<goos>_<goarch>/` directory, so the path is what identifies them.
/// What: matched by [`is_compiler_process`] only when the executable path
/// contains [`GO_TOOL_PATH_SEGMENT`].
/// Test: `go_toolchain_binaries_need_their_path`.
const GO_TOOL_NAMES: &[&str] = &["compile", "link", "asm"];

/// The path segment that identifies a Go toolchain binary. See [`GO_TOOL_NAMES`].
const GO_TOOL_PATH_SEGMENT: &str = "pkg/tool/";

/// Executables that ORCHESTRATE a build and are the unit the cap counts.
///
/// Why: one `cargo build` is one build however many `rustc` processes it fans
/// out, and counting the children would refuse every dispatch on a machine
/// running a single workspace build. The topmost driver in a compiler's
/// ancestry is therefore the group root — topmost, so `make` → `cargo` → `rustc`
/// is one group and not two.
/// What: matched case-insensitively against [`ProcessSnapshot::name`] while
/// walking ancestors in [`build_groups`]. A compiler with no driver above it is
/// its own root — a bare `rustc` or `clang` invocation is still a build.
/// Test: `a_cargo_build_collapses_to_one_group`,
/// `nested_drivers_collapse_to_the_topmost`.
const BUILD_DRIVER_NAMES: &[&str] = &[
    "cargo",
    "go",
    "make",
    "gmake",
    "ninja",
    "dotnet",
    "msbuild",
    "xcodebuild",
    "mvn",
    "gradle",
    "gradlew",
    "swift",
    "bazel",
];

/// How far up the process tree one ancestry walk may go.
///
/// Why: the walk follows `parent` pointers that a reparented process can make
/// circular in principle, and an unbounded loop in a `PreToolUse` path would
/// hang every dispatch on the machine. 64 is far above any real depth from a
/// compiler to `launchd`/`init`.
/// Test: `a_parent_cycle_terminates`.
const MAX_ANCESTRY_HOPS: usize = 64;

/// Is this process a compilation?
///
/// Why: one predicate, so the name table and the Go path rule cannot be applied
/// differently in two places.
/// What: case-insensitive match of `name` (with any `.exe` suffix stripped)
/// against [`COMPILER_PROCESS_NAMES`], or against [`GO_TOOL_NAMES`] when `exe`
/// contains [`GO_TOOL_PATH_SEGMENT`].
/// Test: `compiler_processes_are_recognised`,
/// `ordinary_processes_are_not_compilers`, `go_toolchain_binaries_need_their_path`.
#[must_use]
pub fn is_compiler_process(snap: &ProcessSnapshot) -> bool {
    let name = base_name(&snap.name);
    if COMPILER_PROCESS_NAMES.iter().any(|c| *c == name) {
        return true;
    }
    GO_TOOL_NAMES.iter().any(|c| *c == name)
        && snap.exe.as_ref().is_some_and(|p| {
            p.to_string_lossy()
                .replace('\\', "/")
                .contains(GO_TOOL_PATH_SEGMENT)
        })
}

/// An executable name, lowercased and stripped of a Windows `.exe` suffix.
fn base_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// One build currently running on this host — the unit a builder slot rations.
///
/// Why: the slot is a claim on the machine, and the machine feels one `cargo
/// build` once however many `rustc` it forks. Reporting the group rather than
/// the processes is also what makes a refusal readable: "rustc ×14 under cargo"
/// says what "14 builders" does not.
/// What: `root_pid`/`root_name` are the driver (or the lone compiler);
/// `compilers` names the compiler-class processes under it, deduplicated with
/// counts by [`Self::render`]; `cpu_pct` is their sum, reported and never gated
/// on; `ancestry` is the pid chain from the root upward, which is how the
/// lease census tells a leased build from a foreign one.
/// Test: `a_cargo_build_collapses_to_one_group`,
/// `groups_report_their_ancestry_for_attribution`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct BuildGroup {
    /// The build driver's pid, or the compiler's own when it has no driver.
    pub root_pid: u32,
    /// The driver's executable name.
    pub root_name: String,
    /// Every compiler-class process in this group, by name.
    pub compilers: Vec<String>,
    /// Summed CPU across the group, `100.0` per saturated core.
    pub cpu_pct: f32,
    /// Pids from the root upward, for session attribution.
    pub ancestry: Vec<u32>,
}

impl BuildGroup {
    /// `rustc ×14 under cargo (pid 4412, 780% CPU)`.
    ///
    /// Why: the refusal and the `tm doctor` row must read identically, so the
    /// rendering lives with the data rather than at each surface.
    /// Test: `a_group_renders_its_compilers_with_counts`.
    #[must_use]
    pub fn render(&self) -> String {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for name in &self.compilers {
            match counts.iter_mut().find(|(n, _)| n == name) {
                Some((_, c)) => *c += 1,
                None => counts.push((name.clone(), 1)),
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let which = counts
            .iter()
            .map(|(n, c)| {
                if *c > 1 {
                    format!("{n} ×{c}")
                } else {
                    n.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{which} under {} (pid {}, {:.0}% CPU)",
            self.root_name, self.root_pid, self.cpu_pct
        )
    }
}

/// Fold a process-table sample into the builds running on this host.
///
/// Why: the one place "what is compiling" is decided, shared by the claim path
/// and the `tm doctor` census so a refusal and a diagnosis can never disagree.
/// What: every [`is_compiler_process`] row is walked up its ancestry; the
/// TOPMOST [`BUILD_DRIVER_NAMES`] ancestor becomes the group root, or the
/// compiler itself when there is none. Rows sharing a root are one
/// [`BuildGroup`], with CPU summed. Groups come back ordered by CPU descending
/// then by root pid, so two reads of an unchanged machine render the same way.
/// The walk is bounded by [`MAX_ANCESTRY_HOPS`] and tolerates a parent cycle.
/// Test: `a_cargo_build_collapses_to_one_group`,
/// `nested_drivers_collapse_to_the_topmost`,
/// `two_independent_builds_are_two_groups`, `a_parent_cycle_terminates`.
#[must_use]
pub fn build_groups(snapshot: &[ProcessSnapshot]) -> Vec<BuildGroup> {
    let by_pid: HashMap<u32, &ProcessSnapshot> = snapshot.iter().map(|s| (s.pid, s)).collect();
    let mut groups: Vec<BuildGroup> = Vec::new();

    for snap in snapshot.iter().filter(|s| is_compiler_process(s)) {
        let chain = ancestry_of(snap, &by_pid);
        let root = chain
            .iter()
            .rev()
            .find(|pid| {
                by_pid
                    .get(*pid)
                    .is_some_and(|p| BUILD_DRIVER_NAMES.contains(&base_name(&p.name).as_str()))
            })
            .copied()
            .unwrap_or(snap.pid);
        let root_name = by_pid
            .get(&root)
            .map_or_else(|| snap.name.clone(), |p| p.name.clone());
        // The group's ancestry starts at its ROOT: everything below the root is
        // this build's own fan-out, and a session never lives there.
        let ancestry = by_pid
            .get(&root)
            .map_or_else(|| chain.clone(), |p| ancestry_of(p, &by_pid));

        match groups.iter_mut().find(|g| g.root_pid == root) {
            Some(existing) => {
                existing.compilers.push(base_name(&snap.name));
                existing.cpu_pct += snap.cpu_pct;
            }
            None => groups.push(BuildGroup {
                root_pid: root,
                root_name,
                compilers: vec![base_name(&snap.name)],
                cpu_pct: snap.cpu_pct,
                ancestry,
            }),
        }
    }

    groups.sort_by(|a, b| {
        b.cpu_pct
            .partial_cmp(&a.cpu_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.root_pid.cmp(&b.root_pid))
    });
    groups
}

/// The pid chain from `snap` upward, `snap` first.
///
/// Why: both the group root and the owning session are found on this one walk,
/// and walking twice would let them disagree about where the chain ends.
/// What: follows `parent` for at most [`MAX_ANCESTRY_HOPS`] hops, stopping at a
/// pid already seen so a reparenting cycle cannot spin.
/// Test: `a_parent_cycle_terminates`, `groups_report_their_ancestry_for_attribution`.
fn ancestry_of(snap: &ProcessSnapshot, by_pid: &HashMap<u32, &ProcessSnapshot>) -> Vec<u32> {
    let mut chain = vec![snap.pid];
    let mut cursor = snap.parent;
    while let Some(pid) = cursor {
        if chain.contains(&pid) || chain.len() >= MAX_ANCESTRY_HOPS {
            break;
        }
        chain.push(pid);
        cursor = by_pid.get(&pid).and_then(|p| p.parent);
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One process row, with the fields a case cares about.
    fn proc_row(pid: u32, parent: Option<u32>, name: &str, cpu: f32) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            parent,
            name: name.to_string(),
            exe: None,
            cpu_pct: cpu,
        }
    }

    /// The compiler table is matched by name, case-insensitively.
    #[test]
    fn compiler_processes_are_recognised() {
        for name in ["rustc", "RUSTC", "clang", "cc1plus", "javac", "rustc.exe"] {
            assert!(
                is_compiler_process(&proc_row(1, None, name, 0.0)),
                "{name} is a compilation while it runs"
            );
        }
    }

    /// The cap must not be held by an editor, a shell, or a test runner.
    #[test]
    fn ordinary_processes_are_not_compilers() {
        for name in [
            "python3",
            "node",
            "tsc",
            "bash",
            "claude",
            "tm",
            "terraform",
        ] {
            assert!(
                !is_compiler_process(&proc_row(1, None, name, 90.0)),
                "{name} compiles nothing, whatever its CPU"
            );
        }
    }

    /// `compile`/`link`/`asm` count only from a Go toolchain path.
    #[test]
    fn go_toolchain_binaries_need_their_path() {
        let mut bare = proc_row(1, None, "compile", 50.0);
        assert!(
            !is_compiler_process(&bare),
            "a program merely called `compile` must not hold a slot"
        );
        bare.exe = Some(PathBuf::from("/usr/local/go/pkg/tool/darwin_arm64/compile"));
        assert!(is_compiler_process(&bare));
    }

    /// The unit is the BUILD: one cargo with fourteen rustc is one group.
    #[test]
    fn a_cargo_build_collapses_to_one_group() {
        let mut table = vec![
            proc_row(100, None, "tmux", 0.0),
            proc_row(200, Some(100), "cargo", 3.0),
        ];
        for pid in 300..314 {
            table.push(proc_row(pid, Some(200), "rustc", 50.0));
        }
        let groups = build_groups(&table);
        assert_eq!(groups.len(), 1, "one build, one slot: {groups:?}");
        assert_eq!(groups[0].root_pid, 200);
        assert_eq!(groups[0].compilers.len(), 14);
        assert!((groups[0].cpu_pct - 700.0).abs() < 0.1, "{groups:?}");
    }

    /// `make` above `cargo` is still ONE build — the topmost driver wins.
    #[test]
    fn nested_drivers_collapse_to_the_topmost() {
        let table = vec![
            proc_row(100, None, "make", 1.0),
            proc_row(200, Some(100), "cargo", 1.0),
            proc_row(300, Some(200), "rustc", 90.0),
        ];
        let groups = build_groups(&table);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].root_name, "make");
    }

    /// Two unrelated builds are two slots.
    #[test]
    fn two_independent_builds_are_two_groups() {
        let table = vec![
            proc_row(200, None, "cargo", 1.0),
            proc_row(201, Some(200), "rustc", 80.0),
            proc_row(400, None, "go", 1.0),
            proc_row(401, Some(400), "gcc", 40.0),
        ];
        let groups = build_groups(&table);
        assert_eq!(groups.len(), 2);
        // Ordered by CPU descending, so the reader sees the heavy build first.
        assert_eq!(groups[0].root_name, "cargo");
    }

    /// A lone compiler with no driver above it is its own group.
    #[test]
    fn a_bare_compiler_is_its_own_group() {
        let table = vec![
            proc_row(10, None, "zsh", 0.0),
            proc_row(11, Some(10), "rustc", 99.0),
        ];
        let groups = build_groups(&table);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].root_pid, 11);
    }

    /// The group carries the pid chain the lease census excludes leased builds by.
    #[test]
    fn groups_report_their_ancestry_for_attribution() {
        let table = vec![
            proc_row(50, None, "claude", 0.0),
            proc_row(200, Some(50), "cargo", 1.0),
            proc_row(300, Some(200), "rustc", 80.0),
        ];
        let groups = build_groups(&table);
        assert_eq!(
            groups[0].ancestry,
            vec![200, 50],
            "the chain starts at the root and climbs to the session"
        );
    }

    /// A reparenting cycle must not spin a `PreToolUse` hook forever.
    #[test]
    fn a_parent_cycle_terminates() {
        let table = vec![
            proc_row(1, Some(2), "cargo", 1.0),
            proc_row(2, Some(1), "make", 1.0),
            proc_row(3, Some(1), "rustc", 10.0),
        ];
        let groups = build_groups(&table);
        assert_eq!(groups.len(), 1, "terminated rather than hanging");
    }

    /// The refusal text names the compilers with counts, not fourteen lines.
    #[test]
    fn a_group_renders_its_compilers_with_counts() {
        let table = vec![
            proc_row(200, None, "cargo", 2.0),
            proc_row(201, Some(200), "rustc", 400.0),
            proc_row(202, Some(200), "rustc", 300.0),
            proc_row(203, Some(200), "cc1", 50.0),
        ];
        let rendered = build_groups(&table)[0].render();
        assert!(rendered.contains("rustc ×2"), "{rendered}");
        assert!(rendered.contains("cc1"), "{rendered}");
        assert!(rendered.contains("under cargo (pid 200"), "{rendered}");
        assert!(rendered.contains("750% CPU"), "{rendered}");
    }

    /// An empty table is an ERROR, never "nothing is building" (#8297).
    #[test]
    fn an_empty_process_table_is_an_error() {
        let empty: Vec<ProcessSnapshot> = Vec::new();
        let err = empty.sample().expect_err("an empty host cannot be true");
        assert!(matches!(err, BuildProbeError::EmptyProcessTable));
        assert!(err.to_string().contains("cannot be true"));
    }

    /// The real sampler reads this very process out of the table.
    #[test]
    fn the_real_sampler_sees_this_test_process() {
        let sampler = SysinfoSampler::new();
        let rows = sampler.sample().expect("a running host has processes");
        let me = std::process::id();
        assert!(
            rows.iter().any(|r| r.pid == me),
            "the sample must contain the sampling process itself"
        );
    }

    /// A sampler whose lock is poisoned reports the failure rather than zero.
    #[test]
    fn a_poisoned_sampler_reports_the_failure() {
        let sampler = std::sync::Arc::new(SysinfoSampler::new());
        let poisoner = std::sync::Arc::clone(&sampler);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.sys.lock().expect("lock");
            panic!("poison the mutex");
        })
        .join();
        let err = sampler.sample().expect_err("a poisoned lock cannot sample");
        assert!(matches!(err, BuildProbeError::Unusable(_)), "{err}");
    }
}
