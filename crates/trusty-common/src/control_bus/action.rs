//! The `ActionEvent` taxonomy carried by `HarnessPayload::Action` (DOC-73 §3.2).
//!
//! Why: The dashboard's list and tree views (DOC-73 §5) need one typed vehicle
//!      for "something happened" that names its object by reference rather
//!      than by value, so an event stays small no matter how large the thing
//!      it describes (§3.2 "Required fields"). `HarnessPayload::Hook`'s
//!      untyped `{kind, data}` shape cannot serve this: the tree assembler
//!      needs to pattern-match on a closed, typed set of phases to know what
//!      each node means, and an open-ended `Value` gives it nothing to match
//!      on.
//! What: Six `ActionEvent` variants — `Workflow`, `Agent`, `File`, `Tool`,
//!       `Session`, `Inference` — each carrying the fields every kind needs
//!       (via the flattened [`ActionMeta`]) plus its own phase and payload.
//!       `Actor`, `ObjectRef`, `ObjectType`, and `PathRef` are the supporting
//!       types §3.2 and §6 name. Every new enum here is `#[non_exhaustive]`:
//!       the taxonomy is expected to grow phases and object types as more
//!       harnesses adapt (§3.4), and §3.3 requires that growth be additive
//!       rather than a breaking rename.
//! Test: `super::tests::action_event_round_trips_all_six_kinds`,
//!       `super::tests::action_event_wire_shape_matches_kind_tag`,
//!       `super::tests::action_event_kind_matches_serde_tag`,
//!       `super::tests::action_meta_schema_version_defaults_to_one`,
//!       `super::tests::harness_payload_action_round_trips`,
//!       `super::tests::harness_payload_pre_action_payload_still_deserializes`.

// #6847: added alongside `control_bus::PushClient` to close out the two items
// DOC-73 §3.2/§4 still owed after #7150 landed the envelope's `id`/`parent_id`.
// Fields and phases are transcribed from the spec's code sample and its
// "Required fields on every ActionEvent" table; nothing here is speculative
// beyond what those two sections already specify.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::event_id::EventId;
use super::lifecycle::HarnessSource;

/// Fields every `ActionEvent` variant carries, regardless of `kind`.
///
/// Why: §3.2's "Required fields on every ActionEvent" table lists seven fields
///      the view needs on every kind. Four of them (`id`, `at`, `source`,
///      `parent_id`) already exist on the enclosing [`super::HarnessEvent`]
///      envelope; carrying them here too is what let an object viewer, a log
///      exporter, or a future non-envelope transport read one `ActionEvent`
///      value and answer "who, when, from where, caused by what" without
///      needing the outer envelope in hand. Collecting them in one struct and
///      flattening it into each variant is what keeps six variants from
///      repeating seven field declarations apiece.
/// What: `id`/`at`/`source`/`session`/`parent_id` mirror the envelope fields
///       of the same name; `actor` and `objects` are §3.2's own additions.
///       `schema_version` is §3.3's versioning field, defaulting to `1` so a
///       payload serialized before this field existed still deserializes.
/// Test: `super::tests::action_meta_schema_version_defaults_to_one`, plus the
///       round-trip tests, which cover every field through a real variant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionMeta {
    /// Same identity the envelope carries; duplicated here so `ActionEvent`
    /// is self-describing outside the envelope (DOC-73 §3.2).
    pub id: EventId,
    /// Emit-time UTC timestamp.
    pub at: DateTime<Utc>,
    /// Which harness produced this action.
    pub source: HarnessSource,
    /// Optional task/session correlation key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The call-graph edge: the event that caused this one. `None` is a root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<EventId>,
    /// Who performed the action: an agent, the operator, or the system.
    pub actor: Actor,
    /// What the action acted on, each entry a link target for the object
    /// viewer (DOC-73 §6). Never inlines object content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<ObjectRef>,
    /// The `ActionEvent` schema version (DOC-73 §3.3). Starts at `1`;
    /// defaults to `1` on deserialize so this field's own addition is
    /// itself additive.
    #[serde(default = "ActionMeta::default_schema_version")]
    pub schema_version: u16,
}

impl ActionMeta {
    fn default_schema_version() -> u16 {
        1
    }
}

/// Who performed an action.
///
/// Why: §3.2 — "an agent name plus its stable `agent_id`, or `Operator`, or
///      `System`". The tree view colors and labels a node by actor, so the
///      three cases need to stay structurally distinct rather than collapsing
///      into a display string at emit time.
/// What: `Agent` carries both the display name and the stable id the object
///       viewer links through; `Operator` and `System` are unit variants.
/// Test: `super::tests::action_event_round_trips_all_six_kinds` covers `Agent`
///       through a real event; `Operator`/`System` via
///       `super::tests::actor_operator_and_system_round_trip`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Actor {
    /// A named agent instance.
    Agent {
        /// Display name (e.g. `"rust-engineer"`).
        name: String,
        /// Stable identifier the object viewer links through.
        agent_id: String,
    },
    /// A human operator drove this action directly.
    Operator,
    /// The harness itself performed this action, with no agent or operator
    /// in the loop.
    System,
}

/// The nine object types DOC-73 §6's object viewer routes on.
///
/// Why: `ObjectRef.type` is "a discriminated type tag" (§3.2); modelling it as
///      a closed enum rather than a bare `String` gives the tree/list views a
///      type the compiler checks, while still matching the exact nine
///      `/ui/object/<type>/<id>` routes §6 enumerates.
/// What: One variant per §6 table row. `#[non_exhaustive]` because §6 already
///       reserves the shape for more object types than it lists as MVP.
/// Test: `super::tests::object_type_round_trips_every_variant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ObjectType {
    Session,
    Agent,
    Task,
    Workstream,
    File,
    ToolCall,
    Inference,
    Issue,
    Pr,
}

/// A link target: "what the action acted on" (§3.2), rendered by the object
/// viewer (§6).
///
/// Why: Events stay small no matter how large the object they reference — the
///      view renders `label` and links on `(type, id)` rather than inlining
///      object content (§3.2, §6 "Reached by link only").
/// What: `object_type` is the discriminated tag; `id` is opaque to the view
///       (whatever the owning harness uses to look the object up); `label` is
///       the short display string.
/// Test: `super::tests::action_event_round_trips_all_six_kinds`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObjectRef {
    #[serde(rename = "type")]
    pub object_type: ObjectType,
    pub id: String,
    /// Security: rendered verbatim by the object viewer (§6) — a producer
    /// MUST pass any user- or file-derived text through
    /// `crate::credentials::scrub_secrets` before setting this field.
    pub label: String,
}

/// A `File` action's target: a repo-relative path and, for a write, a diff
/// reference.
///
/// Why: §6 — a `File::Written` row shows "the unified diff"; the object
///      viewer needs a reference to that diff, not the diff itself, for the
///      same reason `ObjectRef` never inlines content.
/// What: `path` is repo-relative; `diff_ref` is opaque to the view and absent
///       for every phase but `Written`.
///
/// Security: both fields are rendered verbatim by the object viewer (§6). A
/// producer MUST pass any user- or file-derived free text through
/// `crate::credentials::scrub_secrets` before setting either one.
/// Test: `super::tests::action_event_round_trips_all_six_kinds`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PathRef {
    pub path: String,
    /// Security: see the struct-level doc — scrub before setting, same as
    /// `path`. This is the field `ActionEvent::File`'s `Written` phase
    /// populates with a reference into the diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_ref: Option<String>,
}

/// Phases of `ActionEvent::Workflow` (DOC-73 §3.2 table, verbatim).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WorkflowPhase {
    Start,
    Stop,
    Spawn,
    Read,
    Write,
}

/// Phases of `ActionEvent::Agent` — maps 1:1 onto `LifecycleEvent`'s
/// `AgentSpawned`/`AgentMessage`/`AgentDone`/`AgentFailed` (DOC-73 §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentPhase {
    Spawned,
    Message,
    Done,
    Failed,
}

/// Phases of `ActionEvent::File` (DOC-73 §3.2 table, verbatim).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FilePhase {
    Read,
    Written,
    Created,
    Deleted,
    Moved,
}

/// Phases shared by `ActionEvent::Tool` and `ActionEvent::Inference` — both
/// rows in DOC-73 §3.2's table list the identical `Started`/`Finished`/
/// `Errored` set, so one enum serves both kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CallPhase {
    Started,
    Finished,
    Errored,
}

/// Phases of `ActionEvent::Session` (DOC-73 §3.2 table, verbatim).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionPhase {
    Started,
    StatusChanged,
    Input,
    Done,
    Cancelled,
}

/// The typed action taxonomy carried by `HarnessPayload::Action` (DOC-73
/// §3.2).
///
/// Why: See module docs. Each variant "names its object by reference rather
///      than by value" — `object`/`agent_id`/`path`/`tool`+`call_id`/`model`
///      are identifiers and short labels, never the object's content.
/// What: `#[serde(tag = "kind", rename_all = "snake_case")]` produces
///       `{"kind":"workflow", ...flattened ActionMeta fields..., "phase":...,
///       "object":...}` and the equivalent shape for the other five kinds.
///       [`ActionMeta`] is flattened into every variant rather than
///       duplicated field-by-field.
/// Test: `super::tests::action_event_round_trips_all_six_kinds`,
///       `super::tests::action_event_wire_shape_matches_kind_tag`,
///       `super::tests::action_event_kind_matches_serde_tag`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ActionEvent {
    /// A workflow's own state transitions — `Spawn` opens a tree node,
    /// `Start`/`Stop` bracket a unit of work, `Read`/`Write` are the
    /// workflow's own transitions (not file I/O).
    Workflow {
        #[serde(flatten)]
        meta: ActionMeta,
        phase: WorkflowPhase,
        object: ObjectRef,
    },
    /// An agent's life, from spawn to done or failed.
    Agent {
        #[serde(flatten)]
        meta: ActionMeta,
        phase: AgentPhase,
        agent_id: String,
    },
    /// A filesystem effect an agent or the harness caused.
    File {
        #[serde(flatten)]
        meta: ActionMeta,
        phase: FilePhase,
        path: PathRef,
    },
    /// One tool invocation, correlated by `call_id`.
    Tool {
        #[serde(flatten)]
        meta: ActionMeta,
        phase: CallPhase,
        tool: String,
        call_id: String,
    },
    /// Session lifecycle — the tree's own root event.
    Session {
        #[serde(flatten)]
        meta: ActionMeta,
        phase: SessionPhase,
    },
    /// One model call.
    Inference {
        #[serde(flatten)]
        meta: ActionMeta,
        phase: CallPhase,
        model: String,
    },
}

impl ActionEvent {
    /// The `kind` string for this action (`"workflow"`, `"agent"`, ...).
    ///
    /// Why: Mirrors [`super::HarnessPayload::domain`] — a consumer that wants
    ///      to route or filter by kind without a full match gets the same
    ///      string serde uses for the `kind` tag.
    /// What: Returns the same string serde uses for the `kind` tag.
    /// Test: `super::tests::action_event_kind_matches_serde_tag`.
    pub fn kind(&self) -> &'static str {
        match self {
            ActionEvent::Workflow { .. } => "workflow",
            ActionEvent::Agent { .. } => "agent",
            ActionEvent::File { .. } => "file",
            ActionEvent::Tool { .. } => "tool",
            ActionEvent::Session { .. } => "session",
            ActionEvent::Inference { .. } => "inference",
        }
    }

    /// The [`ActionMeta`] common to every variant.
    ///
    /// Why: §3.2's required fields (`id`, `actor`, `objects`, ...) are what a
    ///      filter or a list-view row needs regardless of `kind`; this is the
    ///      one accessor rather than six repeated match arms at every call
    ///      site.
    /// What: Destructures whichever variant `self` is and returns its `meta`.
    /// Test: `super::tests::action_event_round_trips_all_six_kinds` reads
    ///       `meta()` back on every kind.
    pub fn meta(&self) -> &ActionMeta {
        match self {
            ActionEvent::Workflow { meta, .. }
            | ActionEvent::Agent { meta, .. }
            | ActionEvent::File { meta, .. }
            | ActionEvent::Tool { meta, .. }
            | ActionEvent::Session { meta, .. }
            | ActionEvent::Inference { meta, .. } => meta,
        }
    }
}
