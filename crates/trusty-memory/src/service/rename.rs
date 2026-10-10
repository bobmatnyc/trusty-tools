//! `MemoryService::rename_palace` — the daemon half of `palace_rename` (#9544).
//!
//! Why: `PalaceRegistry::rename_palace` (trusty-common) moves a palace and
//! leaves `old -> new` aliased, but it touches only the registry's caches. The
//! daemon keys more per-palace state by id — write mutexes, chat-session
//! stores, the BM25 lane, the name and last-used caches, the pin map — and a
//! rename that left any of it under the old id would serve stale state or
//! recreate `<root>/<old>`.
//! What: [`MemoryService::rename_palace`] runs the whole sequence under the
//! write mutexes of both palaces; [`RenameError`] carries each failure at the
//! granularity of its JSON-RPC code.
//! Test: `service/rename_tests.rs`, `tests/palace_rename_9544.rs`.

use std::path::{Component, Path};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::{Mutex, OwnedMutexGuard};
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::registry::{
    PalaceRenameError, RenameOptions, RenameOutcome, DEFAULT_RENAME_BUSY_WAIT,
};
use trusty_common::memory_core::store::{PalaceStore, PalaceStoreError};
use trusty_common::memory_core::timeouts;
use trusty_common::palace_alias::canonical_palace_id;

use super::core::MemoryService;
use super::helpers::palace_info_from;
use crate::transport::rpc::error_codes;
use crate::{AppState, DaemonEvent};

/// How many times the rename re-reads its lock set after a concurrent rename
/// moved one of the ids under it.
const MAX_LOCK_ATTEMPTS: usize = 4;

/// Why a `palace_rename` call failed, at the granularity of its wire code.
///
/// Why: a caller's next move differs per class — fix the arguments, read the
/// refusal, look for the palace, report a fault — so each class is a variant
/// and [`Self::rpc_code`] is the one mapping.
/// What: `InvalidParams` (-32602), `Refused` (-32006, the daemon's own
/// refusals), `Palace` (trusty-common's error, mapped per variant), and
/// `Internal` (-32603).
/// Test: `from_anyhow_maps_palace_rename_errors`.
#[derive(Debug, thiserror::Error)]
pub enum RenameError {
    /// The arguments do not describe a rename; nothing was locked.
    #[error("{0}")]
    InvalidParams(String),
    /// Refused by the daemon itself (a write holds a palace lock).
    #[error("{0}")]
    Refused(String),
    /// Refused or failed inside `PalaceRegistry::rename_palace`.
    #[error("{}", palace_error_message(.0))]
    Palace(PalaceRenameError),
    /// The daemon could not complete or verify the rename.
    #[error("{0}")]
    Internal(String),
}

impl RenameError {
    /// The JSON-RPC code this failure crosses the wire as (#9544 rulings QA-QC).
    pub fn rpc_code(&self) -> i32 {
        match self {
            Self::InvalidParams(_) => error_codes::INVALID_PARAMS,
            Self::Refused(_) => error_codes::REFUSED,
            Self::Internal(_) => error_codes::INTERNAL_ERROR,
            Self::Palace(e) => match e {
                PalaceRenameError::NotFound(_) => error_codes::NOT_FOUND,
                PalaceRenameError::SourceIsAlias { .. }
                | PalaceRenameError::InvalidTarget { .. }
                | PalaceRenameError::TargetIsAlias { .. }
                | PalaceRenameError::TargetExists { .. }
                | PalaceRenameError::TargetNotEmpty { .. }
                | PalaceRenameError::Busy { .. } => error_codes::REFUSED,
                PalaceRenameError::Io { .. } if is_format_too_new(e) => error_codes::REFUSED,
                // Any other Io, and a variant added after this build.
                _ => error_codes::INTERNAL_ERROR,
            },
        }
    }
}

/// Whether a rename failed on a source in a newer on-disk format.
fn is_format_too_new(e: &PalaceRenameError) -> bool {
    match e {
        PalaceRenameError::Io { source, .. } => matches!(
            source.downcast_ref::<PalaceStoreError>(),
            Some(PalaceStoreError::FormatTooNew { .. })
        ),
        _ => false,
    }
}

/// The caller-facing text of a trusty-common rename error.
fn palace_error_message(e: &PalaceRenameError) -> String {
    if is_format_too_new(e) {
        format!("{e}; upgrade trusty-memory to rename this palace")
    } else {
        e.to_string()
    }
}

/// The counts a rename must leave unchanged (acceptance A1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PalaceCounts {
    /// Drawers.
    pub drawers: usize,
    /// Knowledge-graph triples.
    pub triples: usize,
    /// Vectors in the HNSW index.
    pub vectors: usize,
    /// Rooms in the ROOMS registry.
    pub rooms: usize,
    /// Wings in the WINGS registry.
    pub wings: usize,
}

/// Refuse a source id that is not one plain path component (ruling QB).
///
/// Why: `old` is joined onto the data root before any palace check runs; a
/// `../x` would point the lock keys and the counts outside the root.
/// What: trims, then refuses empty, NUL, `/`, `\`, `..`, a leading `.`, and
/// anything that is not exactly one normal path component. Returns the
/// trimmed id.
/// Test: `rename_rejects_path_shaped_old`.
pub(crate) fn validate_rename_source(old: &str) -> Result<&str, RenameError> {
    let old = old.trim();
    let reason = if old.is_empty() {
        Some("it is empty")
    } else if old.contains('\0') {
        Some("it contains a NUL byte")
    } else if old.contains('/') || old.contains('\\') {
        Some("it contains a path separator")
    } else if old.contains("..") {
        Some("it contains `..`")
    } else if old.starts_with('.') {
        Some("it starts with `.`")
    } else if !matches!(
        Path::new(old).components().collect::<Vec<_>>()[..],
        [Component::Normal(_)]
    ) {
        Some("it is not a single path component")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(RenameError::InvalidParams(format!(
            "palace_rename: invalid 'palace_id' {old:?}: {reason}"
        ))),
        None => Ok(old),
    }
}

impl MemoryService {
    /// Rename palace `old` to `new` (#9544).
    ///
    /// Why: see the module docs.
    /// What: validates `old` (before any lock); refuses `new == old` or an
    /// invalid `new`; takes the write mutexes of every id the rename touches
    /// ([`rename_lock_set`]) in sorted order; drops both ids' chat-session
    /// stores and BM25 indexes; counts `old`; runs
    /// `PalaceRegistry::rename_palace` on the blocking pool with
    /// `unaccounted_legacy_data` as the legacy probe; drops both ids' BM25
    /// indexes again (no flush into a moved dir) and the old id's cached
    /// state; emits `PalaceRenamed`; then counts `new`. A count mismatch is
    /// `Internal` naming both sides — the rename is not rolled back. Counts
    /// are skipped only when `<root>/<old>` is already gone (a resumed rename).
    /// Test: `rename_preserves_counts`, `rename_reverse_does_not_deadlock`,
    /// `rename_clears_palace_names_last_used_write_locks_session_stores`,
    /// `rename_emits_palace_renamed_event`, `palace_rename_9544.rs`.
    pub async fn rename_palace(
        &self,
        old: &str,
        new: &str,
        replace_empty: bool,
    ) -> Result<Value, RenameError> {
        let old = validate_rename_source(old)?;
        let new = new.trim();
        if new == old {
            return Err(invalid_target(new, "it is the palace's current id"));
        }
        if !trusty_common::palace_id_is_valid(new) {
            return Err(invalid_target(
                new,
                "not a valid palace id ([a-z0-9][a-z0-9-]{0,62})",
            ));
        }
        let _guards = lock_rename_set(&self.state, old, new).await?;
        let state = &self.state;
        let root = state.data_root.clone();
        let ids = [
            old.to_string(),
            new.to_string(),
            canonical_palace_id(&root, old),
            canonical_palace_id(&root, new),
        ];
        // #9544 (A7): a cached store would read the target as Unconfirmed.
        for id in &ids {
            state.session_stores.remove(id);
        }
        let mut bm25_dropped = evict_bm25(state, &ids).await;
        let before = if root.join(old).join("palace.json").exists() {
            Some(
                count_blocking(state, old)
                    .await
                    .map_err(|e| count_refusal(old, e))?,
            )
        } else {
            None
        };
        let outcome = run_core_rename(state, old, new, replace_empty).await;
        // #9544 (A8): after the move this skips the flush, so it never
        // recreates `<root>/<old>/bm25`.
        bm25_dropped |= evict_bm25(state, &ids).await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(e) => {
                if bm25_dropped {
                    crate::bm25_repair::mark_dirty(state, &canonical_palace_id(&root, old));
                }
                return Err(e);
            }
        };
        if bm25_dropped || state.bm25_dirty.remove(old).is_some() {
            crate::bm25_repair::mark_dirty(state, new);
        }
        forget_old_id(state, old, new);
        state.emit(DaemonEvent::PalaceRenamed {
            old: old.to_string(),
            new: new.to_string(),
        });
        let after = count_blocking(state, new).await.map_err(|e| {
            RenameError::Internal(format!(
                "palace {old:?} was renamed to {new:?}, but its counts could not be read \
                 afterwards ({e:#}); before: {before:?}"
            ))
        })?;
        if let Some(before) = before.filter(|b| *b != after) {
            return Err(RenameError::Internal(format!(
                "palace {old:?} was renamed to {new:?}, but its counts changed: \
                 before {before:?}, after {after:?}; the rename was not rolled back"
            )));
        }
        Ok(outcome_json(&outcome, before, after))
    }
}

/// An `InvalidTarget` refusal, worded as `PalaceRegistry::rename_palace` words it.
fn invalid_target(new: &str, reason: &str) -> RenameError {
    RenameError::Palace(PalaceRenameError::InvalidTarget {
        new: new.to_string(),
        reason: reason.to_string(),
    })
}

/// The write mutexes a rename of `old` to `new` must hold, sorted and deduped.
///
/// Why (#9544, A5): both ids' current palaces must be write-quiet, and so must
/// the literal `new` — after the move every writer of either id lands there.
/// Sorting gives every rename one acquisition order, so `a -> b` racing
/// `b -> a` cannot deadlock; deduping by key and by `Arc::ptr_eq` keeps a
/// reversed rename (both ids resolve to one palace) from locking one mutex
/// twice.
/// What: `(key, mutex)` for `canonical(old)`, `canonical(new)` and the literal
/// `new`, sorted by key, first occurrence kept.
/// Test: `rename_reverse_does_not_deadlock`,
/// `rename_concurrent_opposite_renames_do_not_abba`.
pub(crate) fn rename_lock_set(
    state: &AppState,
    old: &str,
    new: &str,
) -> Vec<(String, Arc<Mutex<()>>)> {
    let root = &state.data_root;
    let literal_new = state
        .palace_write_locks
        .entry(new.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();
    let mut set = vec![
        (canonical_palace_id(root, old), state.palace_write_lock(old)),
        (canonical_palace_id(root, new), state.palace_write_lock(new)),
        (new.to_string(), literal_new),
    ];
    set.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out: Vec<(String, Arc<Mutex<()>>)> = Vec::with_capacity(set.len());
    for (key, mutex) in set {
        if !out.iter().any(|(k, m)| *k == key || Arc::ptr_eq(m, &mutex)) {
            out.push((key, mutex));
        }
    }
    out
}

/// Take every mutex in [`rename_lock_set`], within the write-lock timeout.
///
/// What: acquires in order, then re-reads the set; when a concurrent rename
/// changed it, releases and starts over (bounded). A wait past the timeout is
/// `Refused` (ruling QC: retry when idle).
async fn lock_rename_set(
    state: &AppState,
    old: &str,
    new: &str,
) -> Result<Vec<OwnedMutexGuard<()>>, RenameError> {
    let budget = timeouts::OpBudget::start(timeouts::write_lock_timeout());
    for _ in 0..MAX_LOCK_ATTEMPTS {
        let set = rename_lock_set(state, old, new);
        let mut guards = Vec::with_capacity(set.len());
        for (key, mutex) in &set {
            let wait = budget.leg(timeouts::write_lock_timeout());
            let guard = tokio::time::timeout(wait, Arc::clone(mutex).lock_owned())
                .await
                .map_err(|_| {
                    RenameError::Refused(format!(
                        "palace {key:?} is busy (a write held its lock for {wait:?}); \
                         retry the rename when it is idle"
                    ))
                })?;
            guards.push(guard);
        }
        let again = rename_lock_set(state, old, new);
        let unchanged = again.len() == set.len()
            && again
                .iter()
                .zip(&set)
                .all(|(a, b)| a.0 == b.0 && Arc::ptr_eq(&a.1, &b.1));
        if unchanged {
            return Ok(guards);
        }
    }
    Err(RenameError::Refused(format!(
        "palaces {old:?} and {new:?} kept changing while the rename waited; retry when idle"
    )))
}

/// Drop every id's resident BM25 index; `true` when one held unflushed text.
async fn evict_bm25(state: &AppState, ids: &[String]) -> bool {
    let Some(lane) = state.bm25_lane() else {
        return false;
    };
    let mut dropped = false;
    for id in ids {
        dropped |= lane.evict_palace(id).await;
    }
    dropped
}

/// Read one palace's counts on the blocking pool.
async fn count_blocking(state: &AppState, id: &str) -> anyhow::Result<PalaceCounts> {
    let (state, id) = (state.clone(), id.to_string());
    tokio::task::spawn_blocking(move || count_palace(&state, &id))
        .await
        .map_err(|e| anyhow::anyhow!("count task failed: {e}"))?
}

/// Open `id` and read the counts acceptance A1 compares.
fn count_palace(state: &AppState, id: &str) -> anyhow::Result<PalaceCounts> {
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(id))?;
    let palace = PalaceStore::load_palace(&state.data_root.join(handle.id.as_str()))?;
    let info = palace_info_from(&palace, Some(&handle));
    Ok(PalaceCounts {
        drawers: info.drawer_count,
        triples: info.kg_triple_count,
        vectors: info.vector_count,
        rooms: info.room_count,
        wings: info.wing_count,
    })
}

/// Map a failure to count the source before the rename; nothing has moved.
pub(crate) fn count_refusal(old: &str, e: anyhow::Error) -> RenameError {
    let too_new = e
        .chain()
        .filter_map(|c| c.downcast_ref::<PalaceStoreError>())
        .any(|c| matches!(c, PalaceStoreError::FormatTooNew { .. }));
    if too_new {
        RenameError::Refused(format!(
            "palace {old:?} is in a newer on-disk format ({e:#}); upgrade trusty-memory \
             to rename this palace"
        ))
    } else if trusty_common::memory_core::store::palace_format::is_format_refusal(&e) {
        RenameError::Refused(format!(
            "palace {old:?} cannot be opened in its on-disk format ({e:#}); nothing was renamed"
        ))
    } else {
        RenameError::Internal(format!(
            "palace {old:?} could not be opened to count it ({e:#}); nothing was renamed"
        ))
    }
}

/// Run `PalaceRegistry::rename_palace` on the blocking pool.
async fn run_core_rename(
    state: &AppState,
    old: &str,
    new: &str,
    replace_empty: bool,
) -> Result<RenameOutcome, RenameError> {
    let registry = Arc::clone(&state.registry);
    let root = state.data_root.clone();
    let (old, new) = (old.to_string(), new.to_string());
    tokio::task::spawn_blocking(move || {
        let opts = RenameOptions {
            replace_empty,
            // #9544: never `Default` — its conservative probe refuses any kg.db.
            legacy_probe: &crate::commands::legacy_kg::unaccounted_legacy_data,
            busy_wait: DEFAULT_RENAME_BUSY_WAIT,
        };
        registry.rename_palace(&root, &old, &new, &opts)
    })
    .await
    .map_err(|e| RenameError::Internal(format!("rename task failed: {e}")))?
    .map_err(RenameError::Palace)
}

/// Drop the daemon state the old id no longer owns (A7).
///
/// What: the old id's write-mutex entry (callers hold their guards; a waiter
/// on it re-keys, see `begin_budgeted_write`), both ids' name, last-used and
/// chat-store entries, and moves the pin-map entry to `new`.
pub(crate) fn forget_old_id(state: &AppState, old: &str, new: &str) {
    state.palace_write_locks.remove(old);
    for id in [old, new] {
        state.palace_names.remove(id);
        state.palace_last_used.remove(id);
        state.session_stores.remove(id);
    }
    if let Some((_, root)) = state.pin_project_map.remove(old) {
        state.pin_project_map.insert(new.to_string(), root);
    }
}

/// The success payload.
fn outcome_json(o: &RenameOutcome, before: Option<PalaceCounts>, after: PalaceCounts) -> Value {
    let aliases: Vec<Value> = o
        .alias_changes
        .iter()
        .map(|c| json!({"alias": c.key, "before": c.before, "after": c.after}))
        .collect();
    json!({
        "old": o.old.as_str(),
        "new": o.new.as_str(),
        "trashed_target": o.trashed_target.as_ref().map(|p| p.display().to_string()),
        "resumed": o.resumed,
        "name_rewritten": o.name_rewritten,
        "alias_changes": aliases,
        "counts": {"before": before, "after": after},
    })
}
