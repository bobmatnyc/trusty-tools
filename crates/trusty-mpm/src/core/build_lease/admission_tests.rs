//! Tests for `core::build_lease::admission` (#8261).

use super::*;
use trusty_common::memory_pressure::{PressureLevel, PressureSignal, PressureSource};

pub(crate) fn pressure(level: PressureLevel, pct: f64) -> MemoryPressure {
    MemoryPressure::new(
        level,
        Some(pct),
        PressureSource::MacosSysctl,
        vec![
            PressureSignal::new("kern.memorystatus_vm_pressure_level", format!("{level}")),
            PressureSignal::new("kern.memorystatus_level", format!("{pct:.0}%")),
        ],
    )
}

fn quiet() -> Readings {
    Readings::new(
        Ok(pressure(PressureLevel::Normal, 80.0)),
        Ok(4.0),
        16,
        Ok(Vec::new()),
        "test-host",
    )
}

fn group(pid: u32) -> BuildGroup {
    BuildGroup {
        root_pid: pid,
        root_name: "cargo".into(),
        compilers: vec!["rustc".into()],
        cpu_pct: 90.0,
        ancestry: vec![pid],
    }
}

fn d(lease: &BuildLeaseConfig, ceiling: u32, held: u32, r: &Readings) -> Decision {
    decide(&BuildersConfig::default(), lease, ceiling, held, r)
}

fn dflt() -> BuildLeaseConfig {
    BuildLeaseConfig::default()
}

#[test]
fn a_quiet_machine_admits_up_to_the_ceiling() {
    assert!(d(&dflt(), 4, 3, &quiet()).admit);
    let full = d(&dflt(), 4, 4, &quiet());
    assert!(!full.admit);
    assert!(full.withheld[0].contains("4 of 4 slot(s) held"), "{full:?}");
    assert!(
        full.readings.contains("host test-host"),
        "{}",
        full.readings
    );
}

/// The brief's case (c): warn refuses a NEW build; nothing here can touch a
/// lease already granted.
#[test]
fn warn_pressure_refuses_a_new_build_but_counts_the_holders() {
    let mut r = quiet();
    r.pressure = Ok(pressure(PressureLevel::Warn, 40.0));
    let dec = d(&dflt(), 4, 1, &r);
    assert!(!dec.admit);
    assert!(
        dec.withheld[0].contains("memory pressure is warn"),
        "{dec:?}"
    );
    let relaxed = BuildLeaseConfig {
        memory_pressure_max: Some("warn".into()),
        ..dflt()
    };
    assert!(
        d(&relaxed, 4, 1, &r).admit,
        "memory_pressure_max = warn admits at warn"
    );
}

/// The brief's case (a): low free memory with NO kernel pressure is not a
/// refusal while above the percentage floor.
#[test]
fn low_available_memory_refuses_even_at_normal_pressure() {
    let mut r = quiet();
    r.pressure = Ok(pressure(PressureLevel::Normal, 12.0));
    assert!(d(&dflt(), 4, 0, &r).admit, "12% is above the 10% floor");
    r.pressure = Ok(pressure(PressureLevel::Normal, 5.0));
    let dec = d(&dflt(), 4, 0, &r);
    assert!(!dec.admit);
    assert!(dec.withheld[0].contains("min_available_pct"), "{dec:?}");
}

/// Owner ruling "Load only" (#8261 round 3): a loaded machine with nothing
/// held admits exactly one leased build.
#[test]
fn a_loaded_machine_admits_exactly_one_when_nothing_is_held() {
    let mut r = quiet();
    r.load_avg_1min = Ok(40.0);
    assert!(d(&dflt(), 4, 0, &r).admit, "the load floor admits one");
    let dec = d(&dflt(), 4, 1, &r);
    assert!(!dec.admit, "…and only one");
    assert!(dec.withheld[0].contains("1-minute load 40.00"), "{dec:?}");
}

/// Owner ruling "Load only" (#8261 round 3): the floor never reaches memory —
/// low available memory or high pressure refuses with nothing held.
#[test]
fn low_memory_refuses_even_when_nothing_is_held() {
    let mut r = quiet();
    r.pressure = Ok(pressure(PressureLevel::Normal, 5.0));
    let low = d(&dflt(), 4, 0, &r);
    assert!(!low.admit, "{low:?}");
    assert!(low.withheld[0].contains("min_available_pct"), "{low:?}");
    r.pressure = Ok(pressure(PressureLevel::Warn, 50.0));
    assert!(!d(&dflt(), 4, 0, &r).admit, "pressure over its threshold");
}

/// Owner ruling "Load only" (#8261 round 3): the census has no floor — at or
/// over the ceiling it leaves zero slots, so even the first build waits.
#[test]
fn foreign_builds_can_reduce_the_slots_to_zero() {
    let mut r = quiet();
    r.foreign = Ok(vec![group(10), group(11)]);
    let dec = d(&dflt(), 4, 2, &r);
    assert_eq!(dec.n_effective, 2);
    assert!(!dec.admit);
    r.foreign = Ok((0..9).map(group).collect());
    let full = d(&dflt(), 4, 0, &r);
    assert_eq!(full.n_effective, 0);
    assert!(!full.admit, "no floor: {full:?}");
    r.foreign = Ok((0..4).map(group).collect());
    assert!(!d(&dflt(), 4, 0, &r).admit, "exactly the ceiling refuses");
    let off = BuildLeaseConfig {
        count_foreign_builds: Some(false),
        ..dflt()
    };
    assert_eq!(d(&off, 4, 0, &r).n_effective, 4);
}

#[test]
fn unreadable_pressure_uses_the_ceiling_and_warns() {
    let mut r = quiet();
    r.pressure = Err("sysctl kern.memorystatus_level: EPERM".into());
    let dec = d(&dflt(), 2, 1, &r);
    assert!(dec.admit);
    assert!(dec.degraded[0].contains("pressure gate skipped"), "{dec:?}");
    assert!(!d(&dflt(), 2, 2, &r).admit, "never unlimited");
}

#[test]
fn an_unreadable_load_skips_only_the_load_gate() {
    let mut r = quiet();
    r.load_avg_1min = Err("getloadavg failed".into());
    let dec = d(&dflt(), 2, 1, &r);
    assert!(dec.admit);
    assert!(dec.degraded[0].contains("load gate skipped"), "{dec:?}");
}

#[test]
fn an_unreadable_census_counts_leases_only() {
    let mut r = quiet();
    r.foreign = Err("the process table sampled empty".into());
    let dec = d(&dflt(), 3, 2, &r);
    assert!(dec.admit);
    assert_eq!(dec.n_effective, 3);
    assert!(dec.degraded[0].contains("counting leases only"), "{dec:?}");
}

/// Critic round 1 (MEDIUM): an invalid value no longer skips the pressure
/// gate — the gate runs on the default limits.
#[test]
fn an_invalid_config_still_gates_on_pressure() {
    let cfg = BuildLeaseConfig {
        memory_pressure_max: Some("high".into()),
        ..dflt()
    };
    let mut r = quiet();
    r.pressure = Ok(pressure(PressureLevel::Critical, 1.0));
    let dec = d(&cfg, 2, 1, &r);
    assert!(
        !dec.admit,
        "critical pressure must refuse even with a bad config: {dec:?}"
    );
    assert!(dec.degraded[0].contains("memory_pressure_max"), "{dec:?}");
    assert!(
        d(&cfg, 2, 1, &quiet()).admit,
        "a quiet machine still admits"
    );
}

#[test]
fn a_zero_ceiling_admits_nothing() {
    let dec = d(&dflt(), 0, 0, &quiet());
    assert!(!dec.admit);
    assert_eq!(dec.n_effective, 0);
}

/// Critic round 1 (HIGH 1b): an UNLEASED build is bounded by the census with
/// no floor; only lease AND census failing together runs unbounded, loudly.
#[test]
fn unleased_builds_are_bounded_by_the_census() {
    let mut r = quiet();
    r.foreign = Ok(vec![group(10)]);
    assert!(
        decide_unleased(&dflt(), 2, &r).admit,
        "1 unleased build of 2"
    );
    r.foreign = Ok(vec![group(10), group(11)]);
    let full = decide_unleased(&dflt(), 2, &r);
    assert!(!full.admit, "no floor: the census fills the ceiling");
    assert!(full.withheld[0].contains("fill the ceiling 2"), "{full:?}");
    r.foreign = Ok(Vec::new());
    assert!(
        !decide_unleased(&dflt(), 0, &r).admit,
        "a zero ceiling admits nothing"
    );
    r.pressure = Ok(pressure(PressureLevel::Warn, 50.0));
    assert!(
        !decide_unleased(&dflt(), 2, &r).admit,
        "pressure gates unleased builds too"
    );
}

/// Owner ruling 2026-09-21 on #8261: never unlimited admission. With no lease
/// and no census, nothing bounds a build, so none starts — the error arm the
/// critic-round-1 table had left admitting.
#[test]
fn no_lease_and_no_census_admits_nothing() {
    let mut r = quiet();
    r.foreign = Err("the process table sampled empty".into());
    let blind = decide_unleased(&dflt(), 2, &r);
    assert!(!blind.admit, "{blind:?}");
    assert!(
        blind.withheld[0].contains("process census is unreadable")
            && blind.withheld[0].contains("the process table sampled empty"),
        "the refusal names the failure: {blind:?}"
    );
    assert!(blind.readings.contains("census UNREADABLE"), "{blind:?}");
}
