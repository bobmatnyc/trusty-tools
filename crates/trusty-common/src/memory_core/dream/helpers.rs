//! Pure helper functions and constants for the dream loop.
//!
//! Why: Extracted from dream.rs to keep each file under the 500-SLOC cap
//! (#607). These are stateless helpers shared by the Dreamer passes.
//! What: `extract_keywords`, `is_low_quality_content`, `now_secs`,
//! `merge_into`, `rebuild_index_from_drawers`, content blocklist, stop-words.
//! Test: Indirectly via dream cycle and closet tests.

use crate::memory_core::palace::Drawer;
use crate::memory_core::retrieval::{PalaceHandle, shared_embedder};
use crate::memory_core::store::vector::VectorStore;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Substring patterns whose presence in a drawer's content marks it as
/// low-value auto-capture noise that retroactive dreaming should drop.
///
/// Why: PR #221 introduced an identical blocklist at the write path
/// (`trusty-memory/src/tools.rs`) so new writes never land. But drawers
/// captured before that gate shipped — `Tool use: Bash`, `Claude Code session
/// ended: <uuid>`, etc. — already pollute existing palaces. The dream cycle
/// is the right place to retroactively enforce the same policy without
/// requiring an admin migration script.
/// What: Substring patterns (not regexes) checked via `str::contains` after
/// `str::trim_start`. Mirrors the write-path list exactly so both gates stay
/// in lock-step. Patterns are matched case-sensitively because the
/// auto-capture hooks always emit the exact English prefix.
/// Test: `dream_content_prune_drops_blocklist_drawer`.
pub(crate) const CONTENT_BLOCKLIST: &[&str] = &[
    "Tool use: ",          // Claude Code tool-use captures
    "Claude Code session", // Session lifecycle events
];

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
/// What: Trims leading whitespace, then returns true iff the trimmed content
/// contains any `CONTENT_BLOCKLIST` substring, OR the whitespace-delimited
/// word count is strictly less than `min_words`. An empty `content` (zero
/// words) is always low-quality whenever `min_words >= 1`.
/// Test: `dream_content_prune_drops_blocklist_drawer`,
/// `dream_content_prune_drops_short_drawer`,
/// `dream_content_prune_keeps_good_drawer`.
pub(crate) fn is_low_quality_content(content: &str, min_words: usize) -> bool {
    let trimmed = content.trim_start();
    if CONTENT_BLOCKLIST.iter().any(|pat| trimmed.contains(pat)) {
        return true;
    }
    let word_count = content.split_whitespace().count();
    word_count < min_words
}

/// Current unix timestamp in seconds. Saturates to 0 on clock errors.
pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Merge `loser` content into `survivor` (in-memory drawer table only).
///
/// Why: Dreaming consolidates duplicates without losing information; we
/// concatenate the loser's content into the survivor (capped) and union tags.
/// What: Updates the in-memory drawer entry for `survivor.id`. The vector
/// store entry remains keyed to the survivor; the loser's vector is removed
/// by the caller via `handle.forget`.
pub(crate) fn merge_into(handle: &Arc<PalaceHandle>, survivor: &Drawer, loser: &Drawer) {
    let mut drawers = handle.drawers.write();
    if let Some(target) = drawers.iter_mut().find(|d| d.id == survivor.id) {
        let mut combined = target.content.clone();
        combined.push_str("\n\nAlso: ");
        combined.push_str(&loser.content);
        if combined.len() > 500 {
            combined.truncate(500);
        }
        target.content = combined;
        target.importance = target.importance.max(loser.importance);
        for tag in &loser.tags {
            if !target.tags.contains(tag) {
                target.tags.push(tag.clone());
            }
        }
    }
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
/// up where this one left off).
pub(crate) async fn rebuild_index_from_drawers(
    handle: &Arc<PalaceHandle>,
    started: std::time::Instant,
    budget: Duration,
) -> Result<usize> {
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
        let vecs = embedder
            .embed_batch(std::slice::from_ref(&drawer.content))
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
        for kw in extract_keywords(&drawer.content) {
            new_index.entry(kw).or_default().push(drawer.id);
        }
    }
    new_index
}
