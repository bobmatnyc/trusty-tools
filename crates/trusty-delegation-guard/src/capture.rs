//! Building and validating a [`DgEvent`].

use uuid::Uuid;

use crate::error::{DgError, Result};
use crate::event::{
    Actor, Boundary, DgEvent, DispatchPath, EVENT_SCHEMA, EvidenceHashes, Inherit, Proposed,
    WorkerState,
};
use crate::hash::{Sha256Hex, action_id, canonical_sha256, dedupe_key, session_hash};
use crate::policy::PolicyBundle;
use crate::project::ProjectId;
use crate::redact::{EXCERPT_MAX_BYTES, excerpt, redact};

/// The raw dispatch identifiers. They are hashed by [`DgEvent::capture`] and
/// never copied into the event.
#[derive(Debug, Clone, Copy)]
pub struct RawIds<'a> {
    /// Claude Code session id.
    pub session_id: &'a str,
    /// The dispatching tool call's id.
    pub tool_use_id: &'a str,
}

/// Everything the tap observed for one dispatch.
#[derive(Debug, Clone)]
pub struct CaptureInput<'a> {
    /// Raw ids; hashed, never stored.
    pub ids: RawIds<'a>,
    /// Contract or launch.
    pub boundary: Boundary,
    /// Dispatch path.
    pub dispatch_path: DispatchPath,
    /// Capturing host.
    pub host_id: &'a str,
    /// Session runtime.
    pub runtime: &'a str,
    /// tm version.
    pub tm_version: &'a str,
    /// Capture time, Unix epoch milliseconds.
    pub captured_at: u64,
    /// Owner named by the policy source.
    pub owner: &'a str,
    /// Proposing agent id; `None` for a top-level PM.
    pub agent_id: Option<&'a str>,
    /// Proposing agent type.
    pub agent_type: Option<&'a str>,
    /// Ancestor agent ids.
    pub parent_chain: Vec<String>,
    /// Dispatch description.
    pub description: &'a str,
    /// Requested agent type.
    pub subagent_type: Option<&'a str>,
    /// Requested model override.
    pub model: Option<&'a str>,
    /// Background flag.
    pub run_in_background: bool,
    /// The full prompt; hashed, then redacted and capped into the excerpt.
    pub prompt: &'a str,
    /// Inherited-rule claims.
    pub inherits: Vec<Inherit>,
    /// Worker census; required at launch, refused at contract.
    pub worker_state: Option<WorkerState>,
}

impl DgEvent {
    /// Builds a validated event for `project_id` from `bundle` and `input`.
    ///
    /// Why: one constructor owns the privacy and isolation rules, so no caller
    /// can store a raw id, an unredacted excerpt, or another project's rules.
    /// What: refuses a bundle keyed to another project; hashes the raw ids and
    /// the full prompt; redacts the description and prompt (any hit sets
    /// `redacted`); caps the excerpt; derives evidence hashes and the dedupe
    /// key; then runs [`DgEvent::validate`].
    /// Test: `stored_event_carries_no_raw_ids`, `event_for_a_with_b_bundle_is_refused`,
    /// `redaction_hit_sets_redacted_flag`.
    pub fn capture(
        project_id: &ProjectId,
        bundle: &PolicyBundle,
        input: CaptureInput<'_>,
    ) -> Result<Self> {
        project_id.require_same(&bundle.project_id)?;
        let action = action_id(input.ids.session_id, input.ids.tool_use_id);
        let prompt_sha256 = Sha256Hex::digest(input.prompt.as_bytes());
        let ex = excerpt(input.prompt);
        let description = redact(input.description);
        let tree = input.worker_state.as_ref().map(|w| w.tree_sha256.clone());
        let state = input
            .worker_state
            .as_ref()
            .map(canonical_sha256)
            .transpose()?;
        let key = dedupe_key(
            input.boundary.as_str(),
            &action,
            &bundle.policy_rev,
            &prompt_sha256,
            tree.as_ref(),
        );
        let event = Self {
            schema: EVENT_SCHEMA.to_owned(),
            event_id: Uuid::now_v7(),
            action_id: action,
            boundary: input.boundary,
            dispatch_path: input.dispatch_path,
            host_id: input.host_id.to_owned(),
            runtime: input.runtime.to_owned(),
            tm_version: input.tm_version.to_owned(),
            captured_at: input.captured_at,
            project_id: project_id.clone(),
            owner: input.owner.to_owned(),
            actor: Actor {
                session_hash: session_hash(input.ids.session_id),
                agent_id: input.agent_id.map(str::to_owned),
                agent_type: input.agent_type.map(str::to_owned),
                parent_chain: input.parent_chain,
            },
            proposed: Proposed {
                description: description.text,
                subagent_type: input.subagent_type.map(str::to_owned),
                model: input.model.map(str::to_owned),
                run_in_background: input.run_in_background,
                prompt_excerpt: ex.text,
                prompt_sha256: prompt_sha256.clone(),
                prompt_bytes: input.prompt.len() as u64,
                truncated: ex.truncated,
                redacted: ex.redacted || description.redacted,
            },
            policy: bundle.snapshot(),
            inherits: input.inherits,
            worker_state: input.worker_state,
            evidence_hashes: EvidenceHashes {
                prompt: prompt_sha256,
                policy_rev: bundle.policy_rev.clone(),
                tree,
                state,
            },
            dedupe_key: key,
        };
        event.validate()?;
        Ok(event)
    }

    /// Parses an event strictly: unknown keys and invariant breaks are errors.
    ///
    /// Test: `unknown_top_level_key_is_rejected`, `unknown_nested_key_is_rejected`,
    /// `event_round_trips_through_json`.
    pub fn from_json(json: &str) -> Result<Self> {
        let event: Self = serde_json::from_str(json)?;
        event.validate()?;
        Ok(event)
    }

    /// Serializes the event as JSON.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// Recomputes every derived field and checks every §3 limit.
    ///
    /// Test: `tampered_dedupe_key_is_rejected`, `worker_state_must_match_boundary`,
    /// `oversized_excerpt_is_rejected`.
    pub fn validate(&self) -> Result<()> {
        if self.schema != EVENT_SCHEMA {
            return Err(DgError::SchemaMismatch {
                expected: EVENT_SCHEMA,
                found: self.schema.clone(),
            });
        }
        if self.event_id.get_version_num() != 7 {
            return Err(DgError::Inconsistent("event_id is not a UUID v7"));
        }
        for (name, value) in [
            ("host_id", &self.host_id),
            ("runtime", &self.runtime),
            ("tm_version", &self.tm_version),
            ("owner", &self.owner),
        ] {
            if value.is_empty() {
                return Err(DgError::EmptyField(name));
            }
        }
        if self.proposed.prompt_excerpt.len() > EXCERPT_MAX_BYTES {
            return Err(DgError::LimitExceeded("prompt_excerpt over 8 KiB"));
        }
        self.policy.validate()?;
        if self
            .inherits
            .iter()
            .any(|i| !self.policy.has_rule(&i.rule_id))
        {
            return Err(DgError::Inconsistent("inherits names a rule not in policy"));
        }
        self.validate_derived()
    }

    fn validate_derived(&self) -> Result<()> {
        let tree = match (self.boundary, &self.worker_state) {
            (Boundary::Launch, Some(w)) => Some(&w.tree_sha256),
            (Boundary::Contract, None) => None,
            _ => {
                return Err(DgError::Inconsistent(
                    "worker_state must be present exactly at launch",
                ));
            }
        };
        let state = self
            .worker_state
            .as_ref()
            .map(canonical_sha256)
            .transpose()?;
        let ev = &self.evidence_hashes;
        if ev.prompt != self.proposed.prompt_sha256 {
            return Err(DgError::DerivedMismatch("evidence_hashes.prompt"));
        }
        if ev.policy_rev != self.policy.policy_rev {
            return Err(DgError::DerivedMismatch("evidence_hashes.policy_rev"));
        }
        if ev.tree.as_ref() != tree || ev.state != state {
            return Err(DgError::DerivedMismatch("evidence_hashes.tree/state"));
        }
        let expected = dedupe_key(
            self.boundary.as_str(),
            &self.action_id,
            &self.policy.policy_rev,
            &self.proposed.prompt_sha256,
            tree,
        );
        if expected != self.dedupe_key {
            return Err(DgError::DerivedMismatch("dedupe_key"));
        }
        Ok(())
    }
}
