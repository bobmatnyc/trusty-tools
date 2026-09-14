//! Recall-projection tests — creator-tag hiding and the `min_score` floor
//! (owner ruling 2026-09-14, token savings).
//!
//! Why: both changes are projections over a recall result set, so they are
//! provable against synthetic `RecallResult`s without a palace, an embedder, or
//! a daemon. Driving them through a live recall would test the retrieval lanes
//! instead, and could not place a hit at exactly 0.38 or 0.46 — the band this
//! ruling is about.
//! What: builds drawers carrying the four `creator:*` tags a real MCP write
//! attaches plus topical ones, then asserts on the SERIALIZED JSON rather than
//! on the intermediate `Vec`, because the wire shape is what a caller pays for.
//! Test: this IS the test module.

use crate::tools::recall_projection::{
    apply_score_floor, include_creator_tags_arg, min_score_arg, serialize_recall, RecallProjection,
};
use serde_json::json;
use trusty_common::memory_core::palace::Drawer;
use trusty_common::memory_core::retrieval::RecallResult;
use uuid::Uuid;

/// The attribution a real MCP write attaches to every drawer (DOC-53).
const CREATOR_TAGS: [&str; 4] = [
    "creator:client=claude-code",
    "creator:version=0.26.1",
    "creator:source=mcp",
    "creator:cwd=/Users/masa/projects/trusty-tools",
];

/// Build one hit at `layer`/`score` carrying both topical and creator tags.
fn hit(layer: u8, score: f32, topical: &[&str]) -> RecallResult {
    let mut drawer = Drawer::new(Uuid::nil(), "recall projection fixture body");
    drawer.importance = 0.5;
    drawer.tags = topical
        .iter()
        .map(|t| (*t).to_string())
        .chain(CREATOR_TAGS.iter().map(|t| (*t).to_string()))
        .collect();
    RecallResult {
        drawer,
        score,
        layer,
    }
}

/// The `tags` array of every hit in a serialized recall response.
fn tag_arrays(response: &serde_json::Value) -> Vec<Vec<String>> {
    response["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| {
            r["tags"]
                .as_array()
                .expect("tags array")
                .iter()
                .map(|t| t.as_str().expect("tag string").to_string())
                .collect()
        })
        .collect()
}

fn no_floor(include_creator_tags: bool) -> RecallProjection {
    RecallProjection {
        include_creator_tags,
        dropped_below_floor: 0,
    }
}

/// Why: the default projection is the whole point of the ruling — a caller that
/// changes nothing must stop paying for provenance.
/// What: serializes one hit with four creator tags and two topical ones under
/// the default projection and asserts on the emitted `tags` values.
/// Test: this test.
#[test]
fn creator_tags_are_hidden_by_default() {
    let response = serialize_recall(
        "alpha",
        "q",
        vec![hit(2, 0.9, &["rust", "retrieval"])],
        &no_floor(false),
    );
    assert_eq!(tag_arrays(&response), vec![vec!["rust", "retrieval"]]);
}

/// Why: hiding must be a projection, not a deletion — an audit ("who wrote
/// this?") has to be able to get the same attribution back out.
/// What: same fixture with `include_creator_tags: true`; the stored order is
/// preserved, so the topical tags still lead.
/// Test: this test.
#[test]
fn creator_tags_come_back_when_the_flag_is_set() {
    let response = serialize_recall(
        "alpha",
        "q",
        vec![hit(2, 0.9, &["rust", "retrieval"])],
        &no_floor(true),
    );
    let mut expected = vec!["rust".to_string(), "retrieval".to_string()];
    expected.extend(CREATOR_TAGS.iter().map(|t| (*t).to_string()));
    assert_eq!(tag_arrays(&response), vec![expected]);
}

/// Why: 0.38 and 0.46 are the observed band this ruling names, so the floor is
/// asserted at exactly the two values it was chosen to separate.
/// What: a floor of 0.4 over one 0.38 hit and one 0.46 hit.
/// Test: this test.
#[test]
fn score_floor_drops_below_and_keeps_above() {
    let mut results = vec![hit(2, 0.46, &["kept"]), hit(2, 0.38, &["dropped"])];
    let dropped = apply_score_floor(&mut results, Some(0.4), 10);
    assert_eq!(dropped, 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].drawer.tags[0], "kept");
}

/// Why: L0 scores a flat 1.0 and L1 scores by importance, so neither is a
/// similarity — a floor that filtered them would strip the palace's baseline
/// grounding on a number that never meant relevance.
/// What: L0 and L1 hits scored at 0.10 and 0.25 against a 0.4 floor, alongside
/// an L2 hit at 0.30 that must go.
/// Test: this test.
#[test]
fn score_floor_keeps_identity_and_essential_layers() {
    let mut results = vec![
        hit(0, 0.10, &["identity"]),
        hit(1, 0.25, &["essential"]),
        hit(2, 0.30, &["semantic"]),
    ];
    let dropped = apply_score_floor(&mut results, Some(0.4), 10);
    assert_eq!(dropped, 1);
    let survivors: Vec<&str> = results.iter().map(|r| r.drawer.tags[0].as_str()).collect();
    assert_eq!(survivors, vec!["identity", "essential"]);
}

/// Why: the ordering is the contract. Truncating first would let a below-floor
/// hit occupy one of the caller's `top_k` slots and then disappear, so the
/// caller would pay a slot for a hit it never sees.
/// What: six hits alternating above and below a 0.4 floor, `top_k` 3. Filtering
/// first leaves the three above-floor hits; truncating first would leave two.
/// Test: this test.
#[test]
fn score_floor_applies_before_top_k() {
    let mut results = vec![
        hit(2, 0.90, &["a"]),
        hit(2, 0.35, &["b"]),
        hit(2, 0.80, &["c"]),
        hit(2, 0.30, &["d"]),
        hit(2, 0.70, &["e"]),
        hit(2, 0.20, &["f"]),
    ];
    let dropped = apply_score_floor(&mut results, Some(0.4), 3);
    assert_eq!(dropped, 3);
    let kept: Vec<&str> = results.iter().map(|r| r.drawer.tags[0].as_str()).collect();
    assert_eq!(kept, vec!["a", "c", "e"]);
}

/// Why: a short result list is ambiguous between "the corpus had no more" and
/// "the floor removed them", and a caller tuning the floor needs to tell those
/// apart.
/// What: asserts the envelope carries the count, and that it is present as `0`
/// when no floor ran.
/// Test: this test.
#[test]
fn recall_response_reports_the_dropped_count() {
    let mut results = vec![hit(2, 0.46, &["kept"]), hit(2, 0.38, &["gone"])];
    let dropped_below_floor = apply_score_floor(&mut results, Some(0.4), 10);
    let filtered = serialize_recall(
        "alpha",
        "q",
        results,
        &RecallProjection {
            include_creator_tags: false,
            dropped_below_floor,
        },
    );
    assert_eq!(filtered["dropped_below_floor"], json!(1));

    let unfiltered = serialize_recall("alpha", "q", vec![hit(2, 0.9, &["x"])], &no_floor(false));
    assert_eq!(unfiltered["dropped_below_floor"], json!(0));
}

/// Why: existing callers pass neither argument and must see no change.
/// What: an args object with only `query` yields no floor and no attribution.
/// Test: this test.
#[test]
fn min_score_arg_is_absent_by_default() {
    let args = json!({ "query": "anything" });
    assert_eq!(min_score_arg(&args), None);
    assert!(!include_creator_tags_arg(&args));
}

/// Why: a floor arriving as a JSON string is a client bug, and silently reading
/// it as `0.0` would filter nothing while looking like it filtered.
/// What: a quoted number resolves to "no floor"; a real number is taken, as is
/// an integer, which JSON does not distinguish from `0.4`'s type.
/// Test: this test.
#[test]
fn min_score_arg_ignores_a_non_numeric_floor() {
    assert_eq!(min_score_arg(&json!({ "min_score": "0.4" })), None);
    assert_eq!(min_score_arg(&json!({ "min_score": true })), None);
    assert_eq!(min_score_arg(&json!({ "min_score": 0.4 })), Some(0.4));
    assert_eq!(min_score_arg(&json!({ "min_score": 1 })), Some(1.0));
}

/// Why: the ruling's stated motive is token savings, so the saving is measured
/// rather than asserted in prose.
/// What: serializes the same eight-hit response both ways and compares byte
/// lengths. Observed on this fixture: 2651 bytes with the creator tags, 1667
/// without — a 37% cut. The assertion is a 30% floor rather than those exact
/// numbers, so it pins the direction and rough magnitude without becoming a
/// golden file that a field rename would break. Run with `-- --nocapture` for
/// the numbers.
/// Test: this test.
#[test]
fn eight_hit_recall_response_shrinks_without_creator_tags() {
    let results: Vec<RecallResult> = (0..8)
        .map(|i| hit(2, 0.9 - (i as f32) * 0.05, &["rust", "retrieval"]))
        .collect();
    let with_creator = serialize_recall("alpha", "q", results.clone(), &no_floor(true)).to_string();
    let without_creator = serialize_recall("alpha", "q", results, &no_floor(false)).to_string();
    println!(
        "8-hit recall response: {} bytes with creator tags, {} bytes without ({} saved)",
        with_creator.len(),
        without_creator.len(),
        with_creator.len() - without_creator.len()
    );
    assert!(
        without_creator.len() * 10 <= with_creator.len() * 7,
        "expected the default projection to cut at least 30% of the response: \
         {} bytes vs {} bytes",
        without_creator.len(),
        with_creator.len()
    );
}
