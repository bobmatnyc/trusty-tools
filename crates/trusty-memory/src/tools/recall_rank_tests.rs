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
    demote_stale_snapshots(&mut results, now, &Supersessions::new());
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
    demote_stale_snapshots_across(&mut results, now, &Supersessions::new());
    assert_eq!(results[0].palace_id, "b");
}

/// Why: tags match exactly — a tag that merely contains a ruling or snapshot
/// word must not change a drawer's weight.
#[test]
fn tags_match_exactly_never_by_substring() {
    let now = Utc::now();
    for tag in ["decisionless", "rulings", "Decision", "bob-ruling-draft"] {
        let d = drawer("x", &[tag, "status"], Duration::days(30), now);
        assert!(!is_ruling(&d), "tag {tag} is not a ruling");
        assert!(temporal_weight(&d, now) < 0.51, "tag {tag} exempts nothing");
    }
    for tag in ["statusbar", "snapshots", "resume"] {
        let d = drawer("x", &[tag], Duration::days(30), now);
        assert_eq!(temporal_weight(&d, now), 1.0, "tag {tag} is not a snapshot");
    }
    let exact = drawer("x", &["decision", "status"], Duration::days(30), now);
    assert!(is_ruling(&exact));
}

/// Why (clock skew): a snapshot written by a host whose clock runs ahead has a
/// future `created_at`; that is age zero, never a weight above 1.
#[test]
fn a_future_created_at_is_age_zero_not_a_boost() {
    let now = Utc::now();
    let future = drawer("f", &["status"], -Duration::days(10), now);
    assert_eq!(temporal_weight(&future, now), 1.0);
    let mut results = vec![
        hit(future, 0.5),
        hit(drawer("p", &["kg"], Duration::days(1), now), 0.6),
    ];
    demote_stale_snapshots(&mut results, now, &Supersessions::new());
    assert_eq!(results[0].drawer.content(), "p", "skew must not lift it");
    assert!((results[1].score - 0.5).abs() < 1e-6);
}

/// Why: the comparator must be a total order; with NaN-as-Equal the sort can
/// leave finite scores unordered.
#[test]
fn a_nan_score_never_unorders_the_finite_scores() {
    let now = Utc::now();
    let mut results: Vec<RecallResult> = [0.1, f32::NAN, 0.9, 0.5, f32::NAN, 0.7]
        .into_iter()
        .map(|s| hit(drawer("x", &["kg"], Duration::zero(), now), s))
        .collect();
    demote_stale_snapshots(&mut results, now, &Supersessions::new());
    let finite: Vec<f32> = results
        .iter()
        .map(|r| r.score)
        .filter(|s| !s.is_nan())
        .collect();
    assert_eq!(finite, [0.9, 0.7, 0.5, 0.1]);
}

/// Why (#8246 review): demotion needs candidates past `top_k` to lift.
#[test]
fn ranking_window_doubles_top_k_and_caps() {
    use crate::tools::recall_projection::MAX_CANDIDATE_WINDOW;
    assert_eq!(ranking_window(10, None), 20);
    assert_eq!(ranking_window(0, None), 0);
    assert_eq!(ranking_window(150, None), MAX_CANDIDATE_WINDOW);
    assert_eq!(ranking_window(500, None), 500, "never below top_k");
    assert_eq!(ranking_window(10, Some(0.4)), 40, "the floor's window wins");
}

/// Tied hits in DESCENDING drawer-id order, so an input-order-preserving sort
/// leaves them backwards.
fn tied_hits_in_reverse_id_order(now: DateTime<Utc>) -> Vec<RecallResult> {
    let mut hits: Vec<RecallResult> = (0..6)
        .map(|_| hit(drawer("tie", &["kg"], Duration::zero(), now), 0.5))
        .collect();
    hits.sort_by_key(|a| std::cmp::Reverse(a.drawer.id));
    hits
}

fn ids_ascending(hits: &[RecallResult]) -> bool {
    hits.windows(2).all(|w| w[0].drawer.id < w[1].drawer.id)
}

/// Why (#9280): equal scores kept the lane's order, and a rebuilt vector index
/// can return tied drawers in a different order on each open.
/// What: six tied hits fed in descending id order must rank by ascending
/// drawer id after demotion, and a lower layer still wins a tie first.
#[test]
fn tied_scores_rank_by_drawer_id_whatever_the_input_order() {
    let now = Utc::now();
    let mut results = tied_hits_in_reverse_id_order(now);
    demote_stale_snapshots(&mut results, now, &Supersessions::new());
    assert!(
        ids_ascending(&results),
        "ties must rank by drawer id: {results:#?}"
    );

    let mut pinned = hit(drawer("tie", &["kg"], Duration::zero(), now), 0.5);
    pinned.layer = 1;
    pinned.drawer.id = Uuid::max();
    results.push(pinned);
    demote_stale_snapshots(&mut results, now, &Supersessions::new());
    assert_eq!(
        results[0].layer, 1,
        "a lower layer wins a tie before the id"
    );
}

/// Why (#9280): the RRF fusion re-sort is the last sort before demotion on the
/// BM25 path, and it left tied drawers in input order.
/// What: six tied hits in descending id order, fused with a BM25 hit that
/// boosts none of them; the fused list must rank them by ascending id.
#[test]
fn fusion_ranks_tied_drawers_by_id_whatever_the_input_order() {
    use crate::bm25_lane::BM25Hit;
    let mut results = tied_hits_in_reverse_id_order(Utc::now());
    let unrelated = BM25Hit {
        doc_id: Uuid::new_v4().to_string(),
        score: 1.0,
    };
    crate::tools::bm25::fuse_bm25_into_recall(&mut results, &[unrelated], 10);
    assert!(
        ids_ascending(&results),
        "ties must rank by drawer id: {results:#?}"
    );
}

/// A fresh, untagged drawer of type `t`, as a recall hit scoring `score`.
fn typed_hit(content: &str, t: DrawerType, score: f32, now: DateTime<Utc>) -> RecallResult {
    let mut d = drawer(content, &[], Duration::zero(), now);
    d.drawer_type = t;
    hit(d, score)
}

/// Why (#9144): with equal similarity and age, a typed ruling must outrank a
/// typed status report — the status line is the one a newer write replaces.
/// What: both fresh at 0.70; after demotion the Status hit scores 0.70 * 0.8
/// and the Ruling keeps 0.70, so the Ruling ranks first whatever its id.
#[test]
fn a_ruling_typed_drawer_outranks_an_equal_status_typed_drawer() {
    let now = Utc::now();
    let status = typed_hit("status", DrawerType::Status, 0.70, now);
    let ruling = typed_hit("ruling", DrawerType::Ruling, 0.70, now);
    let mut results = vec![status, ruling];
    demote_stale_snapshots(&mut results, now, &Supersessions::new());
    let score = |c: &str| {
        results
            .iter()
            .find(|r| r.drawer.content() == c)
            .map(|r| r.score)
    };
    assert_eq!(score("ruling"), Some(0.70));
    let s = score("status").expect("status hit kept");
    assert!((s - 0.70 * 0.8).abs() < 1e-6, "status score {s}");
    assert_eq!(results[0].drawer.content(), "ruling", "{results:#?}");
}

/// Why (#9144): a recorded conversational turn is weaker still than a status.
/// What: as above with Turn, whose factor is 0.7.
#[test]
fn a_ruling_typed_drawer_outranks_an_equal_turn_typed_drawer() {
    let now = Utc::now();
    let turn = typed_hit("turn", DrawerType::Turn, 0.70, now);
    let ruling = typed_hit("ruling", DrawerType::Ruling, 0.70, now);
    let mut results = vec![turn, ruling];
    demote_stale_snapshots(&mut results, now, &Supersessions::new());
    let t = results
        .iter()
        .find(|r| r.drawer.content() == "turn")
        .map(|r| r.score)
        .expect("turn hit kept");
    assert!((t - 0.70 * 0.7).abs() < 1e-6, "turn score {t}");
    assert_eq!(results[0].drawer.content(), "ruling", "{results:#?}");
}

/// Why (#9144): a live Tier C slot is current by construction (ADR-0028 D5),
/// so its type must not demote it; once the slot is retired it is an ordinary
/// Status drawer and takes the 0.8 factor.
#[test]
fn a_live_keyed_status_typed_drawer_is_exempt_from_the_type_factor() {
    let now = Utc::now();
    let mut keyed = drawer("s", &[], Duration::zero(), now);
    keyed.drawer_type = DrawerType::Status;
    keyed.fact_key = Some("ws:s1/resume".to_string());
    keyed.expires_at = Some(now + Duration::hours(1));
    assert_eq!(temporal_weight(&keyed, now), 1.0);
    keyed.fact_key = None;
    keyed.expires_at = None;
    assert!((temporal_weight(&keyed, now) - 0.8).abs() < 1e-6);
}

/// Why (#9144): a typed Ruling or Decision is a ruling for the rank pass and
/// the rulings leg alike; the other typed variants are not, and only Status
/// and Turn carry a factor below 1.
/// What: fresh untagged drawers of each writable type.
#[test]
fn ruling_and_decision_types_are_rulings_and_the_rest_keep_their_factor() {
    let now = Utc::now();
    for (t, ruling, weight) in [
        (DrawerType::Ruling, true, 1.0),
        (DrawerType::Decision, true, 1.0),
        (DrawerType::Reference, false, 1.0),
        (DrawerType::Status, false, 0.8),
        (DrawerType::Turn, false, 0.7),
    ] {
        let mut d = drawer("x", &[], Duration::zero(), now);
        d.drawer_type = t;
        assert_eq!(is_ruling(&d), ruling, "{t:?}");
        assert!((temporal_weight(&d, now) - weight).abs() < 1e-6, "{t:?}");
    }
    // The factor multiplies the snapshot decay: floor 0.5 times 0.8.
    let mut old_status = drawer("x", &["status"], Duration::days(365), now);
    old_status.drawer_type = DrawerType::Status;
    assert!((temporal_weight(&old_status, now) - SNAPSHOT_FLOOR * 0.8).abs() < 1e-6);
}

/// Why (#9391): a `checkpoint`-tagged unkeyed drawer is a point-in-time
/// record like a `status` one; it must decay with age and stay whole when fresh.
/// What: 365 days reaches the floor, one half-life lands midway, fresh is 1.0.
#[test]
fn a_checkpoint_tagged_drawer_is_a_snapshot() {
    let now = Utc::now();
    let ancient = drawer("c", &["checkpoint"], Duration::days(365), now);
    let w = temporal_weight(&ancient, now);
    assert!((w - SNAPSHOT_FLOOR).abs() < 1e-6, "ancient weight {w}");
    let day = drawer("c", &["checkpoint"], Duration::hours(24), now);
    let mid = SNAPSHOT_FLOOR + (1.0 - SNAPSHOT_FLOOR) * 0.5;
    let w = temporal_weight(&day, now);
    assert!((w - mid).abs() < 1e-3, "one half-life weight {w}");
    let fresh = drawer("c", &["checkpoint"], Duration::zero(), now);
    assert_eq!(temporal_weight(&fresh, now), 1.0);
}

/// Why (#9391): exact tag match only — near-miss spellings are not snapshots.
#[test]
fn checkpoint_near_miss_tags_are_not_snapshots() {
    let now = Utc::now();
    for tag in ["checkpointed", "pre-checkpoint", "Checkpoint", "checkpoints"] {
        let d = drawer("x", &[tag], Duration::days(30), now);
        assert_eq!(temporal_weight(&d, now), 1.0, "tag {tag} is not a snapshot");
    }
}

/// Why (#9391): the type factor (#9144) and the snapshot decay compose; a
/// Status or Turn drawer tagged `checkpoint` takes both.
/// What: at the floor (365 days), weight is 0.5 * 0.8 for Status, 0.5 * 0.7
/// for Turn; at one half-life it is mid * factor.
#[test]
fn a_typed_checkpoint_takes_both_the_type_factor_and_the_decay() {
    let now = Utc::now();
    let mid = SNAPSHOT_FLOOR + (1.0 - SNAPSHOT_FLOOR) * 0.5;
    for (t, factor) in [(DrawerType::Status, 0.8), (DrawerType::Turn, 0.7)] {
        let mut old = drawer("c", &["checkpoint"], Duration::days(365), now);
        old.drawer_type = t;
        let w = temporal_weight(&old, now);
        assert!((w - SNAPSHOT_FLOOR * factor).abs() < 1e-6, "{t:?} floor {w}");
        let mut day = drawer("c", &["checkpoint"], Duration::hours(24), now);
        day.drawer_type = t;
        let w = temporal_weight(&day, now);
        assert!((w - mid * factor).abs() < 1e-3, "{t:?} half-life {w}");
        let mut fresh = drawer("c", &["checkpoint"], Duration::zero(), now);
        fresh.drawer_type = t;
        let w = temporal_weight(&fresh, now);
        assert!((w - factor).abs() < 1e-6, "{t:?} fresh {w}");
    }
}

/// Why (#9391): a live Tier C slot stays exempt, checkpoint tag or not; once
/// retired it is an unkeyed snapshot and decays.
#[test]
fn a_live_keyed_checkpoint_is_exempt() {
    let now = Utc::now();
    let mut keyed = drawer("c", &["checkpoint"], Duration::days(3), now);
    keyed.fact_key = Some("ws:s1/checkpoint".to_string());
    keyed.expires_at = Some(now + Duration::hours(1));
    assert_eq!(temporal_weight(&keyed, now), 1.0);
    keyed.fact_key = None;
    keyed.expires_at = None;
    assert!(temporal_weight(&keyed, now) < 0.9);
}

/// Why (#9391): the defect itself — a weeks-old checkpoint above fresher
/// material that scores slightly lower.
#[test]
fn a_stale_checkpoint_ranks_below_a_fresher_lower_scoring_drawer() {
    let now = Utc::now();
    let checkpoint = hit(drawer("ckpt", &["checkpoint"], Duration::days(20), now), 0.80);
    let fresh = hit(drawer("fresh", &["kg"], Duration::days(1), now), 0.60);
    let mut results = vec![checkpoint, fresh];
    demote_stale_snapshots(&mut results, now, &Supersessions::new());
    let order: Vec<&str> = results.iter().map(|r| r.drawer.content()).collect();
    assert_eq!(order, ["fresh", "ckpt"], "{results:#?}");
}
