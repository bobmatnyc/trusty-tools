//! Memory Palace data model: Palace -> Wing -> Room -> Closet -> Drawer.
//!
//! Why: A 5-level spatial hierarchy is the load-bearing concept for trusty-memory's
//! progressive retrieval; modeling it as Rust types keeps the rest of the system
//! compiler-checked.
//! What: Defines `PalaceId`, `Palace`, `Wing`, `RoomType`, `Room`, and `Drawer`.
//! Test: `cargo test -p trusty-memory-core palace::` constructs each type and
//! verifies serde round-trips.

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
}

impl Drawer {
    /// Create a new drawer with default importance (0.5) and no tags.
    ///
    /// Why: Most call sites only need to specify room_id and content; this avoids
    /// boilerplate at insertion points.
    /// What: Returns a `Drawer` with a fresh UUID and `created_at = now`.
    /// Test: Assert `Drawer::new(room, "x").importance == 0.5` and `id != Uuid::nil()`.
    pub fn new(room_id: Uuid, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            room_id,
            content: content.into(),
            importance: 0.5,
            source_file: None,
            created_at: Utc::now(),
            tags: Vec::new(),
            last_accessed_at: None,
            access_count: 0,
            drawer_type: DrawerType::Unknown,
            expires_at: None,
            completed_at: None,
        }
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
        let d: Drawer = serde_json::from_value(json).expect("legacy decode");
        assert_eq!(d.drawer_type, DrawerType::Unknown);
        assert!(d.expires_at.is_none());
    }

    #[test]
    fn palace_id_display_matches_str() {
        let id = PalaceId::new("trusty-memory");
        assert_eq!(id.to_string(), "trusty-memory");
        assert_eq!(id.as_str(), "trusty-memory");
    }
}
