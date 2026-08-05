//! The deferred (background) embed lane: retry, then record what was lost.
//!
//! Why (#4906): `PalaceHandle::spawn_deferred_embed` was fire-and-forget. Every
//! failure path was a `warn!` followed by a bare `return` — one embedder blip
//! and the drawer stayed durable in redb and permanently invisible to vector
//! recall, with nothing anywhere that could later say so. Measured on the live
//! `trusty-tools` palace: 39 of 1,241 drawers had no vector, one of them written
//! during an in-flight migration. `memory_recall` cannot detect this class of
//! failure, because it only searches what got embedded.
//!
//! What: the same background lane, with three changes. A transient failure is
//! retried with exponential backoff instead of being dropped on the first
//! error. A final failure writes a durable row to the palace's embed-failure
//! ledger. And "no embedder on this host" is separated from "the embedder is
//! here and this drawer failed" — the first is an expected state on a machine
//! with no model downloaded and must not mark every drawer broken.
//!
//! The write path stays asynchronous on purpose. Storing a drawer must not fail
//! or hang because the embedder is slow or down (issue #1970); the fix is to
//! stop LOSING the failure, not to make writes synchronous.
//!
//! Test: `retry_succeeds_after_transient_failures`,
//! `permanent_failure_writes_a_ledger_row`,
//! `missing_embedder_does_not_mark_the_drawer`.

use super::embedder::shared_embedder;
use crate::memory_core::embed::Embedder;
use crate::memory_core::palace::{Drawer, PalaceId};
use crate::memory_core::store::embed_ledger::{self, EmbedFailure};
use crate::memory_core::store::vector::{UsearchStore, VectorStore};
use crate::memory_core::timeouts;
use parking_lot::RwLock;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Attempts made before a deferred embed is declared lost.
///
/// Why: an ONNX session under memory pressure, or a vector-store write racing a
/// redb commit, fails once and succeeds immediately after. Three attempts cover
/// that without turning a genuinely dead embedder into a long stall on a
/// background task.
const DEFAULT_EMBED_ATTEMPTS: u32 = 3;

/// First backoff step; doubles per attempt (250 ms, 500 ms).
const DEFAULT_EMBED_RETRY_BASE_MS: u64 = 250;

/// Bounded exponential-backoff policy for one embed job.
///
/// Why: the retry has to be a value rather than a constant so the backfill can
/// run a longer policy than the write-path lane, and so tests can collapse the
/// delays to zero instead of sleeping for real.
/// What: `attempts` total tries (not retries — `attempts: 1` means no retry) and
/// a base delay doubled on each successive failure.
/// Test: `retry_succeeds_after_transient_failures`.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub attempts: u32,
    pub base_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: DEFAULT_EMBED_ATTEMPTS,
            base_delay: Duration::from_millis(DEFAULT_EMBED_RETRY_BASE_MS),
        }
    }
}

impl RetryPolicy {
    /// Delay to wait after the `attempt`-th failure (1-based).
    fn delay_for(&self, attempt: u32) -> Duration {
        let shift = attempt.saturating_sub(1).min(6);
        self.base_delay.saturating_mul(1u32 << shift)
    }

    /// Policy with no sleeping, for tests that only care about attempt counts.
    #[cfg(any(test, feature = "embedder-test-support"))]
    pub fn instant(attempts: u32) -> Self {
        Self {
            attempts,
            base_delay: Duration::ZERO,
        }
    }
}

/// Why a drawer ended up without a vector.
///
/// Why: the two cases demand opposite responses. `EmbedderUnavailable` is an
/// expected state on a host with no model downloaded — marking every drawer
/// broken there would be a false alarm at estate scale. Everything else means
/// the embedder was present and this particular drawer failed, which is
/// precisely what the ledger exists to record.
/// What: a three-way split carrying the last error text.
/// Test: `missing_embedder_does_not_mark_the_drawer`.
#[derive(Debug, Clone)]
pub enum EmbedLoss {
    /// No embedder could be initialised on this host.
    EmbedderUnavailable(String),
    /// The embedder was present; producing the vector failed.
    Embed { reason: String, attempts: u32 },
    /// The vector was produced but could not be stored.
    Upsert { reason: String, attempts: u32 },
}

impl EmbedLoss {
    /// Whether this loss is the drawer's fault rather than the host's.
    ///
    /// Why: only a host-independent failure justifies a durable per-drawer
    /// marker. See the type docs.
    /// What: `false` for [`EmbedLoss::EmbedderUnavailable`], `true` otherwise.
    /// Test: `missing_embedder_does_not_mark_the_drawer`.
    pub fn is_drawer_specific(&self) -> bool {
        !matches!(self, EmbedLoss::EmbedderUnavailable(_))
    }

    /// How many attempts were made before giving up (0 when we never got an
    /// embedder to try with).
    pub(super) fn attempts(&self) -> u32 {
        match self {
            EmbedLoss::EmbedderUnavailable(_) => 0,
            EmbedLoss::Embed { attempts, .. } | EmbedLoss::Upsert { attempts, .. } => *attempts,
        }
    }

    /// Human-readable failure text, naming the stage that failed.
    pub(super) fn reason(&self) -> String {
        match self {
            EmbedLoss::EmbedderUnavailable(r) => format!("embedder unavailable: {r}"),
            EmbedLoss::Embed { reason, .. } => format!("embed failed: {reason}"),
            EmbedLoss::Upsert { reason, .. } => format!("vector upsert failed: {reason}"),
        }
    }
}

/// Run `op` up to `policy.attempts` times, sleeping between failures.
///
/// Why: embed and upsert both need the same bounded-retry shape, and writing it
/// twice is how the two drift into disagreeing about what "transient" means.
/// What: returns the first `Ok`, or the LAST error text once attempts are
/// exhausted. Also returns how many attempts were actually made, which is what
/// the ledger records.
/// Test: `retry_succeeds_after_transient_failures`.
pub(super) async fn with_retry<T, F, Fut>(
    policy: &RetryPolicy,
    mut op: F,
) -> Result<(T, u32), (String, u32)>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let total = policy.attempts.max(1);
    let mut last = String::from("no attempt was made");
    for attempt in 1..=total {
        match op(attempt).await {
            Ok(v) => return Ok((v, attempt)),
            Err(e) => last = e,
        }
        if attempt < total {
            tokio::time::sleep(policy.delay_for(attempt)).await;
        }
    }
    Err((last, total))
}

/// Embed `content` and store the vector under `id`, retrying transient failures.
///
/// Why: this is the single embed+store primitive shared by the write-path
/// deferred lane and the repair backfill, so a drawer recovered by the backfill
/// is embedded by exactly the code that would have embedded it at write time.
/// What: retries `embed_batch` (each attempt under the existing
/// `TRUSTY_EMBED_BATCH_TIMEOUT_SECS` bound), then retries the vector upsert.
/// Returns the failing stage in [`EmbedLoss`] so the caller can decide whether
/// it warrants a durable marker.
/// Test: `retry_succeeds_after_transient_failures`,
/// `permanent_failure_writes_a_ledger_row`.
pub(super) async fn embed_and_store(
    embedder: &Arc<dyn Embedder + Send + Sync>,
    vector_store: &Arc<UsearchStore>,
    id: Uuid,
    content: &str,
    policy: &RetryPolicy,
) -> Result<u32, EmbedLoss> {
    let batch = [content.to_string()];
    let embed_timeout = timeouts::embed_batch_timeout();
    let (vector, embed_attempts) = with_retry(policy, |_| {
        let embedder = embedder.clone();
        let batch = &batch;
        async move {
            match tokio::time::timeout(embed_timeout, embedder.embed_batch(batch)).await {
                Ok(Ok(v)) => v
                    .into_iter()
                    .next()
                    .ok_or_else(|| "embedder returned an empty batch".to_string()),
                Ok(Err(e)) => Err(format!("{e:#}")),
                Err(_) => Err(format!("embed_batch timed out after {embed_timeout:?}")),
            }
        }
    })
    .await
    .map_err(|(reason, attempts)| EmbedLoss::Embed { reason, attempts })?;

    let (_, upsert_attempts) = with_retry(policy, |_| {
        let vector = vector.clone();
        let vector_store = vector_store.clone();
        async move {
            vector_store
                .upsert(id, vector)
                .await
                .map_err(|e| format!("{e:#}"))
        }
    })
    .await
    .map_err(|(reason, attempts)| EmbedLoss::Upsert { reason, attempts })?;

    Ok(embed_attempts.max(upsert_attempts))
}

/// The slice of `PalaceHandle` the background lane needs.
///
/// Why: the spawned task outlives the `&self` borrow it was created from, and
/// cloning the individual `Arc`s is cheaper (and clearer about what the lane
/// touches) than requiring callers to hold an `Arc<PalaceHandle>`.
/// What: palace id, vector store, the in-memory drawer table, and the data dir
/// the ledger lives in. `data_dir` is `None` for in-memory test handles, in
/// which case there is nowhere durable to record a failure and the lane says so
/// in the log instead.
/// Test: exercised by every test in `embed_repair_tests`.
#[derive(Clone)]
pub(super) struct DeferredEmbedCtx {
    pub palace_id: PalaceId,
    pub vector_store: Arc<UsearchStore>,
    pub drawers: Arc<RwLock<Vec<Drawer>>>,
    pub data_dir: Option<PathBuf>,
}

/// Body of the background embed task, as an awaitable future.
///
/// Why: kept separate from the `tokio::spawn` wrapper so tests can await the
/// whole job deterministically instead of racing a detached task.
/// What: resolves the shared embedder, embeds and stores with retry, and on a
/// drawer-specific failure records a ledger row. An unavailable embedder is
/// logged and left unmarked — see [`EmbedLoss::is_drawer_specific`].
/// Test: `permanent_failure_writes_a_ledger_row`,
/// `missing_embedder_does_not_mark_the_drawer`.
pub(super) async fn run_deferred_embed(
    ctx: DeferredEmbedCtx,
    id: Uuid,
    content: String,
    policy: RetryPolicy,
) {
    let embedder = match shared_embedder().await {
        Ok(e) => e,
        Err(e) => {
            record_loss(&ctx, id, &EmbedLoss::EmbedderUnavailable(format!("{e:#}")));
            return;
        }
    };
    embed_store_or_record(&ctx, &embedder, id, &content, &policy).await;
}

/// Embed one drawer and durably record the outcome.
///
/// Why: split out of [`run_deferred_embed`] so the failure-recording behaviour
/// can be tested against a deliberately broken embedder — the shared embedder is
/// a process-wide `OnceCell` that tests seed with a working mock, so a test
/// driving the whole function could never reach the failure branch.
/// What: runs [`embed_and_store`]; on success clears any stale ledger row, on
/// failure hands the loss to [`record_loss`].
/// Test: `permanent_failure_writes_a_ledger_row`,
/// `retry_succeeds_after_transient_failures`.
pub(super) async fn embed_store_or_record(
    ctx: &DeferredEmbedCtx,
    embedder: &Arc<dyn Embedder + Send + Sync>,
    id: Uuid,
    content: &str,
    policy: &RetryPolicy,
) {
    match embed_and_store(embedder, &ctx.vector_store, id, content, policy).await {
        Ok(attempts) => {
            tracing::info!(
                palace = %ctx.palace_id, drawer = %id, attempts,
                "deferred embed: vector backfill complete"
            );
            // A drawer that has just been embedded is no longer missing; drop
            // any stale row so the ledger cannot outlive the condition.
            clear_ledger(ctx, id);
        }
        Err(loss) => record_loss(ctx, id, &loss),
    }
}

/// Fire the background embed task.
///
/// Why: `remember_with_options` must return as soon as the durable KG/redb
/// write completes (issue #1970) — the caller is not made to wait for a cold
/// ONNX compile.
/// What: spawns [`run_deferred_embed`] with the default retry policy.
/// Test: covered through `PalaceHandle::remember_with_options` in
/// `retrieval::tests`.
pub(super) fn spawn(ctx: DeferredEmbedCtx, id: Uuid, content: String) {
    tokio::spawn(run_deferred_embed(ctx, id, content, RetryPolicy::default()));
}

/// Persist (or decline to persist) the fact that a drawer has no vector.
///
/// Why: this is the whole point of #4906 — the failure must outlive the log
/// line. It is still logged at `error!`, because a durable marker nobody reads
/// and a log line nobody keeps are complementary failures, not alternatives.
/// What: skips the ledger entirely for a host-level embedder outage; otherwise
/// writes one row keyed by drawer id, but only if the drawer still exists (a
/// drawer forgotten mid-flight has nothing to annotate).
/// Test: `permanent_failure_writes_a_ledger_row`,
/// `missing_embedder_does_not_mark_the_drawer`.
pub(super) fn record_loss(ctx: &DeferredEmbedCtx, id: Uuid, loss: &EmbedLoss) {
    let reason = loss.reason();
    if !loss.is_drawer_specific() {
        // Expected on a host with no model downloaded, and on a cold start
        // before the first successful init. Marking every drawer here would be
        // a false alarm at estate scale; the vector-gap detector still reports
        // the shortfall, alongside `embedder_ready: false` to explain it.
        tracing::warn!(
            palace = %ctx.palace_id, drawer = %id,
            "#4906: deferred embed skipped — {reason}; drawer stored WITHOUT a \
             vector and left unmarked (no embedder on this host). Run the \
             vector backfill once an embedder is available."
        );
        return;
    }

    tracing::error!(
        palace = %ctx.palace_id, drawer = %id, attempts = loss.attempts(),
        "#4906: deferred embed FAILED — {reason}; drawer is durable but not \
         vector-searchable. Recorded in the palace embed-failure ledger."
    );

    let still_present = ctx.drawers.read().iter().any(|d| d.id == id);
    if !still_present {
        return;
    }
    let Some(data_dir) = ctx.data_dir.as_ref() else {
        tracing::warn!(
            palace = %ctx.palace_id, drawer = %id,
            "#4906: no data dir on this handle — failure could not be recorded durably"
        );
        return;
    };
    let entry = EmbedFailure {
        drawer_id: id,
        failed_at: chrono::Utc::now(),
        attempts: loss.attempts(),
        reason,
    };
    if let Err(e) = embed_ledger::record(data_dir, entry) {
        tracing::error!(
            palace = %ctx.palace_id, drawer = %id,
            "#4906: could not write the embed-failure ledger: {e:#}"
        );
    }
}

/// Drop one drawer's ledger row after a successful embed.
fn clear_ledger(ctx: &DeferredEmbedCtx, id: Uuid) {
    let Some(data_dir) = ctx.data_dir.as_ref() else {
        return;
    };
    let ids: std::collections::HashSet<Uuid> = std::iter::once(id).collect();
    if let Err(e) = embed_ledger::clear(data_dir, &ids) {
        tracing::warn!(palace = %ctx.palace_id, drawer = %id, "#4906: ledger clear failed: {e:#}");
    }
}
