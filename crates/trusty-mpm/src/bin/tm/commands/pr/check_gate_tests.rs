//! Tests for the #8614 checks gate in `tm pr merge`.
//!
//! Why: the gate is the difference between "a branch-caused red merged" and
//! "the merge was refused", so each refusal, each waiver and each pass is
//! pinned — the pure half against rollup JSON, the `run` half against a fake
//! `gh` that records every call, with no network.
//! What: [`parse_required`], [`parse_ruleset_required`], [`refusal`],
//! [`waived`], [`commit_body`] and `merge::run` end to end.
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
        allow_no_checks: false,
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
        allow_no_checks: false,
        required: Some(&required),
    };
    let reason = refusal(&checks, &gate, 676).expect("refused");
    assert!(reason.contains("travel-live"), "{reason}");
    assert!(
        !reason.contains("CI gate"),
        "auto-merge waits on it: {reason}"
    );
    // FAILS BEFORE the #8614 fix round: without `--auto`, `gh pr merge` merges
    // an UNSTABLE PR at once, so a running non-required check is no safer.
    let direct = CheckGate {
        auto: false,
        ..gate
    };
    let reason = refusal(&checks, &direct, 676).expect("refused without --auto too");
    assert!(reason.contains("travel-live"), "{reason}");
    assert!(!reason.contains("CI gate"), "gh blocks on it: {reason}");
}

/// A rules payload: one `required_status_checks` rule requiring `names`, plus
/// a parameterless rule the reader must skip.
fn ruleset(names: &[&str]) -> String {
    serde_json::json!([
        {"type": "deletion", "ruleset_id": 1},
        {"type": "required_status_checks", "ruleset_id": 2, "parameters": {
            "strict_required_status_checks_policy": false,
            "required_status_checks": names.iter()
                .map(|n| serde_json::json!({"context": n, "integration_id": 15368}))
                .collect::<Vec<_>>()}}
    ])
    .to_string()
}

#[test]
fn ruleset_payload_lists_the_required_checks() {
    assert_eq!(
        parse_ruleset_required(&ruleset(&["scaffold", "lint"])).expect("parses"),
        vec!["scaffold".to_string(), "lint".to_string()]
    );
    // `gh api --paginate` prints one array per page, back to back.
    let paged = format!(
        "{}{}",
        ruleset(&["scaffold"]),
        ruleset(&["lint", "scaffold"])
    );
    assert_eq!(
        parse_ruleset_required(&paged).expect("parses"),
        vec!["scaffold".to_string(), "lint".to_string()]
    );
    assert_eq!(
        parse_ruleset_required("[]").expect("parses"),
        Vec::<String>::new()
    );
    assert!(parse_ruleset_required("").is_err(), "empty fails closed");
    assert!(parse_ruleset_required("<html>").is_err());
}

#[test]
fn no_registered_check_refuses_unless_one_is_required_or_allowed() {
    let none = rollup(&[]);
    let ci = vec!["CI gate".to_string()];
    for required in [None, Some(&[][..])] {
        let gate = CheckGate {
            required,
            ..CheckGate::default()
        };
        let reason = refusal(&none, &gate, 7).expect("refused");
        assert!(reason.contains("--allow-no-checks"), "{reason}");
        let allowed = CheckGate {
            allow_no_checks: true,
            ..gate
        };
        assert_eq!(refusal(&none, &allowed, 7), None);
    }
    // GitHub holds the merge for a required check that has not registered.
    let gate = CheckGate {
        required: Some(&ci),
        ..CheckGate::default()
    };
    assert_eq!(refusal(&none, &gate, 7), None);
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

/// A fake `gh`: the most recently registered route whose needle the joined
/// argv contains answers; every call is recorded, and the `--body-file` of a
/// `pr merge` call is read back while the file still exists.
struct FakeGh {
    /// (needle, answer), newest first.
    routes: Vec<(String, GhRun)>,
    /// Every argv run, in order.
    seen: RefCell<Vec<String>>,
    /// The contents of the `pr merge --body-file` file, once merged.
    merge_body: RefCell<Option<String>>,
}

/// `gh pr view` for PR 42 on base `main` with `rollup` as its checks.
fn view(rollup: &[serde_json::Value]) -> serde_json::Value {
    serde_json::json!({"number": 42, "title": "fix: x",
        "body": ATTRIBUTION_FOOTER, "state": "OPEN", "headRefName": "fix/x",
        "baseRefName": "main", "statusCheckRollup": rollup})
}

impl FakeGh {
    fn new(view_rollup: &[serde_json::Value]) -> Self {
        Self::from_view(&view(view_rollup))
    }

    /// A fake answering `pr view` with `view`, `pr merge` with success, and
    /// the rules read with no rules.
    fn from_view(view: &serde_json::Value) -> Self {
        let gh = Self {
            routes: Vec::new(),
            seen: RefCell::new(Vec::new()),
            merge_body: RefCell::new(None),
        };
        gh.route("pr view", true, &view.to_string())
            .route("pr merge", true, "")
            .route("api --paginate repos/o/r/rules/branches/main", true, "[]")
    }

    /// Register a route that wins over every earlier one with a matching
    /// needle; a failing route answers on stderr.
    fn route(mut self, needle: &str, success: bool, out: &str) -> Self {
        let (stdout, stderr) = if success { (out, "") } else { ("", out) };
        self.routes.insert(
            0,
            (
                needle.to_string(),
                GhRun {
                    success,
                    stdout: stdout.to_string(),
                    stderr: stderr.to_string(),
                },
            ),
        );
        self
    }

    fn with_branch(self, payload: &str) -> Self {
        self.route("api repos/o/r/branches/main", true, payload)
    }

    fn with_rules(self, payload: &str) -> Self {
        self.route(
            "api --paginate repos/o/r/rules/branches/main",
            true,
            payload,
        )
    }

    fn called(&self, needle: &str) -> bool {
        self.seen.borrow().iter().any(|a| a.contains(needle))
    }
}

impl GhRunner for FakeGh {
    fn run(&self, args: &[String]) -> anyhow::Result<GhRun> {
        let joined = args.join(" ");
        self.seen.borrow_mut().push(joined.clone());
        if joined.starts_with("pr merge") {
            let path = args
                .iter()
                .position(|a| a == "--body-file")
                .and_then(|i| args.get(i + 1))
                .ok_or_else(|| anyhow::anyhow!("pr merge without --body-file: {joined}"))?;
            *self.merge_body.borrow_mut() = Some(std::fs::read_to_string(path)?);
        }
        let (_, run) = self
            .routes
            .iter()
            .find(|(needle, _)| joined.contains(needle.as_str()))
            .ok_or_else(|| anyhow::anyhow!("unexpected gh call: {joined}"))?;
        Ok(run.clone())
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
    assert!(args(&["--allow-no-checks"]).allow_no_checks);
    assert!(!args(&[]).allow_no_checks, "checks required by default");
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

/// FAILS BEFORE the #8614 fix round: a direct merge (no `--auto`) of an
/// UNSTABLE PR went through while a non-required check was still running.
#[test]
fn run_refuses_while_a_non_required_check_runs_without_auto() {
    let gh = FakeGh::new(&[running("CI gate"), running("travel-live")])
        .with_branch(&protected(&["CI gate"]));
    let code = merge::run(&gh, &args(&[])).expect("runs");
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
    assert!(gh.called("api --paginate repos/o/r/rules/branches/main"));
    assert!(gh.called("pr merge 42"));
}

#[test]
fn run_allow_failing_merges_over_a_waived_check() {
    let gh =
        FakeGh::new(&[passed("CI gate"), failed("scaffold")]).with_branch(&protected(&["CI gate"]));
    let code = merge::run(&gh, &args(&["--allow-failing", "scaffold"])).expect("runs");
    assert_eq!(code, EXIT_OK);
    assert!(gh.called("pr merge 42"));
    let body = gh.merge_body.borrow().clone().expect("--body-file read");
    assert!(body.starts_with("Merged over waived check(s)"), "{body}");
    assert!(body.contains("scaffold"), "{body}");
}

/// A check a repository ruleset requires is never waived, even when branch
/// protection requires nothing.
#[test]
fn run_never_waives_a_ruleset_required_check() {
    let gh = FakeGh::new(&[passed("CI gate"), failed("scaffold")])
        .with_branch(UNPROTECTED)
        .with_rules(&ruleset(&["scaffold"]));
    let code = merge::run(&gh, &args(&["--allow-failing", "scaffold"])).expect("runs");
    assert_eq!(code, EXIT_BLOCKED);
    assert!(!gh.called("pr merge"), "{:?}", gh.seen.borrow());
}

/// The failure arms of the required-list read: each one fails closed. The
/// waiver plus a failed check is what makes the read happen.
#[test]
fn run_refuses_when_the_branch_read_fails() {
    let cases = [
        FakeGh::new(&[failed("scaffold")]).route(
            "api repos/o/r/branches/main",
            false,
            "HTTP 404: Branch not found",
        ),
        FakeGh::new(&[failed("scaffold")]).with_branch("<html>not json</html>"),
        FakeGh::new(&[failed("scaffold")])
            .with_branch(UNPROTECTED)
            .route(
                "api --paginate repos/o/r/rules/branches/main",
                false,
                "HTTP 403: Resource not accessible",
            ),
    ];
    for gh in cases {
        let outcome = merge::run(&gh, &args(&["--allow-failing", "scaffold"]));
        assert!(
            outcome.as_ref().is_err() || outcome.as_ref().is_ok_and(|c| *c == EXIT_BLOCKED),
            "{outcome:?}"
        );
        assert!(!gh.called("pr merge"), "{:?}", gh.seen.borrow());
    }
}

#[test]
fn run_refuses_when_the_ruleset_read_fails() {
    let gh = FakeGh::new(&[running("travel-live")])
        .with_branch(UNPROTECTED)
        .with_rules("not json");
    let err = merge::run(&gh, &args(&[])).expect_err("an unreadable ruleset fails closed");
    assert!(format!("{err:#}").contains("rules payload"), "{err:#}");
    assert!(!gh.called("pr merge"), "{:?}", gh.seen.borrow());
}

/// The `ensure!` on an empty `baseRefName`: nothing to read, nothing merges.
#[test]
fn run_refuses_without_a_base_ref_name() {
    let mut v = view(&[failed("scaffold")]);
    v["baseRefName"] = serde_json::json!("");
    let gh = FakeGh::from_view(&v);
    let err = merge::run(&gh, &args(&["--allow-failing", "scaffold"])).expect_err("refused");
    assert!(format!("{err:#}").contains("baseRefName"), "{err:#}");
    assert!(!gh.called("pr merge"), "{:?}", gh.seen.borrow());
}

/// No check has registered and the base requires none: a merge now is
/// unchecked.
#[test]
fn run_refuses_when_no_check_has_registered() {
    let gh = FakeGh::new(&[]).with_branch(UNPROTECTED);
    let code = merge::run(&gh, &args(&[])).expect("runs");
    assert_eq!(code, EXIT_BLOCKED);
    assert!(gh.called("api repos/o/r/branches/main"));
    assert!(!gh.called("pr merge"), "{:?}", gh.seen.borrow());
}

/// `--allow-no-checks` is the named override for a repo with no CI.
#[test]
fn run_allow_no_checks_merges_a_pr_without_checks() {
    let gh = FakeGh::new(&[]);
    let code = merge::run(&gh, &args(&["--allow-no-checks"])).expect("runs");
    assert_eq!(code, EXIT_OK);
    assert!(!gh.called("branches/"), "{:?}", gh.seen.borrow());
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
