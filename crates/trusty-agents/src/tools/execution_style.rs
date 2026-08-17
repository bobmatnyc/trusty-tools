//! `ExecutionStyle` — the ceremony level a caller may attach to a coding
//! delegation, and the one-way resolution that turns a caller REQUEST into an
//! effective style (issues #4349/#4350; spec DOC-62 §5, in review as PR #4529).
//!
//! Why: a caller-supplied style is an **author tag**, and this repository's own
//! captured research
//! (`docs/research/quality-gates-agent-prs-article-2026-04.md:122`) rules that
//! risk classification "must be automatic … not from author tags, since an
//! agent has no self-awareness of how risky its own change is". The spec
//! reconciles that with the owner's decision 3 (callers MAY name a style) by
//! making the value a **ceiling request the callee may raise but never lower**.
//! That asymmetry is the whole safety story of the feature, so it is enforced
//! by CONSTRUCTION here rather than by discipline at the call sites — the same
//! shape `ProposalEnvelope::for_cross_product`
//! (`crate::tools::cross_product`) uses to make DOC-41 §5.5's propose-only rule
//! "structural rather than a convention a future edit could forget".
//! What: [`ExecutionStyle`] is the closed three-variant vocabulary, declared and
//! `Ord`-ered by ascending ceremony so `max` IS "raise, never lower".
//! [`ResolvedStyle`] is the resolution result; its ONLY constructor is
//! [`ResolvedStyle::resolve`], its fields are private, and it exposes no
//! mutator — there is no code path through this module that yields an effective
//! style below the floor it was resolved against. [`StyleSource`] records which
//! precedence level supplied the value and [`StyleEscalation`] why the effective
//! value differs from the request, so a caller is never silently overruled.
//! Style is ceremony ONLY: nothing here reads or writes a tool list, a
//! permission, or a command line (DOC-62 SM-11).
//! Test: `execution_style_tests` — the ceiling property over the full
//! request×floor cross-product, the `vibe` → `engineer` fail-safe, precedence,
//! and the bounded, task-independent policy block.

use serde::{Deserialize, Serialize};

/// The closed vocabulary of ceremony levels (DOC-62 §5.1, epic #4345 decision
/// 1).
///
/// Why: a closed enum rather than a string is how decision 1's "no new axis is
/// invented" becomes mechanical — an open field is a place for a fourth,
/// undocumented tier to appear without a decision. The three names are the
/// already-ratified Execution Patterns tiers
/// (`docs/trusty-code/vision-and-architecture-spec.md` §5.10, §10 D3), not a
/// new taxonomy. An unrecognized wire value is a `serde` error the bridge
/// returns to the caller, never a silent fallback to a default: silently
/// mapping an unknown style onto a default is indistinguishable, from the
/// caller's side, from the style having been honoured.
/// What: `Hack` (QUICK OPS — no code change at all), `Vibe` (VIBE — a small
/// change, reduced ceremony), `Engineer` (FULL LOOP — today's behaviour).
/// **Variant order is load-bearing**: the derived `Ord` is ascending ceremony,
/// so `a.max(b)` is "the higher ceremony of the two" and every escalation in
/// this module is a `max`. Reordering these variants silently inverts the
/// safety property; `ceremony_order_is_ascending` fails if anyone does.
/// Test: `ceremony_order_is_ascending`, `styles_round_trip_lowercase`,
/// `an_unknown_style_string_is_a_deserialization_error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionStyle {
    /// QUICK OPS — trivial, answerable in a couple of reads. Per DOC-62 §4.1
    /// this is the ABSENCE of a code change, not a code change with the checks
    /// removed.
    Hack,
    /// VIBE — a small, self-contained change with reduced pre-PR ceremony.
    /// Unimplemented today (#2596); see [`ExecutionStyle::implemented`].
    Vibe,
    /// FULL LOOP — spec → issue → worktree → branch → PR → review → merge → CI.
    /// The built-in default and the fail-safe direction.
    Engineer,
}

impl ExecutionStyle {
    /// Every variant, in ascending ceremony order (#4353).
    ///
    /// Why: the GUI style selector (#4353) has to draw one control per style
    /// and ask the resolver what each would actually do. Spelling the three
    /// names again at that call site would put the closed vocabulary decision 1
    /// fixes into a second place, which is precisely what the enum exists to
    /// prevent — so the one list lives here and every consumer iterates it.
    /// What: `[Hack, Vibe, Engineer]`, in the same ascending-ceremony order the
    /// derived `Ord` gives. **A new variant MUST be added here too**; the
    /// ordering half of that obligation is pinned mechanically below, and the
    /// membership half by [`ExecutionStyle::as_str`]'s exhaustive match, which
    /// stops compiling until a new variant is named.
    /// Test: `all_is_sorted_by_ascending_ceremony`,
    /// `all_round_trips_through_the_wire_form`.
    pub const ALL: [Self; 3] = [Self::Hack, Self::Vibe, Self::Engineer];

    /// The built-in default when neither the caller nor config supplies one
    /// (DOC-62 §5.2).
    ///
    /// Why: `Engineer` is the most ceremony and matches today's behaviour
    /// exactly, so an absent style is byte-equivalent in behaviour to
    /// pre-#4349 dispatch. Defaulting in the other direction would silently
    /// reduce ceremony for every existing caller.
    /// What: [`ExecutionStyle::Engineer`].
    /// Test: `built_in_default_is_engineer`, `precedence_falls_through_to_built_in`.
    pub const BUILT_IN_DEFAULT: Self = Self::Engineer;

    /// The style whose pipeline actually EXISTS today, and why it differs
    /// (DOC-62 §7.3, SM-9).
    ///
    /// Why: VIBE is unimplemented (#2596 open; vision spec §5.10 marks it "Not
    /// implemented"). Silently accepting `vibe` and running less would ship the
    /// reduced tier without the ratification vision spec §10 D3 requires, so
    /// the fail-safe degrades UPWARD — more ceremony — and says so. When #2596
    /// lands, this one function changes and nothing else does: no interface
    /// change, no re-plumbing.
    /// What: `(Engineer, Some(TierUnimplemented))` for `Vibe`; the style itself
    /// with `None` otherwise. Never returns a style below its input — the
    /// ceiling property holds through this step too.
    /// Test: `vibe_degrades_upward_to_engineer_and_says_why`,
    /// `implemented_never_lowers_ceremony`.
    pub fn implemented(self) -> (Self, Option<StyleEscalation>) {
        match self {
            Self::Vibe => (Self::Engineer, Some(StyleEscalation::TierUnimplemented)),
            other => (other, None),
        }
    }

    /// The lowercase wire/display name.
    ///
    /// Why: the policy preamble is plain text read by an LLM persona, so the
    /// rendered name must come from one place rather than being re-spelled at
    /// each format site (where it could drift from the serde representation).
    /// What: `"hack"` / `"vibe"` / `"engineer"`, matching `rename_all`.
    /// Test: `styles_round_trip_lowercase`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hack => "hack",
            Self::Vibe => "vibe",
            Self::Engineer => "engineer",
        }
    }
}

/// Which precedence level supplied the style (DOC-62 §5.3).
///
/// Why: §3.4's reporting contract entitles a caller to see WHICH level its
/// effective style came from, so an override that was never read is visible
/// rather than mysterious.
/// What: `Caller` (per-delegation parameter) > `Config` (per-agent default) >
/// `BuiltIn` ([`ExecutionStyle::BUILT_IN_DEFAULT`]), first-match-wins.
/// Test: `precedence_is_caller_then_config_then_built_in`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StyleSource {
    /// Supplied on the delegation call itself.
    Caller,
    /// Supplied by the calling agent's configuration default.
    Config,
    /// Neither of the above — [`ExecutionStyle::BUILT_IN_DEFAULT`].
    BuiltIn,
}

/// Why an effective style is HIGHER than the one that was requested.
///
/// Why: DOC-62 §3.4 requires "an explicit statement when the effective style
/// differs from the requested style, with the reason". Without it, a caller
/// cannot distinguish "you got what you asked for" from "we quietly ran
/// something else", which is the failure mode SM-4 exists to prevent.
/// What: the two reasons this module can produce. Both are raises; there is no
/// variant for a reduction because no code path produces one.
/// Test: `vibe_degrades_upward_to_engineer_and_says_why`,
/// `a_floor_above_the_request_is_reported_as_an_escalation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StyleEscalation {
    /// The callee's own lane enforces a higher floor than the request
    /// (DOC-62 §5.4).
    CalleeFloor,
    /// The requested tier has no implementation yet, so the next higher one
    /// ran (DOC-62 §7.3, SM-9; #2596).
    TierUnimplemented,
}

impl StyleEscalation {
    /// Human-readable tag used in the policy preamble and in reports.
    ///
    /// Why: same single-source-of-truth reason as [`ExecutionStyle::as_str`];
    /// SM-9 names the wire reason `tier-unimplemented` verbatim, so the string
    /// must not be re-spelled at render sites.
    /// What: the kebab-case serde name.
    /// Test: `escalation_tags_match_the_spec_wire_names`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CalleeFloor => "callee-floor",
            Self::TierUnimplemented => "tier-unimplemented",
        }
    }
}

/// A resolved style: what was asked for, what will actually run, and why they
/// differ (DOC-62 §5.3, §5.4, §3.4).
///
/// Why: **the ceiling property is structural here, not conventional.** Fields
/// are private, there is no `Default`, no public constructor other than
/// [`ResolvedStyle::resolve`], and no mutator; `resolve` computes `effective`
/// only through `max` and [`ExecutionStyle::implemented`], both of which are
/// monotonically non-decreasing in ceremony. There is therefore no code path
/// through this type that yields an effective style below the floor it was
/// resolved against, regardless of what the caller requested — exactly the
/// guarantee `ProposalEnvelope::for_cross_product` gives `Disposition`.
/// Note what this type deliberately does NOT have: any accessor a caller could
/// use to reach a tool list, a permission, or a command line. Style is a
/// ceremony record and nothing else (DOC-62 SM-11).
/// What: `requested` (the value that entered resolution, `None` when nothing
/// was supplied at any level), `source`, `effective`, and `escalations` (empty
/// when the request was honoured as-is).
/// Test: `a_caller_can_never_lower_ceremony_below_the_callee_floor`,
/// `resolution_is_reported_end_to_end`, `precedence_is_caller_then_config_then_built_in`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedStyle {
    /// The style that entered resolution, or `None` when no level supplied one.
    requested: Option<ExecutionStyle>,
    /// Which precedence level [`Self::requested`] came from.
    source: StyleSource,
    /// The style that will actually be applied — never below the floor.
    effective: ExecutionStyle,
    /// Why `effective` differs from `requested`; empty when it does not.
    escalations: Vec<StyleEscalation>,
}

impl ResolvedStyle {
    /// Resolve caller > config > built-in, then raise to the callee's floor and
    /// to the nearest implemented tier.
    ///
    /// Why: the ONLY constructor, deliberately (see the type docs). Every input
    /// is combined with `max`, so the function is monotonically non-decreasing
    /// in ceremony: a caller asking for LESS than `callee_floor` moves the
    /// effective value up to the floor and is told so, and can never move it
    /// down. That is DOC-62 §5.4 — "the tcode PM MAY apply more ceremony than
    /// the resolved style requests; it MUST NOT apply less" — made mechanical.
    /// What: first-match-wins over `caller` then `config` then
    /// [`ExecutionStyle::BUILT_IN_DEFAULT`]; the result is `max`-ed with
    /// `callee_floor` (recording [`StyleEscalation::CalleeFloor`] if that
    /// raised it) and then passed through [`ExecutionStyle::implemented`]
    /// (recording [`StyleEscalation::TierUnimplemented`] if that raised it
    /// again). Both escalations can apply to one resolution and both are
    /// reported, in the order they were applied.
    /// Test: `a_caller_can_never_lower_ceremony_below_the_callee_floor`,
    /// `precedence_is_caller_then_config_then_built_in`,
    /// `vibe_degrades_upward_to_engineer_and_says_why`,
    /// `both_escalations_are_reported_when_both_apply`.
    pub fn resolve(
        caller: Option<ExecutionStyle>,
        config: Option<ExecutionStyle>,
        callee_floor: ExecutionStyle,
    ) -> Self {
        let (requested, source) = match (caller, config) {
            (Some(style), _) => (Some(style), StyleSource::Caller),
            (None, Some(style)) => (Some(style), StyleSource::Config),
            (None, None) => (None, StyleSource::BuiltIn),
        };
        let asked = requested.unwrap_or(ExecutionStyle::BUILT_IN_DEFAULT);

        let mut escalations = Vec::new();
        // Raise (never lower) to the lane's floor.
        let floored = asked.max(callee_floor);
        if floored > asked {
            escalations.push(StyleEscalation::CalleeFloor);
        }
        // Raise (never lower) to the nearest tier that actually exists.
        let (effective, tier) = floored.implemented();
        if let Some(reason) = tier {
            escalations.push(reason);
        }

        // Postcondition — the ceiling property, asserted at the one site that
        // can produce a `ResolvedStyle`. Cheap and debug-only; the property is
        // pinned unconditionally by
        // `a_caller_can_never_lower_ceremony_below_the_callee_floor`.
        debug_assert!(effective >= callee_floor, "resolution lowered below floor");
        debug_assert!(effective >= asked, "resolution lowered below the request");

        Self {
            requested,
            source,
            effective,
            escalations,
        }
    }

    /// The style that will actually be applied.
    ///
    /// Why: callers that report or branch on style must read the EFFECTIVE
    /// value, never the request — reading the request is how "not run" quietly
    /// becomes "fine" (DOC-62 SM-4).
    /// What: the resolved value, always `>=` both the request and the floor.
    /// Test: `resolution_is_reported_end_to_end`.
    pub fn effective(&self) -> ExecutionStyle {
        self.effective
    }

    /// Which precedence level supplied the requested value.
    ///
    /// Why: DOC-62 §3.4's reporting contract (AC-4).
    /// What: see [`StyleSource`].
    /// Test: `precedence_is_caller_then_config_then_built_in`.
    pub fn source(&self) -> StyleSource {
        self.source
    }

    /// Why the effective style differs from the request; empty when it does not.
    ///
    /// Why: DOC-62 §3.4 — a caller that asked for `vibe` is entitled to know
    /// exactly what it bought.
    /// What: raises only, in application order.
    /// Test: `both_escalations_are_reported_when_both_apply`.
    pub fn escalations(&self) -> &[StyleEscalation] {
        &self.escalations
    }

    /// Whether a style was supplied at all, rather than falling through to the
    /// built-in default.
    ///
    /// Why: DOC-62 §6.4 requires that an absent style render NO policy block,
    /// so the no-style path stays byte-identical to pre-#4349 dispatch. This is
    /// the predicate that keeps that promise, and it deliberately asks about
    /// the SOURCE rather than the effective value (which is always populated).
    /// What: true iff [`Self::source`] is not [`StyleSource::BuiltIn`].
    /// Test: `an_unrequested_style_renders_no_policy_block`.
    pub fn is_explicit(&self) -> bool {
        !matches!(self.source, StyleSource::BuiltIn)
    }

    /// Render the TA-PM-policy block appended to a cross-product preamble
    /// (#4349; DOC-62 §6.1, SM-7).
    ///
    /// Why: the product boundary is one argv slot — `tcode run-task <agent>
    /// <task> --project <dir> --json` — so a structured handoff can only cross
    /// it flattened into the task string (see
    /// `HandoffContext::render_preamble`'s docs). SM-7 fixes what this text MAY
    /// carry: the effective style, its meaning, decision 2's PM-only statement
    /// as INFORMATION, and the §3 gate boundary as INSTRUCTION. SM-8 fixes what
    /// it may not be: the sole enforcement of any security control. Both rules
    /// are visible in the text itself — the `NON_CODING_TARGETS` line says out
    /// loud that it is describing a code-enforced property, so no future reader
    /// mistakes the paragraph for the mechanism.
    /// What: a short block whose length is bounded and INDEPENDENT of the task
    /// text or of any caller-supplied field, so the 4 KiB handoff cap never
    /// becomes a function of task size (DOC-62 §6.4). Rendered after the
    /// caller's own lines and under a heading that labels it as not
    /// caller-supplied.
    /// Test: `policy_block_states_effective_style_and_the_gate_boundary`,
    /// `policy_block_is_bounded_and_task_independent`,
    /// `policy_block_reports_the_tier_unimplemented_fallback`.
    pub fn render_policy_block(&self) -> String {
        let mut out = String::from("Delegation policy (system-supplied, not from the caller):\n");
        out.push_str(&format!(
            "- Effective execution style: {}.\n",
            self.effective.as_str()
        ));
        if let Some(requested) = self.requested.filter(|r| *r != self.effective) {
            let reasons: Vec<&str> = self.escalations.iter().map(|e| e.as_str()).collect();
            out.push_str(&format!(
                "- Requested style was {}; raised to {} ({}). Ceremony may be raised, never lowered.\n",
                requested.as_str(),
                self.effective.as_str(),
                reasons.join(", ")
            ));
        }
        out.push_str(
            "- Style selects only your own internal ceremony. It never relaxes a check the \
             target repository enforces (CI, branch protection, required review), and is never \
             a reason to skip a review gate.\n\
             - A check that was not run is reported as not run, never as passed.\n\
             - Style grants no capability: it does not change which tools you may call or what \
             they may do.\n\
             - Coding work is delegated to you as the project manager; no coding sub-agent is \
             reachable from the caller. That boundary is enforced in code, not by this text.\n",
        );
        out
    }
}

#[cfg(test)]
#[path = "execution_style_tests.rs"]
mod execution_style_tests;
