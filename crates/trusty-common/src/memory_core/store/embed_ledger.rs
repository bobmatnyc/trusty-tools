//! Durable per-palace ledger of drawers whose embedding permanently failed.
//!
//! Why (#4906): the deferred-embed lane used to end every failure branch in a
//! `warn!` and a bare `return`. A log line scrolls past; the drawer stays
//! durable in redb and permanently absent from vector recall, and nothing
//! anywhere records that it happened. This file is the record. It survives a
//! restart, so an operator (or the backfill) can ask "which drawers did we try
//! to embed and fail?" without re-deriving it from a log.
//!
//! What: `<data_dir>/embed_failures.json` — a JSON array of [`EmbedFailure`]
//! rows keyed by drawer id. Every mutation goes through
//! [`crate::json_rmw::update`], this workspace's single implementation of the
//! locked load → mutate → publish-atomically critical section.
//!
//! Routing through `json_rmw` rather than an atomic rename alone is the fix for
//! a lost-update bug the first review round found here. The two hazards are
//! distinct, and conflating them is what produced a comment claiming a safety
//! property the code did not have:
//!
//! - **Torn file.** A unique temp path plus `rename` stops a reader seeing a
//!   half-written document. `json_rmw` does this, with `fsync`.
//! - **Lost update.** That does NOTHING for a read-modify-write: two writers
//!   that both load before either saves each publish their own single-row
//!   result, and the later rename discards the other's work. Each file is
//!   individually well-formed; the row is simply gone. This is the DESIGN LOAD,
//!   not a corner case — `spawn_deferred_embed` spawns one detached task per
//!   drawer, all sharing the same backoff schedule, so a burst against a broken
//!   embedder lands every one of them in `record_loss` within a few
//!   milliseconds. `json_rmw`'s advisory `flock` serialises the whole sequence,
//!   across threads AND across processes.
//!
//! One consequence worth stating: `json_rmw` never fails open, so [`record`] and
//! [`clear`] return `Err` over a ledger that exists but cannot be parsed, rather
//! than replacing it with a fresh single-row document. [`load`] takes the
//! opposite position and degrades to empty, because a read must never be able to
//! stop a palace opening. Losing an annotation costs less than refusing access
//! to the memories; silently discarding one costs more than declining to write.
//!
//! The ledger is an ANNOTATION, never the source of truth. Whether a drawer has
//! a vector is answered by set-differencing the drawer table against the vector
//! index (`PalaceHandle::embed_health`); the ledger only says why a particular
//! one is missing and how hard we tried. That ordering is deliberate: a marker
//! that could disagree with the index would be one more thing to keep in sync,
//! and it repairs nothing for the 39 drawers that were already lost before this
//! code existed.
//!
//! Test: `embed_ledger_roundtrips_and_upserts_by_drawer`,
//! `embed_ledger_clear_removes_only_named_rows`,
//! `embed_ledger_load_is_empty_when_absent`,
//! `embed_ledger_load_degrades_to_empty_on_malformed_json`,
//! `concurrent_ledger_records_keep_every_row`,
//! `concurrent_clear_and_record_do_not_lose_each_other`.

use crate::json_rmw;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Ledger filename inside a palace's data directory.
const EMBED_FAILURES_JSON: &str = "embed_failures.json";

/// Path to a palace's ledger file.
fn ledger_path(data_dir: &Path) -> PathBuf {
    data_dir.join(EMBED_FAILURES_JSON)
}

/// One drawer's recorded embedding failure.
///
/// Why: an operator triaging an unfindable memory needs three things — which
/// drawer, when we gave up, and what the embedder actually said. `attempts`
/// separates "one transient blip" from "we retried and it is genuinely broken".
/// What: a serde row; `reason` is the last error text from the retry loop.
/// Test: `embed_ledger_roundtrips_and_upserts_by_drawer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbedFailure {
    pub drawer_id: Uuid,
    pub failed_at: DateTime<Utc>,
    /// How many embed attempts were made before giving up.
    pub attempts: u32,
    /// Last error text observed. Free-form; for humans, not for matching on.
    pub reason: String,
}

/// Read the ledger, returning an empty vec when the palace has never recorded
/// a failure (the overwhelmingly common case).
///
/// Why: a missing or corrupt ledger must never make a palace unusable — it is
/// diagnostic annotation, and losing it costs strictly less than refusing to
/// open the palace that holds the actual memories. This is the one place that
/// deliberately diverges from `json_rmw`'s never-fail-open contract, and it does
/// so in the direction that cannot hurt: a read, never a write.
/// What: reads `<data_dir>/embed_failures.json`; a missing file yields `[]`, and
/// a malformed one yields `[]` plus a `warn!` rather than an error.
/// Test: `embed_ledger_load_is_empty_when_absent`,
/// `embed_ledger_load_degrades_to_empty_on_malformed_json`.
pub fn load(data_dir: &Path) -> Vec<EmbedFailure> {
    let target = ledger_path(data_dir);
    let Ok(bytes) = std::fs::read(&target) else {
        return Vec::new();
    };
    match serde_json::from_slice::<Vec<EmbedFailure>>(&bytes) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                path = %target.display(),
                "#4906: embed-failure ledger is unreadable, treating as empty: {e}"
            );
            Vec::new()
        }
    }
}

/// Record (or refresh) one drawer's failure, keyed by drawer id.
///
/// Why: the deferred lane can fail for the same drawer more than once — a
/// backfill retry that fails again must update the attempt count and reason,
/// not append a second row that makes the ledger grow without bound.
/// What: inside [`json_rmw::update`]'s locked critical section — which re-reads
/// the document from disk under the lock, so a concurrent writer's rows are
/// never clobbered — replaces any row with the same `drawer_id` and appends this
/// one. Blocking on an advisory `flock`; async callers must run it on a
/// blocking-safe thread.
/// Test: `embed_ledger_roundtrips_and_upserts_by_drawer`,
/// `concurrent_ledger_records_keep_every_row`.
pub fn record(data_dir: &Path, entry: EmbedFailure) -> Result<()> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("create palace data dir {}", data_dir.display()))?;
    let path = ledger_path(data_dir);
    json_rmw::update(&path, |rows: &mut Vec<EmbedFailure>| {
        rows.retain(|r| r.drawer_id != entry.drawer_id);
        rows.push(entry);
        Ok::<(), anyhow::Error>(())
    })
    .with_context(|| format!("record embed failure in {}", path.display()))
}

/// Drop the named drawers from the ledger.
///
/// Why: a backfill that re-embeds a drawer successfully must stop reporting it
/// as broken, and a forgotten drawer must not leave a row behind claiming a
/// failure for an id nothing can look up. Both are the same operation.
/// What: same locked critical section as [`record`] — a clear racing a record
/// has the identical lost-update shape, and the backfill runs both against one
/// palace. Returns before taking the lock when `ids` is empty or the palace has
/// no ledger, so a healthy palace never creates the file or its lock sidecar.
/// Test: `embed_ledger_clear_removes_only_named_rows`,
/// `concurrent_clear_and_record_do_not_lose_each_other`.
pub fn clear(data_dir: &Path, ids: &HashSet<Uuid>) -> Result<()> {
    let path = ledger_path(data_dir);
    if ids.is_empty() || !path.exists() {
        return Ok(());
    }
    json_rmw::update(&path, |rows: &mut Vec<EmbedFailure>| {
        rows.retain(|r| !ids.contains(&r.drawer_id));
        Ok::<(), anyhow::Error>(())
    })
    .with_context(|| format!("clear embed failures in {}", path.display()))
}
