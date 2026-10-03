//! The dream dedup guard for `palace legacy-kg --apply` (#8729).
//!
//! Why: on 2026-09-27 an apply imported legacy rows that repeated each other
//! and the live palace near-verbatim. About 300 s later the idle dream cycle's
//! dedup pass merged them away at its 0.95 cosine rule, so the apply reported
//! drawers imported that the palace then lost. The import must not hand the
//! dream pass drawers it will delete.
//! What: [`screen_near_duplicates`] holds back every drawer the dream dedup
//! rule would merge: one that repeats an earlier drawer of the same batch
//! verbatim, one whose vector scores at least the threshold against the live
//! vector index, and one that scores at least the threshold against a drawer
//! this batch already kept. The threshold is the dream cycle's own
//! (`DreamConfig::default().dedup_threshold`), the value the daemon runs with.
//! The live index only catches a live drawer that has a vector, so
//! [`screen_for_import`] backfills the live drawers' missing vectors first
//! and reports any it could not embed as unscreened.
//! Test: `apply_holds_back_drawers_dream_dedup_would_merge`,
//! `a_failing_embedder_fails_the_near_duplicate_screen`,
//! `apply_screens_against_a_live_drawer_that_had_no_vector`.

use std::collections::HashMap;

use anyhow::{Context, Result};
use trusty_common::memory_core::dream::DreamConfig;
use trusty_common::memory_core::embed::Embedder;
use trusty_common::memory_core::palace::Drawer;
use trusty_common::memory_core::retrieval::{shared_embedder, PalaceHandle, VectorBackfillOptions};
use trusty_common::memory_core::store::VectorStore as _;
use trusty_common::memory_core::{memory_content_hash, timeouts};
use uuid::Uuid;

use super::LegacyReport;

/// Drawers embedded per call, matching the dream dedup pass's chunking (#7106).
const SCREEN_CHUNK: usize = 64;

/// Held-back drawers the report lists one per line; the rest are counted.
pub(crate) const HELD_BACK_SHOWN: usize = 20;

/// Backfill the live drawers' missing vectors, then screen `drawers`; returns
/// the drawers to import.
///
/// Why (#8729 review): the screen searches the live vector index, which holds
/// no entry for a live drawer that was never embedded. The dream pass embeds
/// such a drawer later and can then merge an imported copy of it, so the
/// backfill runs before the screen, not after the import.
/// What: runs `backfill_missing_vectors` over the palace (before the import,
/// its drawers are all live), then [`screen_near_duplicates`] at the dream
/// threshold with the shared embedder. Fills `report`'s `near_duplicates`,
/// `unscreened_live` (live drawers the backfill could not embed) and
/// `vectors` (that backfill's result).
///
/// # Errors
///
/// No embedder, a failed backfill, and every [`screen_near_duplicates`]
/// error. The caller writes nothing on error.
///
/// Test: `apply_screens_against_a_live_drawer_that_had_no_vector`,
/// `apply_holds_back_drawers_dream_dedup_would_merge`.
pub async fn screen_for_import(
    handle: &PalaceHandle,
    drawers: Vec<Drawer>,
    report: &mut LegacyReport,
) -> Result<Vec<Drawer>> {
    let embedder = shared_embedder()
        .await
        .map_err(|e| e.context("acquire the embedder for the near-duplicate screen"))?;
    let backfill = handle
        .backfill_missing_vectors(VectorBackfillOptions {
            dry_run: false,
            ..VectorBackfillOptions::default()
        })
        .await
        .context("backfill live drawer vectors before the near-duplicate screen")?;
    let threshold = DreamConfig::default().dedup_threshold;
    let (kept, held) =
        screen_near_duplicates(handle, drawers, embedder.as_ref(), threshold).await?;
    report.near_duplicates = Some(held);
    report.unscreened_live = backfill.still_missing_ids.len();
    report.vectors = Some((backfill.repaired, report.unscreened_live));
    Ok(kept)
}

/// A legacy drawer held back because dream dedup would merge it.
#[derive(Debug, Clone, PartialEq)]
pub struct NearDuplicate {
    /// The legacy drawer that was not imported.
    pub id: Uuid,
    /// The live or already-kept drawer it duplicates.
    pub of: Uuid,
    /// Cosine similarity between the two; 1.0 for a verbatim repeat.
    pub score: f32,
}

/// Split `drawers` into `(kept, held_back)` under the dream dedup rule.
///
/// Why/What: see the module doc. Verbatim repeats inside the batch are found
/// by content hash before any embedding. The rest are embedded in
/// [`SCREEN_CHUNK`]-sized batches under the shared embed timeout.
///
/// # Errors
///
/// An embed failure or timeout, a vector-count mismatch, or a failed index
/// search. The caller writes nothing on error: a screen that could not run
/// must not let the import through (#8729).
///
/// Test: `apply_holds_back_drawers_dream_dedup_would_merge`,
/// `a_failing_embedder_fails_the_near_duplicate_screen`.
pub async fn screen_near_duplicates(
    handle: &PalaceHandle,
    drawers: Vec<Drawer>,
    embedder: &(dyn Embedder + Send + Sync),
    threshold: f32,
) -> Result<(Vec<Drawer>, Vec<NearDuplicate>)> {
    let mut held = Vec::new();
    let mut first_by_hash = HashMap::new();
    let mut distinct = Vec::with_capacity(drawers.len());
    for d in drawers {
        match first_by_hash.get(&memory_content_hash(d.content())) {
            Some(&of) => held.push(NearDuplicate {
                id: d.id,
                of,
                score: 1.0,
            }),
            None => {
                first_by_hash.insert(memory_content_hash(d.content()), d.id);
                distinct.push(d);
            }
        }
    }

    let mut kept: Vec<Drawer> = Vec::with_capacity(distinct.len());
    let mut kept_vecs: Vec<Vec<f32>> = Vec::new();
    for chunk in distinct.chunks(SCREEN_CHUNK) {
        let contents: Vec<String> = chunk.iter().map(|d| d.content().to_string()).collect();
        let limit = timeouts::embed_batch_timeout();
        let vectors = tokio::time::timeout(limit, embedder.embed_batch(&contents))
            .await
            .map_err(|_| {
                anyhow::anyhow!("embed of {} drawers timed out after {limit:?}", chunk.len())
            })?
            .context("embed legacy drawers for the near-duplicate screen")?;
        anyhow::ensure!(
            vectors.len() == chunk.len(),
            "embedder returned {} vectors for {} drawers",
            vectors.len(),
            chunk.len()
        );
        for (d, v) in chunk.iter().zip(vectors) {
            if let Some((of, score)) = live_match(handle, d.id, &v, threshold).await? {
                held.push(NearDuplicate {
                    id: d.id,
                    of,
                    score,
                });
                continue;
            }
            let batch = kept_vecs
                .iter()
                .zip(&kept)
                .map(|(kv, k)| (k.id, cosine(kv, &v)))
                .find(|(_, s)| *s >= threshold);
            if let Some((of, score)) = batch {
                held.push(NearDuplicate {
                    id: d.id,
                    of,
                    score,
                });
                continue;
            }
            kept.push(d.clone());
            kept_vecs.push(v);
        }
    }
    Ok((kept, held))
}

/// The best live neighbour of `vector` scoring at least `threshold`, as the
/// dream pass's own top-3 index search finds it.
async fn live_match(
    handle: &PalaceHandle,
    id: Uuid,
    vector: &[f32],
    threshold: f32,
) -> Result<Option<(Uuid, f32)>> {
    let hits = handle
        .vector_store
        .search(vector, 3)
        .await
        .context("search the live vector index for near-duplicates")?;
    Ok(hits
        .into_iter()
        .find(|h| h.drawer_id != id && h.score >= threshold)
        .map(|h| (h.drawer_id, h.score)))
}

/// Cosine similarity; 0.0 when either vector has no length.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// The report lines for the guard: what it held back, or that it did not run.
///
/// Why (#8729): an apply that imports drawers the dream pass will merge must
/// say so rather than report them imported and lose them silently.
/// What: on a dry run, one line saying whether the apply with the same flags
/// screens, naming the flag that turns it off. On an apply, the held-back
/// count, the live drawers the screen could not see, and one line per drawer
/// up to [`HELD_BACK_SHOWN`], then "… and M more"; a warning naming the flag
/// when drawers were imported without the screen.
/// Test: `apply_holds_back_drawers_dream_dedup_would_merge`,
/// `dry_run_near_duplicate_line_follows_the_flags`,
/// `held_back_list_stops_at_twenty_and_counts_the_rest`.
pub fn render(report: &LegacyReport) -> String {
    let threshold = DreamConfig::default().dedup_threshold;
    if report.dry_run {
        // #8729 review: the dry run describes the apply these flags will run.
        return match unscreened_by(report) {
            Some(flag) => format!(
                "  near_duplicates: --apply {flag} does NOT screen; the dream dedup pass may \
                 merge imported drawers at cosine >= {threshold}\n"
            ),
            None => format!(
                "  near_duplicates: --apply holds back drawers dream dedup would merge \
                 (cosine >= {threshold})\n"
            ),
        };
    }
    match &report.near_duplicates {
        Some(held) => {
            let mut out = format!(
                "  near_duplicates={} (held back: dream dedup would merge them at cosine >= \
                 {threshold})\n",
                held.len()
            );
            if report.unscreened_live > 0 {
                out.push_str(&format!(
                    "    {} live drawer(s) have no vector; the screen could not check \
                     against them\n",
                    report.unscreened_live
                ));
            }
            // #8729 review: a mass hold-back must not flood the report.
            for n in held.iter().take(HELD_BACK_SHOWN) {
                out.push_str(&format!(
                    "    held back {} ~ {} (score {:.3})\n",
                    n.id, n.of, n.score
                ));
            }
            if held.len() > HELD_BACK_SHOWN {
                out.push_str(&format!(
                    "    … and {} more\n",
                    held.len() - HELD_BACK_SHOWN
                ));
            }
            out
        }
        None if report.imported > 0 => format!(
            "  near_duplicates: NOT screened ({}); the dream dedup pass may merge imported \
             drawers at cosine >= {threshold}\n",
            unscreened_by(report).unwrap_or("--no-embed or --include-content-duplicates")
        ),
        None => String::new(),
    }
}

/// The flag that keeps an apply from screening, if any.
fn unscreened_by(report: &LegacyReport) -> Option<&'static str> {
    if report.include_content_duplicates {
        Some("--include-content-duplicates")
    } else if report.no_embed {
        Some("--no-embed")
    } else {
        None
    }
}
