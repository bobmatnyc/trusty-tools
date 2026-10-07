//! Unit tests for `tools::recall_rulings` (#9143): the env parser, the
//! per-palace search state, and the fold that merges the leg into a recall.

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
    let failed = RulingsDegraded {
        palace: "rulings-a".into(),
        reason: DegradedReason::SearchFailed,
        cached: false,
    };
    let degraded = fold_rulings(&mut results, vec![Err(failed.clone())], "q", 10, None).degraded;
    assert_eq!(contents(&results), ["project a", "project b"]);
    assert_eq!(degraded, [failed]);
    assert_eq!(
        serde_json::to_value(&degraded[0]).expect("json"),
        serde_json::json!({"palace": "rulings-a", "reason": "search_failed", "cached": false})
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
        Ok(vec![hit(text, &["bob-ruling"], 0.6, 2)]),
        Ok(vec![
            hit(text, &["bob-ruling"], 0.6, 2),
            hit(already_in_project, &["standing-rule"], 0.65, 2),
        ]),
    ];
    let degraded = fold_rulings(&mut results, outcomes, "q", 9, None).degraded;
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
    let outcomes = vec![Ok(vec![
        hit("r1", &["bob-ruling"], 0.70, 2),
        hit("r2", &["decision"], 0.60, 2),
        hit("r3", &["ruling"], 0.50, 2),
        hit("r4", &["standing-rule"], 0.45, 2),
        hit("note", &["note"], 0.99, 2),
        hit("weak", &["ruling"], 0.10, 2),
    ])];
    fold_rulings(&mut results, outcomes, "q", 6, Some(0.2));
    assert_eq!(contents(&results), ["project a", "project b", "r1", "r2"]);
    assert!(results[2..].iter().all(|r| r.layer == 1));
}

/// Why (#9143 AC2): the rank floor may only lift rulings the cap admitted, and
/// only those that answer the query.
/// What: three rulings answer the query and one does not; `top_k` 6 caps the
/// leg at 2. The best answering ruling is floored; the off-topic ruling is
/// folded on score but not floored; the third answering ruling, past the cap,
/// is neither folded nor floored.
#[test]
fn only_capped_rulings_that_answer_the_query_are_floored() {
    let query = "issue titles name the symptom";
    let mut results = primary();
    let on_topic = hit("issue titles name the symptom", &["bob-ruling"], 0.70, 2);
    let off_topic = hit("commits never land on local main", &["ruling"], 0.60, 2);
    let past_cap = hit("issue titles name a symptom too", &["ruling"], 0.50, 2);
    let (on_id, past_id) = (on_topic.drawer.id, past_cap.drawer.id);
    let outcomes = vec![Ok(vec![on_topic, off_topic, past_cap])];
    let fold = fold_rulings(&mut results, outcomes, query, 6, None);
    assert_eq!(fold.floored, [on_id]);
    assert_eq!(results.len(), 4, "{:?}", contents(&results));
    assert!(results.iter().all(|r| r.drawer.id != past_id));
}

/// Push `palace`'s recorded failure past [`RULINGS_RETRY_AFTER`].
fn expire_failure(leg: &RulingsLeg, palace: &str) {
    let mut states = leg.states.lock();
    let failure = &mut states.get_mut(palace).expect("state").failure;
    let (at, _) = failure.as_mut().expect("a recorded failure");
    *at = Instant::now() - RULINGS_RETRY_AFTER - Duration::from_secs(1);
}

/// Push every running search of `palace` past the leg's timeout: stalled.
fn stall(leg: &RulingsLeg, palace: &str) {
    let mut states = leg.states.lock();
    let running = &mut states.get_mut(palace).expect("state").running;
    for started in running.values_mut() {
        *started = Instant::now() - leg.timeout - Duration::from_secs(1);
    }
}

/// Why (#9143 review): a failed palace is not retried on every recall, and a
/// recovered one is retried once the window passes. A cached failure carries
/// its original code with `cached: true`.
#[test]
fn a_failed_palace_is_skipped_until_the_retry_window_passes() {
    let leg = RulingsLeg::new(vec!["rulings-a".into()], RULINGS_TIMEOUT);
    let first = leg.admit("rulings-a").expect("first search runs");
    leg.finish("rulings-a", first, Err(DegradedReason::Absent));
    assert_eq!(
        leg.admit("rulings-a"),
        Err((DegradedReason::Absent, true)),
        "cached within the window"
    );
    expire_failure(&leg, "rulings-a");
    let retry = leg.admit("rulings-a").expect("expired: tried again");
    leg.finish("rulings-a", retry, Ok(()));
    assert!(
        leg.admit("rulings-a").is_ok(),
        "a success clears the failure"
    );
    assert_eq!(leg.searches_started("rulings-a"), 3);
}

/// Why (#9143 review): a timed-out search kept running, untracked, and every
/// later recall started another, so a stall piled up blocking tasks.
/// What: once search 1 has run past the timeout, a second admit is refused as
/// `in_flight` and starts nothing; once search 1 records, the next admit
/// starts search 2.
#[test]
fn a_stalled_search_admits_no_second_search() {
    let leg = RulingsLeg::new(vec!["rulings-a".into()], RULINGS_TIMEOUT);
    let first = leg.admit("rulings-a").expect("first search runs");
    assert!(leg.in_flight("rulings-a"));
    stall(&leg, "rulings-a");
    assert_eq!(
        leg.admit("rulings-a"),
        Err((DegradedReason::InFlight, false))
    );
    assert_eq!(leg.searches_started("rulings-a"), 1, "no second search");
    leg.finish("rulings-a", first, Ok(()));
    assert!(!leg.in_flight("rulings-a"));
    assert_eq!(leg.admit("rulings-a"), Ok(2));
}

/// Why (#9143 review): refusing every concurrent search left the second of
/// two overlapping healthy recalls with no rulings, reported as `in_flight`.
/// What: search 2 is admitted while search 1 runs inside the timeout. Search
/// 1 ending leaves the palace in flight and its outcome unrecorded; search 2
/// ending is what lands.
#[test]
fn overlapping_healthy_searches_are_each_admitted() {
    let leg = RulingsLeg::new(vec!["rulings-a".into()], RULINGS_TIMEOUT);
    let first = leg.admit("rulings-a").expect("search 1");
    let second = leg.admit("rulings-a").expect("search 2 overlaps search 1");
    assert_eq!((first, second), (1, 2));
    leg.finish("rulings-a", first, Err(DegradedReason::SearchFailed));
    assert!(leg.in_flight("rulings-a"), "search 2 still runs");
    leg.finish("rulings-a", second, Ok(()));
    assert!(!leg.in_flight("rulings-a"));
    assert_eq!(
        leg.admit("rulings-a"),
        Ok(3),
        "the older failure never landed"
    );
}

/// Why (#9143 review): a search that outlives its recall must still count,
/// so a late success clears an earlier failure instead of being thrown away.
#[test]
fn a_late_success_clears_an_earlier_failure() {
    let leg = RulingsLeg::new(vec!["rulings-a".into()], RULINGS_TIMEOUT);
    let first = leg.admit("rulings-a").expect("first search runs");
    leg.finish("rulings-a", first, Err(DegradedReason::SearchFailed));
    expire_failure(&leg, "rulings-a");
    let late = leg.admit("rulings-a").expect("retry runs");
    // The recall that started `late` has long since reported `timed_out`.
    leg.finish("rulings-a", late, Ok(()));
    assert!(leg.states.lock()["rulings-a"].failure.is_none());
    assert!(leg.admit("rulings-a").is_ok(), "searched again at once");
}

/// Why (#9143 review): concurrent recalls raced on the failure cache, so an
/// older result could overwrite a newer one.
/// What: search 1 failed and search 2 is running; a replayed outcome of search
/// 1 neither clears search 2's running state nor its predecessor's failure,
/// and search 2's own outcome is the one that lands.
#[test]
fn an_older_result_never_overwrites_a_newer_one() {
    let leg = RulingsLeg::new(vec!["rulings-a".into()], RULINGS_TIMEOUT);
    let older = leg.admit("rulings-a").expect("search 1");
    leg.finish("rulings-a", older, Err(DegradedReason::SearchFailed));
    expire_failure(&leg, "rulings-a");
    let newer = leg.admit("rulings-a").expect("search 2");
    leg.finish("rulings-a", older, Ok(()));
    assert!(leg.in_flight("rulings-a"), "search 2 still runs");
    stall(&leg, "rulings-a");
    assert_eq!(
        leg.admit("rulings-a"),
        Err((DegradedReason::InFlight, false))
    );
    leg.finish("rulings-a", newer, Err(DegradedReason::Unreadable));
    assert_eq!(
        leg.admit("rulings-a"),
        Err((DegradedReason::Unreadable, true))
    );
}

/// Why (#9143 review): a panicked search task must not leave its palace
/// `in_flight` forever.
#[test]
fn a_dropped_search_records_task_failed() {
    let leg = Arc::new(RulingsLeg::new(vec!["rulings-a".into()], RULINGS_TIMEOUT));
    let generation = leg.admit("rulings-a").expect("search runs");
    drop(FinishGuard {
        leg: leg.clone(),
        palace: "rulings-a".into(),
        generation,
        recorded: false,
    });
    assert!(!leg.in_flight("rulings-a"));
    assert_eq!(
        leg.admit("rulings-a"),
        Err((DegradedReason::TaskFailed, true))
    );
}

/// Why (#9143 review): `AppState::new` read the rulings variable, so a test
/// state picked up a list exported on the developer's machine.
/// What: with the variable set in-process, a fresh state has no rulings leg;
/// the daemon's opt-in builder reads it.
#[tokio::test]
async fn a_default_state_ignores_the_rulings_env_until_the_daemon_opts_in() {
    let _env = crate::commands::env_test_lock().lock().await;
    // SAFETY: every lib test that writes the environment holds the lock above.
    unsafe {
        std::env::set_var(RULINGS_PALACES_ENV, "rulings-env-a, rulings-env-b");
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf());
    let opted_in = state.clone().with_rulings_palaces_from_env();
    // SAFETY: as above.
    unsafe {
        std::env::remove_var(RULINGS_PALACES_ENV);
    }
    assert!(
        state.rulings.palaces().is_empty(),
        "no rulings leg by default"
    );
    assert_eq!(
        opted_in.rulings.palaces(),
        ["rulings-env-a", "rulings-env-b"]
    );
}
