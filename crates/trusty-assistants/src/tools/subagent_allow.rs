//! The floor-narrowing sub-agent allow-set — ONE gate implementation shared by
//! both delegation mechanisms (#4026 for the cross-product bridge; ADR-0024
//! decision 4 for the in-process `delegate_to_agent` whitelist).
//!
//! Why: two mechanisms now answer the same question — "may THIS caller reach
//! THAT named specialist?" — with the same fail-closed shape: a server-owned
//! floor of names the product will ever admit, intersected with an editable
//! per-agent config list, where config may only ever NARROW the floor and an
//! absent list grants nothing. `SubagentAllowSet` was written for the bridge
//! (#4026, `tools::cross_product`) and its `resolve` hardcoded that one floor.
//! ADR-0024 decision 4 needs the identical machinery over a DIFFERENT
//! vocabulary (in-process agent NAMES, not trusty-code specialist names), and
//! the crate's "no second copy of any gate" principle (see
//! `api::server::agent_subagents`'s module doc) forbids a parallel
//! implementation: a second copy is how the reporting surface and the
//! enforcement point drift apart. So the type moved here and became
//! floor-parameterized; the two floors stay separate constants
//! (`tools::cross_product::NON_CODING_TARGETS`,
//! `agents::delegation::ASSISTANT_REACHABLE_SUBAGENTS`) because their target
//! vocabularies genuinely do not overlap.
//! What: [`TargetDenied`] (why a name was refused) and [`SubagentAllowSet`]
//! (the intersection, with the floor named explicitly at every construction
//! site — there is deliberately no `Default`, so no caller can acquire a floor
//! it did not choose).
//! Test: `subagent_allow_tests` — floor-beats-config, absent-config-denies,
//! normalization, and the two floors' independence.

/// Why a requested sub-agent target was refused.
///
/// Why: both enforcement points must fail CLOSED with a clear, caller-actionable
/// reason and without dispatching anything — but must not enumerate the roster
/// back to a black-boxed persona (the #3052 PR A CRITICAL-2 rule
/// `delegate_to_agent` already follows). A small reason enum keeps the
/// caller-facing string in one place instead of scattered format strings.
/// What: three variants covering the three ways resolution fails. `NotOnFloor`
/// was `NotNonCoding` before ADR-0024 decision 4 generalized the type; the
/// rename is why the variant no longer names one mechanism's floor.
/// Test: `blank_target_is_rejected`, `empty_default_allow_set_denies_everything`,
/// `non_coding_floor_rejects_a_coding_target_even_when_config_allows_it`,
/// `delegate_floor_rejects_an_engineer_even_when_config_allows_it`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetDenied {
    /// The requested name was blank/whitespace-only.
    Blank,
    /// The name is not on this allow-set's floor — the server-owned ceiling
    /// config can never widen.
    NotOnFloor(String),
    /// The name passed the floor but the caller's own config list does not
    /// grant it (this includes the EMPTY default — see
    /// [`SubagentAllowSet::empty_over`]).
    NotGranted(String),
}

impl std::fmt::Display for TargetDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetDenied::Blank => write!(f, "specialist name must not be empty"),
            // Both denials render the SAME generic text on purpose: the
            // difference between "not a permitted kind of specialist" and "not
            // granted to you" would let a caller probe the floor list.
            TargetDenied::NotOnFloor(name) | TargetDenied::NotGranted(name) => {
                write!(f, "specialist '{name}' is not available to this agent")
            }
        }
    }
}

/// The fail-closed set of sub-agent names one calling agent may target, over an
/// explicitly-named server-owned floor.
///
/// Why: see the module docs. Constructed once at tool-registration time from the
/// calling agent's config so `execute()` never consults anything the LLM can
/// influence mid-turn. The floor is a constructor argument rather than a
/// hardcoded constant so the SAME gate serves both mechanisms without either
/// inheriting the other's vocabulary by accident.
/// What: `floor` is the server-owned ceiling; `granted` holds the caller's
/// configured names, already trimmed and lowercased. [`SubagentAllowSet::resolve`]
/// requires membership in BOTH, checking the floor FIRST so a permissive config
/// can never widen it. There is deliberately no `Default` impl — every
/// construction names its floor.
/// Test: `empty_default_allow_set_denies_everything`,
/// `allow_set_accepts_a_named_non_coding_target`,
/// `delegate_allow_set_accepts_a_seeded_floor_target`.
#[derive(Debug, Clone)]
pub struct SubagentAllowSet {
    floor: &'static [&'static str],
    granted: Vec<String>,
}

impl SubagentAllowSet {
    /// The empty allow-set over `floor` — the DEFAULT posture, granting
    /// nothing.
    ///
    /// Why: an absent config section must not widen capability. This is the
    /// pinned posture for BOTH mechanisms: named targeting is off until an
    /// agent's config explicitly turns it on, mirroring
    /// `[tools].allow`/`[skills].allow`'s deny-by-default stance. ADR-0024
    /// decision 4 ratified the same answer for the in-process whitelist
    /// (fail-closed when absent), paired with a SEEDED default in the bundled
    /// assistant personas so nothing silently drops to zero on rollout.
    /// What: an allow-set with no granted names; every `resolve` returns
    /// [`TargetDenied::NotGranted`] (or [`TargetDenied::NotOnFloor`] first).
    /// Test: `empty_default_allow_set_denies_everything`,
    /// `empty_over_grants_nothing_on_either_floor`.
    pub fn empty_over(floor: &'static [&'static str]) -> Self {
        Self {
            floor,
            granted: Vec::new(),
        }
    }

    /// Build from a caller's configured allow list, over `floor`.
    ///
    /// Why: OQ-3's ruling (cross-product) and ADR-0024 decision 4 (in-process)
    /// both make the config list the source of truth — "an editable
    /// configuration whitelist over agents that already exist in the roster,
    /// not a hand-authored Rust constant". This constructor is the single seam
    /// both read through, and the one #4030's runtime-built domain authority
    /// will eventually feed.
    /// What: `None` (absent section) yields [`Self::empty_over`]. Names are
    /// trimmed, lowercased, and de-duplicated; blank entries are dropped. No
    /// glob dialect — exact names only, matching `[skills].allow`'s
    /// literal-ids-only invariant.
    /// Test: `from_allowed_none_is_empty`, `from_allowed_normalizes_entries`.
    pub fn over(floor: &'static [&'static str], allowed: Option<&[String]>) -> Self {
        let Some(list) = allowed else {
            return Self::empty_over(floor);
        };
        let mut granted: Vec<String> = Vec::new();
        for raw in list {
            let name = raw.trim().to_ascii_lowercase();
            if name.is_empty() || granted.contains(&name) {
                continue;
            }
            granted.push(name);
        }
        Self { floor, granted }
    }

    /// Whether this allow-set grants nothing (the fail-closed default posture).
    ///
    /// Why: the tool layer uses this to decide whether to advertise the
    /// specialist parameter at all — an agent with no grants should not be
    /// invited to guess names.
    /// What: true iff no name is granted. Says nothing about the floor.
    /// Test: `empty_default_allow_set_denies_everything`.
    pub fn is_empty(&self) -> bool {
        self.granted.is_empty()
    }

    /// The server-owned floor this allow-set narrows.
    ///
    /// Why: the config panes report the floor verbatim so an operator can see
    /// the ceiling a config edit is bounded by, without a second hardcoded copy
    /// of the constant on the read path.
    /// What: the `floor` slice handed to the constructor.
    /// Test: `floor_is_reported_verbatim`.
    pub fn floor(&self) -> &'static [&'static str] {
        self.floor
    }

    /// Resolve a caller-requested target name, fail-closed.
    ///
    /// Why: THE enforcement point for both mechanisms. Nothing is dispatched
    /// unless this returns `Ok`; both the server-owned floor and the caller's
    /// own grant list must admit the name.
    /// What: trims/lowercases `requested`, then requires membership in
    /// [`Self::floor`] FIRST (so a permissive config can never widen it) and in
    /// `granted` second. Returns the normalized name on success.
    /// Test: `allow_set_accepts_a_named_non_coding_target`,
    /// `non_coding_floor_rejects_a_coding_target_even_when_config_allows_it`,
    /// `delegate_floor_rejects_an_engineer_even_when_config_allows_it`,
    /// `blank_target_is_rejected`.
    pub fn resolve(&self, requested: &str) -> Result<String, TargetDenied> {
        let name = requested.trim().to_ascii_lowercase();
        if name.is_empty() {
            return Err(TargetDenied::Blank);
        }
        if !self.floor.contains(&name.as_str()) {
            return Err(TargetDenied::NotOnFloor(name));
        }
        if !self.granted.contains(&name) {
            return Err(TargetDenied::NotGranted(name));
        }
        Ok(name)
    }
}

/// Narrow a caller-supplied list to a floor, reporting what was refused —
/// the WRITE-path counterpart to [`SubagentAllowSet::resolve`] (ADR-0024
/// decision 4, sub-answer (b)).
///
/// Why: the owner ratified that "the write path MUST enforce a SERVER-SIDE
/// FLOOR — a GUI write must not be able to widen an assistant's reachable set
/// past the floor", explicitly rejecting the `PATCH /api/agents/:name`
/// `tools_allow` precedent, which "inserts the caller-supplied array verbatim…
/// no check that the new patterns are a subset of anything". `resolve` answers
/// the read/dispatch question one name at a time; a write endpoint needs the
/// whole-list answer BEFORE it touches the file, so the rejection is reported
/// rather than silently applied. Living here — beside the gate it mirrors —
/// is what keeps the write ceiling and the dispatch ceiling the same constant.
/// What: returns `Ok(normalized)` (trimmed, lowercased, de-duplicated, order
/// preserved) when every entry is on `floor`; otherwise `Err(offenders)` with
/// the normalized offending names in input order. A blank entry is an offender
/// (reported as the empty string) rather than being silently dropped — a write
/// is an explicit act and a caller should learn its list was malformed.
/// Test: `narrow_to_floor_accepts_a_subset`, `narrow_to_floor_rejects_a_widening`,
/// `narrow_to_floor_normalizes_and_dedups`, `narrow_to_floor_rejects_blank`.
pub fn narrow_to_floor(
    floor: &'static [&'static str],
    requested: &[String],
) -> Result<Vec<String>, Vec<String>> {
    let mut accepted: Vec<String> = Vec::new();
    let mut offenders: Vec<String> = Vec::new();
    for raw in requested {
        let name = raw.trim().to_ascii_lowercase();
        if !floor.contains(&name.as_str()) {
            if !offenders.contains(&name) {
                offenders.push(name);
            }
            continue;
        }
        if !accepted.contains(&name) {
            accepted.push(name);
        }
    }
    if offenders.is_empty() {
        Ok(accepted)
    } else {
        Err(offenders)
    }
}

#[cfg(test)]
#[path = "subagent_allow_tests.rs"]
mod subagent_allow_tests;
