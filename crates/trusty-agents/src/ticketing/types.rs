//! Core value types for the ticketing abstraction.
//!
//! Why: Provider-neutral types (GitHub Issues, JIRA, Linear) let us write
//! one `TicketingClient` trait with unified semantics. Mirrors the
//! adapter pattern used by `mcp-ticketer`'s Python ABC.
//! What: `Ticket` is the canonical ticket shape; `TicketStatus` /
//! `Priority` are enumerated; request types (`CreateTicketReq`,
//! `UpdateTicketReq`, `TicketFilter`) are used as adapter inputs.
//! Test: Serde round-trip for `Ticket` and `TicketStatus` in
//! `src/ticketing/mod.rs` tests.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Canonical ticket status across providers.
///
/// Why: GitHub has `open`/`closed`, JIRA uses transitions, Linear has many
/// workflow states — mapping them all to a small enum keeps callers simple.
/// What: Four canonical values; adapters translate their provider-specific
/// state to the closest match.
/// Test: `TicketStatus` deserializes `"in_progress"` to `InProgress`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TicketStatus {
    Open,
    InProgress,
    /// Ticket is awaiting review (PR open, QA pending, etc.).
    InReview,
    Done,
    Closed,
    /// Ticket is blocked by an external dependency.
    Blocked,
    /// Ticket was cancelled / abandoned without completing.
    Cancelled,
    /// Provider-native status that doesn't map cleanly to our canonical
    /// values (e.g. Linear/Jira workflow states). Adapters that have
    /// custom workflows can preserve the original name here.
    Custom(String),
}

/// Canonical priority levels.
///
/// Why: Each provider has its own priority scale (e.g. Linear uses 0–4,
/// JIRA uses P0/P1 etc). Normalizing to four levels keeps the trait simple.
/// What: Four ordered values; adapters map to/from provider-native.
/// Test: `Priority` serialization uses snake_case.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Medium,
    High,
    Critical,
}

/// A unified ticket returned by any adapter.
///
/// Why: Callers (and the LLM tool surface) should not have to pick between
/// GitHub's `issue.number` + `issue.body` + `issue.html_url` and JIRA's
/// `key` + `fields.description` + `self`. One shape, adapter-filled.
/// What: `id` is the provider-native identifier (GitHub issue number as
/// string, JIRA key, Linear UUID). Timestamps and url are optional because
/// not every adapter surfaces them.
/// Test: Serde round-trip test covers all fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ticket {
    pub id: String,
    pub title: String,
    pub body: String,
    pub status: TicketStatus,
    pub priority: Option<Priority>,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub url: Option<String>,
}

/// Create-ticket request.
///
/// Why: Providers accept a superset of fields at creation time but only a
/// handful are universal (title/body/labels/priority/assignee). Using a
/// struct with `Default` lets adapters ignore fields they can't set without
/// needing a builder trait per provider.
/// What: All fields optional except `title` and `body` (empty string allowed).
/// Test: `CreateTicketReq::default()` compiles; used in adapter stubs.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct CreateTicketReq {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub priority: Option<Priority>,
    pub assignee: Option<String>,
}

/// Update-ticket request; all fields optional (None = don't change).
///
/// Why: Partial updates are the common case — most calls touch one field.
/// What: Every field is `Option<_>`; adapters apply only the `Some` ones.
/// Test: `UpdateTicketReq::default()` yields a no-op request.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct UpdateTicketReq {
    pub title: Option<String>,
    pub body: Option<String>,
    pub status: Option<TicketStatus>,
    /// Replace the entire label set with these labels.
    pub labels: Option<Vec<String>>,
    /// Add these labels to the existing set (independent of `labels`).
    pub add_labels: Option<Vec<String>>,
    /// Remove these labels from the existing set (independent of `labels`).
    pub remove_labels: Option<Vec<String>>,
    pub assignee: Option<String>,
    /// Set/clear the milestone (provider-native id or name).
    pub milestone: Option<String>,
    pub priority: Option<Priority>,
}

/// A canonical tag/label, possibly with provider-supplied metadata.
///
/// Why: `list_available_tags` is the surface used by the LLM to discover
/// what labels exist before tagging — color/description help the model
/// pick the right one rather than inventing.
/// What: `name` is required; `color` (hex) and `description` are optional.
/// Test: Construction is trivial; covered indirectly by adapter tests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Tag {
    pub name: String,
    pub color: Option<String>,
    pub description: Option<String>,
}

/// Capability flags for a `TicketingClient` adapter.
///
/// Why: Different providers support different operations natively
/// (e.g. GitHub has labels but no JIRA-style transitions; Linear has rich
/// workflow states). The agent / UI can introspect capabilities to decide
/// which tools to expose or how to phrase prompts.
/// What: Boolean flags; default is everything `false`. Adapters that
/// support a feature override `capabilities()` and set the flag.
/// Test: `capabilities_returns_correct_flags_for_github`,
/// `capabilities_returns_defaults_for_base_trait`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TicketingCapabilities {
    pub tagging: bool,
    pub transitions: bool,
    pub ownership: bool,
    pub search: bool,
    pub milestones: bool,
}

/// Filter for `list_tickets`.
///
/// Why: Each provider paginates and filters differently — this normalizes
/// the common knobs (status, labels, assignee, limit).
/// What: All fields optional; `limit` caps returned length.
/// Test: `TicketFilter::default()` returns everything.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct TicketFilter {
    pub status: Option<TicketStatus>,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub limit: Option<usize>,
}
