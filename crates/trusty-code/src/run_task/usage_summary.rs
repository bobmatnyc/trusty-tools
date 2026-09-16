//! The `turns`/`usage`/`cost_usd`/`usage_by_role` block every `run-task`
//! report carries, on both the daemon and the legacy in-process path (#8155).
//!
//! Why: `tcode run-task --json` printed a full [`crate::run_task::RunReport`]
//! on the `--legacy-in-process` path and a bare `Session` snapshot on the
//! DEFAULT daemon path, so one command reported per-run token usage and cost
//! through one path and nothing at all through the other — the #8127 parity
//! runner had to fall back to `--legacy-in-process` just to get a cost figure.
//! One shape, built once here, is what lets a single parser read either
//! document instead of two.
//! What: [`RunUsageReport`] carries `turns` (how many turns were recorded),
//! `usage` (the four token counters), `cost_usd` (the run total, `None` only
//! when no priced turn exists) and `usage_by_role` — the PM-versus-delegated-
//! agent split neither path reported before. [`RoleUsage`] is one role's
//! slice, summed from turns priced INDIVIDUALLY: the provider's own
//! authoritative `usage.cost_usd` when the turn carries one, otherwise
//! [`crate::perf::cost_usd`] against that turn's own resolved model slug (the
//! slug `RecordingLlmClient` recorded off the response — #1475 bug 2 — which
//! is why this needs no role-to-model table of its own, unlike
//! [`crate::run_task::aggregate_usage_per_role`]). [`TokenCounts`] is the
//! four-counter projection of [`crate::perf::TokenUsage`], matching the
//! `usage` object `RunReport::render_json` has emitted since #1034 — it drops
//! `TokenUsage::cost_usd`, which is a PER-CALL provider figure with no
//! meaning on an aggregate.
//! Test: `run_task::usage_summary::tests::*`,
//! `tests/cli_e2e.rs::run_task_json_report_carries_turns_usage_and_cost`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::perf::TokenUsage;
use crate::run_task::TurnRecord;

/// The four token counters a run report's `usage` object exposes.
///
/// Why: [`TokenUsage`] additionally carries a per-call `cost_usd` that is
/// meaningless once summed across turns, and the legacy JSON report has always
/// emitted exactly these four keys. Projecting keeps the daemon path's `usage`
/// byte-identical to the legacy path's rather than near-identical.
/// What: field-for-field copy of [`TokenUsage`]'s counters, in the same order
/// the legacy report emitted them. `Deserialize` lets a consumer parse either
/// document back.
/// Test: `tests::token_counts_project_the_four_counters`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCounts {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
}

impl From<&TokenUsage> for TokenCounts {
    fn from(usage: &TokenUsage) -> Self {
        Self {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_creation_tokens: usage.cache_creation_tokens,
        }
    }
}

impl TokenCounts {
    /// Add another turn's counters into this one, saturating.
    ///
    /// Why: mirrors [`TokenUsage::add`]'s saturating contract so a pathological
    /// provider count can never wrap a per-role subtotal.
    /// Test: `tests::role_split_sums_each_roles_turns`.
    fn add(&mut self, other: &TokenUsage) {
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
    }
}

/// One role's slice of a run — the PM, or a delegated agent.
///
/// Why: a run's single total cannot answer "what did the delegation cost",
/// which is the #8127 bake-off's actual question. `role` is the transcript
/// label (`"pm"`, `"python-engineer"`, …), so the split follows whatever roles
/// the run really used rather than a hardcoded two.
/// What: `turns` counts this role's recorded turns; `usage` sums their
/// counters; `cost_usd` sums their per-turn costs. `cost_usd` is a plain
/// `f64`, not an `Option`: a role only appears here because it has at least
/// one turn, and every turn prices (see [`turn_cost_usd`]).
/// Test: `tests::role_split_sums_each_roles_turns`,
/// `tests::role_split_keeps_first_appearance_order`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleUsage {
    pub role: String,
    pub turns: usize,
    pub usage: TokenCounts,
    pub cost_usd: f64,
}

/// The usage/cost block a `run-task` report carries, on either path.
///
/// Why/What: see the module docs.
/// Test: `tests::report_serialises_the_documented_keys`,
/// `tests::report_of_an_empty_run_is_zeroed_with_a_null_cost`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunUsageReport {
    /// How many turns the run recorded across every role.
    pub turns: usize,
    /// Token counters summed over the whole run.
    pub usage: TokenCounts,
    /// The run's total USD cost, `None` when nothing priced it.
    pub cost_usd: Option<f64>,
    /// Per-role split, in first-appearance order.
    pub usage_by_role: Vec<RoleUsage>,
}

impl RunUsageReport {
    /// Build the block from a stored run record.
    ///
    /// Why: the daemon already aggregates `usage`/`cost_usd` when it persists
    /// the run (`task::executor::run_and_record` ->
    /// `SessionRegistry::set_run_outcome`), and the legacy path already has
    /// them on its `RunReport`. Recomputing either here would create a second
    /// costing implementation that could drift from
    /// [`crate::run_task::aggregate_usage_per_role`]; this takes them as given
    /// and derives ONLY what neither producer had — the turn count and the
    /// per-role split.
    /// What: `turns` is `turns.len()`; `usage`/`cost_usd` are passed through
    /// verbatim; `usage_by_role` comes from [`usage_by_role`].
    /// Test: `tests::report_passes_through_the_stored_usage_and_cost`,
    /// `tests::report_of_an_empty_run_is_zeroed_with_a_null_cost`.
    pub fn from_record(turns: &[TurnRecord], usage: &TokenUsage, cost_usd: Option<f64>) -> Self {
        Self {
            turns: turns.len(),
            usage: TokenCounts::from(usage),
            cost_usd,
            usage_by_role: usage_by_role(turns),
        }
    }

    /// Merge this block — plus the transcript itself — into a `session.status`
    /// snapshot object.
    ///
    /// Why: #8155's daemon-path fix prints the session snapshot WITH the run
    /// record attached, rather than replacing one document with another, so
    /// every pre-existing `run-task --json` consumer keeps reading the same
    /// `id`/`status`/`result` keys it always did.
    /// What: inserts `turns`, `usage`, `cost_usd`, `usage_by_role` and
    /// `transcript` into `snapshot`. `transcript` carries the same
    /// `Vec<TurnRecord>` shape under the same key the legacy `RunReport` JSON
    /// uses, so one parser reads both. A serialisation failure on any value is
    /// impossible for these plain types, so a `Value::Null` fallback is used
    /// rather than propagating an error no caller could act on.
    /// Test: `tests::merge_adds_the_report_keys_without_touching_the_session`.
    pub fn merge_into_snapshot(
        &self,
        snapshot: &mut Map<String, Value>,
        transcript: &[TurnRecord],
    ) {
        snapshot.insert("turns".to_string(), Value::from(self.turns));
        snapshot.insert(
            "usage".to_string(),
            serde_json::to_value(self.usage).unwrap_or(Value::Null),
        );
        snapshot.insert(
            "cost_usd".to_string(),
            serde_json::to_value(self.cost_usd).unwrap_or(Value::Null),
        );
        snapshot.insert(
            "usage_by_role".to_string(),
            serde_json::to_value(&self.usage_by_role).unwrap_or(Value::Null),
        );
        snapshot.insert(
            "transcript".to_string(),
            serde_json::to_value(transcript).unwrap_or(Value::Null),
        );
    }

    /// Render the block as the human (non-`--json`) footer lines.
    ///
    /// Why: the daemon path's human output reported only `session=… status=…`,
    /// so an operator watching a real run saw no cost at all — the same gap as
    /// the JSON one, and fixed in the same place.
    /// What: one summary line, then one indented line per role. The vocabulary
    /// (`prompt=`, `completion=`, `cache_read=`, `cache_creation=`, `cost=`)
    /// and the `(pricing unavailable)` placeholder match
    /// `RunReport::render_human`'s existing footer. No trailing newline — the
    /// caller decides.
    /// Test: `tests::human_render_names_turns_usage_and_each_role`,
    /// `tests::human_render_says_pricing_unavailable_for_a_null_cost`.
    pub fn render_human(&self) -> String {
        let mut out = format!(
            "turns={} prompt={} completion={} cache_read={} cache_creation={} cost={}",
            self.turns,
            self.usage.prompt_tokens,
            self.usage.completion_tokens,
            self.usage.cache_read_tokens,
            self.usage.cache_creation_tokens,
            format_cost(self.cost_usd),
        );
        for role in &self.usage_by_role {
            out.push_str(&format!(
                "\n  {}: turns={} prompt={} completion={} cost={}",
                role.role,
                role.turns,
                role.usage.prompt_tokens,
                role.usage.completion_tokens,
                format_cost(Some(role.cost_usd)),
            ));
        }
        out
    }
}

/// Format a USD cost the way both report surfaces already do.
///
/// Why: `RunReport::render_human` chose `$0.000000`/`(pricing unavailable)` in
/// #1034; the daemon path must not invent a second spelling.
/// Test: `tests::human_render_says_pricing_unavailable_for_a_null_cost`.
fn format_cost(cost: Option<f64>) -> String {
    match cost {
        Some(c) => format!("${c:.6}"),
        None => "(pricing unavailable)".to_string(),
    }
}

/// Split a transcript into per-role usage and cost subtotals.
///
/// Why: the PM-versus-delegated-agent breakdown #8155 asks for. Grouping is by
/// the transcript's own `role` label, so a run that delegates to three
/// different agents reports three rows with no code change.
/// What: walks the turns in order, accumulating into the row for each turn's
/// role and appending a new row the first time a role is seen — so the result
/// is in first-appearance order, stable across runs, with no map iteration
/// order to depend on. Each turn's cost comes from [`turn_cost_usd`].
/// Test: `tests::role_split_sums_each_roles_turns`,
/// `tests::role_split_keeps_first_appearance_order`,
/// `tests::role_split_prefers_the_authoritative_per_turn_cost`.
pub fn usage_by_role(turns: &[TurnRecord]) -> Vec<RoleUsage> {
    let mut rows: Vec<RoleUsage> = Vec::new();
    for turn in turns {
        let cost = turn_cost_usd(turn);
        match rows.iter_mut().find(|row| row.role == turn.role) {
            Some(row) => {
                row.turns += 1;
                row.usage.add(&turn.usage);
                row.cost_usd += cost;
            }
            None => rows.push(RoleUsage {
                role: turn.role.clone(),
                turns: 1,
                usage: TokenCounts::from(&turn.usage),
                cost_usd: cost,
            }),
        }
    }
    rows
}

/// Price one recorded turn.
///
/// Why: the same two-tier rule
/// [`crate::run_task::aggregate_usage_per_role`] applies to the run total —
/// the provider's authoritative figure first, static pricing only as the
/// fallback — so the split and the total agree.
/// What: `turn.usage.cost_usd` when the provider reported one (OpenRouter's
/// already-cache-discounted number), otherwise [`crate::perf::cost_usd`]
/// against `turn.model`. That field is the model the RESPONSE resolved to
/// (#1475 bug 2), which is why this prices per turn rather than per role:
/// where a provider routes a turn to a different slug than was requested,
/// this bills the slug that actually ran it.
/// Test: `tests::role_split_prefers_the_authoritative_per_turn_cost`,
/// `tests::role_split_falls_back_to_static_pricing_for_the_turns_own_model`.
fn turn_cost_usd(turn: &TurnRecord) -> f64 {
    turn.usage.cost_usd.unwrap_or_else(|| {
        crate::perf::cost_usd(
            &turn.model,
            turn.usage.prompt_tokens,
            turn.usage.completion_tokens,
            turn.usage.cache_read_tokens,
            turn.usage.cache_creation_tokens,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A turn with the given role, model and counters; no authoritative cost.
    fn turn(role: &str, model: &str, prompt: u32, completion: u32) -> TurnRecord {
        TurnRecord {
            role: role.to_string(),
            model: model.to_string(),
            text: String::new(),
            tool_calls: vec![],
            ran_test_command: false,
            usage: TokenUsage::new(prompt, completion, 0, 0),
        }
    }

    /// The projection drops `TokenUsage::cost_usd` and keeps the four counters
    /// under the key names the legacy JSON report has always used.
    #[test]
    fn token_counts_project_the_four_counters() {
        let mut usage = TokenUsage::new(1, 2, 3, 4);
        usage.cost_usd = Some(9.0);
        let value = serde_json::to_value(TokenCounts::from(&usage)).expect("serialize");
        assert_eq!(
            value,
            serde_json::json!({
                "prompt_tokens": 1,
                "completion_tokens": 2,
                "cache_read_tokens": 3,
                "cache_creation_tokens": 4,
            }),
            "the usage object must carry exactly the legacy four keys: {value}"
        );
    }

    /// Each role's row sums only its own turns.
    #[test]
    fn role_split_sums_each_roles_turns() {
        let turns = vec![
            turn("pm", "anthropic/claude-sonnet-5", 100, 10),
            turn("python-engineer", "anthropic/claude-haiku-4.5", 40, 5),
            turn("pm", "anthropic/claude-sonnet-5", 20, 3),
        ];
        let rows = usage_by_role(&turns);
        assert_eq!(rows.len(), 2, "expected one row per role: {rows:?}");

        let pm = &rows[0];
        assert_eq!(pm.role, "pm");
        assert_eq!(pm.turns, 2);
        assert_eq!(pm.usage.prompt_tokens, 120);
        assert_eq!(pm.usage.completion_tokens, 13);

        let engineer = &rows[1];
        assert_eq!(engineer.role, "python-engineer");
        assert_eq!(engineer.turns, 1);
        assert_eq!(engineer.usage.prompt_tokens, 40);
    }

    /// Rows come back in first-appearance order, not an arbitrary map order —
    /// the property that keeps the emitted JSON stable across runs.
    #[test]
    fn role_split_keeps_first_appearance_order() {
        let turns = vec![
            turn("python-engineer", "m", 1, 1),
            turn("pm", "m", 1, 1),
            turn("reviewer", "m", 1, 1),
            turn("pm", "m", 1, 1),
        ];
        let rows = usage_by_role(&turns);
        let roles: Vec<&str> = rows.iter().map(|r| r.role.as_str()).collect();
        assert_eq!(roles, vec!["python-engineer", "pm", "reviewer"]);
    }

    /// #8155: the provider's own per-turn cost wins over static pricing, and
    /// the two are summed together when a run mixes both.
    #[test]
    fn role_split_prefers_the_authoritative_per_turn_cost() {
        let mut authoritative = turn("pm", "anthropic/claude-opus-5", 1_000_000, 0);
        authoritative.usage.cost_usd = Some(0.25);
        let rows = usage_by_role(&[authoritative]);

        assert_eq!(rows.len(), 1);
        assert!(
            (rows[0].cost_usd - 0.25).abs() < f64::EPSILON,
            "the authoritative cost must be used verbatim, not recomputed: {}",
            rows[0].cost_usd
        );
    }

    /// With no authoritative cost, the turn is priced against ITS OWN model
    /// slug — asserted against `perf::cost_usd` rather than a literal, so a
    /// pricing-table change cannot make this test lie.
    #[test]
    fn role_split_falls_back_to_static_pricing_for_the_turns_own_model() {
        let engineer_turn = turn("python-engineer", "anthropic/claude-haiku-4.5", 1000, 100);
        let expected = crate::perf::cost_usd("anthropic/claude-haiku-4.5", 1000, 100, 0, 0);

        let rows = usage_by_role(&[engineer_turn]);
        assert!(
            (rows[0].cost_usd - expected).abs() < f64::EPSILON,
            "expected {expected}, got {}",
            rows[0].cost_usd
        );
        assert!(expected > 0.0, "the fixture must price to something");
    }

    /// The stored `usage`/`cost_usd` are passed through untouched — this block
    /// never re-prices what the producer already aggregated.
    #[test]
    fn report_passes_through_the_stored_usage_and_cost() {
        let turns = vec![turn("pm", "anthropic/claude-sonnet-5", 10, 2)];
        // A deliberately "wrong" total: if `from_record` recomputed, this would
        // not survive.
        let report =
            RunUsageReport::from_record(&turns, &TokenUsage::new(999, 888, 7, 6), Some(42.0));

        assert_eq!(report.turns, 1);
        assert_eq!(report.usage.prompt_tokens, 999);
        assert_eq!(report.usage.completion_tokens, 888);
        assert_eq!(report.cost_usd, Some(42.0));
        assert_eq!(report.usage_by_role.len(), 1);
    }

    /// A session that never ran reports zeros, no roles, and a null cost.
    #[test]
    fn report_of_an_empty_run_is_zeroed_with_a_null_cost() {
        let report = RunUsageReport::from_record(&[], &TokenUsage::default(), None);
        let value = serde_json::to_value(&report).expect("serialize");
        assert_eq!(value["turns"], 0);
        assert_eq!(value["usage"]["prompt_tokens"], 0);
        assert!(value["cost_usd"].is_null());
        assert_eq!(value["usage_by_role"], serde_json::json!([]));
    }

    /// The serialized block carries exactly the documented keys, which is the
    /// wire contract `tests/cli_e2e.rs` asserts end-to-end.
    #[test]
    fn report_serialises_the_documented_keys() {
        let turns = vec![turn("pm", "anthropic/claude-sonnet-5", 10, 2)];
        let report = RunUsageReport::from_record(&turns, &TokenUsage::new(10, 2, 0, 0), Some(0.5));
        let value = serde_json::to_value(&report).expect("serialize");
        let obj = value.as_object().expect("object");

        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["cost_usd", "turns", "usage", "usage_by_role"]);
        assert_eq!(value["usage_by_role"][0]["role"], "pm");
        assert_eq!(value["usage_by_role"][0]["turns"], 1);

        let back: RunUsageReport = serde_json::from_value(value).expect("round trip");
        assert_eq!(back, report);
    }

    /// Merging adds the five report keys and leaves the session snapshot's own
    /// keys exactly as they were.
    #[test]
    fn merge_adds_the_report_keys_without_touching_the_session() {
        let turns = vec![turn("pm", "anthropic/claude-sonnet-5", 10, 2)];
        let report = RunUsageReport::from_record(&turns, &TokenUsage::new(10, 2, 0, 0), Some(0.5));

        let mut snapshot = serde_json::json!({"id": "s-1", "status": "finished"});
        let obj = snapshot.as_object_mut().expect("object");
        report.merge_into_snapshot(obj, &turns);

        assert_eq!(snapshot["id"], "s-1", "the session keys must survive");
        assert_eq!(snapshot["status"], "finished");
        assert_eq!(snapshot["turns"], 1);
        assert_eq!(snapshot["usage"]["prompt_tokens"], 10);
        assert_eq!(snapshot["cost_usd"], 0.5);
        assert_eq!(snapshot["usage_by_role"][0]["role"], "pm");
        assert_eq!(
            snapshot["transcript"][0]["role"], "pm",
            "the embedded transcript is what makes the run inspectable after \
             the ephemeral daemon exits (#8155)"
        );
    }

    /// The human footer names the turn count, every counter, the total cost,
    /// and one line per role.
    #[test]
    fn human_render_names_turns_usage_and_each_role() {
        let turns = vec![
            turn("pm", "anthropic/claude-sonnet-5", 10, 2),
            turn("python-engineer", "anthropic/claude-haiku-4.5", 5, 1),
        ];
        let report = RunUsageReport::from_record(&turns, &TokenUsage::new(15, 3, 0, 0), Some(0.5));
        let rendered = report.render_human();

        assert!(rendered.contains("turns=2"), "{rendered}");
        assert!(rendered.contains("prompt=15"), "{rendered}");
        assert!(rendered.contains("completion=3"), "{rendered}");
        assert!(rendered.contains("cost=$0.500000"), "{rendered}");
        assert!(rendered.contains("\n  pm: turns=1"), "{rendered}");
        assert!(
            rendered.contains("\n  python-engineer: turns=1"),
            "{rendered}"
        );
    }

    /// A `None` cost renders the same placeholder the legacy footer uses, not
    /// a misleading `$0.000000`.
    #[test]
    fn human_render_says_pricing_unavailable_for_a_null_cost() {
        let report = RunUsageReport::from_record(&[], &TokenUsage::default(), None);
        assert!(
            report.render_human().contains("(pricing unavailable)"),
            "{}",
            report.render_human()
        );
    }
}
