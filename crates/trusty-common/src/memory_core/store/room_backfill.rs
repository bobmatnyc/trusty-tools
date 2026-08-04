//! Additive room backfill at palace open (ADR-0027 T2 / D1.4).
//!
//! Why: rooms have been live in the data since long before they had a
//! registry — a quarter of the drawers in the largest palace already sit in
//! eleven non-`General` rooms whose only identity is a 16-byte fold of a
//! `Debug` string. This pass gives every one of those rooms a row and a name.
//!
//! **Existing drawers are neither reclassified nor moved. They are named.**
//! There is no reclassification *rule* because this never writes to the
//! `DRAWERS` table at all — it is a pure insert into a new table, keyed by the
//! id the drawers already carry, so `list_drawers` and `retrieve_l2` keep
//! filtering byte-identically.
//!
//! What: for each distinct `room_id` on the palace's drawers that has no
//! `ROOMS` row yet, resolve a label in four steps (highest confidence first)
//! and insert a row carrying that exact id verbatim:
//!   1. built-in rainbow table — a match is proof, not a guess;
//!   2. direct fold inversion for reprs of at most 16 bytes;
//!   3. KG `room:` subject dictionary — a dictionary the palace itself wrote;
//!   4. `unresolved-<first8>` — never lossy, renameable later.
//!
//! Operationally: idempotent (insert-only, so a human rename survives every
//! reopen), fail-open (a failure degrades room listing, never palace opening),
//! and per-palace on demand — there is no sweep over every palace on disk.
//!
//! Test: `backfill_changes_no_drawer_rows`, `backfill_is_idempotent`,
//! `backfill_rename_survives_second_run`, `backfill_resolves_builtin_rooms`,
//! `backfill_inverts_short_custom_labels`, `backfill_uses_kg_dictionary`,
//! `backfill_falls_back_to_unresolved`.

use crate::memory_core::palace::{Drawer, RoomType};
use crate::memory_core::room_identity::{
    DEFAULT_WING_ID, canonical_room_key, fold_debug_repr, room_label,
};
use crate::memory_core::store::kg::KnowledgeGraph;
use crate::memory_core::store::rooms::RoomRecord;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use uuid::Uuid;

/// Upper bound on KG subjects scanned for the step-3 dictionary.
///
/// Why: `list_subjects` is a full scan of the active-subject counts table; a
/// palace with a large graph should not pay an unbounded cost at open just to
/// name a handful of rooms. Room subjects are a tiny slice of any real graph,
/// so this is generous.
const KG_SUBJECT_SCAN_LIMIT: usize = 20_000;

/// Prefix the KG extractor uses for room membership subjects.
const KG_ROOM_PREFIX: &str = "room:";

/// How a room's label was recovered. Ordered by confidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelSource {
    /// Matched a built-in variant's `Debug` repr exactly (certain).
    Builtin,
    /// Inverted the fold and re-verified by re-hashing (certain).
    FoldInversion,
    /// Matched a `room:` subject the palace's own KG wrote (high confidence).
    KgDictionary,
    /// Synthesised `unresolved-<first8>` — nothing was lost, a rename fixes it.
    Unresolved,
}

impl LabelSource {
    /// Whether the recovered label should be marked `resolved` on disk.
    pub fn is_resolved(self) -> bool {
        !matches!(self, LabelSource::Unresolved)
    }
}

/// What one backfill pass did. Diagnostic only — nothing branches on it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BackfillReport {
    /// Distinct `room_id`s observed on the palace's drawers.
    pub observed: usize,
    /// Rows newly written to `ROOMS`.
    pub inserted: usize,
    /// Rows that already existed and were left untouched.
    pub skipped: usize,
    /// Of `inserted`, how many got a synthesised `unresolved-*` label.
    pub unresolved: usize,
}

/// Run the backfill, swallowing every failure.
///
/// Why (ADR-0027 D1.4, mirroring the `load_drawers` precedent in
/// `retrieval/handle.rs`): a palace whose room registry cannot be written must
/// still open. The cost of failing open is degraded room listing; the cost of
/// failing closed is an unopenable palace. A read-only (snapshot) palace takes
/// this path every time and that is correct — it has nothing to write to.
/// What: calls [`backfill_rooms`] and logs any error at `warn!`.
/// Test: `backfill_fail_open_is_noop_without_drawers`.
pub fn backfill_rooms_fail_open(palace_id: &str, kg: &KnowledgeGraph, drawers: &[Drawer]) {
    if kg.is_read_only() {
        return;
    }
    match backfill_rooms(kg, drawers) {
        Ok(report) if report.inserted > 0 => {
            tracing::info!(
                palace = palace_id,
                observed = report.observed,
                inserted = report.inserted,
                unresolved = report.unresolved,
                "room backfill registered rooms (no drawer rows touched)"
            );
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(
                palace = palace_id,
                "room backfill failed, room listing degraded: {e:#}"
            );
        }
    }
}

/// Register a row for every distinct `room_id` on `drawers` that lacks one.
///
/// Why: see the module doc — this is the additive half of ADR-0027, and its
/// non-negotiable property is that it writes to `ROOMS` / `ROOM_KEYS` only.
/// What: collects the distinct ids (a `BTreeSet`, so visit order — and hence
/// which room wins a canonical-key collision — is deterministic), skips ids
/// that already have a row, resolves a label for the rest, and inserts.
/// Test: `backfill_changes_no_drawer_rows`, `backfill_is_idempotent`.
pub fn backfill_rooms(kg: &KnowledgeGraph, drawers: &[Drawer]) -> Result<BackfillReport> {
    let observed: BTreeSet<Uuid> = drawers.iter().map(|d| d.room_id).collect();
    let mut report = BackfillReport {
        observed: observed.len(),
        ..BackfillReport::default()
    };
    if observed.is_empty() {
        return Ok(report);
    }

    let store = kg.store();
    let now_ms = chrono::Utc::now().timestamp_millis();
    // Step 3's dictionary is loaded lazily: most palaces resolve everything in
    // steps 1-2 and never pay for the KG scan.
    let mut dictionary: Option<Vec<String>> = None;

    for id in observed {
        if store
            .get_room(id)
            .with_context(|| format!("probe room row {id}"))?
            .is_some()
        {
            report.skipped += 1;
            continue;
        }
        let (room, source) = resolve_room_label(kg, id, &mut dictionary);
        // The recovered label is what we store; the id stays exactly what the
        // drawers already carry, which is the entire point.
        let record = RoomRecord::new(&room, now_ms, source.is_resolved());
        let key = canonical_room_key(DEFAULT_WING_ID, &room_label(&room));
        let inserted = store
            .insert_room_if_absent(id, &key, &record)
            .with_context(|| format!("register legacy room {id}"))?;
        if inserted {
            report.inserted += 1;
            if !source.is_resolved() {
                report.unresolved += 1;
            }
        } else {
            report.skipped += 1;
        }
    }

    if report.inserted > 0 {
        store
            .set_room_schema_version()
            .context("stamp room schema version")?;
    }
    Ok(report)
}

/// Resolve one legacy `room_id` to a `RoomType`, four steps, best first.
///
/// Why: the fold that produced these ids is invertible for short inputs and
/// exhaustively enumerable for the nine built-ins, so most rooms can be named
/// with certainty rather than guessed at. Only the wrapped labels (any
/// `Custom` body of 9+ characters) need a dictionary, and only what neither
/// step can name gets a synthetic name.
/// What: rainbow table -> fold inversion -> KG `room:` dictionary ->
/// `unresolved-<first8>`. Steps 1-3 all end in a re-hash equality check
/// against `id`, so a returned label provably folds back to the observed id.
/// Test: `backfill_resolves_builtin_rooms`,
/// `backfill_inverts_short_custom_labels`, `backfill_uses_kg_dictionary`,
/// `backfill_falls_back_to_unresolved`.
fn resolve_room_label(
    kg: &KnowledgeGraph,
    id: Uuid,
    dictionary: &mut Option<Vec<String>>,
) -> (RoomType, LabelSource) {
    if let Some(room) = match_builtin(id) {
        return (room, LabelSource::Builtin);
    }
    if let Some(room) = invert_fold(id) {
        return (room, LabelSource::FoldInversion);
    }
    let subjects = dictionary.get_or_insert_with(|| load_room_subjects(kg));
    if let Some(room) = match_dictionary(id, subjects) {
        return (room, LabelSource::KgDictionary);
    }
    let short = &id.to_string()[..8];
    (
        RoomType::Custom(format!("unresolved-{short}")),
        LabelSource::Unresolved,
    )
}

/// Every built-in `RoomType`, in declaration order.
///
/// Why: the rainbow table and any future exhaustive check both need this list;
/// declaring it once means a new variant cannot be forgotten in one of them.
fn builtins() -> [RoomType; 9] {
    [
        RoomType::Frontend,
        RoomType::Backend,
        RoomType::Testing,
        RoomType::Planning,
        RoomType::Documentation,
        RoomType::Research,
        RoomType::Configuration,
        RoomType::Meetings,
        RoomType::General,
    ]
}

/// Step 1 — rainbow table over the nine built-in `Debug` reprs.
///
/// A match is proof: this is the exact function that produced the id.
fn match_builtin(id: Uuid) -> Option<RoomType> {
    builtins()
        .into_iter()
        .find(|room| fold_debug_repr(&format!("{room:?}")) == id)
}

/// Step 2 — invert the fold for reprs of at most 16 bytes.
///
/// Why: the fold XORs byte *i* into slot *i mod 16*, so for an input of 16
/// bytes or fewer each slot receives at most one byte and `repr[i] =
/// bytes[i] - i` inverts it exactly. Longer inputs wrap and are not
/// invertible — that is step 3's job.
/// What: for each candidate length 16 down to 1, require the remaining slots
/// to be zero, subtract the position offset, require valid UTF-8, parse the
/// `Debug` repr, and re-hash to confirm. The re-hash is what makes this
/// certain rather than plausible.
fn invert_fold(id: Uuid) -> Option<RoomType> {
    let bytes = id.as_bytes();
    for len in (1..=16usize).rev() {
        if bytes[len..].iter().any(|b| *b != 0) {
            continue;
        }
        let candidate: Vec<u8> = bytes[..len]
            .iter()
            .enumerate()
            .map(|(i, b)| b.wrapping_sub(i as u8))
            .collect();
        let Ok(repr) = std::str::from_utf8(&candidate) else {
            continue;
        };
        let Some(room) = parse_debug_repr(repr) else {
            continue;
        };
        if fold_debug_repr(&format!("{room:?}")) == id {
            return Some(room);
        }
    }
    None
}

/// Step 3 — match against the `room:` subjects the palace's own KG wrote.
///
/// Why: for a wrapped label the hash is not invertible, but it is still
/// checkable against a candidate. The KG carries `room:<label>` subjects for
/// rooms that have been written to, which turns an un-invertible hash into a
/// dictionary lookup against a dictionary this very palace produced.
/// Caveat (ADR-0027, Consequences): for a wrapped label a re-hash match is a
/// collision-vulnerable proof, which is exactly why `resolved` and a rename
/// path exist. A wrong *label* costs a rename; no drawer assignment is made.
fn match_dictionary(id: Uuid, subjects: &[String]) -> Option<RoomType> {
    for label in subjects {
        for candidate in [
            RoomType::Custom(label.clone()),
            crate::memory_core::room_identity::room_type_from_parts(label, label),
        ] {
            if fold_debug_repr(&format!("{candidate:?}")) == id {
                return Some(candidate);
            }
        }
    }
    None
}

/// Collect the `room:`-prefixed KG subjects, stripped of the prefix.
///
/// Errors are swallowed to an empty dictionary: a KG read failure must
/// downgrade step 3 to step 4, not fail the backfill.
fn load_room_subjects(kg: &KnowledgeGraph) -> Vec<String> {
    match kg.list_subjects(KG_SUBJECT_SCAN_LIMIT) {
        Ok(subjects) => subjects
            .into_iter()
            .filter_map(|s| s.strip_prefix(KG_ROOM_PREFIX).map(str::to_string))
            .filter(|s| !s.is_empty())
            .collect(),
        Err(e) => {
            tracing::warn!("room dictionary unavailable, falling back to unresolved: {e:#}");
            Vec::new()
        }
    }
}

/// Parse a `RoomType`'s `Debug` repr back into the value that produced it.
///
/// Why: steps 1-2 recover the *repr*, not the enum; `Custom("work")` has to
/// become `RoomType::Custom("work")` before it can be re-hashed or stored.
/// What: exact match against a built-in name, or `Custom("<body>")` where the
/// body contains no escape sequences. A body with a quote or backslash in it
/// is rejected rather than guessed at — it falls through to step 3/4.
fn parse_debug_repr(repr: &str) -> Option<RoomType> {
    if let Some(room) = builtins().into_iter().find(|r| format!("{r:?}") == repr) {
        return Some(room);
    }
    let body = repr.strip_prefix("Custom(\"")?.strip_suffix("\")")?;
    if body.contains('"') || body.contains('\\') || body.is_empty() {
        return None;
    }
    Some(RoomType::Custom(body.to_string()))
}

#[cfg(test)]
#[path = "room_backfill_tests.rs"]
mod tests;
