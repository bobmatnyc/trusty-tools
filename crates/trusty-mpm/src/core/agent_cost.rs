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
//! **The hard stop ships OFF; the warning ships ON.** The measured population
//! is the reason. Sampling the 600 most recent subagent transcripts on a
//! working machine over 14 days gives p50 136k, p90 268k, p95 323k, and 18 of
//! 600 (3.0%) at or above 400k — so a 400k *stop* would have fired on roughly
//! one dispatch in 33, most of them ordinary long-but-legitimate work rather
//! than the pathology #4837 describes. [`DEFAULT_MAX_TOKENS`] is therefore `0`
//! (no stop) and the shipped behaviour is warn-only; an operator who wants
//! enforcement opts in via `agent_cost.max_tokens` in
//! `~/.trusty-mpm/config.toml`. The earlier justification for a 400k default
//! rested on a single measured agent at 71.5k, which the distribution above
//! shows is not representative of the population.
//!
//! **The stop, when enabled, never strands work.** [`is_persistence_tool`]
//! names the narrow set that stays permitted past the ceiling — `SendMessage`
//! and the git subcommands in [`PERSISTENCE_GIT_SUBCOMMANDS`] — so a stopped
//! agent can always stage, commit, push, and report back. A guard that leaves
//! finished work uncommitted and unreported costs more than the overrun it
//! prevented.
//!
//! Test: the `#[cfg(test)]` suite below covers threshold triggering, every
//! fail-open path, the warn-only default, the persistence allowlist, and
//! config override; `commands::pm_guard_cost` covers payload/transcript
//! resolution and the Bash half of the allowlist.

use serde::{Deserialize, Serialize};

pub use crate::core::budget::BudgetStatus;

/// Default context size at which an agent is flagged as expensive.
///
/// Why (#4837): sized against the measured population, not one sample. Across
/// the 600 most recent subagent transcripts on a working machine (14 days) the
/// distribution is p50 136k / p90 268k / p95 323k, so 250k sits just under p90
/// — it fires on the top ~10-12% of dispatches. That is deliberately loose for
/// a *warning*: this level never blocks anything, it only tells the agent it
/// is now in the expensive tail and should aim to finish. A tighter warn would
/// nag ordinary work; a looser one would arrive too late to act on.
/// What: the [`AgentCostConfig::warn_tokens`] default, in tokens.
/// Test: `defaults_are_warn_only`, `warns_at_the_warn_threshold`.
pub const DEFAULT_WARN_TOKENS: u64 = 250_000;

/// Default context size at which an agent is hard-stopped — `0`, i.e. OFF.
///
/// Why (#4837): the hard stop ships disabled because the same measured
/// population that sets [`DEFAULT_WARN_TOKENS`] shows a 400k default would be
/// too tight to be a default. 18 of those 600 transcripts (3.0%) sat at or
/// above 400k, so the stop would fire on roughly one dispatch in 33 — and at
/// p95 = 323k, 400k is only ~1.24x the 95th percentile, not the "far above
/// anything legitimate" margin a shipped-on stop needs. Blocking one in 33
/// dispatches mid-task to catch a pathology that is rarer than that inverts
/// this module's own fail-open asymmetry. So the default is warn-only and the
/// stop is opt-in: an operator who has decided the trade is worth it sets
/// `agent_cost.max_tokens` in `~/.trusty-mpm/config.toml`, and when they do,
/// [`is_persistence_tool`] guarantees the stopped agent can still commit,
/// push, and report back.
/// What: the [`AgentCostConfig::max_tokens`] default, in tokens. `0` is the
/// unlimited sentinel — see [`evaluate_cost`].
/// Test: `defaults_are_warn_only`, `default_config_never_stops_any_evidence_row`.
pub const DEFAULT_MAX_TOKENS: u64 = 0;

/// Git subcommands that stay permitted after the ceiling is reached.
///
/// Why (#4837 review): the stop exists to end an agent's *life*, not to strand
/// its *output*. These five are exactly the steps between "work is done in the
/// working tree" and "work is durable and described": stage it, look at what
/// you are about to record, record it, publish it. Every one of them acts on
/// changes that already exist — none of them can produce new work, so
/// allowlisting them cannot be used to keep going. `git status`/`git diff` are
/// read-only and earn their place by letting the agent write an accurate
/// handback instead of guessing at what it left behind.
///
/// That "cannot produce new work" claim holds only because the classifier also
/// judges the FLAGS. A subcommand name alone does not bound what git executes:
/// `git -c diff.external='cargo test' diff` and
/// `git push --receive-pack='cargo test' …` are both a listed subcommand
/// running an arbitrary program (#4850 review). The flag rules live with the
/// classifier, not with this list — see `pm_guard_bash::persistence`.
/// What: the git subcommand allowlist consulted by
/// [`commands::pm_guard_cost::is_persistence_escape`](../../bin/tm/commands/pm_guard_cost/index.html),
/// which additionally requires EVERY segment of a composed command to match.
/// Test: `persistence_tools_cover_report_and_commit`, and the Bash half in
/// `pm_guard_bash::command_is_persistence_only_*`.
pub const PERSISTENCE_GIT_SUBCOMMANDS: &[&str] = &["add", "commit", "push", "status", "diff"];

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

/// Whether `tool_name` is permitted unconditionally past the ceiling.
///
/// Why (#4837 review): the first cut of this guard denied EVERY tool in the
/// [`BudgetStatus::Exceeded`] arm, which meant a stopped agent could not
/// commit, push, or even report back — the deny text told it to report back
/// through a channel the same deny had just closed. Tracing it against a real
/// case: the #4841 engineer reached 434k while producing a correct fix, and
/// under that guard could not have committed or pushed it. So the stop now has
/// an escape hatch, and this is the unconditional half of it.
/// What: `true` for `SendMessage` only. Bash is permitted conditionally on the
/// command it carries — see
/// [`commands::pm_guard_cost::is_persistence_escape`](../../bin/tm/commands/pm_guard_cost/index.html)
/// and [`PERSISTENCE_GIT_SUBCOMMANDS`] — and every other tool, including
/// `Write`, `Edit`, `Read`, and `Grep`, stays denied. That line is the point:
/// `Write`/`Edit` are how an agent *does* work, not how it saves it (anything
/// already written is already on disk), so permitting them would make the stop
/// vacuous.
/// Test: `persistence_tools_cover_report_and_commit`.
pub fn is_persistence_tool(tool_name: &str) -> bool {
    tool_name == "SendMessage"
}

/// Render the DENY text for an agent that has exceeded its context ceiling.
///
/// Why (#4837): a bare "denied" leaves the model guessing and it retries. The
/// text has to carry enough for the PM to act, because the PM is who resolves
/// it — so it names the measured spend, the ceiling that was crossed, and the
/// two legitimate continuations (report back for re-dispatch, or have the
/// operator widen the ceiling).
///
/// It also has to describe **this** guard truthfully. The first cut inherited
/// the sentence "SendMessage is never blocked" from
/// [`crate::core::agent`]'s fan-out denial, where it is true because that
/// guard only ever fires on dispatch tools. Here the guard fires on every
/// tool, so the sentence was false as written until [`is_persistence_tool`]
/// made it true. It now enumerates the exact escape hatch rather than gesturing
/// at one, because an agent that believes a closed channel is open will retry
/// into it instead of reporting back.
///
/// It also names the metacharacter rule (#4850 review, LOW 2). The classifier
/// rejects `$`, a backtick, and unquoted `(`, `)`, `<`, `>` anywhere in the
/// command, so `git commit -m "fix $ISSUE"` is denied — and a deny with no hint
/// is precisely the retry burn this text exists to prevent. Naming the rule and
/// the single-quote fix turns that dead end into one retry.
/// What: a one-line reason naming `context_tokens`, `max_tokens`, the tools
/// that remain available, the metacharacter restriction on those tools, and the
/// config key to raise.
/// Test: `stop_reason_names_the_numbers_and_the_escape_hatch`,
/// `stop_reason_names_the_metacharacter_rule`.
pub fn stop_reason(context_tokens: u64, max_tokens: u64) -> String {
    let git = PERSISTENCE_GIT_SUBCOMMANDS.join(", git ");
    format!(
        "Agent context ceiling reached (#4837): this subagent is carrying {context_tokens} tokens \
         of context, at or past the {max_tokens}-token cap. Every further tool call re-sends all \
         of it, so continuing costs more per step than starting over. SAVE AND REPORT BACK NOW. \
         Still permitted so you never lose work: SendMessage, and Bash running only git {git} \
         (every segment of a composed command must be one of those). Those git commands must be \
         plain: no shell metacharacters — $, backtick, ( ) < > — outside single quotes, so write \
         git commit -m 'literal text' and never -m \"fix $ISSUE\"; and no exotic long flags, only \
         the ordinary ones like -m, -A, -u, --amend, --force-with-lease, --stat, --short. \
         Everything else — including Write, Edit, Read, and any other Bash — is denied until the \
         PM re-dispatches you. Commit and push what you have, then SendMessage the PM: what you \
         completed, what remains, and what a fresh agent needs to finish it. To raise the ceiling \
         instead, set agent_cost.max_tokens in ~/.trusty-mpm/config.toml."
    )
}

/// Render the WARNING text handed to an agent approaching its ceiling.
///
/// Why (#4837 review): the graduated warn-then-stop response only exists if the
/// warn actually reaches the agent. The first cut composed this text addressed
/// to the agent ("Wrap up and report back…") and POSTed it only to the daemon,
/// so no agent ever read it. It is now emitted on the `PreToolUse` hook's
/// stdout as `hookSpecificOutput.additionalContext`, which Claude Code injects
/// next to the tool result — and, because the object carries no
/// `permissionDecision`, the call still falls through the normal permission
/// flow ("staying silent doesn't approve it"). That was the author's stated
/// reason for not writing to stdout, and `additionalContext` is the field that
/// satisfies it. Kept pure here so the wording is testable without a hook run.
/// What: a one-line notice naming the measured spend, the warn threshold, and
/// what the agent should do; mentions the hard stop only when one is
/// configured, since [`DEFAULT_MAX_TOKENS`] is `0` and promising a stop that
/// will never come would be the same false statement `stop_reason` just fixed.
/// Test: `warn_reason_addresses_the_agent`,
/// `warn_reason_omits_a_stop_that_is_not_configured`.
pub fn warn_reason(context_tokens: u64, config: &AgentCostConfig) -> String {
    let stop = if config.max_tokens > 0 {
        format!(
            " Your next tool call is DENIED once you reach {}.",
            config.max_tokens
        )
    } else {
        String::new()
    };
    format!(
        "Context-cost notice (#4837): you are carrying {context_tokens} tokens of context, past \
         the {} warn threshold. Every tool call from here re-sends all of it, so each further \
         step costs more than the last.{stop} Wrap up: finish or checkpoint what you are on, \
         commit and push it, and report back to the PM with what remains so a fresh agent can \
         take it. Do not start new lines of investigation.",
        config.warn_tokens
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

    /// A stop-enabled config, i.e. what an operator opts in to. The shipped
    /// default no longer stops anything, so every stop-path test builds this
    /// explicitly rather than leaning on `Default`.
    fn opted_in_stop() -> AgentCostConfig {
        AgentCostConfig {
            enabled: true,
            warn_tokens: DEFAULT_WARN_TOKENS,
            max_tokens: 400_000,
        }
    }

    #[test]
    fn defaults_are_warn_only() {
        // #4837 review BLOCK 1(a): the hard stop ships OFF. Measured over the
        // 600 most recent subagent transcripts (14 days): p50 136k, p90 268k,
        // p95 323k, and 18/600 = 3.0% at or above 400k — so a shipped-on 400k
        // stop fires on ~1 dispatch in 33, which is too often to be a default.
        let cfg = AgentCostConfig::default();
        assert!(
            cfg.enabled,
            "the warn level is on by default — that is what ships"
        );
        assert_eq!(cfg.warn_tokens, 250_000);
        assert_eq!(
            cfg.max_tokens, 0,
            "the hard stop must default to 0 (opt-in), not 400_000"
        );
        // 250k sits just under the measured p90 (268k): the warn is meant to
        // catch the expensive tail, not ordinary work at p50 (136k).
        assert_eq!(evaluate_cost(136_000, &cfg), BudgetStatus::Ok, "p50");
        assert_eq!(evaluate_cost(268_000, &cfg), BudgetStatus::Warning, "p90");
    }

    #[test]
    fn default_config_never_stops_any_evidence_row() {
        // The default may WARN as loudly as it likes, but it must never DENY —
        // that is the whole of BLOCK 1(a). Asserted against the largest inputs
        // there are, not just the evidence table.
        let cfg = AgentCostConfig::default();
        for tokens in EVIDENCE_OVERRUNS
            .into_iter()
            .chain(EVIDENCE_MIDLIFE)
            .chain([u64::MAX])
        {
            assert_eq!(
                evaluate_cost(tokens, &cfg),
                BudgetStatus::Warning,
                "{tokens} must warn, never stop, under the shipped default"
            );
        }
    }

    #[test]
    fn stops_every_overrun_once_the_operator_opts_in() {
        // The guard still does its job when enabled — #4837 option 4's claim
        // holds, it is just no longer the shipped default.
        let cfg = opted_in_stop();
        for tokens in EVIDENCE_OVERRUNS {
            assert_eq!(
                evaluate_cost(tokens, &cfg),
                BudgetStatus::Exceeded,
                "{tokens} tokens must be stopped under an opted-in ceiling"
            );
        }
        // The mid-life snapshots warn — the agent is flagged as expensive and
        // still allowed to finish, which is the intended graduated response.
        for tokens in EVIDENCE_MIDLIFE {
            assert_eq!(evaluate_cost(tokens, &cfg), BudgetStatus::Warning);
        }
        // An agent at the measured p50 is untouched.
        assert_eq!(evaluate_cost(136_000, &cfg), BudgetStatus::Ok);
    }

    #[test]
    fn warns_at_the_warn_threshold() {
        let cfg = AgentCostConfig::default();
        assert_eq!(evaluate_cost(249_999, &cfg), BudgetStatus::Ok);
        assert_eq!(evaluate_cost(250_000, &cfg), BudgetStatus::Warning);
    }

    #[test]
    fn stops_at_the_max_threshold() {
        let cfg = opted_in_stop();
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
            toml::from_str("enabled = true\nmax_tokens = 123456\n").expect("parses");
        assert!(cfg.enabled);
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
    fn persistence_tools_cover_report_and_commit() {
        // #4837 review BLOCK 1(b): a stopped agent must still be able to save
        // and report. SendMessage is the unconditional half.
        assert!(is_persistence_tool("SendMessage"));
        // Everything that would let it keep WORKING stays denied — otherwise
        // the stop is decorative.
        for tool in [
            "Write", "Edit", "Read", "Grep", "Glob", "Bash", "WebFetch", "Task", "Agent",
        ] {
            assert!(
                !is_persistence_tool(tool),
                "{tool} must not be unconditionally permitted past the ceiling"
            );
        }
        // The git allowlist covers stage → inspect → record → publish, and
        // nothing that can produce new work.
        for sub in ["add", "commit", "push", "status", "diff"] {
            assert!(PERSISTENCE_GIT_SUBCOMMANDS.contains(&sub), "git {sub}");
        }
        for sub in ["checkout", "reset", "rebase", "merge", "clean", "worktree"] {
            assert!(
                !PERSISTENCE_GIT_SUBCOMMANDS.contains(&sub),
                "git {sub} is not persistence and must stay denied"
            );
        }
    }

    #[test]
    fn stop_reason_names_the_numbers_and_the_escape_hatch() {
        let reason = stop_reason(622_200, 400_000);
        // The PM must be able to re-scope from this text alone.
        assert!(reason.contains("622200"));
        assert!(reason.contains("400000"));
        assert!(reason.contains("SendMessage"));
        // #4837 review BLOCK 2: the old text claimed "SendMessage is never
        // blocked", copied from the fan-out denial where it is true because
        // that guard only fires on dispatch tools. Here the guard fires on
        // everything, so the claim must be the specific enumeration instead.
        assert!(
            !reason.contains("never blocked"),
            "the deny text must not repeat the fan-out guard's blanket claim"
        );
        // It must name the git commands that still work, or the agent cannot
        // act on the instruction to commit and push.
        for sub in PERSISTENCE_GIT_SUBCOMMANDS {
            assert!(reason.contains(sub), "deny text must name git {sub}");
        }
        // And it must be explicit that the rest is closed, so the agent does
        // not burn rounds retrying Write/Edit.
        assert!(reason.contains("Write, Edit"));
        // The operator must be told how to widen it rather than fight it.
        assert!(reason.contains("agent_cost.max_tokens"));
    }

    #[test]
    fn stop_reason_names_the_metacharacter_rule() {
        // #4850 review LOW 2: naming `git commit` as permitted while the
        // classifier silently rejects every unquoted `$`, backtick, `(`, `)`,
        // `<`, and `>` sends the agent into exactly the retry burn this text
        // exists to prevent — `git commit -m "fix $ISSUE"` denies with no hint.
        // The rule and its fix (single quotes) must both be in the text.
        let reason = stop_reason(622_200, 400_000);
        assert!(
            reason.contains("metacharacter"),
            "the deny text must name the rule that rejects `-m \"fix $ISSUE\"`"
        );
        assert!(
            reason.contains("single quotes"),
            "naming the rule without the fix still costs a retry"
        );
        // Every character SUBSTITUTION_METACHARS/SYNTAX_METACHARS reject must be
        // named. The backtick is spelled out rather than shown: a lone ` in a
        // one-paragraph deny string reads as stray punctuation.
        for ch in ["$", "backtick", "(", ")", "<", ">"] {
            assert!(
                reason.contains(ch),
                "the deny text must name the rejected character {ch:?}"
            );
        }
        // Same shape for the option surface: the flags that still work are
        // named, so a rejected exotic flag costs one retry rather than a guess.
        assert!(reason.contains("--force-with-lease"));
    }

    #[test]
    fn warn_reason_addresses_the_agent() {
        // The warn text is now delivered to the agent via
        // `hookSpecificOutput.additionalContext`, so second-person
        // instructions are finally honest.
        let text = warn_reason(268_000, &opted_in_stop());
        assert!(text.contains("268000"));
        assert!(text.contains("250000"));
        assert!(text.contains("400000"), "a configured stop must be named");
        assert!(text.contains("report back to the PM"));
    }

    #[test]
    fn warn_reason_omits_a_stop_that_is_not_configured() {
        // Under the shipped default there IS no stop, and promising one would
        // be the same false statement BLOCK 2 removed from `stop_reason`.
        let text = warn_reason(268_000, &AgentCostConfig::default());
        assert!(text.contains("268000"));
        assert!(
            !text.contains("DENIED"),
            "must not threaten a stop that will never fire: {text}"
        );
    }
}
