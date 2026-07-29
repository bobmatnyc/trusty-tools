//! Cross-product subagent primitives for the `dispatch_task` bridge (epic
//! #4021, issues #4026/#4028): the bridge-layer allow-set that decides WHICH
//! external non-coding specialist may be targeted, and the propose-only
//! envelope every such specialist's result is wrapped in.
//!
//! Why: `dispatch_task` (`crate::tools::pm_bridge`) historically hardcoded a
//! single opaque target. Widening it so a caller can name a specialist
//! (#4026) opens two questions that must be answered HERE, at the bridge,
//! rather than at the caller: (1) which names are reachable at all — the
//! owner's OQ-7 ruling is bridge-layer, fail-closed enforcement, never
//! caller-trusted alone; and (2) what authority the result carries — DOC-41
//! §5.5's "propose-not-authorize, absolute for M1 — no exceptions"
//! (`docs/specs/trusty-agents-eve-style-agents-spec.md:1398`) plus #3078's
//! AUTH-5 regression pattern on `delegate_to_agent`. An external specialist
//! runs in a DIFFERENT PRODUCT that has no `user_authority` concept at all,
//! so by construction its output can only ever be a proposal.
//! What: [`NON_CODING_TARGETS`] is the hard, bridge-owned floor of reachable
//! specialist names; [`crate::tools::subagent_allow::SubagentAllowSet`]
//! intersects it with the calling agent's own `[subagents].allowed` config
//! list (`crate::agents::config::SubagentsConfig`) and resolves a requested
//! name or returns a [`crate::tools::subagent_allow::TargetDenied`] reason.
//! That allow-set USED to live in this module; ADR-0024 decision 4 gave the
//! in-process `delegate_to_agent` path the same floor-narrowing shape over a
//! DIFFERENT name vocabulary, so the gate moved to `tools::subagent_allow` and
//! became floor-parameterized rather than being copied. This module keeps the
//! bridge's own floor and its wire types. [`HandoffContext`] is the minimal
//! #2809-SHAPED outbound payload (`summary`/`relevant_state`/`constraints`,
//! 4 KiB serialized cap) — a local copy of that shape per the owner's OQ-6
//! ruling, with NO dependency on epic #2809 landing. [`ProposalEnvelope`] is
//! the inbound wrapper: origin agent, target agent, the CALLER's authority
//! tier, and a [`Disposition`] marker that this bridge always sets to
//! `Proposal`.
//! Test: `cross_product_tests` — envelope shape and always-`Proposal`
//! disposition, and the 4 KiB handoff cap. The allow-set's own coverage
//! (empty-default pin, fail-closed rejection, floor-beats-config) moved with
//! it to `subagent_allow_tests`.

use serde::{Deserialize, Serialize};

/// Maximum serialized size of a [`HandoffContext`], in bytes (#2809 §5.2's
/// 4 KiB cap, mirrored locally per OQ-6).
///
/// Why: an unbounded caller-supplied handoff would let the calling LLM smuggle
/// its whole conversation across a process boundary, defeating the
/// clean-context guarantee #2809 specifies and inflating every dispatch. The
/// cap is checked BEFORE the target is invoked so an oversized payload is a
/// recoverable caller error, never a half-executed dispatch.
/// What: 4096 bytes, measured over `serde_json::to_vec`.
/// Test: `handoff_over_cap_is_rejected`, `handoff_at_cap_is_accepted`.
pub const HANDOFF_MAX_BYTES: usize = 4096;

/// The bridge-owned floor of externally reachable NON-CODING specialist names
/// (#4026, epic #4021 OQ-7).
///
/// Why: the owner's OQ-7 ruling is that the bridge itself hard-denies anything
/// outside the non-coding set "regardless of caller configuration" — a
/// misconfigured or LLM-influenced `[subagents].allowed` must never be able to
/// reach a coding agent (which would hand a sandboxed assistant an
/// unrestricted write/shell surface in another product, the same escalation
/// class `delegate_to_agent`'s `allowed_target_roles` gate closed for the
/// in-product path). Two layers, not one: this floor AND the caller's config.
/// What: the two specialists epic #4021's owner directive names explicitly —
/// `research` (already in trusty-code's roster) and `ticketing` (ported by
/// #4027). Deliberately a closed literal list, not a heuristic: #4030's
/// runtime-built domain authority is what will eventually FEED this set, and
/// until it exists an exact list is the only honest source.
/// Test: `non_coding_floor_rejects_a_coding_target_even_when_config_allows_it`,
/// `allow_set_accepts_a_named_non_coding_target`.
pub const NON_CODING_TARGETS: &[&str] = &["research", "ticketing"];

/// Minimal, #2809-SHAPED outbound handoff payload (#4028, OQ-6).
///
/// Why: the owner's OQ-6 ruling is "minimal HandoffContext-shaped envelope
/// now, no dependency on #2809". This is that local copy: the SAME three
/// fields and the SAME 4 KiB cap epic #2809 specifies, so when #2809's struct
/// lands the two reconcile by renaming rather than by re-designing the wire
/// shape. It is deliberately NOT re-exported as #2809's type — this module
/// owns the cross-product copy and nothing else depends on it.
/// What: `summary` (what the caller already knows), `relevant_state`
/// (caller-selected facts), `constraints` (limits the specialist must honour).
/// All optional; an omitted handoff is byte-identical to pre-#4026 behaviour.
/// [`HandoffContext::validate`] enforces [`HANDOFF_MAX_BYTES`].
/// Test: `handoff_over_cap_is_rejected`, `handoff_at_cap_is_accepted`,
/// `handoff_renders_into_the_task_preamble`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HandoffContext {
    /// Short statement of the situation the specialist is being handed.
    #[serde(default)]
    pub summary: Option<String>,
    /// Caller-selected facts the specialist needs; free-form key/value text.
    #[serde(default)]
    pub relevant_state: Option<String>,
    /// Limits the specialist must honour (scope, tone, forbidden actions).
    #[serde(default)]
    pub constraints: Vec<String>,
}

impl HandoffContext {
    /// Enforce the 4 KiB serialized cap BEFORE any dispatch happens.
    ///
    /// Why: #4028's acceptance is explicit — an oversized payload "returns a
    /// recoverable error without invoking the target". Validating here, ahead
    /// of the backend call, is what makes "without invoking" true.
    /// What: `Err(actual_size)` when `serde_json::to_vec` exceeds
    /// [`HANDOFF_MAX_BYTES`]; `Ok(())` otherwise. A serialization failure is
    /// treated as over-cap (fail-closed) rather than silently passing.
    /// Test: `handoff_over_cap_is_rejected`, `handoff_at_cap_is_accepted`.
    pub fn validate(&self) -> Result<(), usize> {
        match serde_json::to_vec(self) {
            Ok(bytes) if bytes.len() <= HANDOFF_MAX_BYTES => Ok(()),
            Ok(bytes) => Err(bytes.len()),
            Err(_) => Err(usize::MAX),
        }
    }

    /// Whether every field is unset — i.e. the caller supplied no handoff.
    ///
    /// Why: an all-empty handoff must render NOTHING into the task text so the
    /// no-handoff path stays byte-identical to pre-#4026 dispatch.
    /// What: true iff both optional fields are `None`/blank and `constraints`
    /// is empty.
    /// Test: `empty_handoff_renders_nothing`.
    pub fn is_empty(&self) -> bool {
        self.summary.as_deref().unwrap_or("").trim().is_empty()
            && self
                .relevant_state
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            && self.constraints.is_empty()
    }

    /// Render the handoff as a plain-text preamble prepended to the task.
    ///
    /// Why: the external CLI leg accepts one task string and nothing else, so
    /// a structured handoff must be flattened to cross the process boundary.
    /// Plain text (not JSON) because the receiving side is an LLM persona, not
    /// a parser.
    /// What: returns `None` when [`Self::is_empty`]; otherwise a labelled
    /// block naming only the fields that are actually set.
    /// Test: `handoff_renders_into_the_task_preamble`,
    /// `empty_handoff_renders_nothing`.
    pub fn render_preamble(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut out = String::from("Context handed to you:\n");
        if let Some(s) = self
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.push_str(&format!("- Summary: {s}\n"));
        }
        if let Some(s) = self
            .relevant_state
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.push_str(&format!("- Relevant state: {s}\n"));
        }
        for c in self
            .constraints
            .iter()
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
        {
            out.push_str(&format!("- Constraint: {c}\n"));
        }
        Some(out)
    }
}

/// The authority tier of the agent that CALLED the bridge (#4028).
///
/// Why: DOC-41 §5.5's `user_authority` is a manifest-level singleton that has
/// no field on `AgentConfig` yet (reserved for #3074/AUTH-1, see
/// `docs/specs/agent-config-five-sections.md:649`). Recording the caller's
/// tier in the envelope — rather than inventing a new tier — lets the
/// authority-holder recognise that IT, in its own turn under its own identity,
/// is the party that may act on a returned proposal. No new tier is defined
/// here: this enum names exactly the two states §5.5 already describes.
/// What: `Standard` (the default, fail-closed) and `UserAuthority`.
/// Test: `envelope_records_caller_authority_without_upgrading_disposition`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallerAuthority {
    /// No `user_authority` — every agent except the singleton holder.
    #[default]
    Standard,
    /// The DOC-41 §5.5 singleton authority-holder.
    UserAuthority,
}

/// Whether an envelope's payload is a PROPOSAL or an executed ACTION (#4028).
///
/// Why: DOC-41 §5.5 line 1398 — "propose-not-authorize, absolute for M1 — no
/// exceptions". A cross-product specialist runs in a different product with no
/// `user_authority` concept at all, so its output is a proposal
/// UNCONDITIONALLY — including when the caller itself holds `user_authority`
/// (that caller may then act, in its own turn, on its own identity; the
/// specialist's output still never IS the action). `Action` exists so the
/// marker is a genuine two-state discriminator rather than a constant, and is
/// documented as never produced by this bridge.
/// What: `Proposal` is the only value [`ProposalEnvelope::for_cross_product`]
/// can emit.
/// Test: `cross_product_result_is_always_a_proposal`,
/// `envelope_records_caller_authority_without_upgrading_disposition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Drafted content for the caller to review — never self-authorizing.
    Proposal,
    /// An action already taken under the actor's own authority. NEVER emitted
    /// by the cross-product bridge; present so `Proposal` is a real choice.
    Action,
}

/// The propose-only wrapper around a cross-product specialist's result
/// (#4028).
///
/// Why: see [`Disposition`]. Without an explicit marker on the wire, a
/// specialist's transcript reaching the caller's context is indistinguishable
/// from the caller's own authorized output — exactly the confusion #3078's
/// AUTH-5 tests exist to prevent on the in-product path.
/// What: the minimal HandoffContext-SHAPED envelope epic #4021 OQ-6 settled
/// on: `origin_agent`, `target_agent`, `authority` (the CALLER's tier),
/// `disposition` (always `Proposal`), and the specialist's already-scrubbed
/// `result` text.
/// Test: `cross_product_result_is_always_a_proposal`, `envelope_json_shape`.
#[derive(Debug, Clone, Serialize)]
pub struct ProposalEnvelope {
    /// The trusty-agents agent that initiated the dispatch.
    pub origin_agent: String,
    /// The external non-coding specialist that produced `result`.
    pub target_agent: String,
    /// The ORIGIN agent's authority tier — never the target's (the target has
    /// none; it is a different product).
    pub authority: CallerAuthority,
    /// Always [`Disposition::Proposal`] for a cross-product result.
    pub disposition: Disposition,
    /// The specialist's output, already branding-scrubbed by the tool layer.
    pub result: String,
}

impl ProposalEnvelope {
    /// Wrap a cross-product result, unconditionally as a proposal.
    ///
    /// Why: making this the ONLY constructor is what makes DOC-41 §5.5's
    /// absolute rule structural rather than a convention a future edit could
    /// forget — there is no code path through this type that yields
    /// [`Disposition::Action`], regardless of `authority`.
    /// What: builds the envelope with `disposition: Proposal` always.
    /// Test: `cross_product_result_is_always_a_proposal`,
    /// `envelope_records_caller_authority_without_upgrading_disposition`.
    pub fn for_cross_product(
        origin_agent: impl Into<String>,
        target_agent: impl Into<String>,
        authority: CallerAuthority,
        result: impl Into<String>,
    ) -> Self {
        Self {
            origin_agent: origin_agent.into(),
            target_agent: target_agent.into(),
            authority,
            // Never `Action` — see the type docs and DOC-41 §5.5 line 1398.
            disposition: Disposition::Proposal,
            result: result.into(),
        }
    }

    /// Render as the tool-result string handed back to the calling LLM.
    ///
    /// Why: the caller is a model, not a parser; a pretty JSON document with a
    /// leading plain-language line states the propose-only status in a form
    /// the model reliably honours while keeping the fields machine-readable.
    /// What: one advisory line plus the pretty-printed envelope. Falls back to
    /// the bare result text if serialization somehow fails, so a returned
    /// transcript is never lost.
    /// Test: `envelope_json_shape`.
    pub fn render(&self) -> String {
        match serde_json::to_string_pretty(self) {
            Ok(json) => format!(
                "The specialist's output below is a PROPOSAL for you to review \
                 and act on yourself; it does not authorize anything on its own.\n\
                 {json}"
            ),
            Err(_) => self.result.clone(),
        }
    }
}

#[cfg(test)]
#[path = "cross_product_tests.rs"]
mod cross_product_tests;
