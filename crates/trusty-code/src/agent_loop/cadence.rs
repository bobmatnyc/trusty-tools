//! Cadence compressor (#2346, epic #2343 "Infinite Sessions" ticket 3):
//! continuous every-N-turn history compression that keeps the model-facing
//! transcript under a fixed fraction of the model's context window at ALL
//! times, so [`super::compaction`]'s threshold compactor becomes a backstop
//! that should never fire in steady state, rather than the primary
//! token-efficiency mechanism.
//!
//! Why: The pre-#2346 design only compacts REACTIVELY, once
//! `estimate_total_tokens` crosses ~75% of the window
//! (`CompactionConfig::for_context_window`) — by the time that fires, the
//! transcript has already grown close to the limit, and every fire triggers
//! a "re-anchor thrash" (the last-user-message replay). The epic's hard
//! constraint is working context >= 60% of the window AT ALL TIMES and ZERO
//! threshold-compaction events in a steady long-running session. A
//! proactive, turn-count-driven cadence — compress every `cadence_turns`
//! turns, THEN continuously enforce the budget every turn regardless of
//! cadence — achieves both: routine cadence firing keeps growth bounded in
//! the common case, and the continuous-enforcement loop is the safety net
//! for a single pathological oversized turn that would otherwise blow the
//! budget between cadence fires.
//! What: [`CadenceConfig`] (`cadence_turns` + `max_overhead_fraction_pct`,
//! resolved from `.claude/settings.json`'s `code_harness.*` keys and two
//! escape-hatch env vars via [`resolve_cadence_config`], mirroring
//! `crate::mode`'s precedence convention); [`maybe_cadence_compress`] (the
//! turn-boundary entry point `agent_loop::AgentLoop` calls) which (1) ticks
//! the transcript's turn counter and, on a cadence-fire turn, unconditionally
//! compacts the span older than the active zone via
//! `Transcript::compact_span_tagged` (#2278 group-safety and #2070 pinned/
//! system/user exemptions reused verbatim, never reimplemented here), then
//! (2) ALWAYS runs [`enforce_budget`] — a small, hard-bounded loop that keeps
//! shrinking the active zone and re-compacting until
//! `estimate_total_tokens(to_messages())` is back under the
//! `max_overhead_fraction_pct` cap or nothing compactable remains (logging a
//! warning, never panicking or looping forever, on that documented floor).
//! Summarisation itself stays the existing deterministic
//! `super::compaction::summarize_span` — see this module's doc note on the
//! LLM-summary follow-up below.
//!
//! **LLM vs. deterministic summary (design decision, ticket-sanctioned):**
//! the epic's prose says "LLM auto-compresses history", but wiring a
//! synchronous `LlmClientTrait::chat` call into this turn boundary would
//! make cadence's own summarisation step a new network-dependent failure
//! mode for a mechanism whose entire point is to keep the loop robust and
//! fast — and every turn already pays for one real model round-trip. Per
//! the ticket's own explicit guidance ("do NOT block the epic on perfect
//! summarization"), this ships the deterministic `summarize_span` fallback
//! now, with the summary text pointing at the durable per-turn memory record
//! (#2345's `chat_turn_append`/`memory_remember` dual-write) via a
//! "turns N-M — use recall_session" pointer (see [`super::transcript::flush_span`]).
//! An LLM-generated abstractive summary (bounded to ~500 tokens, prompted to
//! preserve decisions/open-questions/artifact names) is the natural
//! follow-up once `agent_loop::AgentLoop` exposes a clean async hook at this
//! boundary — tracked for #2349/#2350 rather than blocking this ticket.
//! Test: `cadence::tests::*`.

use std::path::Path;

use super::Transcript;
use super::compaction::estimate_total_tokens;

/// Turn-count cadence at which history compression fires by default (epic
/// #2343: "N=8 default, config knob").
pub const DEFAULT_CADENCE_TURNS: usize = 8;

/// Fraction (as a whole-number percentage) of the model's context window the
/// working transcript may occupy before continuous enforcement kicks in
/// (epic #2343: "max_overhead_fraction=0.40 knob").
pub const DEFAULT_MAX_OVERHEAD_FRACTION_PCT: usize = 40;

/// Escape-hatch env var overriding [`CadenceConfig::cadence_turns`] for one
/// process, mirroring `crate::mode::MODE_ENV_VAR`'s precedent.
pub const CADENCE_TURNS_ENV_VAR: &str = "TCODE_CADENCE_TURNS";

/// Escape-hatch env var overriding
/// [`CadenceConfig::max_overhead_fraction_pct`] for one process.
pub const CADENCE_OVERHEAD_FRACTION_ENV_VAR: &str = "TCODE_CADENCE_MAX_OVERHEAD_FRACTION_PCT";

/// Tuning knobs for the #2346 cadence compressor.
///
/// Why: A caller needs to control both "how often" (turn-count cadence) and
/// "how much of the window is overhead" (the budget the cadence + continuous
/// enforcement together must respect) — bundling them mirrors
/// `CompactionConfig`'s own two-knob shape.
/// What: `cadence_turns` is the turn-count period `maybe_cadence_compress`
/// fires the unconditional compaction pass on (0 disables cadence firing
/// entirely, though continuous enforcement still applies — see
/// `Transcript::tick_cadence_turn`'s docs); `max_overhead_fraction_pct` is
/// the whole-number percentage of the model's real context window
/// [`CadenceConfig::overhead_cap_tokens`] resolves the absolute cap from.
/// Test: `cadence::tests::default_is_sane`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CadenceConfig {
    /// Compress every this-many completed turns.
    pub cadence_turns: usize,
    /// Whole-number percentage of the context window the working transcript
    /// may occupy.
    pub max_overhead_fraction_pct: usize,
}

impl Default for CadenceConfig {
    /// The epic's stated defaults: `cadence_turns: 8`,
    /// `max_overhead_fraction_pct: 40`.
    /// Test: `cadence::tests::default_is_sane`.
    fn default() -> Self {
        Self {
            cadence_turns: DEFAULT_CADENCE_TURNS,
            max_overhead_fraction_pct: DEFAULT_MAX_OVERHEAD_FRACTION_PCT,
        }
    }
}

impl CadenceConfig {
    /// Resolve the absolute overhead-token cap for a given context window.
    ///
    /// Why: The budget arithmetic (epic #2343: on a 200K window, 40% ->
    /// 80K overhead cap, 120K working floor) needs the model's REAL context
    /// window — a property of the model, resolved separately via
    /// `crate::provider::resolve_context_window` — combined with this
    /// config's fraction.
    /// What: `context_window * max_overhead_fraction_pct / 100`.
    /// Test: `cadence::tests::overhead_cap_matches_epic_arithmetic`.
    pub fn overhead_cap_tokens(&self, context_window: usize) -> usize {
        context_window * self.max_overhead_fraction_pct / 100
    }
}

/// Resolve a project's [`CadenceConfig`] via the `code_harness.*`
/// `.claude/settings.json` precedence chain `crate::mode::resolve_mode`
/// establishes, adapted to this config's two knobs.
///
/// Why: An operator needs to tune cadence per-project (`settings.json`) or
/// per-process (the escape-hatch env vars) without a code change, matching
/// every other `code_harness.*` knob in this crate. Unlike `resolve_mode`,
/// there is no `task.run`-request tier — this ticket does not add a
/// per-call cadence override to the wire protocol, so the chain is simply
/// env var (highest) > `settings.json` > built-in default.
/// What: Starts from `.claude/settings.json`'s `code_harness.cadence_turns`/
/// `code_harness.max_overhead_fraction_pct` (via
/// [`read_settings_json_cadence`], falling back to [`CadenceConfig::default`]
/// when the file/keys are absent or malformed — never an error, matching
/// `crate::mode::read_settings_json_mode`'s graceful-degrade convention),
/// then overrides either field from [`CADENCE_TURNS_ENV_VAR`]/
/// [`CADENCE_OVERHEAD_FRACTION_ENV_VAR`] when set to a valid `usize` (an
/// unparseable value is logged via `tracing::warn!` and the prior tier's
/// value is kept, rather than erroring the whole run).
/// Test: `cadence::tests::resolve_cadence_config_settings_json_override`,
/// `cadence::tests::resolve_cadence_config_env_wins_over_settings_json`.
pub fn resolve_cadence_config(project_root: &Path) -> CadenceConfig {
    let mut cfg = read_settings_json_cadence(project_root).unwrap_or_default();

    if let Ok(raw) = std::env::var(CADENCE_TURNS_ENV_VAR) {
        match raw.trim().parse::<usize>() {
            Ok(v) => cfg.cadence_turns = v,
            Err(e) => tracing::warn!(
                "{CADENCE_TURNS_ENV_VAR}={raw:?} is not a valid usize ({e}); keeping {}",
                cfg.cadence_turns
            ),
        }
    }
    if let Ok(raw) = std::env::var(CADENCE_OVERHEAD_FRACTION_ENV_VAR) {
        match raw.trim().parse::<usize>() {
            Ok(v) => cfg.max_overhead_fraction_pct = v,
            Err(e) => tracing::warn!(
                "{CADENCE_OVERHEAD_FRACTION_ENV_VAR}={raw:?} is not a valid usize ({e}); keeping {}",
                cfg.max_overhead_fraction_pct
            ),
        }
    }
    cfg
}

/// Read `<project_root>/.claude/settings.json`'s `code_harness.cadence_turns`
/// / `code_harness.max_overhead_fraction_pct` keys.
///
/// Why: mirrors `crate::mode::read_settings_json_mode`'s graceful-degrade
/// convention for the same project-scoped config file.
/// What: `None` when the file is missing, unreadable, not valid JSON, or has
/// no `code_harness` object at all; otherwise `Some(CadenceConfig)` starting
/// from [`CadenceConfig::default`] with either field overridden by whichever
/// of the two keys is present and a valid non-negative integer (a present
/// but wrong-typed key is silently skipped for that field, not an error for
/// the whole read).
/// Test: `cadence::tests::resolve_cadence_config_settings_json_override`,
/// `cadence::tests::read_settings_json_cadence_missing_file_is_not_an_error`.
fn read_settings_json_cadence(project_root: &Path) -> Option<CadenceConfig> {
    let path = project_root.join(".claude").join("settings.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let harness = value.get("code_harness")?;

    let mut cfg = CadenceConfig::default();
    if let Some(turns) = harness
        .get("cadence_turns")
        .and_then(serde_json::Value::as_u64)
    {
        cfg.cadence_turns = turns as usize;
    }
    if let Some(pct) = harness
        .get("max_overhead_fraction_pct")
        .and_then(serde_json::Value::as_u64)
    {
        cfg.max_overhead_fraction_pct = pct as usize;
    }
    Some(cfg)
}

/// Outcome of one [`maybe_cadence_compress`] call, for observability/tests.
///
/// Why: `agent_loop::AgentLoop`'s turn boundary and unit tests both need to
/// know what happened without re-deriving it from `Transcript`'s counters.
/// What: `fired` — whether the scheduled cadence compaction ran THIS turn;
/// `rounds` — total compaction rounds this call performed (the cadence round,
/// if any, plus every continuous-enforcement round); `within_budget` —
/// whether the transcript ended at/under the overhead cap (`false` only on
/// the documented floor: the active zone alone, after full compaction,
/// still exceeds the cap).
/// Test: `cadence::tests::*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CadenceOutcome {
    pub fired: bool,
    pub rounds: usize,
    pub within_budget: bool,
}

/// The turn-boundary entry point: tick the cadence counter, compress on
/// schedule, then continuously enforce the overhead budget every turn.
///
/// Why: This is what makes the epic's "ZERO threshold-compaction events in
/// steady state" metric achievable — see the module doc's overview. Called
/// BEFORE `Transcript::maybe_compact` at the same turn boundary
/// (`AgentLoop::run_inner`), so the threshold compactor only ever sees a
/// transcript this function already kept under budget.
/// What: `transcript.tick_cadence_turn(cfg.cadence_turns)` advances the
/// counter and reports whether this is a scheduled fire; if so,
/// `transcript.compact_span_tagged` unconditionally compacts everything
/// older than `keep_last_messages` (the active zone — reused from the
/// caller's own `CompactionConfig::keep_last_messages`, not a new knob).
/// Regardless of whether cadence fired this turn, [`enforce_budget`] then
/// runs: it re-checks `estimate_total_tokens(transcript.to_messages())`
/// against `cfg.overhead_cap_tokens(context_window)` and, if still over,
/// keeps shrinking the active zone and re-compacting (still via the SAME
/// group-safe `compact_span_tagged`) until under budget or the active zone
/// hits zero with nothing left to compact — at which point it warns (via
/// `tracing::warn!`) and returns rather than looping forever.
/// Test: `cadence::tests::fires_every_n_turns`,
/// `cadence::tests::stays_under_budget_every_turn`,
/// `cadence::tests::single_oversized_turn_still_ends_under_budget`,
/// `cadence::tests::floor_exceeded_warns_not_panics`.
pub fn maybe_cadence_compress(
    transcript: &mut Transcript,
    cfg: &CadenceConfig,
    keep_last_messages: usize,
    context_window: usize,
) -> CadenceOutcome {
    let fired = transcript.tick_cadence_turn(cfg.cadence_turns);
    let mut rounds = 0;
    if fired {
        let range = transcript.begin_cadence_range();
        if transcript.compact_span_tagged(keep_last_messages, range) {
            transcript.advance_cadence_fire_marker();
        }
        rounds += 1;
    }

    let cap = cfg.overhead_cap_tokens(context_window);
    let (enforcement_rounds, within_budget) = enforce_budget(transcript, cap, keep_last_messages);

    CadenceOutcome {
        fired,
        rounds: rounds + enforcement_rounds,
        within_budget,
    }
}

/// Continuously compact until `transcript`'s model-facing view is at/under
/// `cap_tokens`, or nothing compactable remains.
///
/// Why: A single pathological turn (e.g. a 60K-token `write_file` argument)
/// can blow the overhead cap even immediately after a cadence fire, since
/// cadence only ever protects the last `active_zone` messages, never
/// compacts them. Requirement 3 of #2346 ("continuous budget enforcement")
/// is this loop: shrink the protected active zone one message at a time and
/// re-compact (group-safely) until the budget is satisfied.
/// What: Starting at `keep_last = active_zone`, loop: if already under
/// `cap_tokens`, return `(rounds, true)`. Otherwise compact tagged with the
/// transcript's current cadence range, then either shrink `keep_last` by one
/// (more rounds to go) or, once `keep_last == 0`, either loop back (that
/// round newly compacted something — recheck the budget) or return
/// `(rounds, false)` with a `tracing::warn!` (nothing left to compact and
/// still over budget — the documented floor: pinned/system/user content
/// alone exceeds the cap). Bounded by `active_zone + 2` iterations — never an
/// unbounded loop.
/// Test: `cadence::tests::stays_under_budget_every_turn`,
/// `cadence::tests::single_oversized_turn_still_ends_under_budget`,
/// `cadence::tests::floor_exceeded_warns_not_panics`.
fn enforce_budget(
    transcript: &mut Transcript,
    cap_tokens: usize,
    active_zone: usize,
) -> (usize, bool) {
    let mut keep_last = active_zone;
    let mut rounds = 0;

    loop {
        if estimate_total_tokens(&transcript.to_messages()) <= cap_tokens {
            return (rounds, true);
        }

        let range = transcript.begin_cadence_range();
        let compacted = transcript.compact_span_tagged(keep_last, range);
        rounds += 1;
        if compacted {
            transcript.advance_cadence_fire_marker();
        }

        if keep_last == 0 {
            if !compacted {
                tracing::warn!(
                    cap_tokens,
                    "cadence: overhead cap still exceeded after compacting every eligible \
                     entry — the active zone (pinned + system/user turns) alone exceeds \
                     budget; this is a documented floor, not an error"
                );
                return (rounds, false);
            }
            continue;
        }
        keep_last -= 1;
    }
}

#[cfg(test)]
#[path = "cadence_tests.rs"]
mod tests;
