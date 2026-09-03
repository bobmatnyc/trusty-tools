//! Unit tests for the PM effort scorer and its payload extractor
//! (issue #3915).
//!
//! Why a separate file: keeps `mod.rs` and `extract.rs` under the 500-SLOC
//! production cap while covering every bucket, the recency floor, and each
//! story-point spelling on fixtures.

use super::extract::extract_fields;
use super::{
    is_age_floored_type, plausible_story_points, score, thresholds, EffortBucket, EffortCounts,
    InputsPresent, PmEffortInput, ScoreStatus, FORMULA_VERSION,
};

/// A ticket with no signal at all, for tests that vary one input.
fn bare(item_type: &str) -> PmEffortInput<'_> {
    PmEffortInput {
        item_type,
        age_days: Some(365),
        counts: EffortCounts::default(),
    }
}

/// The scored `effort_score`, or a panic naming the status that blocked it.
fn scored(input: &PmEffortInput<'_>) -> f64 {
    let verdict = score(input);
    verdict
        .effort_score
        .unwrap_or_else(|| panic!("expected a score, got status {}", verdict.status))
}

// --- versioning ------------------------------------------------------------

#[test]
fn scorer_reports_the_v1_formula_version() {
    assert_eq!(FORMULA_VERSION, "pm-effort-1");
}

#[test]
fn effort_bucket_round_trips_through_its_wire_string() {
    for bucket in [EffortBucket::Low, EffortBucket::Medium, EffortBucket::High] {
        let wire = bucket.as_wire_str();
        assert_eq!(
            EffortBucket::from_wire_str(wire),
            Some(bucket),
            "{wire} must round-trip"
        );
    }
    assert_eq!(
        EffortBucket::from_wire_str("XXL"),
        None,
        "an unknown bucket must not be read as LOW"
    );
}

#[test]
fn score_status_round_trips_through_its_wire_string() {
    for status in [ScoreStatus::Scored, ScoreStatus::DeferredRecent] {
        let wire = status.as_wire_str();
        assert_eq!(ScoreStatus::from_wire_str(wire), Some(status));
    }
    assert_eq!(ScoreStatus::from_wire_str("DEFERRED_NO_PARENT"), None);
}

/// The v1 weights are chosen so a ticket that maxes out every term lands
/// exactly on the documented ceiling. A retune that breaks this arithmetic
/// has silently changed the range the buckets were calibrated against.
#[test]
fn weights_sum_to_the_documented_score_ceiling() {
    let ceiling = thresholds::BASE_SCORE
        + f64::from(thresholds::CHILD_COUNT_CAP) * thresholds::CHILD_WEIGHT
        + thresholds::BODY_POINTS_CAP
        + f64::from(thresholds::COMMENT_COUNT_CAP) * thresholds::COMMENT_WEIGHT
        + f64::from(thresholds::TRANSITION_COUNT_CAP) * thresholds::TRANSITION_WEIGHT
        + thresholds::STORY_POINT_CAP;
    assert!(
        (ceiling - thresholds::MAX_SCORE).abs() < f64::EPSILON,
        "capped terms sum to {ceiling}, not MAX_SCORE {}",
        thresholds::MAX_SCORE
    );
}

#[test]
fn bucket_boundaries_are_the_documented_thresholds() {
    assert_eq!(EffortBucket::of_score(1.0), EffortBucket::Low);
    assert_eq!(EffortBucket::of_score(14.99), EffortBucket::Low);
    assert_eq!(
        EffortBucket::of_score(thresholds::BUCKET_MEDIUM_MIN),
        EffortBucket::Medium,
        "the boundary belongs to the higher bucket"
    );
    assert_eq!(EffortBucket::of_score(29.99), EffortBucket::Medium);
    assert_eq!(
        EffortBucket::of_score(thresholds::BUCKET_HIGH_MIN),
        EffortBucket::High
    );
    assert_eq!(EffortBucket::of_score(50.0), EffortBucket::High);
}

// --- buckets ---------------------------------------------------------------

/// A bare meaningful ticket scores the base and nothing else: LOW.
#[test]
fn a_ticket_with_no_signal_records_no_inputs() {
    let verdict = score(&bare("Task"));
    assert_eq!(verdict.status, ScoreStatus::Scored);
    assert_eq!(verdict.effort_score, Some(1.0));
    assert_eq!(verdict.effort_bucket, Some(EffortBucket::Low));
    assert_eq!(verdict.inputs_present, InputsPresent::default());
    assert_eq!(verdict.inputs_present.to_wire_string(), "NONE");
}

/// A story with a real body and some discussion: MEDIUM.
#[test]
fn a_discussed_story_scores_in_the_medium_bucket() {
    let mut input = bare("Story");
    input.counts = EffortCounts {
        epic_children: 0,
        description_words: 320, // 320/40 = 8.0
        comments: 6,            // 6 * 0.8 = 4.8
        transitions: 4,         // 4 * 0.5 = 2.0
        story_points: None,
    };
    // 1.0 + 8.0 + 4.8 + 2.0 = 15.8
    let verdict = score(&input);
    assert_eq!(verdict.effort_score, Some(15.8));
    assert_eq!(verdict.effort_bucket, Some(EffortBucket::Medium));
}

/// A decomposed, long-lived epic: HIGH.
#[test]
fn a_substantive_epic_scores_in_the_high_bucket() {
    let mut input = bare("Epic");
    input.counts = EffortCounts {
        epic_children: 9,        // 9 * 2.0 = 18.0
        description_words: 400,  // 10.0
        comments: 5,             // 4.0
        transitions: 6,          // 3.0
        story_points: Some(8.0), // 3.2 -> capped at 3.0
    };
    // 1.0 + 18.0 + 10.0 + 4.0 + 3.0 + 3.0 = 39.0
    let verdict = score(&input);
    assert_eq!(verdict.effort_score, Some(39.0));
    assert_eq!(verdict.effort_bucket, Some(EffortBucket::High));
    assert_eq!(
        verdict.inputs_present.to_wire_string(),
        "CHILDREN,DESCRIPTION,COMMENTS,TRANSITIONS,STORY_POINTS"
    );
}

#[test]
fn each_term_is_capped_independently() {
    // Children past the cap add nothing.
    let mut many_children = bare("Epic");
    many_children.counts.epic_children = thresholds::CHILD_COUNT_CAP + 40;
    let mut at_cap = bare("Epic");
    at_cap.counts.epic_children = thresholds::CHILD_COUNT_CAP;
    assert_eq!(scored(&many_children), scored(&at_cap));

    // A pasted 100k-word log cannot dominate.
    let mut huge_body = bare("Bug");
    huge_body.counts.description_words = 100_000;
    assert_eq!(
        scored(&huge_body),
        thresholds::BASE_SCORE + thresholds::BODY_POINTS_CAP
    );

    // Comments and transitions likewise.
    let mut chatty = bare("Bug");
    chatty.counts.comments = 500;
    chatty.counts.transitions = 500;
    assert_eq!(
        scored(&chatty),
        thresholds::BASE_SCORE
            + f64::from(thresholds::COMMENT_COUNT_CAP) * thresholds::COMMENT_WEIGHT
            + f64::from(thresholds::TRANSITION_COUNT_CAP) * thresholds::TRANSITION_WEIGHT
    );
}

#[test]
fn the_score_never_leaves_the_documented_range() {
    let mut maxed = bare("Epic");
    maxed.counts = EffortCounts {
        epic_children: u32::MAX,
        description_words: u32::MAX,
        comments: u32::MAX,
        transitions: u32::MAX,
        story_points: Some(thresholds::STORY_POINTS_MAX),
    };
    assert_eq!(scored(&maxed), thresholds::MAX_SCORE);
    assert_eq!(scored(&bare("Task")), thresholds::MIN_SCORE);
}

// --- story points ----------------------------------------------------------

/// Issue #3915: story_points is 76% NULL, so a missing value must degrade
/// the formula to its other inputs, never zero the score.
#[test]
fn missing_story_points_degrade_rather_than_zero_the_score() {
    let mut with_points = bare("Story");
    with_points.counts = EffortCounts {
        epic_children: 2,
        description_words: 160,
        comments: 3,
        transitions: 2,
        story_points: Some(5.0),
    };
    let mut without = with_points;
    without.counts.story_points = None;

    // 1.0 + 4.0 + 4.0 + 2.4 + 1.0 = 12.4, plus 2.0 of story points.
    assert_eq!(scored(&with_points), 14.4);
    assert_eq!(
        scored(&without),
        12.4,
        "the other four terms must still produce a score"
    );

    let verdict = score(&without);
    assert!(
        !verdict.inputs_present.story_points,
        "the row must say the story-point term did not fire"
    );
    assert_eq!(
        verdict.inputs_present.to_wire_string(),
        "CHILDREN,DESCRIPTION,COMMENTS,TRANSITIONS"
    );
}

#[test]
fn implausible_story_points_are_treated_as_absent() {
    for bad in [
        0.0,
        -3.0,
        thresholds::STORY_POINTS_MAX + 1.0,
        86_400_000.0,
        f64::NAN,
        f64::INFINITY,
    ] {
        assert_eq!(
            plausible_story_points(bad),
            None,
            "{bad} must not be read as an estimate"
        );
        let mut input = bare("Story");
        input.counts.story_points = Some(bad);
        let verdict = score(&input);
        assert_eq!(
            verdict.effort_score,
            Some(thresholds::MIN_SCORE),
            "{bad} must degrade to the base score, not crash or skew it"
        );
        assert!(!verdict.inputs_present.story_points);
    }

    for good in [
        thresholds::STORY_POINTS_MIN,
        3.0,
        thresholds::STORY_POINTS_MAX,
    ] {
        assert_eq!(plausible_story_points(good), Some(good));
    }
}

#[test]
fn inputs_present_names_only_contributing_terms() {
    let mut input = bare("Epic");
    input.counts.comments = 2;
    let verdict = score(&input);
    assert_eq!(verdict.inputs_present.to_wire_string(), "COMMENTS");

    // Fewer words than one point still contributes a fraction, so the flag
    // fires; zero words does not.
    let mut one_word = bare("Task");
    one_word.counts.description_words = 1;
    assert!(score(&one_word).inputs_present.description);
    assert!(!score(&bare("Task")).inputs_present.description);
}

// --- recency floor ---------------------------------------------------------

/// Issue #3915's motivating case: three epics authored the same day with
/// zero children. A zero score would read as "no complexity"; the correct
/// record is "too early to tell".
#[test]
fn a_recent_epic_is_deferred_rather_than_scored_low() {
    for age in [0, 1, thresholds::RECENCY_FLOOR_DAYS - 1] {
        let mut input = bare("Epic");
        input.age_days = Some(age);
        let verdict = score(&input);
        assert_eq!(
            verdict.status,
            ScoreStatus::DeferredRecent,
            "an epic {age} day(s) old must not be scored"
        );
        assert_eq!(verdict.effort_score, None, "never a zero, always NULL");
        assert_eq!(verdict.effort_bucket, None);
        assert_eq!(verdict.inputs_present, InputsPresent::default());
    }
}

#[test]
fn an_epic_at_or_past_the_floor_is_scored() {
    let mut input = bare("Epic");
    input.age_days = Some(thresholds::RECENCY_FLOOR_DAYS);
    let verdict = score(&input);
    assert_eq!(verdict.status, ScoreStatus::Scored);
    assert_eq!(verdict.effort_score, Some(thresholds::MIN_SCORE));
}

/// The floor guards types whose child count accrues after creation. A bug
/// filed yesterday has no such input, so deferring it would withhold a
/// score for nothing.
#[test]
fn the_recency_floor_applies_only_to_decomposable_types() {
    for floored in ["Epic", "epic", " EPIC ", "Feature", "Initiative"] {
        assert!(is_age_floored_type(floored), "{floored} must be floored");
        let mut input = bare(floored);
        input.age_days = Some(2);
        assert_eq!(score(&input).status, ScoreStatus::DeferredRecent);
    }
    for open in ["Bug", "Task", "Sub-task", "User Story", ""] {
        assert!(!is_age_floored_type(open), "{open} must not be floored");
        let mut input = bare(open);
        input.age_days = Some(2);
        assert_eq!(
            score(&input).status,
            ScoreStatus::Scored,
            "{open} carries no child input, so the floor must not apply"
        );
    }
}

/// A payload with no parseable creation date cannot be aged, so the floor
/// cannot fire. Scoring on the other inputs is better than withholding a
/// score for every ticket whose provider spelled `created` differently.
#[test]
fn an_epic_with_an_unknown_age_is_scored_not_deferred() {
    let mut input = bare("Epic");
    input.age_days = None;
    input.counts.epic_children = 3;
    let verdict = score(&input);
    assert_eq!(verdict.status, ScoreStatus::Scored);
    assert_eq!(verdict.effort_score, Some(7.0));
}

// --- extractor -------------------------------------------------------------

#[test]
fn extracts_story_points_from_a_named_field() {
    for (key, raw) in [
        ("Story Points", serde_json::json!(5)),
        ("story_points", serde_json::json!(5.0)),
        ("storyPoints", serde_json::json!("5")),
        ("Story point estimate", serde_json::json!(5)),
        ("estimate", serde_json::json!(5)),
    ] {
        let payload = serde_json::json!({ "fields": { key: raw } }).to_string();
        assert_eq!(
            extract_fields(Some(&payload)).story_points,
            Some(5.0),
            "spelling {key} must be recognised"
        );
    }
}

/// Issue #3915: the source instance spells the field as four different
/// per-project custom-field IDs, so one global lookup is not enough.
#[test]
fn extracts_story_points_from_each_known_custom_field_id() {
    for id in thresholds::STORY_POINT_FIELD_IDS {
        let payload = serde_json::json!({
            "fields": { *id: 13.0, "summary": "Blend the occupancy forecast" }
        })
        .to_string();
        assert_eq!(
            extract_fields(Some(&payload)).story_points,
            Some(13.0),
            "{id} must be recognised"
        );
    }
}

#[test]
fn an_unrecognised_custom_field_yields_no_story_points() {
    let payload = serde_json::json!({ "fields": { "customfield_99999": 8.0 } }).to_string();
    assert_eq!(extract_fields(Some(&payload)).story_points, None);
}

#[test]
fn an_implausible_stored_value_is_extracted_as_absent() {
    let payload = serde_json::json!({ "fields": { "customfield_10004": 86_400_000 } }).to_string();
    assert_eq!(
        extract_fields(Some(&payload)).story_points,
        None,
        "a duration parked in the story-point field is not an estimate"
    );
}

#[test]
fn extracts_a_jira_parent_key() {
    let payload = serde_json::json!({ "fields": { "parent": { "key": "ML-2314" } } }).to_string();
    assert_eq!(
        extract_fields(Some(&payload)).parent_key,
        Some("ML-2314".to_string())
    );
}

#[test]
fn extracts_a_parent_key_from_each_provider_spelling() {
    for payload in [
        serde_json::json!({ "fields": { "System.Parent": 4417 } }),
        serde_json::json!({ "parent": { "identifier": "4417" } }),
        serde_json::json!({ "parent": { "number": 4417 } }),
    ] {
        assert_eq!(
            extract_fields(Some(&payload.to_string())).parent_key,
            Some("4417".to_string()),
            "payload {payload} must yield a parent key"
        );
    }
}

#[test]
fn unparseable_payload_yields_no_fields() {
    for raw in [None, Some("not json"), Some("[]"), Some("{}")] {
        let fields = extract_fields(raw);
        assert_eq!(fields.story_points, None, "{raw:?}");
        assert_eq!(fields.parent_key, None, "{raw:?}");
    }
}
