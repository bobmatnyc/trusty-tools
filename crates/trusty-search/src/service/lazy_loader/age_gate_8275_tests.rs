//! #8275: warm-boot age gate — selection and env parsing.
//!
//! Why: warm-boot loaded the top N indexes by recency however old the stamp
//! was, so indexes untouched for days filled the boot set.
//! What: drives `select_fresh_warmboot_entries` and
//! `parse_warmboot_max_age` directly — no env writes, no daemon.
//! Test: this file IS the test module.

use std::path::PathBuf;
use std::time::Duration;

use crate::service::lazy_loader::{
    parse_warmboot_max_age, select_fresh_warmboot_entries, DEFAULT_WARMBOOT_MAX_AGE_HOURS,
};
use crate::service::persistence::PersistedIndex;

const NOW: u64 = 1_790_000_000;
const HOUR: u64 = 3600;
const DAY: Option<Duration> = Some(Duration::from_secs(24 * HOUR));

fn entry(id: &str, last_queried_unix: Option<u64>) -> PersistedIndex {
    PersistedIndex {
        id: id.to_string(),
        root_path: PathBuf::from(format!("/tmp/{id}")),
        last_queried_unix,
        ..Default::default()
    }
}

fn ids(entries: &[PersistedIndex]) -> Vec<&str> {
    let mut v: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
    v.sort_unstable();
    v
}

/// 12 entries, 3 queried inside 24 h, cap 10: exactly the 3 fresh ones load.
///
/// Why: on origin/main the cap is the only filter, so the 7 stale entries
/// ranked next fill the remaining slots and 10 load.
/// Test: this test.
#[test]
fn age_gate_keeps_only_the_fresh_entries_under_the_cap() {
    let mut entries = vec![
        entry("fresh-1h", Some(NOW - HOUR)),
        entry("fresh-12h", Some(NOW - 12 * HOUR)),
        entry("fresh-23h", Some(NOW - 23 * HOUR)),
    ];
    for i in 0..9u64 {
        // 40 h to 120 h old — the 2026-10-04 boot set was 40–67 h old.
        entries.push(entry(
            &format!("stale-{i}"),
            Some(NOW - (40 + 10 * i) * HOUR),
        ));
    }
    let (eager, cold) = select_fresh_warmboot_entries(entries, Some(10), DAY, NOW);
    assert_eq!(ids(&eager), vec!["fresh-12h", "fresh-1h", "fresh-23h"]);
    assert_eq!(cold.len(), 9, "every stale entry is parked for a lazy load");
    assert!(cold.iter().all(|e| e.id.starts_with("stale-")));
}

/// `0` hours (parsed to `None`) means no age limit: the cap alone decides.
///
/// Why: operators who want the pre-#8275 ranking set `0`.
/// Test: this test.
#[test]
fn age_gate_zero_means_no_limit() {
    let entries: Vec<_> = (0..12u64)
        .map(|i| entry(&format!("idx-{i:02}"), Some(NOW - (i + 1) * 30 * HOUR)))
        .collect();
    let no_limit = parse_warmboot_max_age(Some("0"));
    assert!(no_limit.is_none());
    let (eager, cold) = select_fresh_warmboot_entries(entries, Some(10), no_limit, NOW);
    assert_eq!(eager.len(), 10, "with no age limit the cap alone applies");
    assert_eq!(
        ids(&cold),
        vec!["idx-10", "idx-11"],
        "the two oldest go cold"
    );
}

/// An index with no recency stamp at all is stale, even with free slots.
///
/// Why: origin/main loads a never-used entry whenever the cap has room
/// (`entries.len() <= n` returns everything eager). #993 already sorted it
/// last; #8275 keeps it out of the boot set entirely.
/// Test: this test.
#[test]
fn age_gate_treats_never_queried_as_stale() {
    let entries = vec![entry("fresh", Some(NOW - HOUR)), entry("never", None)];
    let (eager, cold) = select_fresh_warmboot_entries(entries, Some(10), DAY, NOW);
    assert_eq!(ids(&eager), vec!["fresh"]);
    assert_eq!(ids(&cold), vec!["never"]);
}

/// The gate also applies with no cap (`TRUSTY_MAX_RESIDENT_INDEXES=off`).
///
/// Why: the cap and the age limit are separate knobs; turning the cap off
/// must not re-admit days-old indexes.
/// Test: this test.
#[test]
fn age_gate_applies_without_a_cap() {
    let entries = vec![
        entry("fresh", Some(NOW - HOUR)),
        entry("stale", Some(NOW - 48 * HOUR)),
    ];
    let (eager, cold) = select_fresh_warmboot_entries(entries, None, DAY, NOW);
    assert_eq!(ids(&eager), vec!["fresh"]);
    assert_eq!(ids(&cold), vec!["stale"]);
}

/// Unset → 24 h, `N` → N h, `0` → no limit, garbage → 24 h (with a warn).
///
/// Why: an unparsable value must never disable the gate.
/// Test: this test.
#[test]
fn warmboot_max_age_parses_hours_zero_and_invalid() {
    let default = Some(Duration::from_secs(DEFAULT_WARMBOOT_MAX_AGE_HOURS * HOUR));
    assert_eq!(DEFAULT_WARMBOOT_MAX_AGE_HOURS, 24);
    assert_eq!(parse_warmboot_max_age(None), default);
    assert_eq!(
        parse_warmboot_max_age(Some(" 6 ")),
        Some(Duration::from_secs(6 * HOUR))
    );
    assert_eq!(parse_warmboot_max_age(Some("0")), None);
    assert_eq!(parse_warmboot_max_age(Some("a day")), default);
    assert_eq!(parse_warmboot_max_age(Some("-1")), default);
    assert_eq!(parse_warmboot_max_age(Some("")), default);
}
