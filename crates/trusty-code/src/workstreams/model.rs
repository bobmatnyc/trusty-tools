//! Workstream domain types (DOC-48 §2).
//!
//! Why: a workstream needs a canonical, serializable shape so it can survive
//! daemon restarts (persisted to the flat JSON store, §3) and be returned
//! over the future RPC/REST surface (#3295) without ambiguity. State is
//! deliberately NOT a stored field — §2.2/§3.2 require it to be *computed*
//! from the store's `active_workstream_id` pointer on every read, so an
//! operator can never desync the persisted state from the pointer that
//! actually governs it.
//! What: [`WorkstreamId`] (a UUID newtype, mirrors
//! `trusty_mpm::session_manager::ManagedSessionId`'s pattern), [`WorkstreamState`]
//! (the computed `active | idle | closed` enum), and [`Workstream`] (the
//! persisted record: id, name, session_ids, timestamps, metadata — §2.1).
//! Test: `model_tests`.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

/// Opaque identifier for a workstream.
///
/// Why: a newtype over [`Uuid`] prevents accidental confusion with other
/// UUID-typed identifiers (e.g. a session id) at the type level.
/// What: wraps `uuid::Uuid`; implements `Display` and serde derive for
/// transparent JSON serialization as a plain UUID string.
/// Test: `model_tests::workstream_id_round_trip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkstreamId(pub Uuid);

impl WorkstreamId {
    /// Generate a new random workstream id.
    ///
    /// Why: every new workstream needs a stable, globally unique identifier
    /// assigned at creation time (§2.1: "Generated on creation").
    /// What: wraps `Uuid::new_v4()`.
    /// Test: used throughout `store_tests`.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Return the inner UUID value.
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for WorkstreamId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for WorkstreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Uuid> for WorkstreamId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

/// Computed lifecycle state of a [`Workstream`] (DOC-48 §2.2).
///
/// Why: the operator never sets this directly — it is inferred fresh on
/// every `get`/`list` from the store's `active_workstream_id` pointer
/// (§3.2), so state can never drift out of sync with the pointer that
/// actually governs which workstream is active.
/// What: `Active` (its id equals the persisted active pointer, regardless of
/// session liveness), `Idle` (default, not active, not closed), `Closed`
/// (marked closed via the reserved `metadata["closed"]` flag — see
/// [`Workstream::is_closed`]; the `workstream.close` RPC verb that sets this
/// flag is #3294/#3295 scope, not this ticket's).
/// Test: `model_tests::state_is_active_iff_pointer_matches`,
/// `model_tests::state_is_closed_when_metadata_flag_set`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkstreamState {
    Active,
    Idle,
    Closed,
}

impl fmt::Display for WorkstreamState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Closed => "closed",
        };
        write!(f, "{s}")
    }
}

/// The reserved `metadata` key marking a workstream as closed (§3.2: "a
/// metadata flag; see future Phase C"). Reserved here so the eventual
/// `workstream.close` verb (#3294/#3295) and this state-inference logic
/// agree on the wire shape without either side inventing its own key.
const CLOSED_METADATA_KEY: &str = "closed";

/// A durable named grouping of sessions, scoped to a single daemon instance
/// (DOC-48 §2.1).
///
/// Why: the domain object that finally gives a workstream daemon-owned
/// identity (DOC-48 §1.1) — previously the in-memory `SessionRegistry` was
/// the sole record and a restart discarded it.
/// What: `id` is immutable; `name` and `metadata` are mutable; `session_ids`
/// is append-only (never removed, per §2.1); `created_at` is immutable,
/// `updated_at` reflects last activity. `state` is deliberately NOT a field —
/// call [`Self::state`] with the store's active pointer to compute it.
/// Test: `model_tests::workstream_serde_round_trip`,
/// `model_tests::new_workstream_starts_idle_with_no_sessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workstream {
    /// Unique, immutable identifier assigned at creation.
    pub id: WorkstreamId,
    /// Human-readable name; mutable (future rename support, Phase C).
    pub name: String,
    /// Sessions bound to this workstream. Append-only — never remove an
    /// entry (§2.1).
    #[serde(default)]
    pub session_ids: Vec<String>,
    /// When the workstream was created (first `workstream.create` call).
    pub created_at: DateTime<Utc>,
    /// Last activity: state inferred, name changed, session attached,
    /// activated/deactivated, or closed.
    pub updated_at: DateTime<Utc>,
    /// Reserved for future use (tags, description, the `closed` flag). Empty
    /// by default; `#[serde(default)]` keeps pre-existing records without
    /// this key deserializable.
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

impl Workstream {
    /// Create a new workstream with the given name, no bound sessions, and
    /// empty metadata.
    ///
    /// Why: the one constructor every creation path (the future
    /// `workstream.create` RPC, and this ticket's store/tests) funnels
    /// through, so `id`/`created_at`/`updated_at` are never hand-assembled
    /// inconsistently.
    /// What: assigns a fresh [`WorkstreamId`], stamps `created_at` ==
    /// `updated_at` == now, and starts with empty `session_ids`/`metadata`.
    /// Test: `model_tests::new_workstream_starts_idle_with_no_sessions`.
    pub fn new(name: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: WorkstreamId::new(),
            name: name.into(),
            session_ids: Vec::new(),
            created_at: now,
            updated_at: now,
            metadata: Map::new(),
        }
    }

    /// Whether this workstream is marked closed via the reserved metadata
    /// flag (§3.2).
    ///
    /// Why: closure itself (`workstream.close`, #3294/#3295) is out of this
    /// ticket's scope, but the domain model must already agree on where that
    /// future verb will record its effect, so state inference is correct
    /// from day one rather than needing a schema migration later.
    /// What: `true` iff `metadata["closed"]` is present and `== true`.
    /// Test: `model_tests::state_is_closed_when_metadata_flag_set`.
    pub fn is_closed(&self) -> bool {
        self.metadata
            .get(CLOSED_METADATA_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// Compute this workstream's state given the store's persisted active
    /// pointer (DOC-48 §2.2/§3.2).
    ///
    /// Why: state must never be read from a stored field — the pointer is
    /// the single source of truth for which workstream is active, and
    /// session liveness never gates this (§3.3: "it does NOT determine
    /// active/idle state").
    /// What: `Active` iff `active_workstream_id == Some(self.id)` (checked
    /// FIRST, matching §3.3 step 3's literal ordering); otherwise `Closed` if
    /// [`Self::is_closed`], else `Idle`.
    /// Test: `model_tests::state_is_active_iff_pointer_matches`,
    /// `model_tests::state_is_closed_when_metadata_flag_set`,
    /// `model_tests::state_defaults_to_idle`.
    pub fn state(&self, active_workstream_id: Option<WorkstreamId>) -> WorkstreamState {
        if active_workstream_id == Some(self.id) {
            WorkstreamState::Active
        } else if self.is_closed() {
            WorkstreamState::Closed
        } else {
            WorkstreamState::Idle
        }
    }

    /// Stamp `updated_at` to now.
    ///
    /// Why: every mutation the store performs on a record (rename, session
    /// bind, activation change, close) must refresh liveness per §2.1's
    /// `updated_at` semantics; centralising it here keeps every call site in
    /// lock-step.
    /// What: sets `updated_at = Utc::now()`.
    /// Test: `model_tests::touch_updates_timestamp`.
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod model_tests;
