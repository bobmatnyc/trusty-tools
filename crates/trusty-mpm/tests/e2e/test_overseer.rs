//! E2E: session overseer — file-backed policy, hook enforcement.
//!
//! Most scenarios drive the overseer through the real `POST /hooks` relay.
//! The auto-responder scenario is the exception: there is no `SessionQuestion`
//! wire event for the relay to dispatch, so that case exercises the overseer's
//! `session_question` strategy directly against a policy loaded from a planted
//! `overseer.toml` — still the file -> config -> overseer path, end to end.

use crate::harness::{TestDaemon, write_overseer_toml};
use serde_json::{Value, json};
use trusty_mpm::core::deterministic_overseer::DeterministicOverseer;
use trusty_mpm::core::overseer::{Overseer, OverseerContext, OverseerDecision};
use trusty_mpm::core::overseer_config::OverseerConfig;
use trusty_mpm::core::session::SessionId;

/// With no `overseer.toml`, oversight is disabled by default.
#[tokio::test]
async fn overseer_disabled_by_default() {
    let daemon = TestDaemon::spawn().await;
    let body: Value = daemon
        .client()
        .get(daemon.url("/overseer"))
        .send()
        .await
        .expect("overseer request")
        .json()
        .await
        .expect("overseer body");
    assert_eq!(body["overseer"]["enabled"], false);
    assert_eq!(body["overseer"]["handler"], "deterministic");
}

/// A planted `overseer.toml` with `enabled = true` is reflected by the API.
#[tokio::test]
async fn overseer_enabled_config() {
    let daemon = TestDaemon::spawn_with(|paths| {
        write_overseer_toml(paths, "[overseer]\nenabled = true\n");
    })
    .await;

    let body: Value = daemon
        .client()
        .get(daemon.url("/overseer"))
        .send()
        .await
        .expect("overseer request")
        .json()
        .await
        .expect("overseer body");
    assert_eq!(body["overseer"]["enabled"], true);
}

/// With the overseer enabled and a blocklist entry, a `PreToolUse` event whose
/// tool input contains the blocked substring is refused with `403`.
#[tokio::test]
async fn blocklist_blocks_pretooluse() {
    let daemon = TestDaemon::spawn_with(|paths| {
        write_overseer_toml(
            paths,
            "[overseer]\nenabled = true\n\n[deterministic]\nblocklist = [\"FORBIDDEN\"]\n",
        );
    })
    .await;
    let client = daemon.client();

    let created: Value = client
        .post(daemon.url("/sessions"))
        .json(&json!({ "project": "/tmp/e2e-block" }))
        .send()
        .await
        .expect("create session")
        .json()
        .await
        .expect("create body");
    let id = created["id"].as_str().expect("id present").to_string();

    let resp = client
        .post(daemon.url("/hooks"))
        .json(&json!({
            "session_id": id,
            "event": "PreToolUse",
            "payload": { "tool": "Bash", "input": "run the FORBIDDEN command" },
        }))
        .send()
        .await
        .expect("hook post");
    assert_eq!(resp.status(), 403);
}

/// With the overseer enabled and an auto-approve entry, a `PreToolUse` event
/// whose input matches is allowed (`200`) even though oversight is active.
#[tokio::test]
async fn auto_approve_allows_pretooluse() {
    let daemon = TestDaemon::spawn_with(|paths| {
        write_overseer_toml(
            paths,
            "[overseer]\nenabled = true\n\n\
             [deterministic]\nblocklist = [\"FORBIDDEN\"]\nauto_approve = [\"git status\"]\n",
        );
    })
    .await;
    let client = daemon.client();

    let created: Value = client
        .post(daemon.url("/sessions"))
        .json(&json!({ "project": "/tmp/e2e-approve" }))
        .send()
        .await
        .expect("create session")
        .json()
        .await
        .expect("create body");
    let id = created["id"].as_str().expect("id present").to_string();

    let resp = client
        .post(daemon.url("/hooks"))
        .json(&json!({
            "session_id": id,
            "event": "PreToolUse",
            "payload": { "tool": "Bash", "input": "git status" },
        }))
        .send()
        .await
        .expect("hook post");
    assert_eq!(resp.status(), 200);
}

/// A planted `overseer.toml` with `[auto_responses]` drives the overseer's
/// `session_question` strategy to a `Respond` decision for a matching pattern.
///
/// The hook relay has no `SessionQuestion` wire event, so this exercises the
/// file -> `OverseerConfig` -> `DeterministicOverseer` path directly — the same
/// boot path the daemon uses, against the same temp-scoped policy file.
#[tokio::test]
async fn auto_responder_answers_question() {
    let daemon = TestDaemon::spawn_with(|paths| {
        write_overseer_toml(
            paths,
            "[overseer]\nenabled = true\n\n\
             [auto_responses]\n\"shall i proceed\" = \"yes, proceed\"\n",
        );
    })
    .await;

    // Load the very policy file the daemon booted from and build the overseer.
    let config = OverseerConfig::load_from(&daemon.paths.overseer_config());
    assert!(config.enabled, "planted policy must be enabled");
    let overseer = DeterministicOverseer::new(config);

    let session = SessionId::new();
    let ctx = OverseerContext::new(session, "tmpm-test-session", None::<String>, None::<String>);
    let decision = overseer.session_question(&ctx, "Shall I proceed with the migration?");

    match decision {
        OverseerDecision::Respond { text } => assert_eq!(text, "yes, proceed"),
        other => panic!("expected a Respond decision, got {other:?}"),
    }
}
