//! Cron parsing, `every` slots, jitter, missed-slot counts (MB1595).

use trusty_architect::sched::cron::Cron;
use trusty_architect::sched::slot::{Sched, day_key, jitter, missed_slots, parse_every, parse_iso};

/// 2023-11-15T00:00:00Z.
const T0: i64 = 1_700_006_400;

#[test]
fn cron_parses_lists_ranges_and_steps() {
    // 12:00 UTC daily, the M08 shape.
    let c = Cron::parse("0 12 * * *").unwrap();
    assert!(c.matches(T0 + 12 * 3600));
    assert!(!c.matches(T0 + 13 * 3600));
    // Lists, ranges, steps and a step on a single value.
    let c = Cron::parse("0,30 8-10 */2 * *").unwrap();
    assert!(c.matches(parse_iso("2023-11-15T09:30:00Z").unwrap())); // 15th is odd: 1,3,..,31
    assert!(!c.matches(parse_iso("2023-11-16T09:30:00Z").unwrap()));
    assert!(!c.matches(parse_iso("2023-11-15T09:15:00Z").unwrap()));
    assert!(Cron::parse("5/20 * * * *").unwrap().matches(T0 + 45 * 60));
}

#[test]
fn cron_rejects_bad_fields() {
    for bad in [
        "",
        "* * * *",
        "* * * * * *",
        "60 * * * *",
        "* 24 * * *",
        "* * 0 * *",
        "* * * 13 *",
        "* * * * 8",
        "5-1 * * * *",
        "*/0 * * * *",
        "a * * * *",
        ", * * * *",
    ] {
        assert!(Cron::parse(bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn cron_next_after_finds_the_next_slot() {
    let c = Cron::parse("0 12 * * *").unwrap();
    assert_eq!(c.next_after(T0), Some(T0 + 12 * 3600));
    // Exactly on a slot: the next one is strictly after.
    assert_eq!(c.next_after(T0 + 12 * 3600), Some(T0 + 36 * 3600));
    let every_5 = Cron::parse("*/5 * * * *").unwrap();
    assert_eq!(every_5.next_after(T0 + 61), Some(T0 + 300));
    // 29 February is found across years.
    let leap = Cron::parse("0 0 29 2 *").unwrap();
    assert_eq!(leap.next_after(T0), parse_iso("2024-02-29T00:00:00Z"));
    // A date that never exists gives None instead of looping.
    assert_eq!(Cron::parse("0 0 31 2 *").unwrap().next_after(T0), None);
}

#[test]
fn cron_dom_or_dow_when_both_restricted() {
    // 2023-11-15 is a Wednesday. Both fields restricted: either may match.
    let c = Cron::parse("0 0 20 * 3").unwrap();
    assert!(c.matches(T0)); // Wednesday, not the 20th
    assert!(c.matches(parse_iso("2023-11-20T00:00:00Z").unwrap())); // the 20th, a Monday
    assert!(!c.matches(parse_iso("2023-11-21T00:00:00Z").unwrap()));
    // Only dow restricted: dom `*` must hold too.
    let c = Cron::parse("0 0 * * 3").unwrap();
    assert!(!c.matches(parse_iso("2023-11-20T00:00:00Z").unwrap()));
}

#[test]
fn cron_dow_seven_is_sunday() {
    // 2023-11-19 is a Sunday.
    let sunday = parse_iso("2023-11-19T00:00:00Z").unwrap();
    assert!(Cron::parse("0 0 * * 0").unwrap().matches(sunday));
    assert!(Cron::parse("0 0 * * 7").unwrap().matches(sunday));
    assert!(!Cron::parse("0 0 * * 7").unwrap().matches(sunday + 86_400));
}

#[test]
fn every_parses_units() {
    assert_eq!(parse_every("30s"), Some(30));
    assert_eq!(parse_every("10m"), Some(600));
    assert_eq!(parse_every("2h"), Some(7200));
    assert_eq!(parse_every("1d"), Some(86_400));
    for bad in ["", "m", "0m", "-5m", "10", "10x", "1.5h", "+5m", "5 m"] {
        assert_eq!(parse_every(bad), None, "{bad:?}");
    }
}

#[test]
fn every_grid_does_not_drift() {
    let s = Sched::Every(600);
    // A late observation does not shift the grid: the next slot stays on it.
    assert_eq!(s.next_after(T0, T0 + 650), Some(T0 + 1200));
    assert_eq!(s.next_after(T0, T0 + 600), Some(T0 + 1200));
    assert_eq!(s.next_after(T0, T0 - 5), Some(T0));
}

#[test]
fn jitter_is_deterministic_and_bounded() {
    assert_eq!(jitter("a", 100, 0), 0);
    let j = jitter("meta-due", T0, 30);
    assert_eq!(j, jitter("meta-due", T0, 30));
    assert!((0..=30).contains(&j));
    // Different slots spread: 200 slots cannot all land on one value.
    let spread: std::collections::HashSet<i64> = (0..200)
        .map(|i| jitter("meta-due", T0 + i * 600, 30))
        .collect();
    assert!(spread.len() > 5);
}

#[test]
fn missed_slots_counts_the_gap() {
    assert_eq!(missed_slots(&Sched::Every(600), T0, T0 + 3000), 5);
    assert_eq!(missed_slots(&Sched::Every(600), T0, T0 + 599), 0);
    let c = Sched::Cron(Cron::parse("*/10 * * * *").unwrap());
    assert_eq!(missed_slots(&c, T0, T0 + 3000), 5);
    assert_eq!(day_key(T0), "2023-11-15");
    assert_eq!(day_key(T0 - 1), "2023-11-14");
}
