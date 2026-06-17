//! Structured transcript schema for one metaharness orchestration run (#1030).
//!
//! Why: The orchestrator drives a PM → sub-agent delegation cycle and must hand
//! back a single, machine-readable record of what happened — both the PM's turns
//! and every sub-agent's turns, the token usage each accrued, and the file
//! artifacts the run produced. Acceptance criterion #1030 requires a *defined
//! transcript schema* that captures PM + subagent turns, both usages, and the
//! engineer's file changes; centralising that schema here keeps the orchestrator
//! focused on wiring and gives downstream tooling a stable shape to parse.
//! What: [`MetaTranscript`] is the top-level record: a `model`, the PM
//! [`AgentTurn`], the ordered list of delegation [`AgentTurn`]s, the
//! [`UsageTotals`] rolled up across PM + sub-agents, and the [`Artifact`] list of
//! files observed under the project after the run. [`UsageTotals::from_token_usage`]
//! and [`MetaTranscript::rollup_usage`] keep the totals consistent. Everything is
//! `Serialize` so the orchestrator can persist it as JSON.
//! Test: `transcript::tests` cover usage rollup, turn capture, and JSON shape.

use serde::Serialize;
use trusty_code::perf::TokenUsage;
use trusty_code::tools::AgentOutput;

/// Token usage for a single actor or for the whole run.
///
/// Why: The PM and each sub-agent accrue tokens independently; the schema must
/// expose both the per-turn usage and the run-wide rollup so a comparison
/// harness can attribute cost. A small `Serialize` mirror of [`TokenUsage`]
/// keeps the JSON shape stable and decoupled from the upstream type.
/// What: prompt/completion/total token counts. [`from_token_usage`] copies from a
/// [`TokenUsage`]; [`add`] accumulates another `UsageTotals` for the rollup.
/// Test: `usage_totals_add_sums_fields`, `transcript_rolls_up_pm_and_subagents`.
///
/// [`from_token_usage`]: UsageTotals::from_token_usage
/// [`add`]: UsageTotals::add
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct UsageTotals {
    /// Prompt (input) tokens consumed.
    pub prompt_tokens: u32,
    /// Completion (output) tokens produced.
    pub completion_tokens: u32,
    /// Cache-read tokens (reused prompt cache), if the provider reports them.
    pub cache_read_tokens: u32,
    /// Cache-creation tokens (prompt cache writes), if the provider reports them.
    pub cache_creation_tokens: u32,
    /// Convenience sum of prompt + completion tokens.
    pub total_tokens: u32,
}

impl UsageTotals {
    /// Build a `UsageTotals` from a trusty-code [`TokenUsage`].
    ///
    /// Why: The agent loop reports usage as [`TokenUsage`] (which has no rolled
    /// total); the transcript schema needs an owned, `Serialize`-friendly copy
    /// plus a derived `total_tokens` so consumers do not have to re-add.
    /// What: Copies the four counters and computes `total = prompt + completion`.
    /// Test: `usage_totals_from_token_usage_copies_fields`.
    pub fn from_token_usage(usage: &TokenUsage) -> Self {
        Self {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_creation_tokens: usage.cache_creation_tokens,
            total_tokens: usage.prompt_tokens.saturating_add(usage.completion_tokens),
        }
    }

    /// Accumulate another `UsageTotals` into this one.
    ///
    /// Why: The run-wide rollup is the sum of the PM's usage and every
    /// sub-agent's usage; a single `add` keeps the summation in one place.
    /// What: Adds each counter field-wise (saturating to avoid overflow panics).
    /// Test: `usage_totals_add_sums_fields`.
    pub fn add(&mut self, other: &UsageTotals) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(other.cache_creation_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }
}

/// One actor's turn in the run — either the PM or a delegated sub-agent.
///
/// Why: A unified turn type lets the schema list the PM turn and each delegation
/// uniformly, so consumers iterate one shape regardless of role. Capturing the
/// `task` for delegations (and `None` for the PM, whose task is the run's prompt)
/// records exactly what each actor was asked to do.
/// What: `role` is the actor name (`"pm"` or the agent slug), `task` is the
/// delegated instruction (only present for sub-agents), `output` is the actor's
/// final text, and `usage` is the tokens that actor accrued.
/// Test: `agent_turn_from_pm_output_sets_role_pm`,
/// `agent_turn_for_delegation_carries_task`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentTurn {
    /// Actor name: `"pm"` for the project manager, else the sub-agent slug.
    pub role: String,
    /// The task this actor was delegated (sub-agents only; `None` for the PM).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The actor's final textual output.
    pub output: String,
    /// Tokens this actor accrued across its own loop.
    pub usage: UsageTotals,
}

impl AgentTurn {
    /// Build the PM turn from the PM loop's [`AgentOutput`].
    ///
    /// Why: The PM is the run's top-level actor; its turn has no delegated task
    /// (its "task" is the run prompt recorded at the transcript level).
    /// What: Sets `role = "pm"`, `task = None`, copies content + usage.
    /// Test: `agent_turn_from_pm_output_sets_role_pm`.
    pub fn from_pm_output(output: &AgentOutput) -> Self {
        Self {
            role: ROLE_PM.to_string(),
            task: None,
            output: output.content.clone(),
            usage: UsageTotals::from_token_usage(&output.usage),
        }
    }

    /// Build a sub-agent turn from a recorded delegation.
    ///
    /// Why: Each `delegate_to_agent` call produces a sub-agent turn the schema
    /// must expose alongside the task it was given.
    /// What: Sets `role = agent`, records `task`, copies content + usage.
    /// Test: `agent_turn_for_delegation_carries_task`.
    pub fn for_delegation(agent: &str, task: &str, output: &AgentOutput) -> Self {
        Self {
            role: agent.to_string(),
            task: Some(task.to_string()),
            output: output.content.clone(),
            usage: UsageTotals::from_token_usage(&output.usage),
        }
    }
}

/// A file artifact observed in the project working directory after the run.
///
/// Why: Acceptance criterion #1030 requires the engineer's file changes to be
/// *visible in the transcript*; recording each produced file (relative path +
/// byte length) makes the side effects auditable without re-reading the disk.
/// What: `path` is the path relative to the project root; `bytes` is the file's
/// size in bytes at capture time.
/// Test: `artifact_serializes_relative_path`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Artifact {
    /// Path relative to the project working directory.
    pub path: String,
    /// File size in bytes.
    pub bytes: u64,
}

/// Actor name stamped on the PM's turn.
///
/// Why: The `"pm"` role string is asserted by tests and parsed by tooling;
/// centralising it keeps the producer and consumers in lockstep.
/// What: the literal `"pm"`.
/// Test: `agent_turn_from_pm_output_sets_role_pm`.
pub(crate) const ROLE_PM: &str = "pm";

/// Schema version stamped into every emitted transcript.
///
/// Why: Downstream tooling must detect schema changes; a version token lets the
/// shape evolve without silently breaking parsers.
/// What: the current transcript schema version string.
/// Test: `transcript_json_carries_schema_version`.
pub(crate) const TRANSCRIPT_SCHEMA_VERSION: &str = "1.0.0";

/// Top-level record of one metaharness orchestration run (#1030).
///
/// Why: The orchestrator must return a *single combined transcript* spanning the
/// PM and every sub-agent, with usage rolled up and artifacts listed — the core
/// #1030 deliverable. Bundling these into one `Serialize` type gives the run a
/// stable persisted shape under `.trusty-mpm/meta-runs/`.
/// What: `schema_version` tags the shape; `model` is the slug the PM loop used;
/// `prompt` is the run's top-level task; `pm` is the PM turn; `delegations` are
/// the sub-agent turns in call order; `usage` is the PM+subagent rollup;
/// `artifacts` are the files observed after the run.
/// Test: `transcript_rolls_up_pm_and_subagents`,
/// `transcript_json_carries_schema_version`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MetaTranscript {
    /// Transcript schema version (see [`TRANSCRIPT_SCHEMA_VERSION`]).
    pub schema_version: String,
    /// Model slug the PM loop ran against.
    pub model: String,
    /// The run's top-level task prompt handed to the PM.
    pub prompt: String,
    /// The PM's turn.
    pub pm: AgentTurn,
    /// Sub-agent turns, in delegation order.
    pub delegations: Vec<AgentTurn>,
    /// Token usage rolled up across the PM and every sub-agent.
    pub usage: UsageTotals,
    /// File artifacts observed under the project after the run.
    pub artifacts: Vec<Artifact>,
}

impl MetaTranscript {
    /// Assemble a transcript from the PM turn, delegations, and artifacts.
    ///
    /// Why: One constructor that also computes the usage rollup guarantees the
    /// `usage` total always matches the parts — callers cannot forget to sum.
    /// What: Stamps the schema version, stores the inputs, and sets `usage` to the
    /// sum of the PM turn's usage and every delegation's usage via
    /// [`rollup_usage`].
    /// Test: `transcript_rolls_up_pm_and_subagents`.
    ///
    /// [`rollup_usage`]: MetaTranscript::rollup_usage
    pub fn assemble(
        model: impl Into<String>,
        prompt: impl Into<String>,
        pm: AgentTurn,
        delegations: Vec<AgentTurn>,
        artifacts: Vec<Artifact>,
    ) -> Self {
        let usage = Self::rollup_usage(&pm, &delegations);
        Self {
            schema_version: TRANSCRIPT_SCHEMA_VERSION.to_string(),
            model: model.into(),
            prompt: prompt.into(),
            pm,
            delegations,
            usage,
            artifacts,
        }
    }

    /// Sum the PM turn's usage with every delegation's usage.
    ///
    /// Why: The run-wide total is exactly the PM plus all sub-agents; factoring
    /// the summation out keeps it unit-testable in isolation.
    /// What: Starts from the PM's usage and folds in each delegation's usage.
    /// Test: `transcript_rolls_up_pm_and_subagents`.
    fn rollup_usage(pm: &AgentTurn, delegations: &[AgentTurn]) -> UsageTotals {
        let mut total = pm.usage.clone();
        for turn in delegations {
            total.add(&turn.usage);
        }
        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `TokenUsage` fixture with explicit prompt/completion counters.
    fn usage(p: u32, c: u32) -> TokenUsage {
        TokenUsage::new(p, c, 0, 0)
    }

    /// `from_token_usage` copies all three counters verbatim.
    ///
    /// Why: Guards the schema-mirror conversion against field drift.
    /// What: Convert a usage fixture; assert each field matches.
    /// Test: this test.
    #[test]
    fn usage_totals_from_token_usage_copies_fields() {
        let totals = UsageTotals::from_token_usage(&usage(30, 10));
        assert_eq!(totals.prompt_tokens, 30);
        assert_eq!(totals.completion_tokens, 10);
        assert_eq!(totals.total_tokens, 40);
    }

    /// `add` sums each counter field-wise.
    ///
    /// Why: The rollup depends on correct accumulation.
    /// What: Add two totals; assert each field is the sum.
    /// Test: this test.
    #[test]
    fn usage_totals_add_sums_fields() {
        let mut a = UsageTotals::from_token_usage(&usage(30, 10));
        a.add(&UsageTotals::from_token_usage(&usage(15, 5)));
        assert_eq!(a.prompt_tokens, 45);
        assert_eq!(a.completion_tokens, 15);
        assert_eq!(a.total_tokens, 60);
    }

    /// The PM turn carries `role = "pm"` and no task.
    ///
    /// Why: Consumers key the PM turn off its role; `task` is `None` for the PM.
    /// What: Build a PM turn from an output; assert role and absent task.
    /// Test: this test.
    #[test]
    fn agent_turn_from_pm_output_sets_role_pm() {
        let out = AgentOutput {
            content: "pm did its job".to_string(),
            summary: None,
            usage: usage(30, 10),
        };
        let turn = AgentTurn::from_pm_output(&out);
        assert_eq!(turn.role, ROLE_PM);
        assert!(turn.task.is_none());
        assert_eq!(turn.output, "pm did its job");
        assert_eq!(turn.usage.total_tokens, 40);
    }

    /// A delegation turn records the agent slug and the delegated task.
    ///
    /// Why: The schema must show what each sub-agent was asked to do.
    /// What: Build a delegation turn; assert role, task, output, usage.
    /// Test: this test.
    #[test]
    fn agent_turn_for_delegation_carries_task() {
        let out = AgentOutput {
            content: "engineer wrote the file".to_string(),
            summary: None,
            usage: usage(15, 5),
        };
        let turn = AgentTurn::for_delegation("python-engineer", "write hello.txt", &out);
        assert_eq!(turn.role, "python-engineer");
        assert_eq!(turn.task.as_deref(), Some("write hello.txt"));
        assert_eq!(turn.usage.total_tokens, 20);
    }

    /// The transcript rolls PM + sub-agent usage into the run-wide total.
    ///
    /// Why: This is the #1030 "both usages captured" criterion at the schema
    /// level — the total must equal PM plus every delegation.
    /// What: Assemble a transcript with a PM turn (30/10) and one delegation
    /// (15/5); assert the rolled-up total is 45/15/60.
    /// Test: this test.
    #[test]
    fn transcript_rolls_up_pm_and_subagents() {
        let pm = AgentTurn::from_pm_output(&AgentOutput {
            content: "pm".to_string(),
            summary: None,
            usage: usage(30, 10),
        });
        let eng = AgentTurn::for_delegation(
            "python-engineer",
            "task",
            &AgentOutput {
                content: "eng".to_string(),
                summary: None,
                usage: usage(15, 5),
            },
        );
        let t = MetaTranscript::assemble("openai/gpt-4o-mini", "prompt", pm, vec![eng], vec![]);
        assert_eq!(t.usage.prompt_tokens, 45);
        assert_eq!(t.usage.completion_tokens, 15);
        assert_eq!(t.usage.total_tokens, 60);
        assert_eq!(t.delegations.len(), 1);
    }

    /// An artifact serializes with its relative path and byte length.
    ///
    /// Why: File changes must be visible in the transcript JSON.
    /// What: Serialize an artifact; assert the JSON fields.
    /// Test: this test.
    #[test]
    fn artifact_serializes_relative_path() {
        let art = Artifact {
            path: "hello_metaharness.txt".to_string(),
            bytes: 12,
        };
        let v = serde_json::to_value(&art).expect("serialize artifact");
        assert_eq!(v["path"], "hello_metaharness.txt");
        assert_eq!(v["bytes"], 12);
    }

    /// The emitted transcript JSON carries the schema version token.
    ///
    /// Why: Tooling detects schema changes via this field.
    /// What: Assemble + serialize; assert `schema_version` is present.
    /// Test: this test.
    #[test]
    fn transcript_json_carries_schema_version() {
        let pm = AgentTurn::from_pm_output(&AgentOutput {
            content: "pm".to_string(),
            summary: None,
            usage: usage(1, 1),
        });
        let t = MetaTranscript::assemble("m", "p", pm, vec![], vec![]);
        let v = serde_json::to_value(&t).expect("serialize transcript");
        assert_eq!(v["schema_version"], TRANSCRIPT_SCHEMA_VERSION);
        assert_eq!(v["model"], "m");
        assert_eq!(v["prompt"], "p");
    }
}
