//! The last complete `disk_survey` pass, kept and refreshed in the background
//! (#8985).
//!
//! Why: on a host with 662 worktrees the survey cannot finish inside the 55 s
//! clamp (#7313), so every call returned the same partial pass — every
//! worktree `review`, every byte figure null — exactly when disk was tight.
//! The clamp cannot grow: the stdio bridge gives up at 60 s.
//! What: [`DiskSurveyCache`] remembers, per (`project`, `group_by`), the last
//! COMPLETE pass and whether a budgeted live pass for that key ran out of
//! budget. A budgeted call for a key known to exceed the budget is answered
//! from the cache at once; a live pass that runs out of budget starts one
//! unbudgeted background pass and answers with the last complete pass, or with
//! its own partial one when there is none yet. A call with no budget never
//! consults the cache — `mcp_disk::disk_survey` runs it live. Every answer is
//! labelled with [`label`]: `freshness` (`live`, `cached`, `partial`),
//! `age_seconds`, and `background_pass`.
//! Test: `disk_survey_cache_tests`, `mcp_disk_cache_tests`.

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

/// The most keys the cache holds (#8985 review).
///
/// Why: `project` is free text from the caller, so without a cap every
/// distinct spelling would keep a whole serialized fleet in memory for the
/// daemon's lifetime. The console and the CLI use two or three keys.
pub(crate) const MAX_KEYS: usize = 16;

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
    /// The last complete pass, which STARTED `age` ago.
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
    /// The last complete pass and the instant that pass STARTED.
    complete: Option<(Value, Instant)>,
    /// Whether the last budgeted live pass for this key ran out of budget.
    exceeds_budget: bool,
}

impl Entry {
    /// Store `survey`, started at `started`, unless the stored pass started
    /// later (#8985 review): a slow pass that began first must not overwrite
    /// the figures of one that began after it.
    fn keep_newer(&mut self, survey: Value, started: Instant) {
        let newer = self.complete.as_ref().is_none_or(|(_, at)| *at <= started);
        if newer {
            self.complete = Some((survey, started));
        }
    }
}

#[derive(Debug, Default)]
struct Inner {
    entries: HashMap<SurveyKey, Entry>,
    /// The key a background pass is running for. One at a time, so two keys
    /// never run two full fleet passes at once.
    refreshing: Option<SurveyKey>,
}

impl Inner {
    /// Whether the background pass running now is for `key`.
    fn refreshing_for(&self, key: &SurveyKey) -> bool {
        self.refreshing.as_ref() == Some(key)
    }

    /// The entry for `key`, evicting one first when a new key would pass
    /// [`MAX_KEYS`] (#8985 review).
    ///
    /// What: the victim is never `key` itself nor the key being refreshed. An
    /// entry holding no complete pass goes first — it holds only a budget bit,
    /// which one live pass relearns — then the entry whose pass started
    /// earliest.
    fn entry(&mut self, key: &SurveyKey) -> &mut Entry {
        if !self.entries.contains_key(key) && self.entries.len() >= MAX_KEYS {
            let refreshing = self.refreshing.as_ref();
            let victim = self
                .entries
                .iter()
                .filter(|(k, _)| Some(*k) != refreshing)
                .min_by_key(|(_, e)| e.complete.as_ref().map(|(_, at)| *at))
                .map(|(k, _)| k.clone());
            if let Some(victim) = victim {
                self.entries.remove(&victim);
            }
        }
        self.entries.entry(key.clone()).or_default()
    }
}

/// The cache itself. Every method is a short critical section; no lock is held
/// across a survey pass.
#[derive(Debug, Default)]
pub(crate) struct DiskSurveyCache {
    inner: Mutex<Inner>,
}

impl DiskSurveyCache {
    /// Decide how to answer a BUDGETED call for `key` at `now`.
    ///
    /// What: [`Plan::Serve`] when the last live pass for `key` ran out of
    /// budget AND a complete pass exists; `refresh` is true, and the refresh
    /// slot is taken, when no background pass runs and the cached pass is at
    /// least [`REFRESH_AFTER`] old. Otherwise [`Plan::Live`]. A call with no
    /// budget must not ask: it runs live (#8985 review).
    /// Test: `a_key_known_to_exceed_the_budget_is_served_from_the_cache`,
    /// `a_fresh_cached_pass_starts_no_refresh`.
    pub(crate) fn plan(&self, key: &SurveyKey, now: Instant) -> Plan {
        let mut inner = self.inner.lock();
        let idle = inner.refreshing.is_none();
        let ours = inner.refreshing_for(key);
        let Some(Entry {
            complete: Some((survey, started)),
            exceeds_budget: true,
        }) = inner.entries.get(key)
        else {
            return Plan::Live;
        };
        let age = now.saturating_duration_since(*started);
        let refresh = idle && age >= REFRESH_AFTER;
        let survey = label(survey.clone(), Freshness::Cached(age), refresh || ours);
        if refresh {
            inner.refreshing = Some(key.clone());
        }
        Plan::Serve { survey, refresh }
    }

    /// Record a live pass for `key`, started at `started`, and choose the
    /// answer at `now`.
    ///
    /// What: a complete pass is stored (unless a later-started pass is
    /// already stored), clears the key's budget bit, and is answered as
    /// [`Freshness::Live`]. A partial pass marks `key` as exceeding the
    /// budget, takes the refresh slot when it is free (the returned `bool` —
    /// the caller must then start the background pass), and answers with the
    /// last complete pass, or with the partial pass itself when there is none.
    /// `background_pass` reads `running` only when the slot is refreshing
    /// `key` (#8985 review).
    /// Test: `a_truncated_live_pass_starts_one_background_pass`,
    /// `a_truncated_live_pass_answers_with_the_last_complete_pass`,
    /// `a_complete_live_pass_is_answered_live_and_cached`,
    /// `another_keys_background_pass_is_not_reported_as_running`.
    pub(crate) fn after_live(
        &self,
        key: &SurveyKey,
        live: Value,
        started: Instant,
        now: Instant,
    ) -> (Value, bool) {
        let mut inner = self.inner.lock();
        if !is_partial(&live) {
            let entry = inner.entry(key);
            entry.keep_newer(live.clone(), started);
            entry.exceeds_budget = false;
            let running = inner.refreshing_for(key);
            return (label(live, Freshness::Live, running), false);
        }
        let refresh = inner.refreshing.is_none();
        if refresh {
            inner.refreshing = Some(key.clone());
        }
        let running = inner.refreshing_for(key);
        let entry = inner.entry(key);
        entry.exceeds_budget = true;
        let answer = match &entry.complete {
            Some((cached, at)) => label(
                cached.clone(),
                Freshness::Cached(now.saturating_duration_since(*at)),
                running,
            ),
            None => label(live, Freshness::Partial, running),
        };
        (answer, refresh)
    }

    /// Record the end of the background pass for `key`, started at `started`.
    ///
    /// What: frees the refresh slot whatever happened. A complete pass is
    /// stored unless a later-started pass already is; a failed or partial one
    /// is logged and stores nothing, so the previous complete pass stays the
    /// answer.
    /// Test: `a_failed_background_pass_frees_the_slot_and_keeps_the_old_pass`,
    /// `an_older_background_pass_never_overwrites_a_newer_live_pass`.
    pub(crate) fn after_background(
        &self,
        key: &SurveyKey,
        result: Result<Value, String>,
        started: Instant,
    ) {
        let mut inner = self.inner.lock();
        if inner.refreshing_for(key) {
            inner.refreshing = None;
        }
        match result {
            Ok(survey) if !is_partial(&survey) => inner.entry(key).keep_newer(survey, started),
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
/// What: `freshness` (`live` / `cached` / `partial`), `age_seconds` (time
/// since the served pass started; zero for a pass run by this call), and
/// `background_pass` (`running` / `idle`, for this survey's key).
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
