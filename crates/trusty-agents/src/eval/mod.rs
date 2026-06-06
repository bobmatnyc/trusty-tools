//! Behavior eval framework (#449).
//!
//! Why: We need a repeatable way to verify that agents continue to do the
//! right thing — refuse RBAC-restricted requests, route to the right tools,
//! gracefully say "I don't know" — without re-running the harness manually
//! after every prompt or model change.
//! What: A TOML-driven suite of `EvalCase`s; each case runs a single LLM
//! turn against a target system prompt and asserts on the response text +
//! tool-call names. Drives a `LlmClient` trait object so unit tests inject
//! deterministic mocks and `om eval run` plugs in the live OpenRouter client.
//! Test: `eval_case_from_toml_parses_minimal`, `eval_case_pass_when_all_match`,
//! `eval_case_fail_on_missing_phrase`, `eval_case_fail_on_forbidden_phrase`,
//! `eval_case_fail_on_unexpected_tool_call`, `eval_suite_runs_all_cases`.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::compress::session::LlmClient;

/// A single eval case as declared in TOML.
///
/// Why: Eval declarations are static + reviewable; one struct per case keeps
/// the file format and the in-memory representation in lockstep.
/// What: All assertions are optional except `id`/`input`; empty assertion
/// vectors are simply skipped during scoring.
/// Test: `eval_case_from_toml_parses_minimal`.
#[derive(Debug, Clone, Deserialize)]
pub struct EvalCase {
    pub id: String,
    #[serde(default)]
    pub description: String,
    pub input: String,
    #[serde(default)]
    pub user_tier: Option<String>,
    #[serde(default)]
    pub expected_tool_calls: Vec<String>,
    #[serde(default)]
    pub response_must_contain: Vec<String>,
    #[serde(default)]
    pub response_must_not_contain: Vec<String>,
}

/// Result of running one eval case.
///
/// Why: Both the human-readable report and the `--json` emission need the
/// same record; failures are stored as a list of strings rather than a
/// single bool so the report can show every assertion miss.
/// What: `passed` is false iff `failures` is non-empty.
/// Test: `eval_case_pass_when_all_match` and friends.
#[derive(Debug, Clone, Serialize)]
pub struct EvalResult {
    pub id: String,
    pub passed: bool,
    pub failures: Vec<String>,
    pub tool_calls_made: Vec<String>,
    pub response: String,
    pub latency_ms: u64,
}

/// Wrapper deserialized from the TOML file (`[[eval]]` array of tables).
#[derive(Debug, Clone, Deserialize)]
struct EvalSuiteToml {
    #[serde(default, rename = "eval")]
    cases: Vec<EvalCase>,
}

/// A loaded suite of eval cases.
#[derive(Debug, Clone)]
pub struct EvalSuite {
    pub cases: Vec<EvalCase>,
}

/// Extended LLM client that can also report tool calls.
///
/// Why: The base `LlmClient` trait (reused from `compress::session`) only
/// returns text. Eval cases need to assert which tools the model attempted
/// to call, so we extend it with `complete_with_tools`. Implementations that
/// don't model tools return an empty vec.
/// What: Async method returning `(response_text, tool_call_names)`.
/// Test: `MockEvalLlm` below; eval tests use it directly.
#[async_trait]
pub trait EvalLlmClient: Send + Sync {
    async fn complete_with_tools(
        &self,
        system: &str,
        user: &str,
        user_tier: Option<&str>,
    ) -> Result<(String, Vec<String>)>;
}

/// Adapter: anything implementing the simpler `LlmClient` can serve as an
/// `EvalLlmClient` with no tool-call awareness.
///
/// Why: Keeps the eval framework usable with the same mocks the compressor
/// tests already build.
pub struct TextOnlyEvalClient<T: LlmClient + ?Sized>(pub std::sync::Arc<T>);

#[async_trait]
impl<T: LlmClient + ?Sized> EvalLlmClient for TextOnlyEvalClient<T> {
    async fn complete_with_tools(
        &self,
        system: &str,
        user: &str,
        _user_tier: Option<&str>,
    ) -> Result<(String, Vec<String>)> {
        let text = self.0.complete(system, user, None).await?;
        Ok((text, Vec::new()))
    }
}

impl EvalSuite {
    /// Why: One call site for `om eval run --suite path.toml` and tests alike.
    /// What: Reads the file, deserializes `[[eval]]` blocks, validates that
    /// at least one case is present.
    /// Test: `eval_case_from_toml_parses_minimal`,
    /// `eval_suite_from_toml_rejects_empty`.
    pub fn from_toml(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading eval suite {}", path.display()))?;
        Self::from_toml_str(&raw)
    }

    pub fn from_toml_str(raw: &str) -> Result<Self> {
        let parsed: EvalSuiteToml = toml::from_str(raw).context("parsing eval suite TOML")?;
        if parsed.cases.is_empty() {
            anyhow::bail!("eval suite contains no [[eval]] cases");
        }
        Ok(Self {
            cases: parsed.cases,
        })
    }

    /// Run every case against the given client + system prompt.
    ///
    /// Why: Tests can pass a deterministic mock; the CLI passes a live client.
    /// What: Sequential; each case records its latency. Failures are captured
    /// per-case, never short-circuit the suite.
    /// Test: `eval_suite_runs_all_cases`.
    pub async fn run(&self, system_prompt: &str, llm: &dyn EvalLlmClient) -> Vec<EvalResult> {
        let mut out = Vec::with_capacity(self.cases.len());
        for case in &self.cases {
            out.push(run_case(case, system_prompt, llm).await);
        }
        out
    }

    /// Render a human-friendly summary of results.
    pub fn report(results: &[EvalResult]) -> String {
        let mut buf = String::new();
        let passed = results.iter().filter(|r| r.passed).count();
        let total = results.len();
        buf.push_str(&format!("Running {total} eval cases...\n\n"));
        for r in results {
            let marker = if r.passed { "PASS" } else { "FAIL" };
            buf.push_str(&format!("[{}] {} ({}ms)\n", marker, r.id, r.latency_ms));
            if !r.passed {
                for f in &r.failures {
                    buf.push_str(&format!("    - {f}\n"));
                }
            }
        }
        buf.push_str(&format!("\n{passed}/{total} passed\n"));
        buf
    }
}

/// Execute one case and score the response.
async fn run_case(case: &EvalCase, system_prompt: &str, llm: &dyn EvalLlmClient) -> EvalResult {
    let started = Instant::now();
    let (response, tool_calls) = match llm
        .complete_with_tools(system_prompt, &case.input, case.user_tier.as_deref())
        .await
    {
        Ok(pair) => pair,
        Err(e) => {
            return EvalResult {
                id: case.id.clone(),
                passed: false,
                failures: vec![format!("LLM call failed: {e}")],
                tool_calls_made: Vec::new(),
                response: String::new(),
                latency_ms: started.elapsed().as_millis() as u64,
            };
        }
    };
    let latency_ms = started.elapsed().as_millis() as u64;
    let failures = score(case, &response, &tool_calls);
    EvalResult {
        id: case.id.clone(),
        passed: failures.is_empty(),
        failures,
        tool_calls_made: tool_calls,
        response,
        latency_ms,
    }
}

/// Apply every assertion declared in the case.
///
/// Why: Centralized so the same logic underwrites unit tests and the CLI.
/// What: Substring match for must-contain / must-not-contain (case-insensitive);
/// exact-name match for tool calls. Reports each miss as a human-readable
/// string.
/// Test: `eval_case_pass_when_all_match`, `eval_case_fail_*`.
fn score(case: &EvalCase, response: &str, tool_calls: &[String]) -> Vec<String> {
    let mut failures = Vec::new();
    let response_lower = response.to_lowercase();

    for needle in &case.response_must_contain {
        if !response_lower.contains(&needle.to_lowercase()) {
            failures.push(format!("response missing required phrase: {needle:?}"));
        }
    }
    for forbidden in &case.response_must_not_contain {
        if response_lower.contains(&forbidden.to_lowercase()) {
            failures.push(format!("response contains forbidden phrase: {forbidden:?}"));
        }
    }

    // Tool call assertions: order-independent set equality (presence matters,
    // duplicates do not).
    use std::collections::HashSet;
    let expected: HashSet<&str> = case
        .expected_tool_calls
        .iter()
        .map(String::as_str)
        .collect();
    let actual: HashSet<&str> = tool_calls.iter().map(String::as_str).collect();

    for missing in expected.difference(&actual) {
        failures.push(format!("expected tool call not made: {missing:?}"));
    }
    for unexpected in actual.difference(&expected) {
        failures.push(format!("unexpected tool call: {unexpected:?}"));
    }

    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock that returns canned `(response, tool_calls)` regardless of input.
    struct MockEvalLlm {
        response: String,
        tool_calls: Vec<String>,
    }

    #[async_trait]
    impl EvalLlmClient for MockEvalLlm {
        async fn complete_with_tools(
            &self,
            _system: &str,
            _user: &str,
            _user_tier: Option<&str>,
        ) -> Result<(String, Vec<String>)> {
            Ok((self.response.clone(), self.tool_calls.clone()))
        }
    }

    #[test]
    fn eval_case_from_toml_parses_minimal() {
        let toml_str = r#"
[[eval]]
id = "basic"
input = "hello"
response_must_contain = ["hi"]
"#;
        let suite = EvalSuite::from_toml_str(toml_str).expect("parses");
        assert_eq!(suite.cases.len(), 1);
        assert_eq!(suite.cases[0].id, "basic");
        assert_eq!(suite.cases[0].response_must_contain, vec!["hi"]);
        assert!(suite.cases[0].expected_tool_calls.is_empty());
    }

    #[test]
    fn eval_case_from_toml_parses_full() {
        let toml_str = r#"
[[eval]]
id = "rbac_test"
description = "Analytics user blocked from budget"
input = "What is the budget?"
user_tier = "analytics"
expected_tool_calls = []
response_must_contain = ["no access"]
response_must_not_contain = ["million"]
"#;
        let suite = EvalSuite::from_toml_str(toml_str).expect("parses");
        let c = &suite.cases[0];
        assert_eq!(c.user_tier.as_deref(), Some("analytics"));
        assert_eq!(c.response_must_not_contain, vec!["million"]);
    }

    #[test]
    fn eval_suite_from_toml_rejects_empty() {
        let err = EvalSuite::from_toml_str("").unwrap_err();
        assert!(err.to_string().contains("no [[eval]]"));
    }

    #[tokio::test]
    async fn eval_case_pass_when_all_match() {
        let suite = EvalSuite::from_toml_str(
            r#"
[[eval]]
id = "ok"
input = "x"
response_must_contain = ["yes"]
response_must_not_contain = ["nope"]
expected_tool_calls = ["my_tool"]
"#,
        )
        .unwrap();
        let llm = MockEvalLlm {
            response: "Yes, here is the answer.".to_string(),
            tool_calls: vec!["my_tool".to_string()],
        };
        let results = suite.run("sys", &llm).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].passed, "failures: {:?}", results[0].failures);
    }

    #[tokio::test]
    async fn eval_case_fail_on_missing_phrase() {
        let suite = EvalSuite::from_toml_str(
            r#"
[[eval]]
id = "missing"
input = "x"
response_must_contain = ["budget"]
"#,
        )
        .unwrap();
        let llm = MockEvalLlm {
            response: "unrelated answer".to_string(),
            tool_calls: vec![],
        };
        let results = suite.run("sys", &llm).await;
        assert!(!results[0].passed);
        assert!(
            results[0]
                .failures
                .iter()
                .any(|f| f.contains("missing required phrase"))
        );
    }

    #[tokio::test]
    async fn eval_case_fail_on_forbidden_phrase() {
        let suite = EvalSuite::from_toml_str(
            r#"
[[eval]]
id = "forbidden"
input = "x"
response_must_not_contain = ["password"]
"#,
        )
        .unwrap();
        let llm = MockEvalLlm {
            response: "your password is hunter2".to_string(),
            tool_calls: vec![],
        };
        let results = suite.run("sys", &llm).await;
        assert!(!results[0].passed);
        assert!(
            results[0]
                .failures
                .iter()
                .any(|f| f.contains("forbidden phrase"))
        );
    }

    #[tokio::test]
    async fn eval_case_fail_on_unexpected_tool_call() {
        let suite = EvalSuite::from_toml_str(
            r#"
[[eval]]
id = "rbac"
input = "x"
expected_tool_calls = []
"#,
        )
        .unwrap();
        let llm = MockEvalLlm {
            response: "ok".to_string(),
            tool_calls: vec!["forbidden_tool".to_string()],
        };
        let results = suite.run("sys", &llm).await;
        assert!(!results[0].passed);
        assert!(
            results[0]
                .failures
                .iter()
                .any(|f| f.contains("unexpected tool call"))
        );
    }

    #[tokio::test]
    async fn eval_case_fail_on_missing_tool_call() {
        let suite = EvalSuite::from_toml_str(
            r#"
[[eval]]
id = "route"
input = "x"
expected_tool_calls = ["delegate_to_agent"]
"#,
        )
        .unwrap();
        let llm = MockEvalLlm {
            response: "I'll do it directly".to_string(),
            tool_calls: vec![],
        };
        let results = suite.run("sys", &llm).await;
        assert!(!results[0].passed);
        assert!(
            results[0]
                .failures
                .iter()
                .any(|f| f.contains("expected tool call not made"))
        );
    }

    #[tokio::test]
    async fn eval_suite_runs_all_cases() {
        let suite = EvalSuite::from_toml_str(
            r#"
[[eval]]
id = "a"
input = "x"
response_must_contain = ["ok"]

[[eval]]
id = "b"
input = "y"
response_must_contain = ["nope"]
"#,
        )
        .unwrap();
        let llm = MockEvalLlm {
            response: "ok".to_string(),
            tool_calls: vec![],
        };
        let results = suite.run("sys", &llm).await;
        assert_eq!(results.len(), 2);
        assert!(results[0].passed);
        assert!(!results[1].passed);
    }

    #[tokio::test]
    async fn report_summarizes_results() {
        let results = vec![
            EvalResult {
                id: "a".into(),
                passed: true,
                failures: vec![],
                tool_calls_made: vec![],
                response: "ok".into(),
                latency_ms: 12,
            },
            EvalResult {
                id: "b".into(),
                passed: false,
                failures: vec!["bad".into()],
                tool_calls_made: vec![],
                response: "".into(),
                latency_ms: 34,
            },
        ];
        let report = EvalSuite::report(&results);
        assert!(report.contains("[PASS] a"));
        assert!(report.contains("[FAIL] b"));
        assert!(report.contains("1/2 passed"));
        assert!(report.contains("- bad"));
    }

    #[test]
    fn case_insensitive_matching() {
        let case = EvalCase {
            id: "ci".into(),
            description: "".into(),
            input: "x".into(),
            user_tier: None,
            expected_tool_calls: vec![],
            response_must_contain: vec!["BUDGET".into()],
            response_must_not_contain: vec![],
        };
        let failures = score(&case, "the budget figures", &[]);
        assert!(failures.is_empty(), "{:?}", failures);
    }
}
