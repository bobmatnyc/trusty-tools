//! Unit tests for `tools::recall_rank` (#8246, #9142).

use super::*;
use chrono::Duration;
use uuid::Uuid;

fn drawer(content: &str, tags: &[&str], age: Duration, now: DateTime<Utc>) -> Drawer {
    let mut d = Drawer::new(Uuid::new_v4(), content);
    d.tags = tags.iter().map(|t| t.to_string()).collect();
    d.created_at = now - age;
    d
}

fn hit(d: Drawer, score: f32) -> RecallResult {
    RecallResult {
        drawer: d,
        score,
        layer: 2,
    }
}

/// Why: the weight is the whole policy; pin its two ends and its half-life.
#[test]
fn snapshot_weight_halves_toward_the_floor() {
    let now = Utc::now();
    let fresh = drawer("s", &["status"], Duration::zero(), now);
    let one_half_life = drawer("s", &["status"], Duration::hours(24), now);
    let ancient = drawer("s", &["status"], Duration::days(365), now);
    assert!((temporal_weight(&fresh, now) - 1.0).abs() < 1e-6);
    let mid = SNAPSHOT_FLOOR + (1.0 - SNAPSHOT_FLOOR) * 0.5;
    assert!((temporal_weight(&one_half_life, now) - mid).abs() < 1e-3);
    assert!((temporal_weight(&ancient, now) - SNAPSHOT_FLOOR).abs() < 1e-6);
}

/// Why: only point-in-time drawers are demoted, and a fresh one still answers
/// "where are we now"; a future `created_at` must not push the weight above 1.
#[test]
fn a_stale_snapshot_loses_score_and_a_fresh_one_does_not() {
    let now = Utc::now();
    for tag in SNAPSHOT_TAGS {
        let stale = drawer("s", &[tag], Duration::days(14), now);
        assert!(temporal_weight(&stale, now) < 0.51, "tag {tag}");
    }
    let mut event = drawer("e", &[], Duration::days(14), now);
    event.drawer_type = DrawerType::SessionEvent;
    assert!(temporal_weight(&event, now) < 0.51);
    let plain = drawer("p", &["kg", "redb"], Duration::days(400), now);
    assert_eq!(temporal_weight(&plain, now), 1.0);
    let skewed = drawer("s", &["status"], -Duration::hours(3), now);
    assert_eq!(temporal_weight(&skewed, now), 1.0);
}

/// Why: a live Tier C slot is current by construction (ADR-0028 D5); once its
/// slot is retired the same drawer is an unkeyed snapshot and is demoted.
#[test]
fn a_live_keyed_snapshot_is_exempt() {
    let now = Utc::now();
    let mut keyed = drawer("s", &["status"], Duration::days(3), now);
    keyed.fact_key = Some("ws:s1/resume".to_string());
    keyed.expires_at = Some(now + Duration::hours(1));
    assert_eq!(temporal_weight(&keyed, now), 1.0);
    keyed.fact_key = None;
    keyed.expires_at = None;
    assert!(temporal_weight(&keyed, now) < 1.0);
}

/// Why: ADR-0028 §C4 found rulings tagged `status` as well; the ruling tag wins.
#[test]
fn ruling_tags_exempt_a_drawer_that_is_also_tagged_status() {
    let now = Utc::now();
    for tag in RULING_TAGS {
        let d = drawer("r", &[tag, "status"], Duration::days(30), now);
        assert!(is_ruling(&d), "tag {tag}");
        assert_eq!(temporal_weight(&d, now), 1.0, "tag {tag}");
    }
    let not_ruling = drawer("x", &["standing-instruction"], Duration::days(1), now);
    assert!(!is_ruling(&not_ruling));
}

/// Why: the defect itself — a more similar but stale snapshot above a ruling.
#[test]
fn demotion_reorders_a_stale_snapshot_below_a_ruling() {
    let now = Utc::now();
    let snapshot = hit(drawer("snap", &["status"], Duration::days(20), now), 0.80);
    let ruling = hit(
        drawer("rule", &["bob-ruling"], Duration::days(1), now),
        0.60,
    );
    let other = hit(drawer("other", &["kg"], Duration::days(1), now), 0.55);
    let mut results = vec![snapshot, ruling, other];
    demote_stale_snapshots(&mut results, now);
    let order: Vec<&str> = results.iter().map(|r| r.drawer.content()).collect();
    assert_eq!(order, ["rule", "other", "snap"]);
    assert_eq!(results.len(), 3, "demotion never drops a hit");
}

#[test]
fn cross_palace_demotion_reorders_the_merged_list() {
    let now = Utc::now();
    let wrap = |palace: &str, r: RecallResult| CrossPalaceResult {
        palace_id: palace.to_string(),
        result: r,
    };
    let mut results = vec![
        wrap(
            "a",
            hit(
                drawer("snap", &["resume-target"], Duration::days(9), now),
                0.9,
            ),
        ),
        wrap(
            "b",
            hit(
                drawer("rule", &["standing-rule"], Duration::days(9), now),
                0.7,
            ),
        ),
    ];
    demote_stale_snapshots_across(&mut results, now);
    assert_eq!(results[0].palace_id, "b");
}
