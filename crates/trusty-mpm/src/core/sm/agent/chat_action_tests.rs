//! Unit tests for the action-chat decision protocol (#1283).
//!
//! Why: the inline-action loop is only as safe as its parser and dispatch — a
//! verb-call must parse to a verb, a final answer must terminate, prose must
//! fall back to a final answer (never a spurious verb), the advertised verb list
//! must come from the catalog SoT, and each verb must map onto the right
//! `SessionControl` method. These tests pin all of that deterministically with a
//! recording mock control.
//! What: exercises [`parse_action`], [`action_instructions`], and [`execute_verb`]
//! against the shared `MockSessionControl`.
//! Test: this is the test module.

use std::sync::Arc;

use serde_json::json;

use super::{Decided, action_instructions, execute_verb, parse_action};
use crate::core::sm::agent::delegate::mock_control::MockSessionControl;
use crate::core::sm::control::SessionControl;

/// Why: a bare verb-call JSON must parse to a `Verb` with the name + args.
/// What: parses `{"action":"sessions.list","args":{}}`.
/// Test: this is the test.
#[test]
fn parse_verb_call() {
    let d = parse_action(r#"{"action":"sessions.list","args":{}}"#);
    match d {
        Decided::Verb { verb, .. } => assert_eq!(verb, "sessions.list"),
        other => panic!("expected verb, got {other:?}"),
    }
}

/// Why: the `final` action must terminate the loop, carrying the message.
/// What: parses `{"action":"final","message":"all good"}`.
/// Test: this is the test.
#[test]
fn parse_final_answer() {
    let d = parse_action(r#"{"action":"final","message":"all good"}"#);
    assert_eq!(
        d,
        Decided::Final {
            message: "all good".to_string()
        }
    );
}

/// Why: plain prose (no JSON) must be treated as the final answer — never a
/// spurious verb — so an unparseable reply is the always-safe terminal.
/// What: parses a prose string and asserts it becomes the final message verbatim.
/// Test: this is the test.
#[test]
fn parse_prose_is_final() {
    let d = parse_action("I checked and everything looks fine.");
    assert_eq!(
        d,
        Decided::Final {
            message: "I checked and everything looks fine.".to_string()
        }
    );
}

/// Why: FIX 4 — `next_balanced_object`'s `depth` decrement must not underflow on a
/// stray unmatched `}` (a `saturating_sub` guard). The parser must never panic and
/// must still extract a valid object when one follows the stray brace.
/// What: parses inputs with leading/trailing stray `}`; asserts no panic and that a
/// real object is still recovered (else the prose fallback).
/// Test: this is the test.
#[test]
fn parse_handles_stray_closing_brace_without_panic() {
    // A bare stray brace + prose: no object → prose fallback, no panic.
    let d = parse_action("} stray");
    assert_eq!(
        d,
        Decided::Final {
            message: "} stray".to_string()
        }
    );

    // A valid object followed by a stray `}`: the object must still be extracted.
    let d = parse_action(r#"{"action":"final","message":"ok"} junk }"#);
    assert_eq!(
        d,
        Decided::Final {
            message: "ok".to_string()
        }
    );
}

/// Why: the model often wraps its JSON in a ```json fence; the parser must still
/// extract the verb-call.
/// What: parses a fenced verb-call and asserts the verb name.
/// Test: this is the test.
#[test]
fn parse_fenced_verb() {
    let reply = "Let me look.\n```json\n{\"action\":\"sessions.get\",\"args\":{\"session_id\":\"s1\"}}\n```";
    match parse_action(reply) {
        Decided::Verb { verb, args } => {
            assert_eq!(verb, "sessions.get");
            assert_eq!(args.get("session_id").unwrap(), "s1");
        }
        other => panic!("expected verb, got {other:?}"),
    }
}

/// Why: the advertised verb list must be rendered from the catalog single source
/// of truth, so the prompt can never drift from the executable surface.
/// What: asserts the instruction block lists every catalog verb + both shapes.
/// Test: this is the test.
#[test]
fn prompt_lists_every_catalog_verb() {
    let prompt = action_instructions();
    for verb in [
        "sessions.launch",
        "sessions.list",
        "sessions.get",
        "sessions.send",
        "sessions.stop",
        "sessions.resume",
        "sessions.kill",
    ] {
        assert!(prompt.contains(verb), "prompt missing `{verb}`");
    }
    assert!(
        prompt.contains("\"action\":\"final\""),
        "missing final shape"
    );
    assert!(prompt.contains("INLINE"), "must state verbs run inline");
}

/// Why: the PRIMARY fix — the action loop must ADVERTISE `sessions.health` so the
/// self-aware coordinator chat knows it can run a health check inline (#1496).
/// What: asserts the instruction block lists the ops `sessions.health` verb.
/// Test: this is the test.
#[test]
fn prompt_lists_health_ops_verb() {
    let prompt = action_instructions();
    assert!(
        prompt.contains("sessions.health"),
        "action prompt must advertise the executable `sessions.health` verb"
    );
}

/// Why: the PRIMARY fix — `sessions.health` must EXECUTE inline in the action loop
/// (not just be advertised), synthesizing a fleet summary from the control surface.
/// What: dispatches `sessions.health` against the mock and asserts the synthesized
/// reachable=true / status=ok / zero-count shape.
/// Test: this is the test.
#[tokio::test]
async fn execute_health_reports_fleet() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let out = execute_verb(&control, "sessions.health", &json!({}))
        .await
        .expect("health dispatches");
    assert_eq!(out.get("reachable").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(out.get("status").and_then(|v| v.as_str()), Some("ok"));
    assert_eq!(out.get("managed_total").and_then(|v| v.as_u64()), Some(0));
    assert_eq!(
        out.get("managed_pending_decisions")
            .and_then(|v| v.as_u64()),
        Some(0)
    );
}

/// Why: the unknown-verb error must now name `sessions.health` among the valid set
/// so a model that mistypes it can recover by picking the real ops verb.
/// What: triggers the unknown-verb error and asserts `sessions.health` is listed.
/// Test: this is the test.
#[tokio::test]
async fn unknown_verb_error_lists_health() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let err = execute_verb(&control, "sessions.bogus", &json!({}))
        .await
        .expect_err("unknown verb errors");
    assert!(err.to_string().contains("sessions.health"));
}

/// Why: `sessions.decommission` (#1524) must be listed in the prompt so the
/// self-aware coordinator knows it can perform terminal teardown inline.
/// What: asserts the instruction block includes `sessions.decommission`.
/// Test: this is the test.
#[test]
fn prompt_lists_decommission_ops_verb() {
    let prompt = action_instructions();
    assert!(
        prompt.contains("sessions.decommission"),
        "action prompt must advertise the executable `sessions.decommission` verb"
    );
}

/// Why: `sessions.inject` (#1524) must be listed in the prompt so the self-aware
/// coordinator knows it can inject text with submit semantics inline.
/// What: asserts the instruction block includes `sessions.inject` and its submit
/// convention.
/// Test: this is the test.
#[test]
fn prompt_lists_inject_ops_verb() {
    let prompt = action_instructions();
    assert!(
        prompt.contains("sessions.inject"),
        "action prompt must advertise the executable `sessions.inject` verb"
    );
    assert!(
        prompt.contains("args.submit"),
        "action prompt must document the inject submit arg"
    );
}

/// Why: `sessions.decommission` (#1524) must EXECUTE inline in the action loop —
/// not just be advertised — calling `SessionControl::decommission`.
/// What: dispatches `sessions.decommission` against the mock and asserts `ok: true`.
/// Test: this is the test.
#[tokio::test]
async fn execute_decommission_dispatches() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let out = execute_verb(
        &control,
        "sessions.decommission",
        &json!({ "session_id": "abc-123" }),
    )
    .await
    .expect("decommission dispatches");
    assert_eq!(
        out.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "decommission must return {{ok: true}}"
    );
}

/// Why: `sessions.decommission` without a session_id must feed a clear error back
/// to the model rather than panicking.
/// What: dispatches with no args and asserts a NotFound error naming `session_id`.
/// Test: this is the test.
#[tokio::test]
async fn execute_decommission_requires_session_id() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let err = execute_verb(&control, "sessions.decommission", &json!({}))
        .await
        .expect_err("missing session_id errors");
    assert!(err.to_string().contains("session_id"));
}

/// Why: `sessions.inject` (#1524) must EXECUTE inline: read `session_id` + `text`
/// + optional `submit` and reach `SessionControl::inject_text`.
/// What: dispatches with `submit` omitted (default `enter`) and asserts the mock
/// recorded the send.
/// Test: this is the test.
#[tokio::test]
async fn execute_inject_dispatches_with_default_submit() {
    let mock = Arc::new(MockSessionControl::default());
    let control: Arc<dyn SessionControl> = mock.clone();
    let out = execute_verb(
        &control,
        "sessions.inject",
        &json!({ "session_id": "sid-1", "text": "cargo test" }),
    )
    .await
    .expect("inject dispatches");
    assert_eq!(
        out.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "inject must return {{ok: true}}"
    );
    // The mock records inject_text calls as sends.
    let sends = mock.sends();
    assert_eq!(sends.len(), 1, "exactly one inject recorded");
    assert_eq!(sends[0].0, "sid-1");
    assert_eq!(sends[0].1, "cargo test");
}

/// Why: `sessions.inject` with an explicit `submit` arg must parse the variant
/// and forward it to `inject_text`; the mock records it identically to `send`.
/// What: dispatches with `submit: "no_submit"` and asserts the text was injected.
/// Test: this is the test.
#[tokio::test]
async fn execute_inject_accepts_no_submit_variant() {
    let mock = Arc::new(MockSessionControl::default());
    let control: Arc<dyn SessionControl> = mock.clone();
    execute_verb(
        &control,
        "sessions.inject",
        &json!({ "session_id": "sid-2", "text": "partial cmd", "submit": "no_submit" }),
    )
    .await
    .expect("inject no_submit dispatches");
    let sends = mock.sends();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].1, "partial cmd");
}

/// Why: `sessions.inject` without a `session_id` or `text` must error cleanly.
/// What: tests both missing fields and asserts the error names the missing arg.
/// Test: this is the test.
#[tokio::test]
async fn execute_inject_requires_session_id_and_text() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());

    let err = execute_verb(&control, "sessions.inject", &json!({ "text": "hi" }))
        .await
        .expect_err("missing session_id errors");
    assert!(err.to_string().contains("session_id"));

    let err = execute_verb(
        &control,
        "sessions.inject",
        &json!({ "session_id": "sid-3" }),
    )
    .await
    .expect_err("missing text errors");
    assert!(err.to_string().contains("text"));
}

/// Why: the unknown-verb error must name the new ops verbs (`decommission`,
/// `inject`) so a model that mistypes either can recover.
/// What: triggers the unknown-verb error and asserts both are listed.
/// Test: this is the test.
#[tokio::test]
async fn unknown_verb_error_lists_new_ops_verbs() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let err = execute_verb(&control, "sessions.bogus", &json!({}))
        .await
        .expect_err("unknown verb errors");
    let msg = err.to_string();
    assert!(
        msg.contains("sessions.decommission"),
        "error must list decommission"
    );
    assert!(msg.contains("sessions.inject"), "error must list inject");
}

/// Why: the action loop must ADVERTISE `sessions.adopt` so the self-aware chat
/// knows it can adopt an existing tmux session inline (#1433).
/// What: asserts the instruction block lists the ops `sessions.adopt` verb and its
/// argument convention.
/// Test: this is the test.
#[test]
fn prompt_lists_adopt_ops_verb() {
    let prompt = action_instructions();
    assert!(
        prompt.contains("sessions.adopt"),
        "action prompt must advertise the executable `sessions.adopt` verb"
    );
    assert!(
        prompt.contains("args.tmux_name"),
        "action prompt must document the adopt args"
    );
}

/// Why: `sessions.adopt` must EXECUTE inline, parsing `tmux_name`/`cwd`/`task`/
/// `runtime` and reaching `SessionControl::adopt` (#1433).
/// What: dispatches `sessions.adopt` against the mock and asserts the args were
/// parsed and forwarded, and a session id comes back.
/// Test: this is the test.
#[tokio::test]
async fn execute_adopt_reads_args() {
    let mock = Arc::new(MockSessionControl::default());
    let control: Arc<dyn SessionControl> = mock.clone();
    let out = execute_verb(
        &control,
        "sessions.adopt",
        &json!({
            "tmux_name": "tmpm-hand-started",
            "cwd": "/Users/op/work/proj",
            "task": "drive it",
            "runtime": "claude-code"
        }),
    )
    .await
    .expect("adopt dispatches");
    assert!(
        out.get("session_id").and_then(|v| v.as_str()).is_some(),
        "adopt must return a session_id"
    );
    let adopts = mock.adopts();
    assert_eq!(adopts.len(), 1, "exactly one adopt recorded");
    assert_eq!(adopts[0].0, "tmpm-hand-started");
    assert_eq!(adopts[0].1, "/Users/op/work/proj");
    assert_eq!(adopts[0].2.as_deref(), Some("drive it"));
    assert_eq!(adopts[0].3.as_deref(), Some("claude-code"));
}

/// Why: `sessions.adopt` requires `tmux_name` and `cwd`; a missing one must feed a
/// clear error back to the model rather than panicking (#1433).
/// What: dispatches adopt with no args and asserts a Backend error naming the
/// missing required field.
/// Test: this is the test.
#[tokio::test]
async fn execute_adopt_requires_tmux_name_and_cwd() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());

    let err = execute_verb(&control, "sessions.adopt", &json!({ "cwd": "/x" }))
        .await
        .expect_err("missing tmux_name errors");
    assert!(err.to_string().contains("tmux_name"));

    let err = execute_verb(
        &control,
        "sessions.adopt",
        &json!({ "tmux_name": "tmpm-x" }),
    )
    .await
    .expect_err("missing cwd errors");
    assert!(err.to_string().contains("cwd"));
}

/// Why: the unknown-verb error must name `sessions.adopt` among the valid set so a
/// model that mistypes it can recover (#1433).
/// What: triggers the unknown-verb error and asserts `sessions.adopt` is listed.
/// Test: this is the test.
#[tokio::test]
async fn unknown_verb_error_lists_adopt() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let err = execute_verb(&control, "sessions.bogus", &json!({}))
        .await
        .expect_err("unknown verb errors");
    assert!(err.to_string().contains("sessions.adopt"));
}

/// Why: `sessions.list` must dispatch to `SessionControl::list`.
/// What: executes the verb and asserts the mock's list body comes back.
/// Test: this is the test.
#[tokio::test]
async fn execute_dispatches_list() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let out = execute_verb(&control, "sessions.list", &json!({}))
        .await
        .expect("list dispatches");
    assert!(out.get("sessions").is_some());
}

/// Why: `sessions.send` must read `session_id` + `text` and reach `send`.
/// What: executes the verb and asserts the mock recorded the send.
/// Test: this is the test.
#[tokio::test]
async fn execute_send_reads_args() {
    let mock = Arc::new(MockSessionControl::default());
    let control: Arc<dyn SessionControl> = mock.clone();
    execute_verb(
        &control,
        "sessions.send",
        &json!({"session_id":"abc","text":"hello"}),
    )
    .await
    .expect("send dispatches");
    let sends = mock.sends();
    assert_eq!(sends, vec![("abc".to_string(), "hello".to_string())]);
}

/// Why: `sessions.get` without an id must error (fed back to the model), not panic.
/// What: executes `sessions.get` with empty args and asserts an error.
/// Test: this is the test.
#[tokio::test]
async fn execute_get_requires_id() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let err = execute_verb(&control, "sessions.get", &json!({}))
        .await
        .expect_err("missing id errors");
    assert!(err.to_string().contains("session_id"));
}

/// Why: an unknown verb must be a recoverable error fed back to the model, never
/// a panic or a silent no-op.
/// What: executes a bogus verb and asserts the error names the valid set.
/// Test: this is the test.
#[tokio::test]
async fn execute_unknown_verb_errors() {
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let err = execute_verb(&control, "sessions.frobnicate", &json!({}))
        .await
        .expect_err("unknown verb errors");
    assert!(err.to_string().contains("unknown verb"));
    assert!(err.to_string().contains("sessions.list"));
}

/// Why: WI-A #1585 — `sessions.launch` must thread an explicit `repo_url` and
/// `ref_` from the action-chat args into the [`LaunchParams`] it passes to
/// `SessionControl::launch`, so the spawn path can provision against the exact
/// canonical URL and git ref the multi-project context supplied.
/// What: dispatches `sessions.launch` with `repo_url` + `ref_` in args, then
/// reads the mock's recorded launch and asserts those fields were forwarded.
/// Test: this is the test.
#[tokio::test]
async fn execute_launch_threads_repo_url_and_ref_() {
    let mock = Arc::new(MockSessionControl::default());
    let control: Arc<dyn SessionControl> = mock.clone();
    let out = execute_verb(
        &control,
        "sessions.launch",
        &json!({
            "workdir": "/local/fallback",
            "repo_url": "https://github.com/example/repo",
            "ref_": "feat/my-branch",
            "prompt": "run tests",
        }),
    )
    .await
    .expect("launch with repo_url+ref_ dispatches");
    assert!(
        out.get("session_id").and_then(|v| v.as_str()).is_some(),
        "launch must return a session_id"
    );
    let launches = mock.launches();
    assert_eq!(launches.len(), 1, "exactly one launch recorded");
    let (_, params) = &launches[0];
    assert_eq!(
        params.repo_url.as_deref(),
        Some("https://github.com/example/repo"),
        "repo_url must be threaded through LaunchParams"
    );
    assert_eq!(
        params.ref_.as_deref(),
        Some("feat/my-branch"),
        "ref_ must be threaded through LaunchParams"
    );
    assert_eq!(params.workdir, "/local/fallback");
    assert_eq!(params.prompt.as_deref(), Some("run tests"));
}

/// Why: WI-A #1585 — when `repo_url`/`ref_` are absent from the action-chat
/// args the launch must still succeed with `None` in those fields (backward-
/// compatible, no regression against existing callers that only provide workdir).
/// What: dispatches `sessions.launch` with only `workdir` and `prompt`, then
/// asserts `repo_url` and `ref_` are `None` in the recorded params.
/// Test: this is the test.
#[tokio::test]
async fn execute_launch_repo_url_and_ref_default_to_none() {
    let mock = Arc::new(MockSessionControl::default());
    let control: Arc<dyn SessionControl> = mock.clone();
    execute_verb(
        &control,
        "sessions.launch",
        &json!({ "workdir": "/some/dir", "prompt": "do work" }),
    )
    .await
    .expect("launch without repo_url dispatches");
    let launches = mock.launches();
    assert_eq!(launches.len(), 1);
    let (_, params) = &launches[0];
    assert!(
        params.repo_url.is_none(),
        "repo_url must be None when not supplied"
    );
    assert!(params.ref_.is_none(), "ref_ must be None when not supplied");
}
