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
//! rows keyed by drawer id, written atomically (tmp + rename) with the same
//! per-invocation tmp naming `L1Cache::save_l1_cache` uses so concurrent
//! writers cannot trample each other.
//!
//! The ledger is an ANNOTATION, never the source of truth. Whether a drawer has
//! a vector is answered by set-differencing the drawer table against the vector
//! index (`PalaceHandle::embed_health`); the ledger only says why a particular
//! one is missing and how hard we tried. That ordering is deliberate: a marker
//! that could disagree with the index would be one more thing to keep in sync,
//! and it repairs nothing for the 39 drawers that were already lost before this
//! code existed. Rows for drawers that no longer exist, or that have since
//! acquired a vector, are pruned on the next write rather than trusted.
//!
//! Test: `embed_ledger_roundtrips_and_upserts_by_drawer`,
//! `embed_ledger_clear_removes_only_named_rows`,
//! `embed_ledger_load_is_empty_when_absent`.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

/// Ledger filename inside a palace's data directory.
const EMBED_FAILURES_JSON: &str = "embed_failures.json";

/// Per-invocation tmp suffix counter — see `L1Cache::save_l1_cache` for why
/// two concurrent writers must not share a tmp path.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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
/// open the palace that holds the actual memories.
/// What: reads `<data_dir>/embed_failures.json`; a missing file yields `[]`, and
/// a malformed one yields `[]` plus a `warn!` rather than an error.
/// Test: `embed_ledger_load_is_empty_when_absent`.
pub fn load(data_dir: &Path) -> Vec<EmbedFailure> {
    let target = data_dir.join(EMBED_FAILURES_JSON);
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
/// What: loads, replaces any row with the same `drawer_id`, writes atomically.
/// Test: `embed_ledger_roundtrips_and_upserts_by_drawer`.
pub fn record(data_dir: &Path, entry: EmbedFailure) -> Result<()> {
    let mut rows = load(data_dir);
    rows.retain(|r| r.drawer_id != entry.drawer_id);
    rows.push(entry);
    save(data_dir, &rows)
}

/// Drop the named drawers from the ledger.
///
/// Why: a backfill that re-embeds a drawer successfully must stop reporting it
/// as broken, and a forgotten drawer must not leave a row behind claiming a
/// failure for an id nothing can look up. Both are the same operation.
/// What: loads, retains rows whose id is not in `ids`, writes atomically. A
/// no-op (no write at all) when nothing matched, so a healthy palace never
/// touches the file.
/// Test: `embed_ledger_clear_removes_only_named_rows`.
pub fn clear(data_dir: &Path, ids: &HashSet<Uuid>) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let rows = load(data_dir);
    let kept: Vec<EmbedFailure> = rows
        .iter()
        .filter(|r| !ids.contains(&r.drawer_id))
        .cloned()
        .collect();
    if kept.len() == rows.len() {
        return Ok(());
    }
    save(data_dir, &kept)
}

/// Atomically overwrite the ledger.
///
/// Why: a half-written ledger read at the next open would be indistinguishable
/// from a corrupt one, and this file is exactly the thing an operator reaches
/// for when they already do not trust what is on disk.
/// What: writes to `<name>.tmp.<pid>.<seq>` then renames over the target — the
/// per-invocation tmp name keeps two concurrent writers from removing each
/// other's temp file (the `L1Cache` #154 fix, same shape).
/// Test: covered by `embed_ledger_roundtrips_and_upserts_by_drawer`.
fn save(data_dir: &Path, rows: &[EmbedFailure]) -> Result<()> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("create palace data dir {}", data_dir.display()))?;
    let target = data_dir.join(EMBED_FAILURES_JSON);
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = data_dir.join(format!(
        "{EMBED_FAILURES_JSON}.tmp.{}.{seq}",
        std::process::id()
    ));
    let bytes = serde_json::to_vec_pretty(rows).context("serialize embed-failure ledger")?;
    std::fs::write(&tmp, &bytes)
        .with_context(|| format!("write embed-failure ledger tmp {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow::Error::from(e)
            .context(format!("rename embed-failure ledger {}", target.display())));
    }
    Ok(())
}
