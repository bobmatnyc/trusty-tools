//! `dg.event.v1`: one observation of a delegation at one boundary (design §3).
//!
//! The record types live here; construction and validation are in
//! `capture.rs`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::hash::Sha256Hex;
use crate::policy::PolicySnapshot;
use crate::project::ProjectId;

/// Schema version carried in every event's `schema` field.
pub const EVENT_SCHEMA: &str = "dg.event.v1";

/// The moment an event observes (§1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Boundary {
    /// `PreToolUse` Agent/Task: the parent proposes a child brief.
    Contract,
    /// `PostToolUse` Agent/Task: a child exists.
    Launch,
}

impl Boundary {
    /// The wire word, as hashed into the dedupe key.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contract => "contract",
            Self::Launch => "launch",
        }
    }
}

/// Dispatch path from the bypass inventory (§2). v1 emits only the two
/// native tool names; other paths are uncovered and have no variant yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchPath {
    /// Native `Agent` tool (B1, B2, B4).
    NativeAgent,
    /// Legacy `Task` tool name (B3).
    NativeTask,
}

/// Who proposed the dispatch. The session id is stored hashed only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Actor {
    /// `sha256(session_id)`.
    pub session_hash: Sha256Hex,
    /// The proposing agent's id; `None` for a top-level PM.
    pub agent_id: Option<String>,
    /// The proposing agent's type.
    pub agent_type: Option<String>,
    /// Ancestor agent ids, nearest last.
    pub parent_chain: Vec<String>,
}

/// The proposed child task. The full prompt is never stored, only hashed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposed {
    /// The dispatch's short description, redacted.
    pub description: String,
    /// Requested agent type.
    pub subagent_type: Option<String>,
    /// Requested model override.
    pub model: Option<String>,
    /// Whether the child runs in the background.
    pub run_in_background: bool,
    /// Redacted prompt text, at most 8 KiB.
    pub prompt_excerpt: String,
    /// sha256 of the full, unredacted prompt.
    pub prompt_sha256: Sha256Hex,
    /// Byte length of the full prompt.
    pub prompt_bytes: u64,
    /// True when the excerpt was cut at the cap.
    pub truncated: bool,
    /// True when any redaction detector matched the prompt or description.
    pub redacted: bool,
}

/// How a rule reached the child (§3 `inherits`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InheritVia {
    /// The ancestor CLAUDE.md chain.
    ClaudeMdChain,
    /// The agent definition.
    AgentDef,
    /// Text in the brief itself.
    BriefText,
}

/// One inherited rule claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inherit {
    /// A rule id present in the event's `policy`.
    pub rule_id: String,
    /// The inheritance channel.
    pub via: InheritVia,
    /// Whether the channel was verified to carry the rule.
    pub verified: bool,
    /// sha256 of the evidence that verified it.
    pub evidence_sha256: Sha256Hex,
}

/// Worker census at launch (launch boundary only). Counts are stored;
/// per-agent rows are reduced to `tree_sha256`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerState {
    /// Age of the registry snapshot.
    pub snapshot_age_ms: u64,
    /// Registry epoch; changes on daemon restart.
    pub registry_epoch: u64,
    /// False when the registry lost tree history (for example, a restart).
    pub complete: bool,
    /// Running children.
    pub running: u32,
    /// Queued children.
    pub queued: u32,
    /// Background children.
    pub background: u32,
    /// Depth of the new child in the tree.
    pub depth: u32,
    /// Canonical sha256 of the project's descendant tree.
    pub tree_sha256: Sha256Hex,
}

/// Digests of the evidence the verdict depends on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceHashes {
    /// Equals `proposed.prompt_sha256`.
    pub prompt: Sha256Hex,
    /// Equals `policy.policy_rev`.
    pub policy_rev: Sha256Hex,
    /// Equals `worker_state.tree_sha256`; `None` at the contract boundary.
    pub tree: Option<Sha256Hex>,
    /// Canonical sha256 of `worker_state`; `None` at the contract boundary.
    pub state: Option<Sha256Hex>,
}

/// One `dg.event.v1` record.
///
/// Why: the tap, the outbox, the evaluator request and the findings row all
/// carry the same record, so its privacy rules live in the type: no raw
/// `session_id` or `tool_use_id` field exists, and the prompt appears only as
/// a redacted, capped excerpt plus a digest.
/// What: every §3 field; unknown keys are rejected on parse. Build one with
/// [`DgEvent::capture`], parse one with [`DgEvent::from_json`].
/// Test: `event_round_trips_through_json`, `stored_event_carries_no_raw_ids`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DgEvent {
    /// Always [`EVENT_SCHEMA`].
    pub schema: String,
    /// UUID v7 minted at capture (#10892: stands in for the design's ULID).
    pub event_id: Uuid,
    /// `sha256(session_id | tool_use_id)`.
    pub action_id: Sha256Hex,
    /// Contract or launch.
    pub boundary: Boundary,
    /// Dispatch path.
    pub dispatch_path: DispatchPath,
    /// Capturing host.
    pub host_id: String,
    /// Session runtime (for example `claude`).
    pub runtime: String,
    /// tm version on the capturing host.
    pub tm_version: String,
    /// Capture time, Unix epoch milliseconds.
    pub captured_at: u64,
    /// The project the dispatch belongs to.
    pub project_id: ProjectId,
    /// Owner named by the policy source.
    pub owner: String,
    /// Proposing actor.
    pub actor: Actor,
    /// Proposed child task.
    pub proposed: Proposed,
    /// The project's policy at capture.
    pub policy: PolicySnapshot,
    /// Inherited-rule claims.
    pub inherits: Vec<Inherit>,
    /// Worker census; present exactly at the launch boundary.
    pub worker_state: Option<WorkerState>,
    /// Evidence digests.
    pub evidence_hashes: EvidenceHashes,
    /// `sha256(boundary | action_id | policy_rev | prompt_sha256 | tree_sha256)`.
    pub dedupe_key: Sha256Hex,
}
