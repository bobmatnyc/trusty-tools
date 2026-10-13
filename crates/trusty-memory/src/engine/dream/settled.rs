//! The settled-corpus marker: lets a dream cycle skip a palace it already
//! dreamed with no change since (#9391).
//!
//! Why: the idle loop re-embedded every drawer of an unchanged palace every
//! `idle_secs`, and each run raised the daemon's RSS by 0.8–1.7 GB. A cycle
//! whose embedding passes ran on exactly this drawer set and changed nothing
//! will change nothing again, so it can skip them.
//! What: [`corpus_fingerprint`] digests the inputs of the embedding passes —
//! each drawer's id, content digest and protected flag, plus the dedup
//! threshold and the semantic phase's switch and model. [`record_settled`] stores a fingerprint in
//! `<data_dir>/dream_settled.json` after a cycle that changed nothing, and
//! [`is_settled`] compares the current fingerprint with it. The fingerprint is
//! computed from the drawer table, which is reloaded from disk on open, so the
//! signal survives a daemon restart.
//! Test: `settled_corpus_tests::a_second_cycle_on_an_unchanged_palace_embeds_nothing`,
//! `settled_corpus_tests::a_write_between_cycles_makes_the_next_cycle_embed`,
//! `settled_corpus_tests::an_unreadable_marker_makes_the_cycle_run`,
//! `settled_corpus_tests::enabling_semantic_consolidation_makes_the_next_cycle_run`.

use super::config::DreamConfig;
use crate::memory_core::retrieval::PalaceHandle;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// File name of the per-palace settled-corpus marker.
pub(super) const FILE_NAME: &str = "dream_settled.json";

/// Domain separator and version for [`corpus_fingerprint`]. Bumping the
/// version invalidates every stored marker, so each palace dreams once more.
const FINGERPRINT_DOMAIN: &[u8] = b"trusty-memory/dream-settled/v1";

/// The on-disk marker: the fingerprint of a corpus a full cycle left unchanged.
#[derive(Debug, Serialize, Deserialize)]
struct SettledMarker {
    fingerprint: String,
    recorded_at: chrono::DateTime<chrono::Utc>,
}

/// Digest of everything the embedding passes read from `handle`.
///
/// Why: a write marker or timestamp only catches the write paths that update
/// it, and a forget leaves no timestamp. The drawer table is what dedup, the
/// recall benchmark and semantic consolidation consume, so a digest of it sees
/// every add, forget and content edit, whichever path made it. Recall-side
/// metadata (`access_count`, `last_accessed_at`) is left out, so reading a
/// palace does not make it dream again.
/// #9391: `semantic.enabled` and `semantic.model` are folded in too, so a
/// palace settled while the phase was off, or under another model, runs again
/// once the config changes.
/// What: SHA-256 over the domain tag, `dedup_threshold`, `semantic.enabled`,
/// the length-prefixed `semantic.model`, and one entry per drawer sorted by id:
/// the id, the protected flag, and the stored content digest. A drawer whose
/// digest was never recorded contributes a digest of its raw content instead.
/// Lowercase hex.
/// Test: `settled_corpus_tests::a_write_between_cycles_makes_the_next_cycle_embed`,
/// `settled_corpus_tests::enabling_semantic_consolidation_makes_the_next_cycle_run`,
/// `settled_corpus_tests::changing_the_semantic_model_makes_the_next_cycle_run`.
pub(super) fn corpus_fingerprint(handle: &PalaceHandle, config: &DreamConfig) -> String {
    let mut entries: Vec<(uuid::Uuid, bool, [u8; 32])> = {
        let drawers = handle.drawers.read();
        drawers
            .iter()
            .map(|d| {
                let stored = d.content_hash();
                let mut digest = *stored.as_bytes();
                if stored.is_unset() {
                    digest.copy_from_slice(&Sha256::digest(d.content().as_bytes()));
                }
                (d.id, d.drawer_type.is_protected(), digest)
            })
            .collect()
    };
    // The table's order differs between a live palace and a reopened one.
    entries.sort_unstable_by_key(|(id, _, _)| *id);
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_DOMAIN);
    hasher.update(config.dedup_threshold.to_bits().to_le_bytes());
    hasher.update([u8::from(config.semantic.enabled)]);
    let model = config.semantic.model.as_bytes();
    hasher.update((model.len() as u64).to_le_bytes());
    hasher.update(model);
    for (id, protected, digest) in &entries {
        hasher.update(id.as_bytes());
        hasher.update([u8::from(*protected)]);
        hasher.update(digest);
    }
    hex::encode(hasher.finalize())
}

/// Whether the marker in `data_dir` names `fingerprint`.
///
/// Why: this is the skip decision, so every doubt resolves to "dream". A
/// marker that cannot be read must not skip a palace forever.
/// What: `true` only when `dream_settled.json` exists, parses, and holds
/// exactly `fingerprint`. A missing file is `false`. A read or parse error is
/// `false` and logs a warning — the fail-closed branch.
/// Test: `settled_corpus_tests::an_unreadable_marker_makes_the_cycle_run`.
pub(super) fn is_settled(data_dir: &Path, fingerprint: &str) -> bool {
    match load(data_dir) {
        Ok(Some(marker)) => marker.fingerprint == fingerprint,
        Ok(None) => false,
        Err(e) => {
            // #9391: fail closed — an unreadable signal means the cycle runs.
            tracing::warn!(
                dir = %data_dir.display(),
                "dream settled marker unreadable; running the full cycle: {e:#}"
            );
            false
        }
    }
}

/// Store `fingerprint` as the corpus a full cycle left unchanged.
///
/// What: publishes `dream_settled.json` atomically. On `Err` the previous
/// marker, if any, is unchanged — and it names an older corpus, so the next
/// cycle still runs.
/// Test: `settled_corpus_tests::a_second_cycle_on_an_unchanged_palace_embeds_nothing`.
pub(super) fn record_settled(data_dir: &Path, fingerprint: &str) -> Result<()> {
    let marker = SettledMarker {
        fingerprint: fingerprint.to_string(),
        recorded_at: chrono::Utc::now(),
    };
    let raw = serde_json::to_string_pretty(&marker).context("serialize dream settled marker")?;
    let path = data_dir.join(FILE_NAME);
    crate::atomic_file::write_atomic(&path, raw.as_bytes())
        .with_context(|| format!("write {}", path.display()))
}

/// Read the marker in `data_dir`; `Ok(None)` when there is none.
fn load(data_dir: &Path) -> Result<Option<SettledMarker>> {
    let path = data_dir.join(FILE_NAME);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let marker = serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(marker))
}
