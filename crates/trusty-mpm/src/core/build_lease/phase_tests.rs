//! Tests for `core::build_lease::phase` (#8692): the flip state machine with
//! scripted lock reads, the record's cross-version read, and how a run-phase
//! holder is counted.

use super::*;
use crate::core::build_lease::slots::{HolderRecord, compiling};

/// Feed `reads` to a fresh tracker with a write that always succeeds; return
/// the phase after each read and how many writes ran.
fn run(reads: &[std::io::Result<bool>]) -> (Vec<Phase>, usize) {
    let mut tracker = PhaseTracker::new();
    let mut writes = 0;
    let phases = reads
        .iter()
        .map(|read| {
            let read = read
                .as_ref()
                .map(|held| *held)
                .map_err(|e| std::io::Error::new(e.kind(), e.to_string()));
            tracker.tick(
                || read,
                || {
                    writes += 1;
                    Ok(())
                },
            )
        })
        .collect();
    (phases, writes)
}

fn unreadable() -> std::io::Result<bool> {
    Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
}

/// A build too fast for the poll, or one still resolving: capacity is kept.
#[test]
fn a_lock_never_seen_held_never_flips() {
    let (phases, writes) = run(&[Ok(false), Ok(false), Ok(false), Ok(false)]);
    assert!(phases.iter().all(|p| *p == Phase::Compile), "{phases:?}");
    assert_eq!(writes, 0);
}

/// Hysteresis: a single free read between two cargo steps does not flip.
#[test]
fn one_free_read_after_a_held_one_does_not_flip() {
    let (phases, writes) = run(&[Ok(true), Ok(false), Ok(true), Ok(false)]);
    assert!(phases.iter().all(|p| *p == Phase::Compile), "{phases:?}");
    assert_eq!(writes, 0);
}

/// Held, then two free reads: the flip is written once and never again.
#[test]
fn two_free_reads_after_a_held_one_flip() {
    let (phases, writes) = run(&[Ok(true), Ok(false), Ok(false), Ok(true), Ok(false)]);
    assert_eq!(
        phases,
        vec![
            Phase::Compile,
            Phase::Compile,
            Phase::Run,
            Phase::Run,
            Phase::Run
        ],
        "one-way: a later held read does not flip back"
    );
    assert_eq!(writes, 1);
}

/// Fail-closed: an unreadable lock reads as held, so it never counts toward
/// the free reads that end the compile phase.
#[test]
fn a_probe_error_reads_as_held() {
    let (phases, writes) = run(&[Ok(true), Ok(false), unreadable(), Ok(false)]);
    assert!(phases.iter().all(|p| *p == Phase::Compile), "{phases:?}");
    assert_eq!(writes, 0);
    let (phases, _) = run(&[unreadable(), unreadable(), unreadable()]);
    assert!(phases.iter().all(|p| *p == Phase::Compile), "{phases:?}");
}

/// Fail-closed: a record write that fails keeps the phase at compile, and the
/// write is retried on the next poll without probing again.
#[test]
fn a_failed_write_stays_compile_and_retries() {
    let mut tracker = PhaseTracker::new();
    let ok = || Ok(());
    assert_eq!(tracker.tick(|| Ok(true), ok), Phase::Compile);
    assert_eq!(tracker.tick(|| Ok(false), ok), Phase::Compile);
    let failed = tracker.tick(
        || Ok(false),
        || Err(std::io::Error::from(std::io::ErrorKind::StorageFull)),
    );
    assert_eq!(failed, Phase::Compile);
    assert!(!tracker.is_done());
    let mut probed = false;
    let retried = tracker.tick(
        || {
            probed = true;
            Ok(true)
        },
        ok,
    );
    assert_eq!(retried, Phase::Run, "the due flip is retried");
    assert!(!probed, "a due flip is not undone by a later lock read");
    assert!(tracker.is_done());
}

/// A record an older `tm` wrote has no `phase`: it counts for capacity.
#[test]
fn a_record_without_a_phase_counts_for_capacity() {
    let old = r#"{"slot":0,"pid":41,"child_pid":42,"command":"cargo test",
        "cwd":"/repo","started_at":"2026-10-01T00:00:00Z","target_dir":null}"#;
    let record: HolderRecord = serde_json::from_str(old).expect("an old record parses");
    assert_eq!(record.phase, Phase::Compile);
    assert!(record.counts_for_capacity());
    let unknown = old.replace(
        "\"target_dir\":null",
        "\"target_dir\":null,\"phase\":\"link\"",
    );
    assert!(
        serde_json::from_str::<HolderRecord>(&unknown).is_err(),
        "an unknown phase is unparseable, which `holders()` counts as compiling"
    );
}

/// The record shape before #8692, as an older `tm` deserializes it.
#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct OldHolderRecord {
    slot: u32,
    pid: u32,
    child_pid: Option<u32>,
    command: String,
    cwd: String,
    started_at: String,
    target_dir: Option<String>,
}

/// An older `tm` reading a run-phase record ignores `phase`, so it counts the
/// slot as held — stricter than a new `tm`, never looser.
#[test]
fn an_older_reader_ignores_the_phase_field() {
    let mut record = HolderRecord::new(3, "cargo test -p x", "/repo");
    record.phase = Phase::Run;
    let body = serde_json::to_string(&record).expect("serialize");
    assert!(body.contains("\"phase\":\"run\""), "{body}");
    let old: OldHolderRecord = serde_json::from_str(&body).expect("an old reader parses it");
    assert_eq!(old.slot, 3);
    assert_eq!(old.pid, std::process::id());
}

/// Run-phase holders own their slots but are not counted against the ceiling,
/// and their later compilers are foreign to the census.
#[test]
fn run_phase_holders_free_capacity_but_keep_their_slots() {
    use crate::core::build_lease::census::foreign_groups;
    use crate::core::build_probe::ProcessSnapshot;
    let mut running = HolderRecord::new(0, "cargo watch", "/repo");
    running.pid = 20;
    running.child_pid = Some(21);
    running.phase = Phase::Run;
    let mut building = HolderRecord::new(1, "cargo build", "/repo");
    building.pid = 30;
    building.child_pid = Some(31);
    assert!(!running.counts_for_capacity());
    assert!(building.counts_for_capacity());
    let holders = vec![running.clone(), building.clone()];
    assert_eq!(compiling(&holders), vec![building]);
    assert!(
        running.render().contains(", running, /repo)"),
        "{}",
        running.render()
    );
    let row = |pid: u32, parent: u32, name: &str| ProcessSnapshot {
        pid,
        parent: Some(parent),
        name: name.into(),
        exe: None,
        cpu_pct: 50.0,
    };
    let table = vec![
        row(20, 1, "tm"),
        row(21, 20, "cargo"),
        row(22, 21, "rustc"),
        row(30, 1, "tm"),
        row(31, 30, "cargo"),
        row(32, 31, "rustc"),
    ];
    let foreign = foreign_groups(&table, &compiling(&holders));
    assert_eq!(foreign.len(), 1, "{foreign:?}");
    assert!(
        foreign_groups(&table, &holders).is_empty(),
        "both excluded when both count"
    );
}

/// The lock is read in the lease's target dir, or each default `target`.
#[test]
fn the_lock_dirs_are_the_target_or_each_default_target() {
    let repo = Path::new("/repo");
    assert_eq!(
        lock_dirs(Some("/slots/slot-2"), &repo.join("crates/a"), repo),
        vec![PathBuf::from("/slots/slot-2")]
    );
    assert_eq!(
        lock_dirs(Some("rel"), &repo.join("crates/a"), repo),
        vec![PathBuf::from("/repo/crates/a/rel")]
    );
    assert_eq!(
        lock_dirs(None, &repo.join("crates/a"), repo),
        vec![
            PathBuf::from("/repo/crates/a/target"),
            PathBuf::from("/repo/crates/target"),
            PathBuf::from("/repo/target"),
        ]
    );
    assert_eq!(
        lock_dirs(None, Path::new("/elsewhere"), repo),
        vec![PathBuf::from("/elsewhere/target")]
    );
}
