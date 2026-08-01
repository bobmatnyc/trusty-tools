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

use crate::tools::execution_style::{ExecutionStyle, ResolvedStyle};

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

/// The ONE addressable name for the external coding project manager (#4350).
///
/// Why: the coding route already exists and is already the PM — `run_tcode`
/// spawns `tcode run-task pm …` whenever `route_task` classifies the caller's
/// text as coding — but it is UNADDRESSABLE: a caller cannot say which target
/// it means, so it cannot attach a style to the handoff either. #4350 makes
/// that existing route nameable. It deliberately does NOT join
/// [`NON_CODING_TARGETS`]: that floor is the #4126 prompt-injection protection
/// and epic #4345 decision 2 forbids widening it, so the coding PM is a
/// SEPARATE, single, closed literal resolved on its own path. The name carries
/// no product branding, because `dispatch_task`'s black-box contract (epic
/// #3052 PR B, owner-locked) still holds — `scrub_branding` removes backend
/// identity from every transcript, and a schema that named the backend would
/// re-leak from the front what the result path strips from the back.
/// What: `"coding-pm"`, matched case-insensitively after trimming. It resolves
/// to [`DispatchTarget::CodingPm`], which carries no caller string onward.
/// Test: `coding_pm_target_name_is_pinned`,
/// `coding_pm_is_not_reachable_through_the_non_coding_floor`.
pub const CODING_PM_TARGET: &str = "coding-pm";

/// The resolved destination of one `dispatch_task` call — a CLOSED two-variant
/// type, not a string (#4350).
///
/// Why: this is where "the tcode PM became addressable" is prevented from also
/// meaning "an arbitrary caller string became an agent name". Before #4350 the
/// bridge carried `Option<&str>` all the way into `run_tcode`, where it became
/// the `<AGENT>` argv slot; the ONLY thing keeping a coding agent out of that
/// slot was [`NON_CODING_TARGETS`]. Adding a second admissible target as another
/// string would have made that floor the sole guard for two vocabularies at
/// once. Instead the coding lane gets a variant that carries NO name:
/// [`Self::backend_agent`] returns `None` for it, so the backend falls through
/// to its own hardcoded `DEFAULT_TCODE_AGENT` and the coding leg's argv is
/// byte-identical to an unnamed dispatch. No caller-supplied string can reach
/// the coding leg's agent argument by construction — the same "structural
/// rather than a convention" shape [`ProposalEnvelope::for_cross_product`] uses
/// for `Disposition`.
/// What: `CodingPm` (the single coding delegation surface) and `NonCoding(name)`
/// (a name ALREADY resolved against the [`NON_CODING_TARGETS`] floor by
/// `subagent_allow::SubagentAllowSet`). This type never decides admissibility;
/// it records a decision already made.
/// Test: `coding_pm_carries_no_caller_string_into_the_backend`,
/// `non_coding_target_still_carries_its_resolved_name`,
/// `coding_pm_floor_is_at_least_vibe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchTarget {
    /// The external coding project manager — epic #4345 decision 2's sole
    /// coding delegation surface. Carries no caller-supplied name.
    CodingPm,
    /// A non-coding specialist, already floor-resolved.
    NonCoding(String),
}

impl DispatchTarget {
    /// Recognise the reserved coding-PM name, or `None` if this is not it.
    ///
    /// Why: the recognition MUST happen before the non-coding allow-set is
    /// consulted, because `coding-pm` is deliberately absent from that floor and
    /// would otherwise be denied. Keeping the match here — one closed literal,
    /// trimmed and lowercased exactly like `SubagentAllowSet::resolve` does —
    /// means the two lanes normalise names identically and neither can be
    /// reached by a differently-cased spelling of the other's name.
    /// What: `Some(Self::CodingPm)` iff `requested` equals
    /// [`CODING_PM_TARGET`] after trim + ASCII-lowercase.
    /// Test: `coding_pm_target_name_is_pinned`,
    /// `coding_pm_name_matching_is_case_and_space_insensitive`.
    pub fn for_reserved_name(requested: &str) -> Option<Self> {
        (requested.trim().to_ascii_lowercase() == CODING_PM_TARGET).then_some(Self::CodingPm)
    }

    /// The agent name handed across the process boundary, if any.
    ///
    /// Why: see the type docs — `None` for the coding PM is the structural
    /// guarantee that no caller string becomes the coding leg's `<AGENT>` argv
    /// slot. This method is the single place that property is expressed, so a
    /// future edit that wanted to pass a name would have to change a documented
    /// return value rather than quietly thread a string through.
    /// What: `None` for [`Self::CodingPm`]; the already-resolved name for
    /// [`Self::NonCoding`].
    /// Test: `coding_pm_carries_no_caller_string_into_the_backend`,
    /// `non_coding_target_still_carries_its_resolved_name`.
    pub fn backend_agent(&self) -> Option<&str> {
        match self {
            Self::CodingPm => None,
            Self::NonCoding(name) => Some(name),
        }
    }

    /// The label recorded as the envelope's `target_agent`.
    ///
    /// Why: [`ProposalEnvelope`] must identify what produced a result, and for
    /// the coding lane there is no wire name to report — the reserved literal is
    /// the honest answer, and it is the same token the caller used.
    /// What: [`CODING_PM_TARGET`] or the resolved specialist name.
    /// Test: `styled_coding_delegation_returns_a_proposal_with_the_resolution`.
    pub fn label(&self) -> &str {
        match self {
            Self::CodingPm => CODING_PM_TARGET,
            Self::NonCoding(name) => name,
        }
    }

    /// The ceremony FLOOR this lane enforces on a resolved style.
    ///
    /// Why: DOC-62 §4.1 defines `hack` as the ABSENCE of a code change — "a
    /// delegation that asks for `hack` and then turns out to require a code
    /// change MUST escalate, not proceed unchecked. Without this rule `hack`
    /// becomes the gate-suppression channel SM-2 forbids, wearing a different
    /// name." A delegation explicitly addressed to the coding PM is by
    /// definition asking for a code change, so its floor is `vibe`. A non-coding
    /// specialist produces no code, so it imposes no coding-ceremony floor.
    /// What: `Vibe` for [`Self::CodingPm`], `Hack` (no floor) otherwise. Note
    /// that `vibe` itself then degrades UPWARD to `engineer` today (SM-9), so a
    /// `hack`-styled coding delegation currently runs the full loop and says so.
    /// Test: `coding_pm_floor_is_at_least_vibe`,
    /// `a_hack_request_to_the_coding_pm_does_not_lower_ceremony`.
    pub fn style_floor(&self) -> ExecutionStyle {
        match self {
            Self::CodingPm => ExecutionStyle::Vibe,
            Self::NonCoding(_) => ExecutionStyle::Hack,
        }
    }
}

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
///
/// #4349 added `style` — a TYPED field rather than prose the caller writes into
/// `summary`/`constraints`, per DOC-62 §6.4: only a typed field keeps the wire,
/// the config default and the GUI (#4353) in agreement, and only a typed field
/// makes the ceiling property testable. It is a REQUEST, not a setting: what
/// actually runs is [`crate::tools::execution_style::ResolvedStyle`], resolved
/// at the bridge against the lane's floor.
/// Test: `handoff_over_cap_is_rejected`, `handoff_at_cap_is_accepted`,
/// `handoff_renders_into_the_task_preamble`,
/// `a_styled_handoff_stays_within_the_cap`.
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
    /// The ceremony level the caller REQUESTS for a coding handoff (#4349).
    ///
    /// Why: DOC-41 §5.5 — "no `HandoffContext` field grants elevated authority
    /// to a callee". This one is no exception: it selects ceremony and confers
    /// nothing (DOC-62 SM-6, SM-11). `None` is the pre-#4349 behaviour exactly.
    /// What: a closed [`ExecutionStyle`]; an unrecognized wire value is a
    /// deserialization error the bridge returns to the caller, never a silent
    /// default.
    /// Test: `a_styled_handoff_stays_within_the_cap`,
    /// `an_unknown_style_is_a_caller_error_not_a_silent_default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<ExecutionStyle>,
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
    /// is empty. Deliberately says nothing about `style`: this predicate governs
    /// the CALLER-authored block, and the style-derived policy block is
    /// governed separately by `ResolvedStyle::is_explicit` (DOC-62 §6.4), so a
    /// style-only handoff renders the policy block and no caller block.
    /// Test: `empty_handoff_renders_nothing`,
    /// `a_style_only_handoff_renders_only_the_policy_block`.
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
    /// What: returns `None` when [`Self::is_empty`] AND no explicit style was
    /// resolved; otherwise a labelled block naming only the fields that are
    /// actually set, followed by the TA-PM-policy block when `policy` is
    /// explicit.
    ///
    /// #4349: `policy` is the RESOLVED style (never `self.style`, which is only
    /// the request). Two ordering rules from DOC-62 §6.4 are load-bearing and
    /// are why the policy block is appended here rather than merged into the
    /// caller's lines: it comes AFTER the caller's `Constraint` lines, and it
    /// carries its own heading, so a caller-supplied constraint can never be
    /// mistaken for the policy block. Note SM-8's limit on what that separation
    /// buys — a preamble is advisory text read by a model, so this ordering
    /// makes the policy legible, NOT tamper-proof; every security property it
    /// mentions is enforced in code elsewhere.
    /// Test: `handoff_renders_into_the_task_preamble`,
    /// `empty_handoff_renders_nothing`,
    /// `policy_block_follows_caller_supplied_constraints`,
    /// `a_style_only_handoff_renders_only_the_policy_block`.
    pub fn render_preamble(&self, policy: Option<&ResolvedStyle>) -> Option<String> {
        let policy = policy.filter(|p| p.is_explicit());
        if self.is_empty() {
            return policy.map(ResolvedStyle::render_policy_block);
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
        if let Some(policy) = policy {
            out.push('\n');
            out.push_str(&policy.render_policy_block());
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
    /// The style that was actually applied and how it was arrived at (#4350).
    ///
    /// Why: DOC-62 §3.4 makes the resolution path part of the RESULT, not just
    /// of the outbound preamble — "so a caller can see when its request was
    /// overridden or degraded". A caller that asked for `vibe` and silently
    /// received `engineer` would otherwise have no way to tell. Absent (and
    /// omitted from the JSON) when no style was supplied at any level, keeping
    /// the envelope byte-identical for every pre-#4350 caller.
    /// What: the resolved record, never the raw request.
    /// Test: `styled_coding_delegation_returns_a_proposal_with_the_resolution`,
    /// `an_unstyled_envelope_is_byte_identical`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<ResolvedStyle>,
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
            style: None,
            result: result.into(),
        }
    }

    /// Record the resolved style on an already-built envelope (#4350).
    ///
    /// Why: a BUILDER rather than a fifth constructor parameter, deliberately.
    /// [`Self::for_cross_product`] stays the sole constructor and keeps
    /// hardcoding `Disposition::Proposal`, so the propose-only invariant is
    /// untouched by this change — a style can be attached to an envelope but
    /// there is still no code path through this type that yields
    /// `Disposition::Action`, with or without one (DOC-62 AC-10, SM-6).
    /// What: sets [`Self::style`]; `None` leaves the envelope byte-identical to
    /// a pre-#4350 one.
    /// Test: `styled_coding_delegation_returns_a_proposal_with_the_resolution`,
    /// `a_styled_envelope_is_still_only_a_proposal`.
    pub fn with_style(mut self, style: Option<ResolvedStyle>) -> Self {
        self.style = style;
        self
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
