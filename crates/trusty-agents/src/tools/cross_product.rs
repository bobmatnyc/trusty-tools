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
//! specialist names; [`SubagentAllowSet`] intersects it with the calling
//! agent's own `[subagents].allowed` config list
//! (`crate::agents::config::SubagentsConfig`) and resolves a requested name
//! or returns a [`TargetDenied`] reason. [`HandoffContext`] is the minimal
//! #2809-SHAPED outbound payload (`summary`/`relevant_state`/`constraints`,
//! 4 KiB serialized cap) — a local copy of that shape per the owner's OQ-6
//! ruling, with NO dependency on epic #2809 landing. [`ProposalEnvelope`] is
//! the inbound wrapper: origin agent, target agent, the CALLER's authority
//! tier, and a [`Disposition`] marker that this bridge always sets to
//! `Proposal`.
//! Test: `cross_product_tests` — empty-default pin, allow-set fail-closed
//! rejection, non-coding floor beating a permissive config, envelope shape
//! and always-`Proposal` disposition, and the 4 KiB handoff cap.

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

/// Why a requested cross-product target was refused.
///
/// Why: the bridge must fail CLOSED with a clear, caller-actionable reason and
/// without dispatching anything — but it must not enumerate the roster back to
/// a black-boxed persona (the #3052 PR A CRITICAL-2 rule `delegate_to_agent`
/// already follows). A small reason enum keeps the caller-facing string in one
/// place instead of scattered format strings.
/// What: three variants covering the three ways resolution fails.
/// Test: `blank_target_is_rejected`, `empty_default_allow_set_denies_everything`,
/// `non_coding_floor_rejects_a_coding_target_even_when_config_allows_it`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetDenied {
    /// The requested name was blank/whitespace-only.
    Blank,
    /// The name is not in [`NON_CODING_TARGETS`] — the bridge's hard floor.
    NotNonCoding(String),
    /// The name passed the floor but the caller's `[subagents].allowed` list
    /// does not grant it (this includes the EMPTY default — see
    /// [`SubagentAllowSet::empty`]).
    NotGranted(String),
}

impl std::fmt::Display for TargetDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetDenied::Blank => write!(f, "specialist name must not be empty"),
            // Both denials render the SAME generic text on purpose: the
            // difference between "not a permitted kind of specialist" and "not
            // granted to you" would let a caller probe the floor list.
            TargetDenied::NotNonCoding(name) | TargetDenied::NotGranted(name) => {
                write!(f, "specialist '{name}' is not available to this agent")
            }
        }
    }
}

/// The fail-closed set of cross-product specialist names one calling agent may
/// target through the bridge (#4026).
///
/// Why: see the module docs. Constructed once at tool-registration time from
/// the calling agent's config so `execute()` never consults anything the LLM
/// can influence mid-turn.
/// What: `granted` holds the caller's configured names, already trimmed and
/// lowercased. [`SubagentAllowSet::resolve`] requires membership in BOTH
/// `granted` and [`NON_CODING_TARGETS`]. The default ([`Self::empty`]) grants
/// nothing — an absent `[subagents]` section is NOT a silent capability grant.
/// Test: `empty_default_allow_set_denies_everything`,
/// `allow_set_accepts_a_named_non_coding_target`.
#[derive(Debug, Clone, Default)]
pub struct SubagentAllowSet {
    granted: Vec<String>,
}

impl SubagentAllowSet {
    /// The empty allow-set — the DEFAULT, granting nothing.
    ///
    /// Why: an absent `[subagents]` section must not widen capability. This is
    /// the pinned posture for the whole feature: named cross-product targeting
    /// is off until an agent's config explicitly turns it on, mirroring
    /// `[tools].allow`/`[skills].allow`'s deny-by-default stance.
    /// What: an allow-set with no granted names; every `resolve` returns
    /// [`TargetDenied::NotGranted`].
    /// Test: `empty_default_allow_set_denies_everything`,
    /// `default_impl_is_the_empty_allow_set`.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build from a caller's configured `[subagents].allowed` list.
    ///
    /// Why: OQ-3's ruling makes the config list the source of truth for now,
    /// with #4030's runtime-built domain authority feeding the same set later
    /// — so this constructor is the single seam that will gain that second
    /// source without touching the bridge or the tool.
    /// What: `None` (absent section) yields [`Self::empty`]. Names are
    /// trimmed, lowercased, and de-duplicated; blank entries are dropped. No
    /// glob dialect — exact names only, matching `[skills].allow`'s
    /// literal-ids-only invariant.
    /// Test: `from_allowed_none_is_empty`, `from_allowed_normalizes_entries`.
    pub fn from_allowed(allowed: Option<&[String]>) -> Self {
        let Some(list) = allowed else {
            return Self::empty();
        };
        let mut granted: Vec<String> = Vec::new();
        for raw in list {
            let name = raw.trim().to_ascii_lowercase();
            if name.is_empty() || granted.contains(&name) {
                continue;
            }
            granted.push(name);
        }
        Self { granted }
    }

    /// Whether this allow-set grants nothing (the default posture).
    ///
    /// Why: the tool layer uses this to decide whether to advertise the
    /// specialist parameter at all — an agent with no grants should not be
    /// invited to guess names.
    /// What: true iff no name is granted.
    /// Test: `empty_default_allow_set_denies_everything`.
    pub fn is_empty(&self) -> bool {
        self.granted.is_empty()
    }

    /// Resolve a caller-requested specialist name, fail-closed.
    ///
    /// Why: THE enforcement point (OQ-7). Nothing is dispatched unless this
    /// returns `Ok`; both the non-coding floor and the caller's own grant list
    /// must admit the name.
    /// What: trims/lowercases `requested`, then requires membership in
    /// [`NON_CODING_TARGETS`] first (the bridge's own floor, checked BEFORE
    /// config so a permissive config can never widen it) and in `granted`
    /// second. Returns the normalized name on success.
    /// Test: `allow_set_accepts_a_named_non_coding_target`,
    /// `non_coding_floor_rejects_a_coding_target_even_when_config_allows_it`,
    /// `blank_target_is_rejected`.
    pub fn resolve(&self, requested: &str) -> Result<String, TargetDenied> {
        let name = requested.trim().to_ascii_lowercase();
        if name.is_empty() {
            return Err(TargetDenied::Blank);
        }
        if !NON_CODING_TARGETS.contains(&name.as_str()) {
            return Err(TargetDenied::NotNonCoding(name));
        }
        if !self.granted.contains(&name) {
            return Err(TargetDenied::NotGranted(name));
        }
        Ok(name)
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
