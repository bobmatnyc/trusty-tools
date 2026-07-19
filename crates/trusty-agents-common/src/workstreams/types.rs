//! Value types for the DOC-44 Phase 2 workstream ledger (issue #3008, twin
//! Phase 2, DOC-44 §4.2).
//!
//! Why: DOC-44 §4.2 sketches a `Workstream` struct the lead agent (Layer 2,
//! not built by this ticket) uses to track work across both `tm` and `tcode`
//! without ever migrating a workstream between tools mid-lifecycle (§4.1).
//! The sketch also carries `lead_confidence: f64` and
//! `state: ConfidenceState (Closed | Open | HalfOpen)` — those belong to
//! Phase 4 ("Confidence Gate & State Machine", DOC-44 §8), which has no
//! consumer yet; adding them here now would be a speculative field with
//! nothing to read or write it, so they are deliberately deferred to the
//! Phase 4 ticket rather than stubbed in early.
//! What: [`Harness`] (tags a workstream's tool assignment, aligned with
//! [`crate::connectors::BackendParams`]'s `Tm`/`Tcode` variants — see that
//! type's docs for why Phase 1 modelled backend-specific creation params as
//! an enum), [`WorkstreamStatus`] (the DOC-44 §4.2 lifecycle:
//! proposed/in_progress/blocked/complete/delivered), [`Priority`]
//! (P0..P3), [`Workstream`] (the persisted record), and [`NewWorkstream`]
//! (the creation input — deliberately excludes `id`/timestamps/`status`,
//! which [`super::WorkstreamLedger::create`] assigns).
//! Test: `types::tests` covers `Harness::from(&BackendParams)`, serde
//! round-trips for every enum/struct, and `Priority`'s declared ordering
//! (P0 is the highest priority, so `P0 < P1 < P2 < P3`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::connectors::BackendParams;

/// Which harness a workstream is assigned to — immutable after creation
/// (DOC-44 §4.1's "no mid-life migration" decision).
///
/// Why: DOC-44 §4.2 sketches `assigned_tool: ToolId (tm | tcode)`. Rather
/// than invent a third naming (`ToolId`) alongside Phase 1's
/// `BackendParams::{Tm, Tcode}` (issue #3008's scope note: "the harness
/// enum/tag should align with the connectors' `BackendParams` variants...
/// rather than inventing a third naming"), this type reuses those exact
/// variant names. It stays a separate type from `BackendParams` rather than
/// a re-export because `BackendParams` also carries backend-specific
/// *session-creation* fields (`repo_url`, `project`, ...) the ledger has no
/// use for — a workstream just needs the discriminant.
/// What: two variants, `Tm` and `Tcode`. `From<&BackendParams>` gives a
/// lossless, no-panic conversion for a caller that already built a
/// connector creation request. Serializes lowercase (`"tm"` / `"tcode"`) for
/// a stable, human-diffable on-disk JSON shape.
/// Test: `types::tests::harness_from_backend_params_tm_and_tcode`,
/// `types::tests::harness_serde_round_trip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    /// trusty-mpm (`tm`) — worktree-provisioned CLI/TUI sessions.
    Tm,
    /// trusty-code (`tcode`) — local-project-scoped GUI sessions.
    Tcode,
}

impl From<&BackendParams> for Harness {
    fn from(params: &BackendParams) -> Self {
        match params {
            BackendParams::Tm { .. } => Harness::Tm,
            BackendParams::Tcode { .. } => Harness::Tcode,
        }
    }
}

/// Lifecycle status of a workstream (DOC-44 §4.2).
///
/// Why: the spec's `WorkstreamStatus (proposed | in_progress | blocked |
/// complete | delivered)` is reproduced verbatim — this is the one enum in
/// the sketch with a fully concrete, closed vocabulary and no dependency on
/// unbuilt Phase 4 machinery.
/// What: five variants, serialized `snake_case` to match the spec's literal
/// tokens on disk (`"in_progress"`, not `"InProgress"`).
/// Test: `types::tests::workstream_status_serde_round_trip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkstreamStatus {
    /// Identified but not yet started.
    Proposed,
    /// Actively being worked in the assigned harness.
    InProgress,
    /// Stalled on a blocker (see [`Workstream::blockers`]).
    Blocked,
    /// Work is done; not yet delivered/merged.
    Complete,
    /// Delivered (merged/shipped) — terminal state.
    Delivered,
}

/// Priority band (DOC-44 §4.2: `Priority (P0 | P1 | P2 | P3)`).
///
/// Why: reproduced verbatim from the spec sketch.
/// What: `P0` is the highest priority. Derives `Ord` in declaration order so
/// `P0 < P1 < P2 < P3`, matching the intuitive "lower number sorts first /
/// matters more" convention used elsewhere in this workspace's issue
/// tracker.
/// Test: `types::tests::priority_orders_p0_highest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Priority {
    P0,
    P1,
    P2,
    P3,
}

/// A persisted workstream record (DOC-44 §4.2).
///
/// Why: the lead agent's unit of tracking — one ticket, lived out in one or
/// more sessions within its `assigned_harness`, per DOC-44 §4.1's "no
/// mid-life migration" rule. `assigned_harness` is documented-immutable;
/// [`super::WorkstreamLedger`]'s mutation methods enforce this at runtime
/// (see that type's docs).
/// What: mirrors DOC-44 §4.2's sketch minus the two Phase-4-only fields
/// (`lead_confidence`, `state: ConfidenceState`) — see module docs for why
/// those are deferred rather than stubbed. `blockers`/`dependencies` are
/// plain `String` ids (a ticket ref or a workstream id) rather than a
/// `BlockerRef` struct: DOC-44 doesn't specify `BlockerRef`'s shape, and
/// inventing one now would be speculative structure with no reader.
/// Test: `types::tests::workstream_serde_round_trip`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workstream {
    /// Ledger-assigned unique id (a UUIDv4 string).
    pub id: String,
    /// External ticket reference (e.g. `"#2109"`).
    pub ticket_ref: String,
    /// Human-readable title/task summary.
    pub title: String,
    /// Tool assignment — set at creation, never mutated afterward.
    pub assigned_harness: Harness,
    /// Backend-native session ids this workstream has spawned. `tm`
    /// workstreams typically hold one; `tcode` workstreams may hold several
    /// concurrent sessions (DOC-44 §4.2).
    pub session_ids: Vec<String>,
    /// Current lifecycle status.
    pub status: WorkstreamStatus,
    /// When the workstream was created.
    pub created_at: DateTime<Utc>,
    /// Optional target delivery date.
    pub target_date: Option<DateTime<Utc>>,
    /// Priority band.
    pub priority: Priority,
    /// Free-text blocker references (ticket ids, human descriptions).
    pub blockers: Vec<String>,
    /// Ids of other workstreams this one depends on.
    pub dependencies: Vec<String>,
    /// When the workstream was last mutated (creation counts as the first
    /// update).
    pub last_updated: DateTime<Utc>,
}

/// Creation input for [`super::WorkstreamLedger::create`].
///
/// Why: separates "what a caller supplies" from "what the ledger assigns"
/// (`id`, `created_at`/`last_updated`, and the initial `status`, which is
/// always [`WorkstreamStatus::Proposed`]) so a caller can never construct a
/// `Workstream` with a forged id or a stale/duplicated timestamp.
/// What: the four fields a caller chooses at creation time; `target_date`
/// defaults to `None` and `blockers`/`dependencies`/`session_ids` always
/// start empty (use the ledger's mutation methods to populate them once the
/// workstream exists).
/// Test: `ledger::tests::create_assigns_id_and_proposed_status`.
#[derive(Debug, Clone)]
pub struct NewWorkstream {
    /// External ticket reference (e.g. `"#3008"`).
    pub ticket_ref: String,
    /// Human-readable title/task summary.
    pub title: String,
    /// Tool assignment — immutable after this call.
    pub assigned_harness: Harness,
    /// Priority band.
    pub priority: Priority,
    /// Optional target delivery date.
    pub target_date: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_from_backend_params_tm_and_tcode() {
        let tm = BackendParams::Tm {
            repo_url: "https://example/repo".into(),
            git_ref: "main".into(),
            runtime: None,
            ephemeral: false,
        };
        let tcode = BackendParams::Tcode {
            project: std::path::PathBuf::from("/tmp/proj"),
        };
        assert_eq!(Harness::from(&tm), Harness::Tm);
        assert_eq!(Harness::from(&tcode), Harness::Tcode);
    }

    #[test]
    fn harness_serde_round_trip() {
        for h in [Harness::Tm, Harness::Tcode] {
            let json = serde_json::to_string(&h).unwrap();
            let back: Harness = serde_json::from_str(&json).unwrap();
            assert_eq!(h, back);
        }
        assert_eq!(serde_json::to_string(&Harness::Tm).unwrap(), "\"tm\"");
        assert_eq!(serde_json::to_string(&Harness::Tcode).unwrap(), "\"tcode\"");
    }

    #[test]
    fn workstream_status_serde_round_trip() {
        let statuses = [
            WorkstreamStatus::Proposed,
            WorkstreamStatus::InProgress,
            WorkstreamStatus::Blocked,
            WorkstreamStatus::Complete,
            WorkstreamStatus::Delivered,
        ];
        for s in statuses {
            let json = serde_json::to_string(&s).unwrap();
            let back: WorkstreamStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
        assert_eq!(
            serde_json::to_string(&WorkstreamStatus::InProgress).unwrap(),
            "\"in_progress\""
        );
    }

    #[test]
    fn priority_orders_p0_highest() {
        assert!(Priority::P0 < Priority::P1);
        assert!(Priority::P1 < Priority::P2);
        assert!(Priority::P2 < Priority::P3);
    }

    #[test]
    fn workstream_serde_round_trip() {
        let now = Utc::now();
        let ws = Workstream {
            id: "ws-1".into(),
            ticket_ref: "#3008".into(),
            title: "heterogeneous ledger".into(),
            assigned_harness: Harness::Tm,
            session_ids: vec!["sess-1".into()],
            status: WorkstreamStatus::InProgress,
            created_at: now,
            target_date: None,
            priority: Priority::P1,
            blockers: vec![],
            dependencies: vec![],
            last_updated: now,
        };
        let json = serde_json::to_string(&ws).unwrap();
        let back: Workstream = serde_json::from_str(&json).unwrap();
        assert_eq!(ws, back);
    }
}
