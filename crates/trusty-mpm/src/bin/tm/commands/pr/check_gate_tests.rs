//! Tests for the #8614 checks gate in `tm pr merge`.
//!
//! Why: the gate is the difference between "a branch-caused red merged" and
//! "the merge was refused", so each refusal, each waiver and each pass is
//! pinned — the pure half against rollup JSON, the `run` half against a fake
//! `gh` that records every call, with no network.
//! What: [`parse_required`], [`refusal`], [`waived`], [`commit_body`] and
//! `merge::run` end to end.
//! Test: this file IS the test module.

use std::cell::RefCell;

use super::super::body::ATTRIBUTION_FOOTER;
use super::super::{EXIT_BLOCKED, EXIT_OK, GhRun, GhRunner, merge};
use super::*;
use crate::cli::PrMergeArgs;

/// A failing `CheckRun` named `name`.
fn failed(name: &str) -> serde_json::Value {
    serde_json::json!({"__typename": "CheckRun", "name": name,
        "status": "COMPLETED", "conclusion": "FAILURE"})
}

/// A `CheckRun` named `name` still in progress.
fn running(name: &str) -> serde_json::Value {
    serde_json::json!({"__typename": "CheckRun", "name": name,
        "status": "IN_PROGRESS", "conclusion": ""})
}

/// A passing `CheckRun` named `name`.
fn passed(name: &str) -> serde_json::Value {
    serde_json::json!({"__typename": "CheckRun", "name": name,
        "status": "COMPLETED", "conclusion": "SUCCESS"})
}

/// Parse a list of rollup entries.
fn rollup(entries: &[serde_json::Value]) -> Vec<RollupEntry> {
    serde_json::from_value(serde_json::Value::Array(entries.to_vec())).expect("rollup parses")
}

/// A branch payload whose protection requires `names`.
fn protected(names: &[&str]) -> String {
    serde_json::json!({"name": "main", "protected": true, "protection": {
        "enabled": true, "required_status_checks": {"enforcement_level": "non_admins",
        "contexts": names, "checks": names.iter()
            .map(|n| serde_json::json!({"context": n, "app_id": 15368}))
            .collect::<Vec<_>>()}}})
    .to_string()
}

/// The payload GitHub returns for an unprotected branch (read live 2026-10-03).
const UNPROTECTED: &str = r#"{"name":"b","protected":false,"protection":{"enabled":false,
"required_status_checks":{"checks":[],"contexts":[],"enforcement_level":"off"}}}"#;

#[test]
fn branch_payload_lists_the_enforced_checks() {
    let names = parse_required(&protected(&["CI gate", "lint"])).expect("parses");
    assert_eq!(names, Some(vec!["CI gate".to_string(), "lint".to_string()]));
}

#[test]
fn an_unprotected_branch_requires_nothing() {
    assert_eq!(
        parse_required(UNPROTECTED).expect("parses"),
        Some(Vec::new())
    );
}

#[test]
fn a_payload_without_protection_is_unknown() {
    assert_eq!(parse_required(r#"{"name":"b"}"#).expect("parses"), None);
}

/// The #8614 incident: a red check merged because protection required none.
#[test]
fn a_failing_check_refuses_with_or_without_auto() {
    let checks = rollup(&[passed("CI gate"), failed("scaffold")]);
    for auto in [false, true] {
        let gate = CheckGate {
            auto,
            required: Some(&[]),
            ..CheckGate::default()
        };
        let reason = refusal(&checks, &gate, 676).expect("refused");
        assert!(reason.contains("scaffold"), "{reason}");
    }
}

#[test]
fn allow_failing_waives_only_a_named_non_required_check() {
    let checks = rollup(&[failed("scaffold"), failed("CI gate")]);
    let required = vec!["CI gate".to_string()];
    let allow = vec!["scaffold".to_string(), "CI gate".to_string()];
    let gate = CheckGate {
        auto: false,
        allow_failing: &allow,
        required: Some(&required),
    };
    let reason = refusal(&checks, &gate, 1).expect("a required red is never waived");
    assert!(
        reason.contains("CI gate") && !reason.contains("scaffold"),
        "{reason}"
    );
    assert_eq!(waived(&checks, &gate), vec!["scaffold".to_string()]);

    // An unknown required list waives nothing.
    let unknown = CheckGate {
        required: None,
        ..gate
    };
    let reason = refusal(&checks, &unknown, 1).expect("refused");
    assert!(reason.contains("scaffold"), "{reason}");
}

#[test]
fn auto_refuses_while_a_non_required_check_runs() {
    let checks = rollup(&[running("CI gate"), running("travel-live")]);
    let required = vec!["CI gate".to_string()];
    let gate = CheckGate {
        auto: true,
        allow_failing: &[],
        required: Some(&required),
    };
    let reason = refusal(&checks, &gate, 676).expect("refused");
    assert!(reason.contains("travel-live"), "{reason}");
    assert!(
        !reason.contains("CI gate"),
        "auto-merge waits on it: {reason}"
    );
    // Without `--auto` GitHub decides on the running checks.
    let direct = CheckGate {
        auto: false,
        ..gate
    };
    assert_eq!(refusal(&checks, &direct, 676), None);
}

#[test]
fn a_waiver_heads_the_commit_body() {
    let body = commit_body("the body", &["scaffold".to_string()]);
    assert!(body.starts_with("Merged over waived check(s)"), "{body}");
    assert!(
        body.contains("scaffold") && body.ends_with("the body"),
        "{body}"
    );
    assert_eq!(commit_body("the body", &[]), "the body");
}

// ── `merge::run` end to end ──────────────────────────────────────────────

/// A fake `gh`: the first route whose needle the joined argv contains
/// answers; every call is recorded.
struct FakeGh {
    /// (needle, stdout) pairs, all successful.
    routes: Vec<(String, String)>,
    /// Every argv run, in order.
    seen: RefCell<Vec<String>>,
}

impl FakeGh {
    fn new(view_rollup: &[serde_json::Value]) -> Self {
        let view = serde_json::json!({"number": 42, "title": "fix: x",
            "body": ATTRIBUTION_FOOTER, "state": "OPEN", "headRefName": "fix/x",
            "baseRefName": "main", "statusCheckRollup": view_rollup});
        Self {
            routes: vec![
                ("pr view".to_string(), view.to_string()),
                ("pr merge".to_string(), String::new()),
            ],
            seen: RefCell::new(Vec::new()),
        }
    }

    fn with_branch(mut self, payload: &str) -> Self {
        self.routes.push((
            "api repos/o/r/branches/main".to_string(),
            payload.to_string(),
        ));
        self
    }

    fn called(&self, needle: &str) -> bool {
        self.seen.borrow().iter().any(|a| a.contains(needle))
    }
}

impl GhRunner for FakeGh {
    fn run(&self, args: &[String]) -> anyhow::Result<GhRun> {
        let joined = args.join(" ");
        self.seen.borrow_mut().push(joined.clone());
        let (_, stdout) = self
            .routes
            .iter()
            .find(|(needle, _)| joined.contains(needle.as_str()))
            .ok_or_else(|| anyhow::anyhow!("unexpected gh call: {joined}"))?;
        Ok(GhRun {
            success: true,
            stdout: stdout.clone(),
            stderr: String::new(),
        })
    }
}

/// `tm pr merge 42 --repo o/r` plus `extra`, parsed the way the CLI parses it.
fn args(extra: &[&str]) -> PrMergeArgs {
    use clap::Parser as _;
    let mut argv = vec!["trusty-mpm", "pr", "merge", "42", "--repo", "o/r"];
    argv.extend_from_slice(extra);
    let cli = crate::cli::Cli::try_parse_from(argv).expect("parses");
    match cli.command {
        Some(crate::cli::Command::Pr {
            cmd: crate::cli::PrCmd::Merge(a),
        }) => a,
        other => panic!("expected pr merge, got {other:?}"),
    }
}

#[test]
fn cli_parses_pr_merge_allow_failing_8614() {
    let waived = args(&[
        "--allow-failing",
        "travel-live",
        "--allow-failing",
        "scaffold",
    ]);
    assert_eq!(waived.allow_failing, vec!["travel-live", "scaffold"]);
    assert!(args(&[]).allow_failing.is_empty(), "no waiver by default");
}

/// FAILS BEFORE #8614: `--auto` armed while a check auto-merge does not wait
/// for was still running, so its later failure merged anyway.
#[test]
fn run_auto_refuses_while_a_non_required_check_runs() {
    let gh = FakeGh::new(&[passed("CI gate"), running("travel-live")]).with_branch(UNPROTECTED);
    let code = merge::run(&gh, &args(&["--auto"])).expect("runs");
    assert_eq!(code, EXIT_BLOCKED);
    assert!(!gh.called("pr merge"), "{:?}", gh.seen.borrow());
}

/// FAILS BEFORE #8614: a failing check on an unprotected base did not stop
/// the merge.
#[test]
fn run_refuses_a_failing_check_without_auto() {
    let gh = FakeGh::new(&[failed("scaffold")]);
    let code = merge::run(&gh, &args(&[])).expect("runs");
    assert_eq!(code, EXIT_BLOCKED);
    assert!(!gh.called("pr merge"), "{:?}", gh.seen.borrow());
}

/// A running REQUIRED check is what auto-merge waits for: `--auto` arms.
#[test]
fn run_auto_arms_when_only_required_checks_run() {
    let gh = FakeGh::new(&[running("CI gate")]).with_branch(&protected(&["CI gate"]));
    let code = merge::run(&gh, &args(&["--auto"])).expect("runs");
    assert_eq!(code, EXIT_OK);
    assert!(gh.called("api repos/o/r/branches/main"));
    assert!(gh.called("pr merge 42"));
}

#[test]
fn run_allow_failing_merges_over_a_waived_check() {
    let gh =
        FakeGh::new(&[passed("CI gate"), failed("scaffold")]).with_branch(&protected(&["CI gate"]));
    let code = merge::run(&gh, &args(&["--allow-failing", "scaffold"])).expect("runs");
    assert_eq!(code, EXIT_OK);
    assert!(gh.called("pr merge 42"));
}

/// Green checks need no protection read — no extra `gh` call.
#[test]
fn run_without_running_or_waived_checks_reads_no_protection() {
    let gh = FakeGh::new(&[passed("CI gate")]);
    let code = merge::run(&gh, &args(&["--auto"])).expect("runs");
    assert_eq!(code, EXIT_OK);
    assert!(!gh.called("branches/"), "{:?}", gh.seen.borrow());
}
