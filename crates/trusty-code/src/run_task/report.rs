//! Run report: structured result, exit codes, and human/JSON rendering (#1034).
//!
//! Why: `run-task` must emit its outcome in two faithful forms — a human-readable
//! summary on a normal terminal, and a clean machine-readable JSON document when
//! `--json` is set (stdout must then carry *only* that JSON so callers can parse
//! it). Both forms draw from one `RunReport` so they never drift. Meaningful exit
//! codes let scripts branch on success vs. config error vs. run failure vs. a
//! no-op run.
//! What: `RunReport` bundles the diff, transcript, and usage/cost; `ExitCode`
//! enumerates the distinct outcomes; `render_human` / `render_json` produce the
//! two output forms.
//! Test: `run_task::tests` assert the JSON shape, the human summary content, and
//! the exit-code mapping.

use serde_json::json;

use crate::perf::TokenUsage;
use crate::run_task::recorder::TurnRecord;

/// Distinct process exit codes for a `run-task` invocation.
///
/// Why: A caller (CI, the PM smoke-test, a wrapper script) needs to distinguish
/// outcomes without parsing output. The ladder separates a clean success, a
/// successful run that changed nothing, a configuration error (bad agent, bad
/// project), a runtime failure (the loop or LLM errored), (#2207) the
/// wall-clock deadline firing before the PM reached `finish_task` — a run that
/// may have made real, useful progress and must not be conflated with a
/// genuine `RunFailure` (the bug that blocked the M3 bake-off L1 pilot: a
/// deadline hit at turn 13 on an otherwise fully-passing solution reported the
/// same `run_failure` a real crash would) — and (#2265) a PM turn-cap or
/// re-delegation-cap hit that STILL produced a deliverable, the same class of
/// bug as #2207's but triggered by exhausting turns/re-delegation attempts
/// rather than wall-clock time.
/// What: `repr(i32)` so the values are stable and documented; `Success = 0`.
/// Test: `run_task::tests::exit_codes_are_distinct`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    /// The run completed and produced a diff. `0`.
    Success = 0,
    /// Configuration error: missing/invalid agent, bad project path, missing key. `2`.
    ConfigError = 2,
    /// Runtime failure: the agent loop or LLM call errored (excludes the
    /// deadline and partial paths — see `DeadlineExceeded` and `Partial`). `3`.
    RunFailure = 3,
    /// The run completed but changed nothing (empty diff). `4`.
    NoChanges = 4,
    /// The configured wall-clock deadline elapsed before the PM loop reached
    /// a terminal state (#2207). Distinct from `RunFailure` so a caller (e.g.
    /// the M3 bake-off runner) can tell "timed out, possibly close to done"
    /// from "genuinely errored". `5`.
    DeadlineExceeded = 5,
    /// The PM's own turn cap, or the (#2265) engineer re-delegation cap, was
    /// exhausted, but a non-empty diff shows a deliverable was actually
    /// produced. Distinct from `RunFailure` so a caller does not discard a
    /// working, on-disk solution as an opaque crash purely because the PM ran
    /// out of turns/retries absorbing engineer failures — trusty-code cannot
    /// generically verify correctness, so this status only claims "work was
    /// produced", leaving scoring to the downstream harness. `6`.
    Partial = 6,
}

impl ExitCode {
    /// The numeric exit code.
    ///
    /// Why: `process::exit` takes an `i32`; this is the single conversion point.
    /// What: Returns `self as i32`.
    /// Test: `run_task::tests::exit_codes_are_distinct`.
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// The full result of a `run-task` invocation, ready to render.
///
/// Why: One source of truth for both output forms keeps the human and JSON
/// reports in lockstep and makes the success path trivially testable.
/// What: `agent` is the top-level (PM) agent name; `task` is the request; `diff`
/// is the captured working-tree change; `transcript` is the ordered PM+engineer
/// turns; `usage` is the aggregated token usage; `cost_usd` is the summed cost;
/// `exit` is the computed outcome.
/// Test: `run_task::tests::report_renders_json` and `report_renders_human`.
#[derive(Debug, Clone)]
pub struct RunReport {
    /// Top-level agent name (the PM).
    pub agent: String,
    /// The task description that was run.
    pub task: String,
    /// Captured working-tree diff (empty when nothing changed).
    pub diff: String,
    /// Ordered transcript of every PM and engineer turn.
    pub transcript: Vec<TurnRecord>,
    /// Aggregated token usage across the whole run.
    pub usage: TokenUsage,
    /// Summed USD cost across the whole run (`None` when pricing is unavailable).
    pub cost_usd: Option<f64>,
    /// Computed process exit outcome.
    pub exit: ExitCode,
}

impl RunReport {
    /// Render the report as a single machine-readable JSON document.
    ///
    /// Why: `--json` mode requires clean, parseable stdout; this is the only thing
    /// printed in that mode.
    /// What: Serialises all fields into a stable JSON object, including a `status`
    /// string derived from `exit` and a `cost_usd` that is `null` when pricing is
    /// unavailable. Pretty-printed for readability without affecting parseability.
    /// Test: `run_task::tests::report_renders_json`.
    pub fn render_json(&self) -> String {
        let value = json!({
            "status": self.status_str(),
            "exit_code": self.exit.code(),
            "agent": self.agent,
            "task": self.task,
            "diff": self.diff,
            "transcript": self.transcript,
            "usage": {
                "prompt_tokens": self.usage.prompt_tokens,
                "completion_tokens": self.usage.completion_tokens,
                "cache_read_tokens": self.usage.cache_read_tokens,
                "cache_creation_tokens": self.usage.cache_creation_tokens,
            },
            "cost_usd": self.cost_usd,
        });
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
    }

    /// Render the report as a human-readable summary.
    ///
    /// Why: On a normal terminal the operator wants a scannable summary, not raw
    /// JSON. The diff is shown verbatim; the transcript is condensed to one line
    /// per turn; usage and cost are a footer.
    /// What: Builds a multi-section string: a header, the transcript (role, model,
    /// truncated text, tool calls), the diff (or "no changes"), and a usage/cost
    /// footer.
    /// Test: `run_task::tests::report_renders_human`.
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "tcode run-task: agent={} status={}\n",
            self.agent,
            self.status_str()
        ));
        out.push_str(&format!("task: {}\n\n", self.task));

        out.push_str("── Transcript ──\n");
        if self.transcript.is_empty() {
            out.push_str("(no turns recorded)\n");
        } else {
            for (i, turn) in self.transcript.iter().enumerate() {
                let text_preview: String = turn.text.chars().take(120).collect();
                let tools = if turn.tool_calls.is_empty() {
                    String::new()
                } else {
                    format!(" tools=[{}]", turn.tool_calls.join(", "))
                };
                out.push_str(&format!(
                    "{n}. [{role}|{model}]{tools} {text}\n",
                    n = i + 1,
                    role = turn.role,
                    model = turn.model,
                    text = text_preview,
                ));
            }
        }

        out.push_str("\n── Diff ──\n");
        if self.diff.trim().is_empty() {
            out.push_str("(no changes)\n");
        } else {
            out.push_str(&self.diff);
            if !self.diff.ends_with('\n') {
                out.push('\n');
            }
        }

        out.push_str("\n── Usage ──\n");
        let cost = match self.cost_usd {
            Some(c) => format!("${c:.6}"),
            None => "(pricing unavailable)".to_string(),
        };
        out.push_str(&format!(
            "prompt={} completion={} cache_read={} cache_creation={} cost={}\n",
            self.usage.prompt_tokens,
            self.usage.completion_tokens,
            self.usage.cache_read_tokens,
            self.usage.cache_creation_tokens,
            cost,
        ));
        out
    }

    /// Map the exit outcome to a stable status string.
    ///
    /// Why: Both output forms label the outcome with the same vocabulary.
    /// What: Returns `"success"`, `"no_changes"`, `"config_error"`,
    /// `"run_failure"`, (#2207) `"deadline_exceeded"`, or (#2265) `"partial"`.
    /// Test: `run_task::tests::report_renders_json`,
    /// `run_task::tests::exit_code_reflects_deadline_exceeded_distinct_from_run_failure`.
    fn status_str(&self) -> &'static str {
        match self.exit {
            ExitCode::Success => "success",
            ExitCode::NoChanges => "no_changes",
            ExitCode::ConfigError => "config_error",
            ExitCode::RunFailure => "run_failure",
            ExitCode::DeadlineExceeded => "deadline_exceeded",
            ExitCode::Partial => "partial",
        }
    }
}

/// Sum every transcript turn's usage, pricing EACH turn against its OWN
/// role's resolved model rather than blending the whole transcript under
/// one model (#1475 bug 1 fix).
///
/// Why: The PM's `AgentOutput.usage` only covers the PM's own turns — the
/// engineer's usage is lost when its output flows back as a tool-result
/// string. The transcript recorder, by contrast, captures the
/// provider-reported usage of *every* PM and engineer turn, so summing
/// those turns is the faithful run total. Pricing the WHOLE total against a
/// single (PM) model — the original #1475 bug — silently mis-prices every
/// engineer turn whenever the engineer routes to a different, often more
/// expensive, model: directly undercuts the #1035 cost-comparison use case
/// this run exists to support. This is the SAME fix `task::executor`
/// applied for the daemon path (#2056); this is now the ONE shared
/// implementation both paths call, so they can never independently drift.
/// What: Field-wise sums each turn's `usage` into a single `TokenUsage`
/// (unchanged). Per turn, PREFERS the turn's own `usage.cost_usd` —
/// OpenRouter's authoritative, already cache-discount-aware cost
/// (response-side cache-usage fix) — when present; only falls back to the
/// static per-token recompute (priced per role: `pm_model` for
/// `role == "pm"`, `engineer_model` otherwise) when the turn carries no
/// authoritative cost. Sums the per-turn costs. Returns `(total_usage,
/// cost)`.
/// Test: `run_task::tests::usage_and_cost_aggregate_end_to_end`,
/// `report::tests::aggregate_usage_per_role_prices_each_role_separately`,
/// `report::tests::aggregate_usage_per_role_prefers_authoritative_cost`.
pub fn aggregate_usage_per_role(
    turns: &[TurnRecord],
    pm_model: &str,
    engineer_model: &str,
) -> (TokenUsage, f64) {
    let mut total = TokenUsage::default();
    let mut cost = 0.0;
    for turn in turns {
        total.add(&turn.usage);
        cost += turn.usage.cost_usd.unwrap_or_else(|| {
            let model = if turn.role == "pm" {
                pm_model
            } else {
                engineer_model
            };
            crate::perf::cost_usd(
                model,
                turn.usage.prompt_tokens,
                turn.usage.completion_tokens,
                turn.usage.cache_read_tokens,
                turn.usage.cache_creation_tokens,
            )
        });
    }
    (total, cost)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report(exit: ExitCode, diff: &str) -> RunReport {
        RunReport {
            agent: "pm".into(),
            task: "do the thing".into(),
            diff: diff.into(),
            transcript: vec![
                TurnRecord {
                    role: "pm".into(),
                    model: "openai/gpt-4o-mini".into(),
                    text: String::new(),
                    tool_calls: vec!["delegate_to_agent".into()],
                    usage: TokenUsage::new(60, 20, 0, 0),
                },
                TurnRecord {
                    role: "python-engineer".into(),
                    model: "deepseek/deepseek-chat".into(),
                    text: "wrote the file".into(),
                    tool_calls: vec!["write_file".into()],
                    usage: TokenUsage::new(40, 20, 0, 0),
                },
            ],
            usage: TokenUsage::new(100, 40, 0, 0),
            cost_usd: Some(0.001),
            exit,
        }
    }

    /// Exit codes are distinct and stable.
    ///
    /// Why: Scripts branch on these; collisions would be silent breakage.
    /// What: Assert the six codes are 0/2/3/4/5/6 and pairwise distinct.
    /// Test: this test.
    #[test]
    fn exit_codes_are_distinct() {
        assert_eq!(ExitCode::Success.code(), 0);
        assert_eq!(ExitCode::ConfigError.code(), 2);
        assert_eq!(ExitCode::RunFailure.code(), 3);
        assert_eq!(ExitCode::NoChanges.code(), 4);
        assert_eq!(ExitCode::DeadlineExceeded.code(), 5);
        assert_eq!(ExitCode::Partial.code(), 6);
    }

    /// The `"partial"` status string renders for `ExitCode::Partial` (#2265).
    ///
    /// Why: Downstream callers (the bake-off harness) branch on this exact
    /// string; it must render distinctly from `"run_failure"`.
    /// What: Render a report with `exit: ExitCode::Partial`; assert the JSON
    /// `status` field and the human summary line both say `"partial"`.
    /// Test: this test.
    #[test]
    fn partial_status_renders_distinctly_from_run_failure() {
        let report = sample_report(ExitCode::Partial, "+++ added: x.py\n");
        let rendered = report.render_json();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
        assert_eq!(parsed["status"], "partial");
        assert_eq!(parsed["exit_code"], 6);
        assert!(report.render_human().contains("status=partial"));
    }

    /// JSON render carries status, diff, transcript, usage, and cost.
    ///
    /// Why: `--json` consumers depend on this exact shape.
    /// What: Render a success report; parse it back; assert key fields.
    /// Test: this test.
    #[test]
    fn report_renders_json() {
        let report = sample_report(ExitCode::Success, "+++ added: x.py\n");
        let rendered = report.render_json();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");

        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["exit_code"], 0);
        assert_eq!(parsed["agent"], "pm");
        assert!(parsed["diff"].as_str().unwrap().contains("added: x.py"));
        assert_eq!(parsed["transcript"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["usage"]["prompt_tokens"], 100);
        assert_eq!(parsed["cost_usd"], 0.001);
        // The engineer turn must carry its own model slug for #1035 assertions.
        assert_eq!(parsed["transcript"][1]["model"], "deepseek/deepseek-chat");
    }

    /// Human render summarises transcript, diff, and usage.
    ///
    /// Why: Operators read this; key facts must be present.
    /// What: Render a success report; assert role labels, the diff, and usage line.
    /// Test: this test.
    #[test]
    fn report_renders_human() {
        let report = sample_report(ExitCode::Success, "+++ added: x.py\n");
        let text = report.render_human();
        assert!(text.contains("status=success"));
        assert!(text.contains("[pm|openai/gpt-4o-mini]"));
        assert!(text.contains("[python-engineer|deepseek/deepseek-chat]"));
        assert!(text.contains("added: x.py"));
        assert!(text.contains("prompt=100 completion=40"));
    }

    /// A no-changes report renders "(no changes)" in the human form.
    ///
    /// Why: The no-op outcome must be visible to a human reader.
    /// What: Render with an empty diff; assert the marker.
    /// Test: this test.
    #[test]
    fn human_render_shows_no_changes() {
        let report = sample_report(ExitCode::NoChanges, "");
        let text = report.render_human();
        assert!(text.contains("(no changes)"), "text: {text}");
        assert!(text.contains("status=no_changes"));
    }

    /// `aggregate_usage_per_role` sums every turn's usage and prices the total.
    ///
    /// Why: The run cost is the combined cost across PM + engineer turns; the math
    /// must be exact and span the whole transcript.
    /// What: Sum two turns' usages (same model for both roles here — the
    /// per-role split itself is covered by
    /// `aggregate_usage_per_role_prices_each_role_separately` below), assert
    /// field-wise totals and a non-negative cost.
    /// Test: this test.
    #[test]
    fn usage_and_cost_aggregate() {
        let turns = vec![
            TurnRecord {
                role: "pm".into(),
                model: "openai/gpt-4o-mini".into(),
                text: String::new(),
                tool_calls: vec![],
                usage: TokenUsage::new(10, 5, 0, 0),
            },
            TurnRecord {
                role: "python-engineer".into(),
                model: "openai/gpt-4o-mini".into(),
                text: "done".into(),
                tool_calls: vec![],
                usage: TokenUsage::new(20, 8, 0, 0),
            },
        ];
        let (total, cost) =
            aggregate_usage_per_role(&turns, "openai/gpt-4o-mini", "openai/gpt-4o-mini");
        assert_eq!(total.prompt_tokens, 30);
        assert_eq!(total.completion_tokens, 13);
        assert!(cost >= 0.0, "cost must be non-negative");
    }

    /// `aggregate_usage_per_role` must price the PM's and the engineer's
    /// turns against their OWN resolved models — the #1475 bug 1 fix.
    ///
    /// Why: When the engineer routes to a different (here, deliberately
    /// pricier) model than the PM, pricing the whole transcript against the
    /// PM model alone would be wrong by construction; this pins the fix.
    /// What: One PM turn priced at a cheap model, one engineer turn priced
    /// at an expensive model; assert the per-role total differs from (and
    /// exceeds) what blending everything under the PM model would produce.
    /// Test: this test.
    #[test]
    fn aggregate_usage_per_role_prices_each_role_separately() {
        let turns = vec![
            TurnRecord {
                role: "pm".into(),
                model: "anthropic/claude-haiku-4".into(),
                text: String::new(),
                tool_calls: vec!["delegate_to_agent".into()],
                usage: TokenUsage::new(1000, 500, 0, 0),
            },
            TurnRecord {
                role: "python-engineer".into(),
                model: "anthropic/claude-sonnet-4-5".into(),
                text: "done".into(),
                tool_calls: vec![],
                usage: TokenUsage::new(2000, 1000, 0, 0),
            },
        ];
        let (_, per_role_cost) = aggregate_usage_per_role(
            &turns,
            "anthropic/claude-haiku-4",
            "anthropic/claude-sonnet-4-5",
        );
        // Blending everything under the PM (haiku, cheaper) model would
        // understate the true cost since the engineer's turn actually ran
        // on the pricier sonnet model.
        let blended_cost = crate::perf::cost_usd("anthropic/claude-haiku-4", 3000, 1500, 0, 0);
        assert_ne!(
            per_role_cost, blended_cost,
            "per-role pricing must differ from blending everything under the PM model"
        );
    }

    /// `aggregate_usage_per_role` prefers a turn's own authoritative
    /// `usage.cost_usd` over the static per-token recompute (response-side
    /// cache-usage fix).
    ///
    /// Why: The static pricing table cannot see OpenRouter's cache discount;
    /// only the provider-reported `cost` reflects it. This is the exact gap
    /// that made #2156's cost thesis unverifiable end-to-end.
    /// What: One turn carries an authoritative cost that is deliberately far
    /// below what static pricing would compute for its token counts; one turn
    /// carries no authoritative cost (static fallback). Assert the total cost
    /// is exactly the authoritative value plus the static fallback for the
    /// second turn — i.e. the first turn's static price is NOT used.
    /// Test: this test.
    #[test]
    fn aggregate_usage_per_role_prefers_authoritative_cost() {
        let mut authoritative_usage = TokenUsage::new(1_000_000, 0, 900_000, 100_000);
        authoritative_usage.cost_usd = Some(0.0005); // far below static per-token price
        let turns = vec![
            TurnRecord {
                role: "pm".into(),
                model: "anthropic/claude-sonnet-4-5".into(),
                text: String::new(),
                tool_calls: vec![],
                usage: authoritative_usage,
            },
            TurnRecord {
                role: "python-engineer".into(),
                model: "anthropic/claude-sonnet-4-5".into(),
                text: "done".into(),
                tool_calls: vec![],
                usage: TokenUsage::new(100, 50, 0, 0),
            },
        ];
        let (_, cost) = aggregate_usage_per_role(
            &turns,
            "anthropic/claude-sonnet-4-5",
            "anthropic/claude-sonnet-4-5",
        );
        let expected_fallback = crate::perf::cost_usd("anthropic/claude-sonnet-4-5", 100, 50, 0, 0);
        assert!(
            (cost - (0.0005 + expected_fallback)).abs() < 1e-12,
            "expected authoritative cost + static fallback, got {cost}"
        );
    }
}
