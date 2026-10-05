//! Pure helper functions and constants for the dream loop.
//!
//! Why: Extracted from dream.rs to keep each file under the 500-SLOC cap
//! (#607). These are stateless helpers shared by the Dreamer passes.
//! What: `extract_keywords`, `is_low_quality_content`, `now_secs`,
//! `merged_drawer`, `pick_survivor`, `persist_merge`, `rebuild_index_from_drawers`, content blocklist, stop-words.
//! Test: Indirectly via dream cycle and closet tests.

use crate::memory_core::palace::Drawer;
use crate::memory_core::retrieval::{PalaceHandle, shared_embedder};
use crate::memory_core::store::vector::VectorStore;
use crate::memory_core::timeouts;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Stop-word filter for closet keyword extraction.
pub(crate) const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "of", "in", "on", "at",
    "to", "for", "with", "and", "or", "but", "not", "no", "yes", "i", "you", "he", "she", "it",
    "we", "they", "this", "that", "these", "those", "as", "by", "from", "into", "over", "under",
    "if", "then", "than", "so", "do", "does", "did", "have", "has", "had", "will", "would",
    "shall", "should", "can", "could", "may", "might", "must", "about", "any", "all", "some",
    "more", "most", "such",
];

/// Extract keyword tokens from a drawer's content.
///
/// Why: Closets are a lightweight pre-computed index; we want stable, deduped
/// keyword tokens so the dream cycle's index is reproducible.
/// What: Lowercases, strips non-alphanumeric chars, drops stop-words and
/// tokens shorter than 3 chars, and dedups within a single drawer.
/// Test: Indirectly via `closet_refresh_builds_index`.
pub fn extract_keywords(content: &str) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for raw in content.split_whitespace() {
        let token: String = raw
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect();
        if token.len() < 3 {
            continue;
        }
        if STOP_WORDS.iter().any(|s| *s == token) {
            continue;
        }
        if seen.insert(token.clone()) {
            out.push(token);
        }
    }
    out
}

/// Returns true when `content` should be dropped by the content-quality
/// prune pass.
///
/// Why: Centralises the "is this drawer noise?" decision so the prune pass
/// and its tests share one rule. The rule mirrors the write-path gate
/// (`trusty-memory::tools::blocklist_gate` plus a minimum word-count
/// floor) so a drawer that wouldn't be written today is also a drawer
/// that should not survive the next dream cycle.
/// Issue #2442: previously carried its own `CONTENT_BLOCKLIST` copy checked
/// via `str::contains` (substring-anywhere), which could retroactively prune
/// a drawer that merely QUOTED an auto-capture phrase mid-prose — thinning
/// the recall surface the same way the write-path over-match did. Now
/// delegates to [`crate::memory_core::filter::blocklist_match`], the single
/// prefix-anchored source of truth shared with the write path.
/// What: Trims leading whitespace, then returns true iff the trimmed content
/// is FRAMED by a known blocklist pattern (see `blocklist_match`), OR the
/// whitespace-delimited word count is strictly less than `min_words`. An
/// empty `content` (zero words) is always low-quality whenever
/// `min_words >= 1`.
/// Test: `dream_content_prune_drops_blocklist_drawer`,
/// `dream_content_prune_drops_short_drawer`,
/// `dream_content_prune_keeps_good_drawer`.
pub(crate) fn is_low_quality_content(content: &str, min_words: usize) -> bool {
    if crate::memory_core::filter::blocklist_match(content).is_some() {
        return true;
    }
    let word_count = content.split_whitespace().count();
    word_count < min_words
}

/// Largest prefix of `s` that fits in `max_bytes` and ends on a UTF-8 char
/// boundary.
///
/// Why (#5187): the semantic pass caps its log preview of drawer text at a
/// fixed byte count. Applied as a raw byte offset, the cap panics the moment it
/// lands inside a multi-byte `char` (`assertion failed: self.is_char_boundary`),
/// which killed a `tokio-rt-worker` in the shipped `com.trusty.memory` daemon.
/// Drawer content is arbitrary user text, so CJK, Cyrillic, emoji, and
/// accented Latin all reach the cap.
/// What: rounds `max_bytes` DOWN to the nearest char boundary via
/// `str::floor_char_boundary`, so the result never exceeds `max_bytes` and
/// never splits a `char`. Returns all of `s` when `s` already fits.
/// Test: `char_safe_prefix_stops_below_a_multibyte_char`.
pub(crate) fn char_safe_prefix(s: &str, max_bytes: usize) -> &str {
    &s[..s.floor_char_boundary(max_bytes)]
}

/// Current unix timestamp in seconds. Saturates to 0 on clock errors.
pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Tag that marks a drawer as an owner ruling (#9172).
pub(crate) const RULING_TAG: &str = "ruling";

/// The survivor `loser` leaves behind when a dedup merge folds it in.
///
/// Why (#9172): the merge used to cut the combined text at 500 bytes, which
/// dropped the loser's text and, past the cap, the survivor's own. A dedup
/// merge must not lose text.
/// What: a copy of `survivor` whose content is its own body plus
/// `"\n\nAlso: " + loser` — skipped when the survivor already contains the
/// loser's text (exact duplicates) — with the higher importance and the union
/// of both tag sets. Nothing is truncated.
/// Test: `dedup_survivor_tests::a_dedup_merge_survives_a_palace_reopen`,
/// `a_merge_keeps_multibyte_loser_text_whole`,
/// `merge_into_keeps_the_content_hash_in_step`.
pub(crate) fn merged_drawer(survivor: &Drawer, loser: &Drawer) -> Drawer {
    let mut merged = survivor.clone();
    if !survivor.content().contains(loser.content()) {
        // #5902: `set_content`, so the content digest moves with the body.
        merged.set_content(format!(
            "{}\n\nAlso: {}",
            survivor.content(),
            loser.content()
        ));
    }
    merged.importance = merged.importance.max(loser.importance);
    for tag in &loser.tags {
        if !merged.tags.contains(tag) {
            merged.tags.push(tag.clone());
        }
    }
    merged
}

/// Order a near-duplicate pair as `(survivor, loser)`, or refuse to merge it.
///
/// Why (#9172): importance alone let an older note replace the current one.
/// A drawer holding a `fact_key` slot is the live fact for that slot; deleting
/// it releases the slot.
/// What: returns `None` when both drawers hold a slot. Otherwise the survivor
/// is the drawer that ranks higher on, in order: holds a slot, carries
/// [`RULING_TAG`], newer `created_at`, higher importance. A full tie keeps `a`.
/// Test: `dedup_survivor_tests::a_newer_status_note_survives_an_older_higher_importance_duplicate`,
/// `dedup_survivor_tests::a_slot_holder_or_ruling_outlives_a_more_important_duplicate`.
pub(crate) fn pick_survivor<'a>(a: &'a Drawer, b: &'a Drawer) -> Option<(&'a Drawer, &'a Drawer)> {
    if a.is_tier_c() && b.is_tier_c() {
        return None;
    }
    let rank = |d: &Drawer| (d.is_tier_c(), d.tags.iter().any(|t| t == RULING_TAG));
    let order = rank(a)
        .cmp(&rank(b))
        .then(a.created_at.cmp(&b.created_at))
        .then(a.importance.total_cmp(&b.importance));
    Some(if order.is_lt() { (b, a) } else { (a, b) })
}

/// Merge the pair `a`/`b` and persist the survivor; returns `(survivor, loser)`.
///
/// Why (#9172): the merge changed only the in-memory table, so the loser's
/// text was lost at the next open while its row was already deleted.
/// What: under the palace write mutex and then the commit-order guard (the
/// write pipeline's lock order), reads both drawers from the live table,
/// picks the survivor with [`pick_survivor`], writes [`merged_drawer`] to redb,
/// and only then mirrors it into the table. Returns `Ok(None)` when either
/// drawer is gone or the pair must not merge. The caller deletes the loser
/// only after `Ok(Some(_))`: a failed persist leaves both drawers intact.
/// Test: `dedup_survivor_tests::a_dedup_merge_survives_a_palace_reopen`,
/// `dedup_survivor_tests::a_failed_merge_persist_keeps_the_duplicate`.
pub(crate) async fn persist_merge(
    handle: &Arc<PalaceHandle>,
    a: Uuid,
    b: Uuid,
) -> Result<Option<(Uuid, Uuid)>> {
    let _write_guard = timeouts::lock_with_timeout(
        &handle.write_mutex,
        timeouts::write_lock_timeout(),
        handle.id.as_str(),
    )
    .await?;
    let _order = handle.commit_mutex.lock().await;
    let (a, b) = {
        let drawers = handle.drawers.read();
        let find = |id: Uuid| drawers.iter().find(|d| d.id == id).cloned();
        (find(a), find(b))
    };
    let (Some(a), Some(b)) = (a, b) else {
        return Ok(None);
    };
    let Some((survivor, loser)) = pick_survivor(&a, &b) else {
        return Ok(None);
    };
    let merged = merged_drawer(survivor, loser);
    handle
        .kg
        .upsert_drawer(&merged)
        .await
        .with_context(|| format!("persist merged dedup survivor {}", merged.id))?;
    let pair = (survivor.id, loser.id);
    if let Some(row) = handle.drawers.write().iter_mut().find(|d| d.id == pair.0) {
        *row = merged;
    }
    Ok(Some(pair))
}

/// Reset the vector index and re-upsert every drawer from the in-memory
/// drawer table. Returns the number of drawers re-embedded.
///
/// Why: When the HNSW index accumulates orphans we can't address through
/// `key_map` (pre-fix data, partial writes, schema migrations), the cheapest
/// correct fix is to throw away the index and rebuild from the authoritative
/// drawer table.
/// What: Snapshots drawers, calls `UsearchStore::reset` to truncate the
/// index, then re-embeds and re-upserts each drawer. Respects the budget by
/// stopping early — incomplete rebuilds are still safe (the next cycle picks
/// up where this one left off). #6208: the snapshot→reset→re-upsert runs under
/// the palace write mutex so a concurrent `remember` cannot land a vector that
/// `reset()` then wipes without re-adding.
pub(crate) async fn rebuild_index_from_drawers(
    handle: &Arc<PalaceHandle>,
    started: std::time::Instant,
    budget: Duration,
) -> Result<usize> {
    // #6208: hold the write mutex across the whole rebuild. `reset()` truncates
    // the entire index; a `remember` that upserts between the snapshot and the
    // re-upsert loop would be dropped by the reset and never re-added, because
    // only the snapshotted drawers are re-embedded. Guarding snapshot→reset→
    // re-upsert makes a mid-flight remember impossible during this region — its
    // vector is either in the snapshot (re-added) or lands after release (never
    // reset). The sole caller, `compact_pass`, releases its own guard before
    // calling this, so there is no reentrancy.
    let _write_guard = timeouts::lock_with_timeout(
        &handle.write_mutex,
        timeouts::write_lock_timeout(),
        handle.id.as_str(),
    )
    .await?;
    let snapshot: Vec<Drawer> = handle.drawers.read().clone();
    handle
        .vector_store
        .reset()
        .context("reset vector index for rebuild")?;

    if snapshot.is_empty() {
        return Ok(0);
    }

    let embedder = shared_embedder()
        .await
        .context("acquire shared embedder for dream rebuild")?;

    let mut rebuilt: usize = 0;
    for drawer in snapshot.iter() {
        if started.elapsed() >= budget {
            break;
        }
        let body = drawer.content().to_string();
        let vecs = embedder
            .embed_batch(std::slice::from_ref(&body))
            .await
            .with_context(|| format!("re-embed drawer {}", drawer.id))?;
        if let Some(v) = vecs.into_iter().next() {
            handle
                .vector_store
                .upsert(drawer.id, v)
                .await
                .with_context(|| format!("re-upsert drawer {}", drawer.id))?;
            rebuilt += 1;
        }
    }
    Ok(rebuilt)
}

/// Rebuild closets: simple whitespace tokenization, stop-word filter,
/// keyword -> drawer ids. Returns the number of keywords indexed.
///
/// Why: Centralises closet rebuild logic so both `Dreamer::refresh_closets`
/// and future callers share one implementation.
/// What: Snapshots the drawer table, tokenizes each drawer's content via
/// `extract_keywords`, and builds a fresh `HashMap<String, Vec<Uuid>>`.
/// Test: `closet_refresh_builds_index`.
pub(crate) fn build_closet_index(drawers: &[Drawer]) -> HashMap<String, Vec<Uuid>> {
    let mut new_index: HashMap<String, Vec<Uuid>> = HashMap::new();
    for drawer in drawers.iter() {
        for kw in extract_keywords(drawer.content()) {
            new_index.entry(kw).or_default().push(drawer.id);
        }
    }
    new_index
}
