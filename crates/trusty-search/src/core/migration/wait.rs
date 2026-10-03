//! A schema migration waiting for its index's permit is visible (#8659).
//!
//! Why: `run_migrations_exclusive` waits on the same per-index permit a reindex
//! holds for its whole run (#6581). The wait logged nothing and no status field
//! showed it. In the 0.27.1 → 0.54.2 upgrade an M005 chain sat queued for about
//! 25 minutes with zero log lines, and the operators rolled back an upgrade
//! that was only waiting.
//! What: [`acquire_index_permit`] takes the permit at once when it is free and
//! is silent then. Otherwise it logs one INFO line (index, pending migrations,
//! holder), records the wait where `GET /indexes/:id/status` reads it
//! ([`migration_wait_json`]), logs the elapsed time every
//! [`WAIT_LOG_INTERVAL`], and logs the total wait once the permit arrives.
//! Test: `a_waiting_migration_is_logged_and_reported_with_its_holder`.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use dashmap::DashMap;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::MigrationRegistry;
use crate::core::registry::{IndexHandle, IndexId};

/// How often a still-waiting migration logs its elapsed time (#8659).
pub(crate) const WAIT_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// One migration chain's wait for its index permit (#8659).
#[derive(Debug, Clone)]
struct MigrationWait {
    since: Instant,
    since_unix_ms: u128,
    pending: Vec<String>,
}

static WAITS: OnceLock<DashMap<IndexId, MigrationWait>> = OnceLock::new();

fn waits() -> &'static DashMap<IndexId, MigrationWait> {
    WAITS.get_or_init(DashMap::new)
}

/// Removes the wait record when the waiter gets the permit or is dropped.
struct WaitRecord(IndexId);

impl Drop for WaitRecord {
    fn drop(&mut self) {
        waits().remove(&self.0);
    }
}

/// The pending/waiting migration state for `GET /indexes/:id/status`, or
/// `None` when no chain is waiting on this index (#8659).
///
/// What: `{pending, waiting_since_unix_ms, waited_secs, holder}`. `holder` is
/// the label the permit holder registered, or `null` when it registered none.
/// Test: `a_waiting_migration_is_logged_and_reported_with_its_holder`.
pub(crate) fn migration_wait_json(id: &IndexId) -> Option<serde_json::Value> {
    let wait = waits().get(id)?.clone();
    Some(serde_json::json!({
        "pending": wait.pending,
        "waiting_since_unix_ms": wait.since_unix_ms,
        "waited_secs": wait.since.elapsed().as_secs(),
        "holder": crate::service::reindex::index_permit_holder(id),
    }))
}

/// The migrations `index` still has to run, as `vN->vM description` (#8659).
async fn pending_migrations(index: &IndexHandle, registry: &MigrationRegistry) -> Vec<String> {
    let current = index.read_schema_version().await.unwrap_or(0);
    registry
        .chain_from(current)
        .iter()
        .map(|m| {
            format!(
                "v{}->v{} {}",
                m.source_version(),
                m.target_version(),
                m.description()
            )
        })
        .collect()
}

/// Take `index`'s permit, logging and recording the wait when it is held
/// (#8659).
///
/// Why: see the module docs.
/// What: `try_acquire_owned` first; on success nothing is logged. Otherwise
/// records the wait, logs the start, then waits on the permit with a
/// [`WAIT_LOG_INTERVAL`] ticker, and logs the total on acquisition. The record
/// is removed when this returns or its future is dropped.
/// Test: `a_waiting_migration_is_logged_and_reported_with_its_holder`.
pub(crate) async fn acquire_index_permit(
    index: &IndexHandle,
    registry: &MigrationRegistry,
    semaphore: Arc<Semaphore>,
) -> OwnedSemaphorePermit {
    if let Ok(permit) = Arc::clone(&semaphore).try_acquire_owned() {
        return permit;
    }
    let pending = pending_migrations(index, registry).await;
    let holder = crate::service::reindex::index_permit_holder(&index.id);
    let since = Instant::now();
    let since_unix_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    waits().insert(
        index.id.clone(),
        MigrationWait {
            since,
            since_unix_ms,
            pending: pending.clone(),
        },
    );
    let _record = WaitRecord(index.id.clone());
    tracing::info!(
        index_id = %index.id,
        pending = ?pending,
        holder = holder.unwrap_or("unidentified"),
        "schema migrations waiting for the index permit (held by {}) (#8659)",
        holder.unwrap_or("an unidentified task")
    );
    let acquire = semaphore.acquire_owned();
    tokio::pin!(acquire);
    let mut tick = tokio::time::interval_at(
        tokio::time::Instant::now() + WAIT_LOG_INTERVAL,
        WAIT_LOG_INTERVAL,
    );
    let permit = loop {
        tokio::select! {
            permit = &mut acquire => break permit,
            _ = tick.tick() => tracing::info!(
                index_id = %index.id,
                waited_secs = since.elapsed().as_secs(),
                holder = crate::service::reindex::index_permit_holder(&index.id)
                    .unwrap_or("unidentified"),
                "schema migrations still waiting for the index permit (#8659)"
            ),
        }
    };
    tracing::info!(
        index_id = %index.id,
        waited_secs = since.elapsed().as_secs(),
        "schema migrations acquired the index permit after waiting (#8659)"
    );
    permit.expect("per-index semaphore is never closed — a fresh Semaphore per IndexId")
}
