//! Per-subagent context-cost policy (issue #4837).
//!
//! Why: trusty-mpm routes mechanical, deterministic work through LLM inference
//! loops, and cost grows superlinearly because every tool round re-sends the
//! agent's accumulated context. On 2026-08-04 three subagents burned 1.96M
//! tokens between them (622.2k / 418.5k / 413.3k) doing work — a file-copy
//! loop, running `cargo fmt`, grepping a gates file — that needed no inference
//! at all. #4837's own interpretation is the load-bearing one: those figures
//! are not the cost of the step they are labelled with. They are the agent's
//! **accumulated context at the moment it reached that step**, so the last
//! cheap action in a long-lived agent is the most expensive one it takes. The
//! driver is agent lifetime, not action price.
//!
//! A prose rule against this shipped in PR #4790 on the same day as the
//! overrun and did not prevent it. This module is the mechanical answer,
//! following the precedent set by `pm_guard`'s file-change budget and #4796's
//! `PreToolUse` fan-out denial: enforce the economy at a guard, not in an
//! instruction a model under task pressure can talk itself past.
//!
//! What: [`AgentCostConfig`] is the `[agent_cost]` section of
//! `~/.trusty-mpm/config.toml`; [`latest_context_tokens`] extracts the current
//! context size from a transcript tail; [`evaluate_cost`] classifies it; and
//! [`stop_reason`] renders the DENY text handed back to the agent.
//!
//! **The metric is context-window occupancy, not cumulative billed spend.**
//! One assistant turn's `usage` block reports `input_tokens +
//! cache_creation_input_tokens + cache_read_input_tokens` — the whole prompt
//! that turn re-sent, which *is* the accumulated context #4837 names as the
//! driver. Choosing it is also what makes the guard affordable: the newest
//! turn sits at the END of the JSONL, so the number is recoverable from a
//! bounded tail read in constant time no matter how large the transcript has
//! grown. Summing billed tokens across every turn would instead require
//! scanning the entire file on every single tool call — paying an O(n) cost to
//! police an O(n) cost.
//!
//! **Everything here fails OPEN.** An unreadable transcript, a tail with no
//! usage record, a disabled or zeroed limit, and a caller that is not a
//! subagent all resolve to [`BudgetStatus::Ok`]. Killing legitimate work over
//! a broken counter is strictly worse than the overrun the counter watches
//! for: a false stop lands on an agent mid-task and costs a re-dispatch, while
//! a false allow merely reproduces the behaviour that existed before this
//! module. No future signal may invert that asymmetry.
//!
//! Test: the `#[cfg(test)]` suite below covers threshold triggering, every
//! fail-open path, and config override; `commands::pm_guard_cost` covers
//! payload/transcript resolution.

use serde::{Deserialize, Serialize};

pub use crate::core::budget::BudgetStatus;

/// Default context size at which an agent is flagged as expensive.
///
/// Why (#4837): a healthy subagent in this workspace completes far below this
/// — a finished delegation measured 71.5k of window occupancy — so 250k sits
/// roughly 3.5x above ordinary completion and cannot fire on normal work. It
/// is also comfortably under the smallest logged overrun (413.3k), which
/// leaves ~150k of headroom (tens of tool rounds) for the agent to finish or
/// hand back before [`DEFAULT_MAX_TOKENS`] stops it.
/// What: the [`AgentCostConfig::warn_tokens`] default, in tokens.
/// Test: `defaults_match_the_4837_evidence`.
pub const DEFAULT_WARN_TOKENS: u64 = 250_000;

/// Default context size at which an agent is hard-stopped.
///
/// Why (#4837): deliberately set just below the *smallest* overrun in the
/// evidence table (413.3k) so every logged overrun — 413.3k, 418.5k, and
/// 622.2k — would have been stopped, rather than only the worst one. It stays
/// well above any legitimate agent observed in this workspace, so a long build
/// or a wide refactor does not trip it: reaching 400k of window occupancy
/// means an agent has been alive across hundreds of rounds, which is the
/// condition #4837 identifies as pathological regardless of what the work was.
/// A long-but-legitimate task is not blocked so much as split — the agent
/// reports back and the PM re-dispatches with fresh context, which is cheaper
/// than letting it continue.
/// What: the [`AgentCostConfig::max_tokens`] default, in tokens.
/// Test: `defaults_match_the_4837_evidence`, `stops_every_overrun_in_the_evidence_table`.
pub const DEFAULT_MAX_TOKENS: u64 = 400_000;

/// `[agent_cost]` — per-subagent context ceiling (#4837).
///
/// Why: thresholds belong in user config because the right ceiling depends on
/// the model's context window and the shape of the work; hard-coding 400k
/// would be wrong for a 200k-window model and needlessly tight for a genuinely
/// long migration. Both bounds are independently settable rather than derived
/// from one another (unlike [`crate::core::budget::TokenBudget`]'s fixed 80%
/// warning fraction) so an operator can widen the stop without losing the
/// early signal.
/// What: `enabled` (master switch), `warn_tokens`, and `max_tokens`. A `0` in
/// either threshold means "no limit at this level", matching
/// [`TokenBudget`](crate::core::budget::TokenBudget)'s unlimited sentinel.
/// Test: `config_override_changes_thresholds`, `disabled_config_always_allows`,
/// `zero_max_is_unlimited`, `config_section_parses_from_toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentCostConfig {
    /// Whether the guard evaluates at all. `false` → always
    /// [`BudgetStatus::Ok`].
    pub enabled: bool,
    /// Context size (tokens) at which the agent is flagged but still allowed.
    /// `0` disables the warning level.
    pub warn_tokens: u64,
    /// Context size (tokens) at which the agent's next tool call is denied.
    /// `0` disables the hard stop.
    pub max_tokens: u64,
}

impl Default for AgentCostConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            warn_tokens: DEFAULT_WARN_TOKENS,
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }
}

/// Classify an agent's current context size against its configured ceiling.
///
/// Why: kept pure — it takes the already-measured token count rather than
/// reading a transcript itself — so every threshold and fail-open branch is
/// unit-testable without touching the filesystem. The I/O half lives in
/// `commands::pm_guard_cost`.
/// What: [`BudgetStatus::Exceeded`] at or above a non-zero `max_tokens`,
/// [`BudgetStatus::Warning`] at or above a non-zero `warn_tokens`, else
/// [`BudgetStatus::Ok`]. A disabled config is always `Ok`. `warn_tokens` above
/// `max_tokens` is not an error: the `Exceeded` arm is checked first, so a
/// misordered pair degrades to "stop only", never to a silent no-op.
/// Test: `warns_at_the_warn_threshold`, `stops_at_the_max_threshold`,
/// `disabled_config_always_allows`, `zero_max_is_unlimited`,
/// `misordered_thresholds_still_stop`.
pub fn evaluate_cost(context_tokens: u64, config: &AgentCostConfig) -> BudgetStatus {
    if !config.enabled {
        return BudgetStatus::Ok;
    }
    if config.max_tokens > 0 && context_tokens >= config.max_tokens {
        return BudgetStatus::Exceeded;
    }
    if config.warn_tokens > 0 && context_tokens >= config.warn_tokens {
        return BudgetStatus::Warning;
    }
    BudgetStatus::Ok
}

/// Extract the newest turn's context size from a transcript tail.
///
/// Why: the guard needs one number — how much context this agent is currently
/// re-sending on every round — and it needs it in bounded time on a file that
/// may be hundreds of megabytes. Scanning forward for a maximum, or summing
/// across turns, would make the guard's own cost scale with the overrun it
/// exists to catch. Reading the LAST complete `usage` block instead is
/// constant-cost and, because window occupancy grows monotonically across an
/// agent's life until a compaction resets it, is also the correct number:
/// after a compaction the agent genuinely *is* cheaper per round again, and
/// the guard should reflect that rather than punish it for history.
/// What: scans `jsonl` for `"usage"` objects and returns the last one's
/// `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`.
/// Lines are parsed independently and unparseable ones are skipped, so a tail
/// read slicing mid-line costs at most the one truncated leading record.
/// Returns `None` when no line carries a usage block — the FAIL-OPEN answer.
/// Test: `reads_context_tokens_from_the_last_usage_record`,
/// `tolerates_a_truncated_leading_line`, `returns_none_without_a_usage_record`,
/// `ignores_a_usage_block_with_no_token_fields`.
pub fn latest_context_tokens(jsonl: &str) -> Option<u64> {
    /// Sum the three fields that together make up one turn's prompt size.
    fn context_of(usage: &serde_json::Value) -> Option<u64> {
        let field = |k: &str| usage.get(k).and_then(serde_json::Value::as_u64);
        // At least one field must be present, else this is not a usage block
        // we understand and reporting 0 would read as "free".
        let parts = [
            field("input_tokens"),
            field("cache_creation_input_tokens"),
            field("cache_read_input_tokens"),
        ];
        parts
            .iter()
            .any(Option::is_some)
            .then(|| parts.iter().flatten().sum())
    }

    jsonl
        .lines()
        .rev()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            let usage = value.get("message")?.get("usage")?;
            context_of(usage)
        })
        .next()
}

/// Render the DENY text for an agent that has exceeded its context ceiling.
///
/// Why (#4837): a bare "denied" leaves the model guessing and it retries. The
/// text has to carry enough for the PM to act, because the PM is who resolves
/// it — so it names the measured spend, the ceiling that was crossed, and the
/// two legitimate continuations (report back for re-dispatch, or have the
/// operator widen the ceiling). It also states that `SendMessage` still works,
/// mirroring [`crate::core::agent`]'s fan-out denial, so reporting back is
/// never mistaken for a second blocked path — an agent that could not report
/// back would be stranded rather than re-scoped.
/// What: a one-line reason naming `context_tokens`, `max_tokens`, and the
/// config key to raise.
/// Test: `stop_reason_names_the_numbers_and_the_way_out`.
pub fn stop_reason(context_tokens: u64, max_tokens: u64) -> String {
    format!(
        "Agent context ceiling reached (#4837): this subagent is carrying {context_tokens} tokens \
         of context, at or past the {max_tokens}-token cap. Every further tool call re-sends all \
         of it, so continuing costs more per step than starting over. STOP and report back to the \
         PM now: say what you completed, what remains, and what a fresh agent needs to finish it — \
         the PM will re-dispatch with clean context. SendMessage is never blocked. To raise the \
         ceiling instead, set agent_cost.max_tokens in ~/.trusty-mpm/config.toml."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Context sizes from #4837's evidence table, in tokens.
    const EVIDENCE_OVERRUNS: [u64; 3] = [413_300, 418_500, 622_200];
    /// The two sub-300k rows from the same table — mid-life snapshots of an
    /// agent, not overruns in their own right.
    const EVIDENCE_MIDLIFE: [u64; 2] = [255_500, 269_900];

    #[test]
    fn defaults_match_the_4837_evidence() {
        let cfg = AgentCostConfig::default();
        assert!(
            cfg.enabled,
            "the guard is on by default — that is the point"
        );
        assert_eq!(cfg.warn_tokens, 250_000);
        assert_eq!(cfg.max_tokens, 400_000);
        // The stop must sit BELOW the smallest logged overrun, or it would
        // have caught only the worst one.
        assert!(
            cfg.max_tokens < EVIDENCE_OVERRUNS[0],
            "max must be under the smallest overrun to catch all three"
        );
        // The warning must arrive with real headroom, not one round early.
        assert!(cfg.warn_tokens < cfg.max_tokens);
    }

    #[test]
    fn stops_every_overrun_in_the_evidence_table() {
        // The whole claim of #4837 option 4: this would have caught all of
        // them "regardless of what any prompt said".
        let cfg = AgentCostConfig::default();
        for tokens in EVIDENCE_OVERRUNS {
            assert_eq!(
                evaluate_cost(tokens, &cfg),
                BudgetStatus::Exceeded,
                "{tokens} tokens must be stopped under default config"
            );
        }
        // The mid-life snapshots warn — the agent is flagged as expensive and
        // still allowed to finish, which is the intended graduated response.
        for tokens in EVIDENCE_MIDLIFE {
            assert_eq!(evaluate_cost(tokens, &cfg), BudgetStatus::Warning);
        }
        // A healthy completed agent (measured: 71.5k) is untouched.
        assert_eq!(evaluate_cost(71_540, &cfg), BudgetStatus::Ok);
    }

    #[test]
    fn warns_at_the_warn_threshold() {
        let cfg = AgentCostConfig::default();
        assert_eq!(evaluate_cost(249_999, &cfg), BudgetStatus::Ok);
        assert_eq!(evaluate_cost(250_000, &cfg), BudgetStatus::Warning);
    }

    #[test]
    fn stops_at_the_max_threshold() {
        let cfg = AgentCostConfig::default();
        assert_eq!(evaluate_cost(399_999, &cfg), BudgetStatus::Warning);
        assert_eq!(evaluate_cost(400_000, &cfg), BudgetStatus::Exceeded);
        assert_eq!(evaluate_cost(u64::MAX, &cfg), BudgetStatus::Exceeded);
    }

    #[test]
    fn disabled_config_always_allows() {
        // FAIL-OPEN arm 1: the master switch beats every threshold.
        let cfg = AgentCostConfig {
            enabled: false,
            ..Default::default()
        };
        for tokens in [0, 250_000, 400_000, u64::MAX] {
            assert_eq!(evaluate_cost(tokens, &cfg), BudgetStatus::Ok);
        }
    }

    #[test]
    fn zero_max_is_unlimited() {
        // FAIL-OPEN arm 2: `0` is the unlimited sentinel, matching TokenBudget.
        let cfg = AgentCostConfig {
            enabled: true,
            warn_tokens: 0,
            max_tokens: 0,
        };
        assert_eq!(evaluate_cost(u64::MAX, &cfg), BudgetStatus::Ok);
        // Warning alone can be disabled without disabling the stop.
        let stop_only = AgentCostConfig {
            enabled: true,
            warn_tokens: 0,
            max_tokens: 400_000,
        };
        assert_eq!(evaluate_cost(300_000, &stop_only), BudgetStatus::Ok);
        assert_eq!(evaluate_cost(400_000, &stop_only), BudgetStatus::Exceeded);
    }

    #[test]
    fn config_override_changes_thresholds() {
        // An operator raising the ceiling for a genuinely long migration.
        let cfg = AgentCostConfig {
            enabled: true,
            warn_tokens: 700_000,
            max_tokens: 900_000,
        };
        for tokens in EVIDENCE_OVERRUNS {
            assert_eq!(
                evaluate_cost(tokens, &cfg),
                BudgetStatus::Ok,
                "{tokens} must pass under a widened ceiling"
            );
        }
        assert_eq!(evaluate_cost(700_000, &cfg), BudgetStatus::Warning);
        assert_eq!(evaluate_cost(900_000, &cfg), BudgetStatus::Exceeded);
    }

    #[test]
    fn misordered_thresholds_still_stop() {
        // warn > max is operator error, not a reason to no-op: the stop wins.
        let cfg = AgentCostConfig {
            enabled: true,
            warn_tokens: 900_000,
            max_tokens: 100_000,
        };
        assert_eq!(evaluate_cost(150_000, &cfg), BudgetStatus::Exceeded);
    }

    #[test]
    fn config_section_parses_from_toml() {
        let cfg: AgentCostConfig =
            toml::from_str("enabled = false\nmax_tokens = 123456\n").expect("parses");
        assert!(!cfg.enabled);
        assert_eq!(cfg.max_tokens, 123_456);
        // `#[serde(default)]` on the struct: omitted keys keep their defaults
        // rather than failing the parse, so a partial section is valid.
        assert_eq!(cfg.warn_tokens, DEFAULT_WARN_TOKENS);
    }

    /// One assistant line as Claude Code actually writes it (field set copied
    /// verbatim from a live subagent transcript, 2026-08-04).
    fn assistant_line(input: u64, cache_creation: u64, cache_read: u64) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "usage": {
                    "input_tokens": input,
                    "cache_creation_input_tokens": cache_creation,
                    "cache_read_input_tokens": cache_read,
                    "output_tokens": 873,
                    "service_tier": "standard"
                }
            }
        })
        .to_string()
    }

    #[test]
    fn reads_context_tokens_from_the_last_usage_record() {
        let jsonl = format!(
            "{}\n{}\n",
            assistant_line(8, 776, 70_756),
            assistant_line(12, 1_000, 410_000)
        );
        // The NEWEST record wins — 12 + 1000 + 410000.
        assert_eq!(latest_context_tokens(&jsonl), Some(411_012));
    }

    #[test]
    fn tolerates_a_truncated_leading_line() {
        // A bounded tail read slices mid-line; the partial record must be
        // skipped rather than poisoning the result.
        let jsonl = format!(
            "ache_read_input_tokens\":70756}}}}}}\n{}\n",
            assistant_line(8, 776, 300_000)
        );
        assert_eq!(latest_context_tokens(&jsonl), Some(300_784));
    }

    #[test]
    fn returns_none_without_a_usage_record() {
        // FAIL-OPEN arm 3: user turns and tool results carry no usage block.
        let jsonl = "{\"type\":\"user\",\"message\":{\"role\":\"user\"}}\n";
        assert_eq!(latest_context_tokens(jsonl), None);
        assert_eq!(latest_context_tokens(""), None);
        assert_eq!(latest_context_tokens("not json at all\n"), None);
    }

    #[test]
    fn ignores_a_usage_block_with_no_token_fields() {
        // A usage object carrying only `output_tokens` says nothing about
        // context size; reporting 0 would read as "free" and silently disarm
        // the guard, so it must be skipped in favour of an older complete
        // record — or None.
        let bare = serde_json::json!({
            "message": { "usage": { "output_tokens": 5 } }
        })
        .to_string();
        assert_eq!(latest_context_tokens(&bare), None);

        let jsonl = format!("{}\n{}\n", assistant_line(8, 776, 500_000), bare);
        assert_eq!(latest_context_tokens(&jsonl), Some(500_784));
    }

    #[test]
    fn stop_reason_names_the_numbers_and_the_way_out() {
        let reason = stop_reason(622_200, 400_000);
        // The PM must be able to re-scope from this text alone.
        assert!(reason.contains("622200"));
        assert!(reason.contains("400000"));
        assert!(reason.contains("report back to the PM"));
        assert!(reason.contains("SendMessage is never blocked"));
        // And the operator must be told how to widen it rather than fight it.
        assert!(reason.contains("agent_cost.max_tokens"));
    }
}
