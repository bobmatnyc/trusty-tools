//! A query embed bounded by its own deadline, on a detached task (#10194).
//!
//! Why: a query that needed the embedder sidecar spawned waited out the whole
//! spawn — 3-21 s measured, up to the 30 s startup timeout — inside the 30 s
//! query deadline, so a slow spawn answered `408 query_timeout`. A plain
//! `tokio::time::timeout` around the embed would drop the spawn with it
//! (`kill_on_drop` kills the half-started child), so the next query would
//! start over.
//! What: [`CodeIndexer::embed_query_within`] runs the cache-miss embed on a
//! detached task that also fills the query-embed cache, and waits for it at
//! most the deadline. A missed deadline returns [`QueryEmbedDeadline`] and
//! leaves the task running, so the spawn finishes and later queries use it.
//! While a missed embed is still running, later cache misses on the same index
//! degrade at once instead of queueing one more task behind the spawn
//! ([`QueryEmbedGate`]); the gate lapses after [`GATE_STALE_AFTER`] so a hung
//! embed cannot hold an index lexical forever.
//! Test: `service::server::tests_10194`, `embedder_supervisor::deadline_tests`,
//! and the unit tests below.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::super::{hash_query, CodeIndexer};

/// The env var naming the query-embed deadline in milliseconds.
pub(crate) const QUERY_EMBED_DEADLINE_ENV: &str = "TRUSTY_QUERY_EMBED_DEADLINE_MS";

/// Default query-embed deadline: 5 s (#10194 plan; the sidecar spawns in
/// 3-21 s, so a warm one is far inside it).
pub(crate) const DEFAULT_QUERY_EMBED_DEADLINE_MS: u64 = 5_000;

/// How long an abandoned embed keeps later cache misses lexical before they
/// try again. Above the 30 s startup timeout, below the 120 s call timeout.
pub(crate) const GATE_STALE_AFTER: Duration = Duration::from_secs(60);

/// Parse a deadline value: unset → default, `0` → no deadline (wait as before
/// #10194), malformed → default with a warning.
///
/// Test: `deadline_env_parses_default_zero_and_garbage`.
pub(crate) fn parse_query_embed_deadline(raw: Option<&str>) -> Option<Duration> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Some(Duration::from_millis(DEFAULT_QUERY_EMBED_DEADLINE_MS));
    };
    match raw.parse::<u64>() {
        Ok(0) => None,
        Ok(ms) => Some(Duration::from_millis(ms)),
        Err(_) => {
            tracing::warn!(
                value = raw,
                "{QUERY_EMBED_DEADLINE_ENV} is not a whole number of milliseconds; using \
                 {DEFAULT_QUERY_EMBED_DEADLINE_MS} (#10194)"
            );
            Some(Duration::from_millis(DEFAULT_QUERY_EMBED_DEADLINE_MS))
        }
    }
}

/// The deadline from [`QUERY_EMBED_DEADLINE_ENV`], read per query.
pub(crate) fn resolve_query_embed_deadline() -> Option<Duration> {
    parse_query_embed_deadline(std::env::var(QUERY_EMBED_DEADLINE_ENV).ok().as_deref())
}

/// A query embed missed its deadline (#10194).
///
/// Why: the reply's `meta.embedder_error` must say why the vector lane is
/// missing and that it is temporary, not "the embedder failed".
#[derive(Debug, thiserror::Error)]
#[error(
    "index '{index_id}': query embed missed its {deadline_ms} ms deadline \
     ({QUERY_EMBED_DEADLINE_ENV}) — {why}; answering from the lexical lane (#10194)"
)]
pub struct QueryEmbedDeadline {
    /// The index the query was sent to.
    pub index_id: String,
    /// The deadline that applied.
    pub deadline_ms: u128,
    /// Why this query did not get a vector.
    pub why: &'static str,
}

/// Abandoned-but-running query embeds on one index (#10194).
///
/// Why: each degraded query would otherwise leave one more detached embed
/// queued behind a spawn that is failing or slow, without bound.
/// What: counts the embeds whose caller gave up and records when the latest
/// was abandoned. [`Self::blocked`] is true while any is outstanding and the
/// latest is younger than [`GATE_STALE_AFTER`].
/// Test: `gate_blocks_while_an_abandoned_embed_runs_and_lapses_when_stale`.
#[derive(Debug, Default)]
pub(crate) struct QueryEmbedGate {
    inner: Mutex<GateState>,
}

#[derive(Debug, Default)]
struct GateState {
    outstanding: usize,
    latest: Option<Instant>,
}

impl QueryEmbedGate {
    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        // A poisoned gate only loses a count; never fail a query over it.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn blocked(&self, now: Instant) -> bool {
        let g = self.lock();
        g.outstanding > 0
            && g.latest
                .is_some_and(|t| now.saturating_duration_since(t) < GATE_STALE_AFTER)
    }

    fn abandon(&self, now: Instant) {
        let mut g = self.lock();
        g.outstanding += 1;
        g.latest = Some(now);
    }

    fn finish_abandoned(&self) {
        let mut g = self.lock();
        g.outstanding = g.outstanding.saturating_sub(1);
        if g.outstanding == 0 {
            g.latest = None;
        }
    }
}

/// Who still owns one detached embed's outcome.
const WAITING: u8 = 0;
const ABANDONED: u8 = 1;
const DONE: u8 = 2;

impl CodeIndexer {
    /// Embed a query, waiting at most `deadline` on a cache miss (#10194).
    ///
    /// Why: see the module doc.
    /// What: `None` deadline is [`Self::embed_query`] unchanged. Otherwise:
    /// no embedder → `Ok(None)`; cache hit → the vector; gate blocked →
    /// `Err(QueryEmbedDeadline)` at once; else the embed runs on a detached
    /// task that fills the cache, and this waits for it at most `deadline`.
    /// On expiry it marks the task abandoned and returns
    /// `Err(QueryEmbedDeadline)`; the task logs its own outcome when it ends.
    /// Test: `a_cold_embedder_answers_lexically_within_the_query_embed_deadline`,
    /// `a_slow_spawn_misses_the_deadline_then_serves_the_next_query`.
    pub(crate) async fn embed_query_within(
        &self,
        query: &str,
        deadline: Option<Duration>,
    ) -> Result<Option<Vec<f32>>> {
        let Some(deadline) = deadline else {
            return self.embed_query(query).await;
        };
        let Some(embedder) = self.embedder.clone() else {
            return Ok(None);
        };
        let key = hash_query(query);
        if let Some(v) = self
            .query_cache
            .lock()
            .expect("query_cache mutex poisoned")
            .get(&key)
        {
            return Ok(Some(v.clone()));
        }
        let missed = |why| QueryEmbedDeadline {
            index_id: self.index_id.clone(),
            deadline_ms: deadline.as_millis(),
            why,
        };
        if self.query_embed_gate.blocked(Instant::now()) {
            return Err(missed("an earlier query's embed is still waiting on the embedder").into());
        }

        let pool = self.resolve_embed_pool().await;
        let cache = Arc::clone(&self.query_cache);
        let gate = Arc::clone(&self.query_embed_gate);
        let owner = Arc::new(AtomicU8::new(WAITING));
        let task_owner = Arc::clone(&owner);
        let (index_id, text) = (self.index_id.clone(), query.to_string());
        let started = Instant::now();
        let mut task = tokio::spawn(async move {
            let result = super::lanes::embed_one(pool, &embedder, &text).await;
            if let Ok(v) = &result {
                cache
                    .lock()
                    .expect("query_cache mutex poisoned")
                    .put(key, v.clone());
            }
            if task_owner.swap(DONE, Ordering::AcqRel) == ABANDONED {
                gate.finish_abandoned();
                let ms = started.elapsed().as_millis();
                match &result {
                    Ok(_) => tracing::info!(
                        index_id = %index_id,
                        elapsed_ms = ms,
                        "query embed finished after its query degraded; the embedder is \
                         serving again (#10194)"
                    ),
                    Err(e) => tracing::warn!(
                        index_id = %index_id,
                        elapsed_ms = ms,
                        error = %format!("{e:#}"),
                        "query embed failed after its query degraded (#10194)"
                    ),
                }
            }
            result
        });

        let joined = match tokio::time::timeout(deadline, &mut task).await {
            Ok(joined) => joined,
            Err(_) => {
                if owner
                    .compare_exchange(WAITING, ABANDONED, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    self.query_embed_gate.abandon(Instant::now());
                    return Err(missed("the embedder is still starting").into());
                }
                // The task finished between the timer and the claim.
                task.await
            }
        };
        let vector = joined
            .context("query embed task failed")?
            .context("embed query")?;
        Ok(Some(vector))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: this test.
    #[test]
    fn deadline_env_parses_default_zero_and_garbage() {
        let default = Some(Duration::from_millis(DEFAULT_QUERY_EMBED_DEADLINE_MS));
        assert_eq!(parse_query_embed_deadline(None), default);
        assert_eq!(parse_query_embed_deadline(Some("  ")), default);
        assert_eq!(parse_query_embed_deadline(Some("abc")), default);
        assert_eq!(parse_query_embed_deadline(Some("0")), None);
        assert_eq!(
            parse_query_embed_deadline(Some("750")),
            Some(Duration::from_millis(750))
        );
    }

    /// Test: this test.
    #[test]
    fn gate_blocks_while_an_abandoned_embed_runs_and_lapses_when_stale() {
        let gate = QueryEmbedGate::default();
        let t0 = Instant::now();
        assert!(!gate.blocked(t0), "an idle gate never blocks");
        gate.abandon(t0);
        gate.abandon(t0);
        assert!(gate.blocked(t0));
        assert!(
            !gate.blocked(t0 + GATE_STALE_AFTER),
            "a hung embed must not hold the index lexical forever"
        );
        gate.finish_abandoned();
        assert!(gate.blocked(t0), "one embed is still outstanding");
        gate.finish_abandoned();
        assert!(!gate.blocked(t0), "the gate clears when every embed ends");
    }
}
