//! Unit tests for the `ompm` binary's arg/prefix parsing helpers.
//!
//! Why: The task-text + flag parsing is pure and worth pinning independent of
//! the full binary run.
//! What: Parse-path tests.
//! Test: This module is itself the test coverage.

use super::*;

#[test]
fn route_prefix_handles_known_slash_and_at_prefixes() {
    let r = route_prefix(&["/research".into(), "query".into()]).unwrap();
    assert_eq!(r.agent.as_deref(), Some("research-agent"));
    assert!(r.workflow.is_none());

    let r = route_prefix(&["/implement".into(), "x".into()]).unwrap();
    assert_eq!(r.workflow.as_deref(), Some("prescriptive"));
    assert!(r.agent.is_none());

    let r = route_prefix(&["/qa".into(), ".".into()]).unwrap();
    assert_eq!(r.workflow.as_deref(), Some("qa-only"));

    let r = route_prefix(&["/plan".into(), "x".into()]).unwrap();
    assert_eq!(r.workflow.as_deref(), Some("plan-only"));

    let r = route_prefix(&["@engineer".into(), "do X".into()]).unwrap();
    assert_eq!(r.agent.as_deref(), Some("engineer"));

    // Unknown prefixes → None.
    assert!(route_prefix(&["/unknown".into()]).is_none());
    assert!(route_prefix(&["@".into()]).is_none());
    assert!(route_prefix(&["task".into()]).is_none());
}

#[test]
fn parse_task_args_basic() {
    let (task, flags) = parse_task_args(
        &["hello".into(), "--json".into()],
        &RoutedRequest::default(),
    )
    .unwrap();
    assert_eq!(task, "hello");
    assert!(flags.json_output);
    assert!(flags.workflow.is_none());
}

#[test]
fn parse_task_args_merges_routed_but_explicit_wins() {
    // Routed gives workflow=prescriptive; explicit --workflow beats it.
    let routed = RoutedRequest {
        workflow: Some("prescriptive".into()),
        agent: None,
        task_prefix: None,
    };
    let (task, flags) = parse_task_args(
        &[
            "do X".into(),
            "--workflow".into(),
            "custom".into(),
            "--out-dir".into(),
            "/tmp/o".into(),
        ],
        &routed,
    )
    .unwrap();
    assert_eq!(task, "do X");
    assert_eq!(flags.workflow.as_deref(), Some("custom"));
    assert_eq!(flags.out_dir.as_deref(), Some("/tmp/o"));
}

#[test]
fn parse_task_args_errors_on_missing_task() {
    let r = parse_task_args(&["--json".into()], &RoutedRequest::default());
    assert!(r.is_err());
}

#[test]
fn parse_task_args_applies_routed_task_prefix() {
    // #151 phase-4: `/qa ./path` should produce "run QA on ./path".
    let routed = RoutedRequest {
        workflow: Some("qa-only".into()),
        agent: None,
        task_prefix: Some("run QA on".into()),
    };
    let (task, _) = parse_task_args(&["./path".into()], &routed).unwrap();
    assert_eq!(task, "run QA on ./path");
}

#[test]
fn parse_task_args_errors_on_unknown_flag() {
    let r = parse_task_args(&["t".into(), "--nope".into()], &RoutedRequest::default());
    assert!(r.is_err());
}
