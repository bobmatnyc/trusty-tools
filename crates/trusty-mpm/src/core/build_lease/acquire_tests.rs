//! Tests for `core::build_lease::acquire` (#8261): real lock files, real
//! `flock`s, scripted readings.

use super::*;
use crate::core::build_lease::admission::Readings;
use crate::core::build_lease::census::FixedSampler;
use trusty_common::memory_pressure::{
    MemoryPressure, PressureLevel, PressureSignal, PressureSource,
};

/// Readings at `level` with an empty census.
fn scripted(level: PressureLevel) -> FixedSampler {
    let unleased = readings(level, 0);
    FixedSampler::new(readings(level, 0)).with_unleased(unleased)
}

/// A normal machine on which `unleased` builds already run without a lease.
fn with_unleased(unleased: usize) -> FixedSampler {
    FixedSampler::new(readings(PressureLevel::Normal, 0))
        .with_unleased(readings(PressureLevel::Normal, unleased))
}

fn readings(level: PressureLevel, foreign: usize) -> Readings {
    Readings::new(
        Ok(MemoryPressure::new(
            level,
            Some(50.0),
            PressureSource::MacosSysctl,
            vec![PressureSignal::new(
                "kern.memorystatus_vm_pressure_level",
                level.to_string(),
            )],
        )),
        Ok(1.0),
        8,
        Ok((0..foreign)
            .map(|i| crate::core::build_probe::BuildGroup {
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

/// Owner ruling "allow up to the cap" (#8261 round 3): an unopenable
/// `admission.lock` runs the build unleased only while the census has room,
/// and the outcome names the store, the error and the repair.
#[test]
fn an_unopenable_admission_lock_is_bounded_by_the_census() {
    let (_tmp, slots) = slot_dir();
    std::fs::create_dir(slots.path().join("admission.lock")).expect("mkdir");
    let c = configs();
    match acquire(
        &slots,
        &params(&c, 1, 200),
        &mut with_unleased(0),
        &mut |_, _| {},
    ) {
        Outcome::Unleased { fault, decision } => {
            assert!(fault.store.ends_with("admission.lock"), "{fault:?}");
            assert!(fault.repair.contains("rm -rf"), "{fault:?}");
            assert!(
                decision.degraded[0].contains("UNKNOWN lease state"),
                "{decision:?}"
            );
        }
        other => panic!("an idle machine runs it unleased: {other:?}"),
    }
}

/// "Allow up to the cap": admitted while fewer than `ceiling` builds run
/// without a lease.
#[test]
fn unleased_is_admitted_while_the_count_is_below_the_ceiling() {
    let (_tmp, slots) = slot_dir();
    std::fs::create_dir(slots.path().join("admission.lock")).expect("mkdir");
    let c = configs();
    assert!(matches!(
        acquire(
            &slots,
            &params(&c, 2, 200),
            &mut with_unleased(1),
            &mut |_, _| {}
        ),
        Outcome::Unleased { .. }
    ));
}

/// "Allow up to the cap": refused once `ceiling` builds run without a lease.
#[test]
fn unleased_is_refused_when_the_count_reaches_the_ceiling() {
    let (_tmp, slots) = slot_dir();
    std::fs::create_dir(slots.path().join("admission.lock")).expect("mkdir");
    let c = configs();
    match acquire(
        &slots,
        &params(&c, 2, 200),
        &mut with_unleased(2),
        &mut |_, _| {},
    ) {
        Outcome::TimedOut { decision, .. } => {
            assert!(
                decision.withheld[0].contains("fill the ceiling 2"),
                "{decision:?}"
            );
            assert!(
                decision.degraded[0].contains("admission.lock"),
                "{decision:?}"
            );
        }
        other => panic!("the census is full: {other:?}"),
    }
}

/// Every slot file broken: the census bound applies, not a free pass.
#[test]
fn unlockable_slot_files_fall_back_to_the_census_bound() {
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
    let full = acquire(
        &slots,
        &params(&c, 1, 200),
        &mut with_unleased(1),
        &mut |_, _| {},
    );
    let idle = acquire(
        &slots,
        &params(&c, 1, 200),
        &mut with_unleased(0),
        &mut |_, _| {},
    );
    std::fs::set_permissions(slots.path(), std::fs::Permissions::from_mode(0o755))
        .expect("restore");
    assert!(matches!(full, Outcome::TimedOut { .. }), "{full:?}");
    match idle {
        Outcome::Unleased { fault, .. } => {
            assert_eq!(fault.store, slots.path());
            assert!(fault.error.contains("could be locked"), "{fault:?}");
        }
        other => panic!("expected an unleased run: {other:?}"),
    }
}

/// #8261 round 3: a free slot whose directory an orphaned build still uses is
/// skipped; a slot freed that way is never handed to a new lease.
#[test]
fn a_slot_whose_directory_is_busy_is_skipped() {
    let (_tmp, slots) = slot_dir();
    let c = configs();
    let busy = |slot: u32| slot == 0;
    let p = params(&c, 2, 200).with_slot_busy(&busy);
    let guard = leased(acquire(
        &slots,
        &p,
        &mut scripted(PressureLevel::Normal),
        &mut |_, _| {},
    ));
    assert_eq!(guard.slot(), 1, "slot 0's directory is in use");
    let one = params(&c, 1, 200).with_slot_busy(&busy);
    drop(guard);
    assert!(
        matches!(
            acquire(
                &slots,
                &one,
                &mut scripted(PressureLevel::Normal),
                &mut |_, _| {}
            ),
            Outcome::TimedOut { .. }
        ),
        "a ceiling of one with slot 0 busy waits"
    );
}
