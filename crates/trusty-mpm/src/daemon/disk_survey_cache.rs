//! The last complete `disk_survey` pass, kept and refreshed in the background
//! (#8985).
//!
//! Why: on a host with 662 worktrees the survey cannot finish inside the 55 s
//! clamp (#7313), so every call returned the same partial pass — every
//! worktree `review`, every byte figure null — exactly when disk was tight.
//! The clamp cannot grow: the stdio bridge gives up at 60 s.
//! What: [`DiskSurveyCache`] remembers, per (`project`, `group_by`), the last
//! COMPLETE pass and whether a live pass for that key ran out of budget. A key
//! known to exceed the budget is answered from the cache at once; a live pass
//! that runs out of budget starts one unbudgeted background pass and answers
//! with the last complete pass, or with its own partial one when there is none
//! yet. Every answer is labelled with [`label`]: `freshness` (`live`,
//! `cached`, `partial`), `age_seconds`, and `background_pass`.
//! Test: `disk_survey_cache_tests`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;

use crate::disk::size_index::DirSizeIndex;

/// How old a served cached pass may get before a call starts a refresh.
///
/// What: the size index's own 60 s root-total cadence, so a refresh re-reads
/// the figures that have actually aged.
pub(crate) const REFRESH_AFTER: Duration = Duration::from_secs(60);

/// The daemon's disk-survey resources: the size index and this cache.
///
/// Why: one `DaemonState` field, so the two share one lifetime and the state
/// struct does not grow per feature.
/// Test: exercised through `mcp_disk::disk_survey`.
#[derive(Debug)]
pub(crate) struct DiskResources {
    /// The shared directory-size index (#6926).
    pub(crate) index: std::sync::Arc<Mutex<DirSizeIndex>>,
    /// The last complete pass per key (#8985).
    pub(crate) surveys: std::sync::Arc<DiskSurveyCache>,
}

impl Default for DiskResources {
    fn default() -> Self {
        Self {
            index: std::sync::Arc::new(Mutex::new(DirSizeIndex::new())),
            surveys: std::sync::Arc::default(),
        }
    }
}

/// Which survey a cached pass answers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SurveyKey {
    /// The `project` filter, as the caller passed it.
    pub(crate) project: Option<String>,
    /// The `group_by` value, as the caller passed it.
    pub(crate) group_by: Option<String>,
}

/// How fresh an answer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Freshness {
    /// A complete pass run for this call.
    Live,
    /// The last complete pass, `age` old.
    Cached(Duration),
    /// This call's own pass, cut short by the budget.
    Partial,
}

/// What [`DiskSurveyCache::plan`] tells the caller to do.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Plan {
    /// Answer with this labelled pass; start a background pass if `refresh`.
    Serve {
        /// The labelled cached pass.
        survey: Value,
        /// Whether this caller must start the background pass.
        refresh: bool,
    },
    /// Run a live, budgeted pass, then call [`DiskSurveyCache::after_live`].
    Live,
}

#[derive(Debug, Default)]
struct Entry {
    /// The last complete pass and when it finished.
    complete: Option<(Value, Instant)>,
    /// Whether the last live pass for this key ran out of budget.
    exceeds_budget: bool,
}

#[derive(Debug, Default)]
struct Inner {
    entries: HashMap<SurveyKey, Entry>,
    /// The key a background pass is running for. One at a time, so two keys
    /// never run two full fleet passes at once.
    refreshing: Option<SurveyKey>,
}

/// The cache itself. Every method is a short critical section; no lock is held
/// across a survey pass.
#[derive(Debug, Default)]
pub(crate) struct DiskSurveyCache {
    inner: Mutex<Inner>,
}

impl DiskSurveyCache {
    /// Decide how to answer a call for `key` at `now`.
    ///
    /// What: [`Plan::Serve`] when the last live pass for `key` ran out of
    /// budget AND a complete pass exists; `refresh` is true, and the refresh
    /// slot is taken, when no background pass runs and the cached pass is at
    /// least [`REFRESH_AFTER`] old. Otherwise [`Plan::Live`].
    /// Test: `a_key_known_to_exceed_the_budget_is_served_from_the_cache`,
    /// `a_fresh_cached_pass_starts_no_refresh`.
    pub(crate) fn plan(&self, key: &SurveyKey, now: Instant) -> Plan {
        let mut inner = self.inner.lock();
        let idle = inner.refreshing.is_none();
        let Some(Entry {
            complete: Some((survey, at)),
            exceeds_budget: true,
        }) = inner.entries.get(key)
        else {
            return Plan::Live;
        };
        let age = now.saturating_duration_since(*at);
        let refresh = idle && age >= REFRESH_AFTER;
        let survey = label(survey.clone(), Freshness::Cached(age), refresh || !idle);
        if refresh {
            inner.refreshing = Some(key.clone());
        }
        Plan::Serve { survey, refresh }
    }

    /// Record a live pass for `key` and choose the answer.
    ///
    /// What: a complete pass is stored and answered as [`Freshness::Live`]. A
    /// partial pass marks `key` as exceeding the budget, takes the refresh slot
    /// when it is free (the returned `bool` — the caller must then start the
    /// background pass), and answers with the last complete pass, or with the
    /// partial pass itself when there is none.
    /// Test: `a_truncated_live_pass_starts_one_background_pass`,
    /// `a_truncated_live_pass_answers_with_the_last_complete_pass`,
    /// `a_complete_live_pass_is_answered_live_and_cached`.
    pub(crate) fn after_live(&self, key: &SurveyKey, live: Value, now: Instant) -> (Value, bool) {
        let mut inner = self.inner.lock();
        if !is_partial(&live) {
            let entry = inner.entries.entry(key.clone()).or_default();
            entry.complete = Some((live.clone(), now));
            entry.exceeds_budget = false;
            let running = inner.refreshing.is_some();
            return (label(live, Freshness::Live, running), false);
        }
        let refresh = inner.refreshing.is_none();
        if refresh {
            inner.refreshing = Some(key.clone());
        }
        let entry = inner.entries.entry(key.clone()).or_default();
        entry.exceeds_budget = true;
        let answer = match &entry.complete {
            Some((cached, at)) => label(
                cached.clone(),
                Freshness::Cached(now.saturating_duration_since(*at)),
                true,
            ),
            None => label(live, Freshness::Partial, true),
        };
        (answer, refresh)
    }

    /// Record the end of the background pass for `key`.
    ///
    /// What: frees the refresh slot whatever happened. A complete pass is
    /// stored; a failed or partial one is logged and stores nothing, so the
    /// previous complete pass stays the answer.
    /// Test: `a_failed_background_pass_frees_the_slot_and_keeps_the_old_pass`.
    pub(crate) fn after_background(
        &self,
        key: &SurveyKey,
        result: Result<Value, String>,
        now: Instant,
    ) {
        let mut inner = self.inner.lock();
        if inner.refreshing.as_ref() == Some(key) {
            inner.refreshing = None;
        }
        match result {
            Ok(survey) if !is_partial(&survey) => {
                inner.entries.entry(key.clone()).or_default().complete = Some((survey, now));
            }
            Ok(_) => tracing::warn!(?key, "disk_survey: background pass came back partial"),
            Err(e) => tracing::warn!(?key, "disk_survey: background pass failed: {e}"),
        }
    }
}

/// Whether a serialized survey says its pass was cut short.
fn is_partial(survey: &Value) -> bool {
    survey
        .get("partial")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Add the freshness fields to a serialized survey (#8985).
///
/// What: `freshness` (`live` / `cached` / `partial`), `age_seconds` (zero for
/// a pass run by this call), and `background_pass` (`running` / `idle`).
/// Test: `a_truncated_live_pass_answers_with_the_last_complete_pass`.
pub(crate) fn label(mut survey: Value, freshness: Freshness, background_running: bool) -> Value {
    let (name, age) = match freshness {
        Freshness::Live => ("live", 0),
        Freshness::Cached(age) => ("cached", age.as_secs()),
        Freshness::Partial => ("partial", 0),
    };
    if let Value::Object(map) = &mut survey {
        map.insert("freshness".into(), Value::from(name));
        map.insert("age_seconds".into(), Value::from(age));
        let pass = if background_running {
            "running"
        } else {
            "idle"
        };
        map.insert("background_pass".into(), Value::from(pass));
    }
    survey
}

#[cfg(test)]
#[path = "disk_survey_cache_tests.rs"]
mod disk_survey_cache_tests;
