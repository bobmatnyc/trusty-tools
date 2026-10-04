//! Unit tests for `tools::recall_rulings` (#9143): the env parser, the failed-
//! palace cache, and the fold that merges the leg into a project recall.

use super::*;
use trusty_common::memory_core::Drawer;
use uuid::Uuid;

fn hit(content: &str, tags: &[&str], score: f32, layer: u8) -> RecallResult {
    let mut drawer = Drawer::new(Uuid::new_v4(), content);
    drawer.tags = tags.iter().map(|t| t.to_string()).collect();
    RecallResult {
        drawer,
        score,
        layer,
    }
}

/// Two project hits; the fold must never remove or reorder them.
fn primary() -> Vec<RecallResult> {
    vec![hit("project a", &[], 0.9, 2), hit("project b", &[], 0.8, 2)]
}

fn contents(results: &[RecallResult]) -> Vec<&str> {
    results.iter().map(|r| r.drawer.content()).collect()
}

/// Why: blank entries, surrounding space and repeats in the env value must
/// neither open a palace named "" nor search one palace twice.
#[test]
fn rulings_palace_list_parses_commas_blanks_and_duplicates() {
    assert_eq!(
        parse_rulings_palaces(" rulings-a , ,rulings-b, rulings-a,,"),
        ["rulings-a", "rulings-b"]
    );
    assert!(parse_rulings_palaces("").is_empty());
    assert!(parse_rulings_palaces(" , ,").is_empty());
    let leg = RulingsLeg::new(vec!["x".into(), "x".into()], RULINGS_TIMEOUT);
    assert_eq!(leg.palaces(), ["x"], "the constructor de-duplicates too");
    assert_eq!(rulings_window(3), 32);
    assert_eq!(rulings_window(20), 80);
}

/// Why (#9143 review): a search failure in a rulings palace is reported, and
/// the project's own hits come back untouched.
#[test]
fn a_search_error_degrades_and_keeps_every_primary_hit() {
    let mut results = primary();
    let outcomes = vec![(
        "rulings-a".to_string(),
        Err("search failed: index unreadable".to_string()),
    )];
    let degraded = fold_rulings(&mut results, outcomes, 10, None);
    assert_eq!(contents(&results), ["project a", "project b"]);
    assert_eq!(
        degraded,
        [RulingsDegraded {
            palace: "rulings-a".into(),
            reason: "search failed: index unreadable".into(),
        }]
    );
}

/// Why (#9143 review): one ruling stored in two palaces, or already in the
/// project palace, must appear once — by content, not only by drawer id.
#[test]
fn the_same_ruling_from_two_palaces_appears_once() {
    let mut results = primary();
    let text = "issue titles name the symptom";
    let already_in_project = "commits never land on local main";
    results.push(hit(already_in_project, &["standing-rule"], 0.7, 2));
    let outcomes = vec![
        (
            "rulings-a".to_string(),
            Ok(vec![hit(text, &["bob-ruling"], 0.6, 2)]),
        ),
        (
            "rulings-b".to_string(),
            Ok(vec![
                hit(text, &["bob-ruling"], 0.6, 2),
                hit(already_in_project, &["standing-rule"], 0.65, 2),
            ]),
        ),
    ];
    let degraded = fold_rulings(&mut results, outcomes, 9, None);
    assert!(degraded.is_empty());
    let count = |c: &str| results.iter().filter(|r| r.drawer.content() == c).count();
    assert_eq!(count(text), 1, "{:?}", contents(&results));
    assert_eq!(count(already_in_project), 1, "{:?}", contents(&results));
}

/// Why (#9143 review): the leg must not crowd the project's answer.
/// What: four on-topic rulings, `top_k` 6, so at most `ceil(6 / 3) = 2` join,
/// the two best, at layer 1; a non-ruling and a below-floor ruling never do.
#[test]
fn rulings_contribute_at_most_a_third_of_top_k() {
    let mut results = primary();
    let outcomes = vec![(
        "rulings-a".to_string(),
        Ok(vec![
            hit("r1", &["bob-ruling"], 0.70, 2),
            hit("r2", &["decision"], 0.60, 2),
            hit("r3", &["ruling"], 0.50, 2),
            hit("r4", &["standing-rule"], 0.45, 2),
            hit("note", &["note"], 0.99, 2),
            hit("weak", &["ruling"], 0.10, 2),
        ]),
    )];
    fold_rulings(&mut results, outcomes, 6, Some(0.2));
    assert_eq!(contents(&results), ["project a", "project b", "r1", "r2"]);
    assert!(results[2..].iter().all(|r| r.layer == 1));
}

/// Why (#9143 review): a failed palace is not retried on every recall, and a
/// recovered one is retried once the window passes.
#[test]
fn a_failed_palace_is_skipped_until_the_retry_window_passes() {
    let leg = RulingsLeg::new(vec!["rulings-a".into()], RULINGS_TIMEOUT);
    assert_eq!(leg.cached_failure("rulings-a"), None);
    leg.record("rulings-a", &Err("absent: no palace with this id".into()));
    let cached = leg.cached_failure("rulings-a").expect("cached");
    assert!(cached.starts_with("absent"), "{cached}");
    leg.failed.lock().get_mut("rulings-a").expect("entry").0 =
        Instant::now() - RULINGS_RETRY_AFTER - Duration::from_secs(1);
    assert_eq!(leg.cached_failure("rulings-a"), None, "expired");
    leg.record("rulings-a", &Ok(Vec::new()));
    assert!(leg.failed.lock().is_empty(), "a success clears the entry");
}
