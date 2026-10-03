//! Unit tests for [`super`] — the cached `disk_survey` pass (#8985).
//!
//! Every test drives the cache with synthetic serialized surveys and an
//! injected clock, so nothing walks a disk or shells out.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::*;

fn key() -> SurveyKey {
    SurveyKey {
        project: None,
        group_by: None,
    }
}

/// A serialized survey; `bytes` stands in for the per-worktree figures.
fn survey(partial: bool, bytes: Option<u64>) -> Value {
    json!({ "partial": partial, "root": { "bytes": bytes } })
}

/// 🔴 REGRESSION (#8985): the 662-worktree host. The live pass runs out of
/// budget; one background pass is started; once it lands, the next call gets
/// the complete pass — byte figures present — labelled with its age, at once.
///
/// Fails on origin/main, where every call re-ran the budgeted pass and
/// answered the same partial survey with null bytes.
#[test]
fn a_truncated_live_pass_starts_one_background_pass() {
    let cache = DiskSurveyCache::default();
    let t0 = Instant::now();
    assert_eq!(cache.plan(&key(), t0), Plan::Live);

    let (first, refresh) = cache.after_live(&key(), survey(true, None), t0, t0);
    assert!(refresh, "the truncated pass must start a background pass");
    assert_eq!(first["freshness"], "partial", "{first}");
    assert_eq!(first["background_pass"], "running", "{first}");

    // A second truncated call while the pass runs starts no second pass.
    let (_, again) = cache.after_live(&key(), survey(true, None), t0, t0);
    assert!(!again, "one background pass at a time");

    let landed = t0 + Duration::from_secs(300);
    cache.after_background(&key(), Ok(survey(false, Some(4096))), landed);

    let later = landed + Duration::from_secs(10);
    let Plan::Serve { survey, refresh } = cache.plan(&key(), later) else {
        panic!("a key known to exceed the budget must be served from the cache");
    };
    assert!(!refresh, "a 10-second-old pass needs no refresh");
    assert_eq!(survey["freshness"], "cached", "{survey}");
    assert_eq!(survey["age_seconds"], 10, "{survey}");
    assert_eq!(survey["partial"], false, "{survey}");
    assert_eq!(survey["root"]["bytes"], 4096, "{survey}");
}

/// #8985: with a complete pass on hand, a truncated live pass answers with it
/// rather than with its own nulls, and says how old it is.
#[test]
fn a_truncated_live_pass_answers_with_the_last_complete_pass() {
    let cache = DiskSurveyCache::default();
    let t0 = Instant::now();
    let (live, _) = cache.after_live(&key(), survey(false, Some(7)), t0, t0);
    assert_eq!(live["freshness"], "live", "{live}");

    let t1 = t0 + Duration::from_secs(90);
    let (answer, refresh) = cache.after_live(&key(), survey(true, None), t1, t1);
    assert!(refresh);
    assert_eq!(answer["freshness"], "cached", "{answer}");
    assert_eq!(answer["age_seconds"], 90, "{answer}");
    assert_eq!(answer["root"]["bytes"], 7, "{answer}");
}

/// #8985: a complete live pass is answered live and becomes the cached pass,
/// and the key no longer counts as exceeding the budget.
#[test]
fn a_complete_live_pass_is_answered_live_and_cached() {
    let cache = DiskSurveyCache::default();
    let t0 = Instant::now();
    let (answer, refresh) = cache.after_live(&key(), survey(false, Some(1)), t0, t0);
    assert!(!refresh);
    assert_eq!(answer["freshness"], "live");
    assert_eq!(answer["age_seconds"], 0);
    assert_eq!(answer["background_pass"], "idle");
    assert_eq!(
        cache.plan(&key(), t0),
        Plan::Live,
        "a fleet that fits the budget keeps running live"
    );
}

/// #8985: a served pass older than [`REFRESH_AFTER`] starts exactly one
/// refresh; a fresh one starts none.
#[test]
fn a_key_known_to_exceed_the_budget_is_served_from_the_cache() {
    let cache = DiskSurveyCache::default();
    let t0 = Instant::now();
    cache.after_live(&key(), survey(true, None), t0, t0);
    cache.after_background(&key(), Ok(survey(false, Some(2))), t0);

    let stale = t0 + REFRESH_AFTER;
    let Plan::Serve { refresh, survey } = cache.plan(&key(), stale) else {
        panic!("served from the cache");
    };
    assert!(refresh, "an aged pass starts a refresh");
    assert_eq!(survey["background_pass"], "running");
    let Plan::Serve { refresh, .. } = cache.plan(&key(), stale) else {
        panic!("served from the cache");
    };
    assert!(!refresh, "the refresh slot is already taken");
}

/// #8985: a fresh cached pass starts no refresh.
#[test]
fn a_fresh_cached_pass_starts_no_refresh() {
    let cache = DiskSurveyCache::default();
    let t0 = Instant::now();
    cache.after_live(&key(), survey(true, None), t0, t0);
    cache.after_background(&key(), Ok(survey(false, Some(2))), t0);
    let Plan::Serve { refresh, survey } = cache.plan(&key(), t0 + Duration::from_secs(5)) else {
        panic!("served from the cache");
    };
    assert!(!refresh);
    assert_eq!(survey["background_pass"], "idle");
}

/// #8985 error arm: a failed or partial background pass frees the slot and
/// leaves the previous complete pass as the answer.
#[test]
fn a_failed_background_pass_frees_the_slot_and_keeps_the_old_pass() {
    let cache = DiskSurveyCache::default();
    let t0 = Instant::now();
    cache.after_live(&key(), survey(false, Some(9)), t0, t0);
    let (_, refresh) = cache.after_live(&key(), survey(true, None), t0, t0);
    assert!(refresh);
    cache.after_background(&key(), Err("the pass panicked".into()), t0);

    let (answer, refresh) = cache.after_live(&key(), survey(true, None), t0, t0);
    assert!(refresh, "the failed pass handed the slot back");
    assert_eq!(answer["root"]["bytes"], 9, "{answer}");

    cache.after_background(&key(), Ok(survey(true, None)), t0);
    let Plan::Serve { survey, .. } = cache.plan(&key(), t0) else {
        panic!("served from the cache");
    };
    assert_eq!(
        survey["root"]["bytes"], 9,
        "a partial background pass replaces nothing"
    );
}

fn project_key(project: &str) -> SurveyKey {
    SurveyKey {
        project: Some(project.to_string()),
        group_by: None,
    }
}

/// #8985 review: `background_pass: running` describes THIS key. A pass
/// refreshing another key is not reported as running for this one, and this
/// key's truncated pass, which could not take the slot, says `idle`.
#[test]
fn another_keys_background_pass_is_not_reported_as_running() {
    let cache = DiskSurveyCache::default();
    let t0 = Instant::now();
    let (_, took) = cache.after_live(&project_key("a"), survey(true, None), t0, t0);
    assert!(took, "key a holds the slot");

    let (live, _) = cache.after_live(&key(), survey(false, Some(1)), t0, t0);
    assert_eq!(live["background_pass"], "idle", "{live}");
    let (partial, refresh) = cache.after_live(&key(), survey(true, None), t0, t0);
    assert!(!refresh, "the slot belongs to key a");
    assert_eq!(partial["background_pass"], "idle", "{partial}");

    let (own, _) = cache.after_live(&project_key("a"), survey(true, None), t0, t0);
    assert_eq!(own["background_pass"], "running", "{own}");
}

/// #8985 review: a background pass that STARTED before a live pass, and lands
/// after it, does not overwrite the live pass. `age_seconds` counts from the
/// served pass's start.
#[test]
fn an_older_background_pass_never_overwrites_a_newer_live_pass() {
    let cache = DiskSurveyCache::default();
    let t0 = Instant::now();
    let (_, took) = cache.after_live(&key(), survey(true, None), t0, t0);
    assert!(took);
    let background_started = t0;

    let live_started = t0 + Duration::from_secs(5);
    cache.after_live(&key(), survey(false, Some(20)), live_started, live_started);
    // Mark the key as exceeding the budget again, so `plan` serves the cache.
    cache.after_live(&key(), survey(true, None), live_started, live_started);
    cache.after_background(&key(), Ok(survey(false, Some(10))), background_started);

    let at = live_started + Duration::from_secs(7);
    let Plan::Serve { survey, .. } = cache.plan(&key(), at) else {
        panic!("served from the cache");
    };
    assert_eq!(
        survey["root"]["bytes"], 20,
        "the later-started pass stays: {survey}"
    );
    assert_eq!(survey["age_seconds"], 7, "{survey}");
}

/// #8985 review: the map holds at most [`MAX_KEYS`] keys. A new key evicts
/// the entry whose complete pass started earliest, never the one being
/// refreshed.
#[test]
fn the_cache_holds_at_most_max_keys_and_evicts_the_oldest_pass() {
    let cache = DiskSurveyCache::default();
    let t0 = Instant::now();
    // Key 0 holds the refresh slot and has the oldest pass of all.
    cache.after_live(&project_key("p0"), survey(false, Some(0)), t0, t0);
    let (_, took) = cache.after_live(&project_key("p0"), survey(true, None), t0, t0);
    assert!(took);
    for i in 1..MAX_KEYS {
        let at = t0 + Duration::from_secs(u64::try_from(i).expect("small index"));
        cache.after_live(
            &project_key(&format!("p{i}")),
            survey(false, Some(1)),
            at,
            at,
        );
    }
    assert_eq!(cache.inner.lock().entries.len(), MAX_KEYS);

    let late = t0 + Duration::from_secs(1000);
    cache.after_live(&project_key("new"), survey(false, Some(1)), late, late);
    let inner = cache.inner.lock();
    assert_eq!(inner.entries.len(), MAX_KEYS, "the cap holds");
    assert!(
        inner.entries.contains_key(&project_key("p0")),
        "the refreshing key stays"
    );
    assert!(
        !inner.entries.contains_key(&project_key("p1")),
        "the oldest non-refreshing pass is evicted"
    );
    assert!(inner.entries.contains_key(&project_key("new")));
}
