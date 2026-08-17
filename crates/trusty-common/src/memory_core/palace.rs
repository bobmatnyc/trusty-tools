//! Memory Palace data model: Palace -> Wing -> Room -> Drawer.
//!
//! Why: A 4-level spatial hierarchy is the load-bearing concept for trusty-memory's
//! progressive retrieval; modeling it as Rust types keeps the rest of the system
//! compiler-checked.
//! What: Defines `PalaceId`, `Palace`, `Wing`, `RoomType`, `Room`, and `Drawer`.
//! A *closet* is NOT a hierarchy level: it is the cross-cutting keyword ->
//! drawer-ids inverted index held on `PalaceHandle::closets`
//! (`retrieval/handle.rs`), rebuilt on write and worth a small topical boost
//! during L2/L3 scoring. A drawer belongs to exactly one room and to every
//! closet whose keyword its content contains, so it cannot be a level
//! (ADR-0027 D3).
//! Test: `cargo test -p trusty-memory-core palace::` constructs each type and
//! verifies serde round-trips.

use crate::memory_core::content_hash::{ContentHash, memory_content_hash};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Stable, human-readable identifier for a Palace (e.g. `"trusty-memory"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PalaceId(pub String);

impl PalaceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PalaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Top-level namespace for a project or domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Palace {
    pub id: PalaceId,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub data_dir: PathBuf,
}

/// A wing groups rooms by domain area or agent persona.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wing {
    pub id: Uuid,
    pub palace_id: PalaceId,
    pub name: String,
}

/// Topical category for a Room. Custom variants allow project-specific topics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum RoomType {
    Frontend,
    Backend,
    Testing,
    Planning,
    Documentation,
    Research,
    Configuration,
    Meetings,
    General,
    Custom(String),
}

impl RoomType {
    /// Parse a string into a `RoomType`, falling back to `Custom` for unknown
    /// values.
    ///
    /// Why: CLI and MCP accept a free-form room string; centralizing the
    /// canonicalization keeps the matching logic in one place.
    /// What: Lowercases the input and matches against the stock variants;
    /// any unrecognized value is wrapped in `Custom`.
    /// Test: `room_type_parse` asserts case-insensitive matches and Custom
    /// fallback.
    pub fn parse(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "frontend" => RoomType::Frontend,
            "backend" => RoomType::Backend,
            "testing" | "tests" | "test" => RoomType::Testing,
            "planning" => RoomType::Planning,
            "documentation" | "docs" | "doc" => RoomType::Documentation,
            "research" => RoomType::Research,
            "configuration" | "config" => RoomType::Configuration,
            "meetings" | "meeting" => RoomType::Meetings,
            "general" | "" => RoomType::General,
            other => RoomType::Custom(other.to_string()),
        }
    }
}

/// A room is a topic-bound container of drawers within a wing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub id: Uuid,
    pub wing_id: Uuid,
    pub room_type: RoomType,
}

/// Signal-vs-noise classification for a drawer.
///
/// Why: Issue #61 — palaces accumulated thousands of low-value drawers from
/// auto-capture hooks (tool-use events, raw prompts, commit SHAs). Tagging
/// each drawer with its provenance lets recall, UIs, and TTL sweeps treat
/// curated user facts (`UserFact`) differently from disposable session
/// events (`SessionEvent`).
/// What: An enum stored on every `Drawer`. `Unknown` is the migration
/// default so legacy rows (written before this field existed) deserialize
/// cleanly via `#[serde(default)]`.
/// Test: `drawer_type_serde_default_is_unknown` confirms missing field
/// round-trips to `Unknown`; the classifier tests live in `filter.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DrawerType {
    /// Explicitly stored by user or model for long-term recall.
    UserFact,
    /// Auto-captured session/tool event (lower signal, TTL-eligible).
    SessionEvent,
    /// Written by an agent for inter-turn coordination.
    AgentNote,
    /// Git commit message captured by a hook.
    Commit,
    /// Legacy / unclassified (the serde default for backward compat).
    ///
    /// SERIALIZATION SAFETY: this variant must remain at index 4 (its original
    /// position before `Task` was added) so existing redb-stored drawers whose
    /// postcard bytes carry index 4 still deserialise as `Unknown`. Any new
    /// variant must be appended AFTER this one.
    #[default]
    Unknown,
    /// Task / goal / milestone (spec-001, issue #1722). Protected from the dream cycle:
    /// never evicted (regardless of age or importance) and never consolidated.
    /// Use for long-lived context an application must re-derive across
    /// sessions. An optional [`Drawer::completed_at`] marks a task done; once
    /// set, the drawer becomes eligible for *manual* cleanup but is still never
    /// auto-evicted.
    ///
    /// SERIALIZATION SAFETY: appended at the END (index 5) so pre-existing
    /// postcard data is unaffected. Do NOT insert new variants before this one.
    Task,
}

impl DrawerType {
    /// String tag suitable for JSON serialization in MCP / HTTP responses.
    ///
    /// Why: Consumers want a stable, human-readable label without coupling
    /// to serde's enum encoding.
    /// What: Returns the variant name (`"UserFact"`, `"SessionEvent"`, etc.).
    /// Test: `drawer_type_as_str_matches_variant`.
    pub fn as_str(&self) -> &'static str {
        match self {
            DrawerType::UserFact => "UserFact",
            DrawerType::SessionEvent => "SessionEvent",
            DrawerType::AgentNote => "AgentNote",
            DrawerType::Commit => "Commit",
            DrawerType::Task => "Task",
            DrawerType::Unknown => "Unknown",
        }
    }

    /// The inverse of [`Self::as_str`]: a stored or transmitted tag back to a
    /// variant.
    ///
    /// Why (#5902): the projection existed only as `parse_drawer_type` inside
    /// `store::kg_redb::types`, private to that module, so the JSONL import path
    /// would have needed a second copy of the same match — and a copy is how the
    /// two spellings drift the next time a variant is added. It belongs beside
    /// `as_str`, whose inverse it is. `kg_redb`'s helper now delegates here.
    /// What: `None` and any unrecognised tag both yield `Unknown`, which is the
    /// documented decode default for a row or record written by a version that
    /// knew a variant this one does not.
    /// Test: `drawer_type_tag_round_trips_every_variant`,
    /// `drawer_type_from_unknown_tag_is_unknown`.
    pub fn from_tag(tag: Option<&str>) -> Self {
        match tag {
            Some("UserFact") => DrawerType::UserFact,
            Some("SessionEvent") => DrawerType::SessionEvent,
            Some("AgentNote") => DrawerType::AgentNote,
            Some("Commit") => DrawerType::Commit,
            Some("Task") => DrawerType::Task,
            _ => DrawerType::Unknown,
        }
    }

    /// Whether this drawer type is protected from the dream cycle.
    ///
    /// Why (spec-001): `Task` drawers hold goals/checkpoints an application
    /// must survive history compaction; the dream cycle's eviction and
    /// consolidation passes consult this so the protection lives in one place
    /// rather than being re-derived at every call site.
    /// What: returns `true` only for `DrawerType::Task`.
    /// Test: `task_drawer_is_protected` in this module; behavioural coverage in
    /// `tests/memory_palace.rs` and the dream cycle tests.
    pub fn is_protected(&self) -> bool {
        matches!(self, DrawerType::Task)
    }
}

/// Atomic memory unit: verbatim text plus metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drawer {
    pub id: Uuid,
    pub room_id: Uuid,
    pub content: String,
    /// Importance in [0.0, 1.0]. Used to rank L1 essential drawers.
    pub importance: f32,
    pub source_file: Option<PathBuf>,
    pub created_at: DateTime<Utc>,
    pub tags: Vec<String>,
    /// Timestamp of the most recent recall hit, if any.
    #[serde(default)]
    pub last_accessed_at: Option<DateTime<Utc>>,
    /// Number of times this drawer has been returned in a recall result.
    #[serde(default)]
    pub access_count: u32,
    /// Signal-vs-noise classification (issue #61). Legacy rows decode to
    /// `DrawerType::Unknown` via `#[serde(default)]`.
    #[serde(default)]
    pub drawer_type: DrawerType,
    /// Optional expiry timestamp. When set and in the past, the drawer is
    /// pruned by `PalaceHandle::purge_expired` on open (issue #61). Session
    /// events default to a 7-day TTL; user facts never expire.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// spec-001: completion timestamp for `DrawerType::Task` drawers. `None`
    /// for an open task (and for every non-Task drawer). Setting it marks the
    /// task done so the application can treat it as eligible for manual
    /// cleanup; it never triggers automatic eviction. Legacy rows decode to
    /// `None` via `#[serde(default)]`.
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    /// #4884: ADR-0028 D5 slot name for a Tier C ("current") fact — an
    /// explicit, writer-chosen, namespaced key of the form
    /// `<domain>:<id>/<aspect>` (`pr:4818/state`, `ws:foo/resume`). One slot
    /// holds one live fact, so writing a key that is already occupied is what
    /// will retire the prior occupant. Storage groundwork only: nothing writes
    /// a non-`None` value yet — the Tier C write path is a later ticket — but
    /// the field and its index (`DRAWERS_BY_FACT_KEY`) must exist first so no
    /// row has to be rewritten when it lands. Legacy rows decode to `None` via
    /// `#[serde(default)]`.
    #[serde(default)]
    pub fact_key: Option<String>,
    /// #5902: content-addressed identity of [`Self::content`] — the join key
    /// that lets two machines recognise one fact written twice.
    ///
    /// Why: this coexists with [`Self::id`] rather than replacing it. That UUID
    /// is the HNSW vector-store key, the `drawer:{id}` KG triple subject, and
    /// the value in the `DRAWERS_BY_FACT_KEY` slot index; replacing it would
    /// mean rewiring all three. The digest answers a different question — "is
    /// this the same fact?" — which no UUID can answer across machines.
    ///
    /// Why it is DERIVED, not persisted: the digest is a pure function of
    /// `content`, so storing it in the redb `DrawerRecord` would create a second
    /// source of truth that can disagree with the first. It would, too:
    /// `dream::helpers::merge_into` rewrites `content` in place on the
    /// in-memory table. So this field is a cache, recomputed from `content` at
    /// every point a drawer enters memory — [`Self::new`], the redb hydration in
    /// `store::kg_redb::read_ops::load_drawers`, and the L1-snapshot load — and
    /// recomputed on the one path that changes content, [`Self::set_content`].
    /// Nothing reads it from disk, so no postcard migration and no stale digest
    /// is reachable.
    ///
    /// What: [`ContentHash::UNSET`] via `#[serde(default)]` for JSON written
    /// before this field existed; [`Self::refresh_content_hash`] replaces the
    /// sentinel (or a stale value) with the digest of the current content.
    /// Test: `drawer_new_hashes_its_content`,
    /// `set_content_recomputes_the_hash`,
    /// `refresh_content_hash_heals_an_unset_or_stale_digest`,
    /// `drawer_type_serde_default_is_unknown` (the legacy-decode arm).
    #[serde(default)]
    pub content_hash: ContentHash,
}

impl Drawer {
    /// Create a new drawer with default importance (0.5) and no tags.
    ///
    /// Why: Most call sites only need to specify room_id and content; this avoids
    /// boilerplate at insertion points.
    /// What: Returns a `Drawer` with a fresh UUID and `created_at = now`.
    /// Test: Assert `Drawer::new(room, "x").importance == 0.5` and `id != Uuid::nil()`.
    pub fn new(room_id: Uuid, content: impl Into<String>) -> Self {
        let content = content.into();
        // #5902: the digest is derived from content at every entry point, so a
        // freshly built drawer never carries the UNSET sentinel.
        let content_hash = memory_content_hash(&content);
        Self {
            id: Uuid::new_v4(),
            room_id,
            content,
            importance: 0.5,
            source_file: None,
            created_at: Utc::now(),
            tags: Vec::new(),
            last_accessed_at: None,
            access_count: 0,
            drawer_type: DrawerType::Unknown,
            expires_at: None,
            completed_at: None,
            // #4884: a drawer claims no slot until a Tier C write names one,
            // exactly as `expires_at` stays `None` until a TTL is set.
            fact_key: None,
            content_hash,
        }
    }

    /// Replace this drawer's body, keeping [`Self::content_hash`] in agreement
    /// with it (#5902).
    ///
    /// Why: `content` is a public field, so a direct assignment can desync the
    /// derived digest. This is the one supported way to change a body, and
    /// `dream::helpers::merge_into` — the only production path that rewrites
    /// content in place — routes through it. A drawer whose digest disagreed
    /// with its content would export under an identity nobody else can
    /// reproduce, so the two must move together.
    /// What: stores `content` verbatim (no normalization — normalization is for
    /// hashing only) and recomputes the digest from it.
    /// Test: `set_content_recomputes_the_hash`,
    /// `dream::tests::merge_into_keeps_the_content_hash_in_step`.
    pub fn set_content(&mut self, content: impl Into<String>) {
        self.content = content.into();
        self.content_hash = memory_content_hash(&self.content);
    }

    /// Recompute [`Self::content_hash`] from the current content (#5902).
    ///
    /// Why: the digest is derived, and every path that materialises a `Drawer`
    /// from bytes it did not compute — redb hydration, the L1 JSON snapshot, an
    /// imported JSONL record — must derive it rather than trust what it read.
    /// That covers three cases with one call: a legacy row that has no digest
    /// (decodes to [`ContentHash::UNSET`]), a snapshot written by an older
    /// binary under a different [`crate::memory_core::content_hash::CONTENT_HASH_VERSION`],
    /// and a hostile or corrupt file asserting a digest its body does not have.
    /// What: unconditionally sets the digest to the hash of the current content.
    /// Cheap enough to call per row on a cold open — a SHA-256 over a few
    /// hundred bytes, against the redb decode and NFC pass already happening.
    /// Test: `refresh_content_hash_heals_an_unset_or_stale_digest`.
    pub fn refresh_content_hash(&mut self) {
        self.content_hash = memory_content_hash(&self.content);
    }

    /// Builder helper: set the `drawer_type` and apply the matching default
    /// expiry policy.
    ///
    /// Why: Issue #61 — `SessionEvent` drawers should auto-expire after 7
    /// days so the dream/open sweep can reclaim them; `UserFact` /
    /// `AgentNote` / `Commit` never expire by default. Centralising the
    /// policy here keeps call sites from forgetting to set the TTL.
    /// What: Stores the type and, when it is `SessionEvent`, sets
    /// `expires_at = Some(created_at + 7 days)`. Other types leave
    /// `expires_at` untouched.
    /// Test: `drawer_with_type_sets_session_ttl`.
    pub fn with_type(mut self, drawer_type: DrawerType) -> Self {
        self.drawer_type = drawer_type;
        if drawer_type == DrawerType::SessionEvent && self.expires_at.is_none() {
            self.expires_at = Some(self.created_at + chrono::Duration::days(7));
        }
        self
    }

    /// Whether this drawer's TTL has elapsed as of `now`.
    ///
    /// Why: #4885 — before this existed, "is it expired" was written out by
    /// hand in two places (the sweep inside `PalaceHandle::open_with_intent`
    /// and `PalaceHandle::purge_expired`) and in neither of the paths that
    /// actually serve recall. Production opens the palace once as
    /// `OpenIntent::Writer` and holds it for the process lifetime, so a drawer
    /// that expired mid-session kept being returned until the daemon
    /// restarted. ADR-0028 D4 makes read-time the enforcement point precisely
    /// because a sweep that fails silently reintroduces that bug; one shared
    /// predicate is what keeps the sweep and the read paths from drifting to
    /// different answers.
    /// What: `true` when `expires_at` is set and strictly earlier than `now`.
    /// `None` never expires, and a TTL exactly equal to `now` has not yet
    /// elapsed. `now` is a parameter, not `Utc::now()`, so one filtering pass
    /// judges every drawer against a single instant and tests can pin the
    /// clock.
    /// Test: `is_expired_at_is_strict_and_none_never_expires` in this module;
    /// `expired_l1_drawer_is_excluded_without_reopen` and
    /// `expired_drawer_is_excluded_from_l2` cover the read path,
    /// `purge_expired_drops_only_past_ttl` the sweep.
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|t| t < now)
    }

    /// Whether this drawer currently occupies an ADR-0028 Tier C slot (#4886).
    ///
    /// Why: expiry means two different things for the two kinds of drawer, and
    /// the reclamation sweeps need to tell them apart. For an ordinary drawer,
    /// `expires_at` is a lifetime — when it elapses the row is reclaimable, and
    /// the sweeps hard-delete it. For a Tier C fact, `expires_at` is the
    /// *retirement condition* D4 required at admission: when it elapses the
    /// fact stops being privileged, which read-time expiry (#4885) already
    /// enforces on every recall. Deleting the row on top of that would
    /// contradict D6 ("Demoted, never deleted") and destroy the record the
    /// supersession pointer (#4887) is meant to hang off — and D6 rejects hard
    /// deletion on evidence, not principle: this estate hand-wrote 109
    /// amendment edges precisely so corrections survive. So the sweeps skip
    /// these, and a Tier C fact leaves the tier by being superseded (which
    /// clears both fields, returning it to an ordinary permanent Tier E
    /// drawer) rather than by being erased.
    /// What: `true` while `fact_key` is set. A superseded drawer has had its
    /// `fact_key` cleared by the retire-on-write path, so it is `false` again
    /// and the sweeps treat it exactly as they treat any other drawer.
    /// Test: `expired_tier_c_drawer_survives_the_open_time_sweep`,
    /// `purge_expired_leaves_tier_c_drawers_alone`.
    pub fn is_tier_c(&self) -> bool {
        self.fact_key.is_some()
    }

    /// Accumulated access boost for decay calculation.
    ///
    /// Why: Frequently recalled drawers should resist decay; this exposes the
    /// computed boost so `DecayConfig::effective_importance` stays pure.
    /// What: `(access_count * config.access_boost).min(config.access_boost_cap)`
    /// Test: See `decay::tests::drawer_accumulated_boost`.
    pub fn accumulated_boost(&self, config: &crate::memory_core::decay::DecayConfig) -> f32 {
        (self.access_count as f32 * config.access_boost).min(config.access_boost_cap)
    }

    /// Record a recall hit: update `last_accessed_at` and increment `access_count`.
    ///
    /// Why: Retrieval paths must call this when a drawer is returned so the
    /// access boost reflects real usage.
    /// What: Sets `last_accessed_at = now()` and saturates `access_count`.
    /// Test: After two `record_access()` calls, `access_count == 2`.
    pub fn record_access(&mut self) {
        self.last_accessed_at = Some(Utc::now());
        self.access_count = self.access_count.saturating_add(1);
    }
}

/// Total order for picking a bounded set of drawers: importance desc, then
/// recency desc, then id asc.
///
/// Why (#4836): four call sites ranked on importance ALONE before truncating —
/// `PalaceHandle::list_drawers`, `list_drawers_in_wing`,
/// `PalaceHandle::refresh_l1`, and `L1Cache::save_l1_cache`. Rust's sort is
/// stable, so drawers tied on importance kept whatever order the input
/// happened to have — UUID ascending out of the drawer table, insertion order
/// out of the in-memory Vec — and the truncation cut on that. `Drawer::new`
/// mints a v4 UUID, so neither order correlates with age. Importance is
/// effectively bimodal in a live palace (1.0 for curated facts, 0.5 for
/// everything else), so the tie-break decided almost every result:
/// `memory_list(tag = "pre-authorized", limit = 12)` against the
/// `trusty-tools` palace matched 94 drawers and returned the 12 lowest UUIDs,
/// leaving all nine drawers written that day outside the window.
///
/// This lives beside `Drawer` rather than in `retrieval` because the two L1
/// sites straddle the module boundary: `store::l1_cache` needs the same order
/// and must not depend upward on `retrieval`.
///
/// Importance stays the PRIMARY key — this only decides ties, so curated
/// essentials still lead. `list_drawers_ranks_importance_above_recency` is the
/// guard against that being flipped.
///
/// What: `importance` descending, then `created_at` descending, then `id`
/// ascending. A NaN importance compares `Equal` and falls through to the two
/// total keys rather than leaving the pair unordered.
/// Test: `list_drawers_keeps_the_newest_drawer_within_an_importance_tie`,
/// `list_drawers_in_wing_keeps_the_newest_drawer_within_an_importance_tie`,
/// `list_drawers_ranks_importance_above_recency`,
/// `refresh_l1_keeps_the_newest_drawers_within_an_importance_tie`,
/// `l1_snapshot_keeps_the_newest_drawers_within_an_importance_tie`.
pub(crate) fn drawer_listing_order(a: &Drawer, b: &Drawer) -> std::cmp::Ordering {
    b.importance
        .partial_cmp(&a.importance)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| b.created_at.cmp(&a.created_at))
        .then_with(|| a.id.cmp(&b.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawer_new_has_default_importance() {
        let d = Drawer::new(Uuid::new_v4(), "hello");
        assert_eq!(d.importance, 0.5);
        assert_eq!(d.content, "hello");
        assert!(d.tags.is_empty());
    }

    #[test]
    fn room_type_parse() {
        assert_eq!(RoomType::parse("backend"), RoomType::Backend);
        assert_eq!(RoomType::parse("Backend"), RoomType::Backend);
        assert_eq!(RoomType::parse("docs"), RoomType::Documentation);
        assert_eq!(RoomType::parse("general"), RoomType::General);
        assert_eq!(RoomType::parse("ops"), RoomType::Custom("ops".to_string()));
    }

    #[test]
    fn drawer_type_as_str_matches_variant() {
        assert_eq!(DrawerType::UserFact.as_str(), "UserFact");
        assert_eq!(DrawerType::SessionEvent.as_str(), "SessionEvent");
        assert_eq!(DrawerType::AgentNote.as_str(), "AgentNote");
        assert_eq!(DrawerType::Commit.as_str(), "Commit");
        assert_eq!(DrawerType::Task.as_str(), "Task");
        assert_eq!(DrawerType::Unknown.as_str(), "Unknown");
    }

    /// #5902: `as_str` and `from_tag` are one projection, so every variant must
    /// survive a round trip. A variant added to only one half is the drift this
    /// pairing exists to make impossible.
    #[test]
    fn drawer_type_tag_round_trips_every_variant() {
        for t in [
            DrawerType::UserFact,
            DrawerType::SessionEvent,
            DrawerType::AgentNote,
            DrawerType::Commit,
            DrawerType::Task,
            DrawerType::Unknown,
        ] {
            assert_eq!(DrawerType::from_tag(Some(t.as_str())), t, "{t:?}");
        }
    }

    #[test]
    fn drawer_type_from_unknown_tag_is_unknown() {
        assert_eq!(DrawerType::from_tag(None), DrawerType::Unknown);
        assert_eq!(
            DrawerType::from_tag(Some("SomethingFromTheFuture")),
            DrawerType::Unknown
        );
        assert_eq!(DrawerType::from_tag(Some("userfact")), DrawerType::Unknown);
    }

    #[test]
    fn task_drawer_is_protected() {
        assert!(DrawerType::Task.is_protected());
        for t in [
            DrawerType::UserFact,
            DrawerType::SessionEvent,
            DrawerType::AgentNote,
            DrawerType::Commit,
            DrawerType::Unknown,
        ] {
            assert!(!t.is_protected(), "{t:?} must not be protected");
        }
    }

    #[test]
    fn task_drawer_has_no_default_ttl() {
        // Task drawers must never auto-expire — `with_type` only sets a TTL for
        // SessionEvent, so Task falls through with `expires_at == None`.
        let d = Drawer::new(Uuid::new_v4(), "ship v2").with_type(DrawerType::Task);
        assert_eq!(d.drawer_type, DrawerType::Task);
        assert!(d.expires_at.is_none(), "task drawers never expire");
        assert!(d.completed_at.is_none(), "fresh task is not completed");
    }

    #[test]
    fn drawer_with_type_sets_session_ttl() {
        let d =
            Drawer::new(Uuid::new_v4(), "auto-captured event").with_type(DrawerType::SessionEvent);
        assert_eq!(d.drawer_type, DrawerType::SessionEvent);
        let ttl = d.expires_at.expect("session events get a TTL");
        let delta = ttl - d.created_at;
        // 7 days ± 1 second tolerance.
        assert!(delta.num_seconds() >= 6 * 24 * 3600);

        let fact = Drawer::new(Uuid::new_v4(), "x").with_type(DrawerType::UserFact);
        assert!(fact.expires_at.is_none(), "user facts must not expire");
    }

    #[test]
    fn drawer_type_serde_default_is_unknown() {
        // Legacy JSON without `drawer_type` / `expires_at` must deserialize.
        let json = serde_json::json!({
            "id": Uuid::new_v4(),
            "room_id": Uuid::new_v4(),
            "content": "legacy",
            "importance": 0.5,
            "source_file": null,
            "created_at": Utc::now().to_rfc3339(),
            "tags": [],
        });
        let mut d: Drawer = serde_json::from_value(json).expect("legacy decode");
        assert_eq!(d.drawer_type, DrawerType::Unknown);
        assert!(d.expires_at.is_none());
        // #4884: JSON written before `fact_key` existed must decode to "claims
        // no slot", not fail the whole drawer.
        assert!(d.fact_key.is_none());
        // #5902: JSON written before `content_hash` existed decodes to the UNSET
        // sentinel, and the hydration paths' `refresh_content_hash` is what
        // turns that into a real digest.
        assert!(d.content_hash.is_unset());
        d.refresh_content_hash();
        assert_eq!(d.content_hash, memory_content_hash("legacy"));
    }

    /// #5902: the derived-digest invariant at the drawer's own two write points.
    #[test]
    fn drawer_new_hashes_its_content() {
        let d = Drawer::new(Uuid::new_v4(), "the daemon binds loopback only");
        assert!(!d.content_hash.is_unset());
        assert_eq!(
            d.content_hash,
            memory_content_hash("the daemon binds loopback only")
        );
        // Two drawers with the same body share an identity while their UUIDs
        // differ — the whole point of the field.
        let e = Drawer::new(Uuid::new_v4(), "the daemon binds loopback only");
        assert_eq!(d.content_hash, e.content_hash);
        assert_ne!(d.id, e.id);
    }

    #[test]
    fn set_content_recomputes_the_hash() {
        let mut d = Drawer::new(Uuid::new_v4(), "first");
        let first = d.content_hash;
        d.set_content("second");
        assert_eq!(d.content, "second", "content is stored verbatim");
        assert_ne!(d.content_hash, first);
        assert_eq!(d.content_hash, memory_content_hash("second"));
    }

    /// Why: the hydration paths must DERIVE the digest, never trust what they
    /// read. This covers all three ways a read digest can be wrong: absent,
    /// stale, and outright false.
    /// Test: This test.
    #[test]
    fn refresh_content_hash_heals_an_unset_or_stale_digest() {
        let mut d = Drawer::new(Uuid::new_v4(), "body");

        d.content_hash = ContentHash::UNSET;
        d.refresh_content_hash();
        assert_eq!(d.content_hash, memory_content_hash("body"));

        d.content_hash = memory_content_hash("a different body entirely");
        d.refresh_content_hash();
        assert_eq!(d.content_hash, memory_content_hash("body"));
    }

    /// #4885: the one predicate the open-time sweep, `purge_expired`, and every
    /// retrieval layer share. Its two edges are what the callers depend on:
    /// `None` must mean "never expires" (most drawers), and the comparison must
    /// be strict so a drawer is not treated as expired at the exact instant its
    /// TTL is reached.
    #[test]
    fn is_expired_at_is_strict_and_none_never_expires() {
        let now = Utc::now();
        let room = Uuid::new_v4();

        let permanent = Drawer::new(room, "standing rule");
        assert!(
            !permanent.is_expired_at(now),
            "expires_at = None must never expire"
        );

        let mut past = Drawer::new(room, "stale");
        past.expires_at = Some(now - chrono::Duration::seconds(1));
        assert!(past.is_expired_at(now));

        let mut future = Drawer::new(room, "live");
        future.expires_at = Some(now + chrono::Duration::seconds(1));
        assert!(!future.is_expired_at(now));

        let mut exact = Drawer::new(room, "boundary");
        exact.expires_at = Some(now);
        assert!(
            !exact.is_expired_at(now),
            "a TTL equal to now has not yet elapsed"
        );
    }

    #[test]
    fn palace_id_display_matches_str() {
        let id = PalaceId::new("trusty-memory");
        assert_eq!(id.to_string(), "trusty-memory");
        assert_eq!(id.as_str(), "trusty-memory");
    }
}
