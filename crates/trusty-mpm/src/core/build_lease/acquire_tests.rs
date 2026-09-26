//! Tests for `core::build_lease::acquire` (#8261): real lock files, real
//! `flock`s, scripted readings.

use super::*;
use crate::core::build_lease::admission::Readings;
use crate::core::build_probe::BuildGroup;
use trusty_common::memory_pressure::{
    MemoryPressure, PressureLevel, PressureSignal, PressureSource,
};

/// Scripted readings: a pressure level, and how many builds run with no lease.
struct Scripted {
    level: PressureLevel,
    unleased: usize,
}

fn scripted(level: PressureLevel) -> Scripted {
    Scripted { level, unleased: 0 }
}

impl Scripted {
    fn readings(&self, foreign: usize) -> Readings {
        Readings::new(
            Ok(MemoryPressure::new(
                self.level,
                Some(50.0),
                PressureSource::MacosSysctl,
                vec![PressureSignal::new(
                    "kern.memorystatus_vm_pressure_level",
                    self.level.to_string(),
                )],
            )),
            Ok(1.0),
            8,
            Ok((0..foreign)
                .map(|i| BuildGroup {
                    root_pid: 100 + u32::try_from(i).unwrap_or(0),
                    root_name: "tm build-lease (no lease)".into(),
                    compilers: vec!["rustc".into()],
                    cpu_pct: 0.0,
                    ancestry: Vec::new(),
                })
                .collect()),
            "test-host",
        )
    }
}

impl Sampler for Scripted {
    fn sample(&mut self, _holders: &[HolderRecord]) -> Readings {
        self.readings(0)
    }
    fn sample_unleased(&mut self) -> Readings {
        self.readings(self.unleased)
    }
}

fn configs() -> (BuildersConfig, BuildLeaseConfig) {
    (BuildersConfig::default(), BuildLeaseConfig::default())
}

fn params<'a>(
    c: &'a (BuildersConfig, BuildLeaseConfig),
    ceiling: u32,
    wait_ms: u64,
) -> AcquireParams<'a> {
    AcquireParams::new(&c.0, &c.1, ceiling, Duration::from_millis(wait_ms), "/repo")
        .with_poll(Duration::from_millis(50))
}

fn slot_dir() -> (tempfile::TempDir, SlotDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let slots = SlotDir::at(tmp.path().join("slots")).expect("slots");
    (tmp, slots)
}

fn leased(outcome: Outcome) -> SlotGuard {
    match outcome {
        Outcome::Leased { guard, .. } => guard,
        other => panic!("expected a lease, got {other:?}"),
    }
}

#[test]
fn a_free_machine_leases_immediately() {
    let (_tmp, slots) = slot_dir();
    let c = configs();
    let guard = leased(acquire(
        &slots,
        &params(&c, 2, 500),
        &mut scripted(PressureLevel::Normal),
        &mut |_, _| panic!("no wait expected"),
    ));
    assert_eq!(guard.slot(), 0);
}

#[test]
fn the_n_plus_first_waits_then_times_out() {
    let (_tmp, slots) = slot_dir();
    let c = configs();
    let p = params(&c, 2, 300);
    let mut s = scripted(PressureLevel::Normal);
    let a = leased(acquire(&slots, &p, &mut s, &mut |_, _| {}));
    let b = leased(acquire(&slots, &p, &mut s, &mut |_, _| {}));
    assert_ne!(a.slot(), b.slot());
    let mut waits = 0;
    let started = Instant::now();
    match acquire(&slots, &p, &mut s, &mut |_, _| waits += 1) {
        Outcome::TimedOut { decision, holders } => {
            assert!(!decision.admit);
            assert_eq!(holders.len(), 2, "both holders are named");
            assert!(
                decision.withheld[0].contains("2 of 2 slot(s) held"),
                "{decision:?}"
            );
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
    assert!(waits >= 1, "the caller is told it is waiting");
    assert!(started.elapsed() >= Duration::from_millis(250), "it waited");
}

#[test]
fn a_slot_freed_mid_wait_is_taken() {
    let (_tmp, slots) = slot_dir();
    let c = configs();
    let p = params(&c, 1, 2_000);
    let mut s = scripted(PressureLevel::Normal);
    let mut holder = Some(leased(acquire(&slots, &p, &mut s, &mut |_, _| {})));
    let guard = leased(acquire(&slots, &p, &mut s, &mut |_, _| {
        // The first refused poll releases the holder, as its build exiting would.
        holder.take();
    }));
    assert_eq!(guard.slot(), 0);
}

/// The brief's case (c): warn pressure refuses a new build for the whole wait
/// while the existing holder keeps its slot.
#[test]
fn warn_pressure_times_out_while_the_holder_keeps_its_slot() {
    let (_tmp, slots) = slot_dir();
    let c = configs();
    let p = params(&c, 4, 200);
    let holder = leased(acquire(
        &slots,
        &p,
        &mut scripted(PressureLevel::Normal),
        &mut |_, _| {},
    ));
    match acquire(
        &slots,
        &p,
        &mut scripted(PressureLevel::Warn),
        &mut |_, _| {},
    ) {
        Outcome::TimedOut { decision, .. } => {
            assert!(
                decision.withheld[0].contains("memory pressure is warn"),
                "{decision:?}"
            );
        }
        other => panic!("warn must not admit: {other:?}"),
    }
    assert_eq!(
        slots.holders().len(),
        1,
        "the running holder still holds its slot"
    );
    drop(holder);
}

/// Critic round 1 (HIGH 1a): a broken slot-0 file is skipped — the next index
/// is leased and the ceiling still binds; it never disables the cap.
#[test]
fn a_broken_lowest_slot_is_skipped() {
    let (_tmp, slots) = slot_dir();
    std::fs::create_dir(slots.path().join("slot-0.lock")).expect("mkdir");
    let c = configs();
    let p = params(&c, 1, 200);
    let mut s = scripted(PressureLevel::Normal);
    let first = leased(acquire(&slots, &p, &mut s, &mut |_, _| {}));
    assert_eq!(first.slot(), 1, "the broken index is skipped");
    assert_eq!(slots.broken().len(), 1);
    match acquire(&slots, &p, &mut s, &mut |_, _| {}) {
        Outcome::TimedOut { holders, .. } => assert_eq!(holders.len(), 1),
        other => panic!("the ceiling of 1 must still bind: {other:?}"),
    }
    drop(first);
}

/// Critic round 1 (HIGH 1b): an unopenable `admission.lock` no longer runs
/// unleased unconditionally — the census bounds it.
#[test]
fn an_unopenable_admission_lock_is_bounded() {
    let (_tmp, slots) = slot_dir();
    std::fs::create_dir(slots.path().join("admission.lock")).expect("mkdir");
    assert!(slots.check_admission_lock().is_err());
    let c = configs();
    let p = params(&c, 1, 200);
    let mut busy = Scripted {
        level: PressureLevel::Normal,
        unleased: 1,
    };
    match acquire(&slots, &p, &mut busy, &mut |_, _| {}) {
        Outcome::TimedOut { decision, .. } => {
            assert!(
                decision.withheld[0].contains("fill the ceiling 1"),
                "{decision:?}"
            );
        }
        other => panic!("one unleased build already fills a ceiling of 1: {other:?}"),
    }
    match acquire(
        &slots,
        &p,
        &mut scripted(PressureLevel::Normal),
        &mut |_, _| {},
    ) {
        Outcome::Unleased { why, .. } => assert!(why.contains("admission lock"), "{why}"),
        other => panic!("an idle machine runs it unleased: {other:?}"),
    }
}

/// Every slot file broken: the census bound applies, not a free pass.
#[test]
fn unusable_slot_files_fall_back_to_the_census_bound() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, slots) = slot_dir();
    slots
        .check_admission_lock()
        .expect("create admission.lock first");
    std::fs::create_dir(slots.path().join("slot-0.lock")).expect("mkdir");
    // A read-only directory: slot-0 is broken and no further slot file can be
    // created, so no candidate index is lockable.
    std::fs::set_permissions(slots.path(), std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let c = configs();
    let p = params(&c, 1, 200);
    let mut busy = Scripted {
        level: PressureLevel::Normal,
        unleased: 1,
    };
    assert!(matches!(
        acquire(&slots, &p, &mut busy, &mut |_, _| {}),
        Outcome::TimedOut { .. }
    ));
    let idle = acquire(
        &slots,
        &p,
        &mut scripted(PressureLevel::Normal),
        &mut |_, _| {},
    );
    std::fs::set_permissions(slots.path(), std::fs::Permissions::from_mode(0o755))
        .expect("restore");
    match idle {
        Outcome::Unleased { why, .. } => assert!(why.contains("could be locked"), "{why}"),
        other => panic!("expected an unleased run: {other:?}"),
    }
}
