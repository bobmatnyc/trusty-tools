//! Tests for `channel_send.rs` (#10423): the daemon's send wiring and its
//! Fail-Open error arms. Every arm asserts that nothing was posted.

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;
use trusty_common::credentials::Secret;

use super::channel_send::*;
use crate::channels::approval::ApprovalVia;
use crate::channels::orgs_tests::INITIAL_TOML;
use crate::channels::send_store::{orgs_path, send_audit_path, table_path};
use crate::channels::slack_bot::{Posted, SendFailure, SlackPost};
use crate::channels::staged_send::{Decision, SecretResolver, SendState, StagedSends, digest_of};

const TEXT: &str = "the staged text";

/// Records every post as `(channel, text)`.
#[derive(Default)]
struct Poster(Mutex<Vec<(String, String)>>);
impl SlackPost for Poster {
    fn post(&self, _t: &Secret<String>, channel: &str, text: &str) -> Result<Posted, SendFailure> {
        self.0
            .lock()
            .expect("lock")
            .push((channel.to_owned(), text.to_owned()));
        Ok(Posted {
            channel: "D1".into(),
            ts: "1.0".into(),
        })
    }
}
impl Poster {
    fn sends_to_users(&self) -> usize {
        // The owner's DMs are approval requests; anything else is a send.
        let posts = self.0.lock().expect("lock");
        posts
            .iter()
            .filter(|(_, t)| !t.starts_with("Approve send"))
            .count()
    }
}

/// Resolves every ref, or fails every one.
struct Resolver(bool);
impl SecretResolver for Resolver {
    fn resolve(&self, _s: Option<&str>, _r: &str) -> Result<Secret<String>, String> {
        if self.0 {
            Ok(Secret::new("tok".into()))
        } else {
            Err("credential store fault".into())
        }
    }
}

fn env(home: &Path, resolves: bool) -> SendEnv<Poster, Resolver> {
    SendEnv {
        home: home.to_path_buf(),
        poster: Poster::default(),
        resolver: Resolver(resolves),
    }
}

fn configured() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = orgs_path(dir.path());
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, INITIAL_TOML).expect("write");
    dir
}

fn req() -> SendRequest {
    SendRequest {
        user: "bob-duetto".into(),
        text: TEXT.into(),
        via_org: None,
    }
}

fn approve() -> Decision {
    Decision::Approve {
        digest: digest_of(TEXT),
    }
}

const OWNER: (&str, &str) = ("duetto", "U0OWNER01");

#[test]
fn send_stages_and_asks_the_owner_dm_then_posts_once_on_approve() {
    let home = configured();
    let env = env(home.path(), true);
    let mut slot = None;
    let staged = env.stage(&mut slot, &req(), 1).expect("staged");
    assert_eq!(staged.approval, ApprovalVia::OwnerDm);
    assert_eq!(
        env.poster.sends_to_users(),
        0,
        "nothing posts before Approve"
    );
    let state = env.decide(&mut slot, OWNER, staged.id, &approve(), 2);
    assert_eq!(state, Ok(SendState::Sent));
    assert_eq!(env.poster.sends_to_users(), 1);
    let audit = std::fs::read_to_string(send_audit_path(home.path())).expect("audit");
    assert!(!audit.contains(TEXT), "{audit}");
    // A restart (an empty slot) reloads the table; the send stays decided.
    let mut fresh = None;
    let again = env.decide(&mut fresh, OWNER, staged.id, &approve(), 3);
    assert_eq!(again.map_err(|e| e.code()), Err("already_decided"));
    assert_eq!(env.poster.sends_to_users(), 1);
}

/// #10423 Fail-Open regression: an unauthenticated Approve posts nothing.
#[test]
fn unauthenticated_approve_posts_nothing_through_the_daemon() {
    let home = configured();
    let env = env(home.path(), true);
    let mut slot = None;
    let staged = env.stage(&mut slot, &req(), 1).expect("staged");
    for who in [("duetto", "U0OTHER01"), ("bta", "U0OWNER01")] {
        let err = env
            .decide(&mut slot, who, staged.id, &approve(), 2)
            .expect_err("refused");
        assert_eq!(err.code(), "approver_unauthenticated");
    }
    assert_eq!(env.poster.sends_to_users(), 0);
    let table = slot.as_ref().expect("loaded");
    assert_eq!(
        table.get(staged.id).map(|s| &s.state),
        Some(&SendState::Staged)
    );
}

/// #10423 Fail-Open regression: a credential that cannot resolve posts
/// nothing; the approval falls back to the dashboard.
#[test]
fn resolve_failure_posts_nothing_through_the_daemon() {
    let home = configured();
    let env = env(home.path(), false);
    let mut slot = None;
    let staged = env.stage(&mut slot, &req(), 1).expect("staged");
    assert_eq!(
        staged.approval,
        ApprovalVia::Dashboard("credential_unavailable".into())
    );
    let state = env.decide(&mut slot, OWNER, staged.id, &approve(), 2);
    assert_eq!(
        state,
        Ok(SendState::NotSent("credential_unavailable".into()))
    );
    assert!(env.poster.0.lock().expect("lock").is_empty());
}

/// #10423 Fail-Open regression: a table that cannot load refuses the send
/// and the Approve, and is not cached.
#[test]
fn table_load_failure_refuses_and_posts_nothing() {
    let home = configured();
    std::fs::write(table_path(home.path()), "{ corrupt").expect("write");
    let env = env(home.path(), true);
    let mut slot: Option<StagedSends> = None;
    let err = env.stage(&mut slot, &req(), 1).expect_err("refused");
    assert_eq!(err.code(), "table_unavailable");
    let err = env
        .decide(&mut slot, OWNER, 1, &approve(), 2)
        .expect_err("refused");
    assert_eq!(err.code(), "table_unavailable");
    assert!(slot.is_none());
    assert!(env.poster.0.lock().expect("lock").is_empty());
}

/// #10423 Fail-Open regression: an audit write failure stages nothing and
/// asks nobody.
#[test]
fn audit_failure_refuses_and_posts_nothing() {
    let home = configured();
    // A directory where the audit file must go makes every append fail.
    std::fs::create_dir_all(send_audit_path(home.path())).expect("mkdir");
    let env = env(home.path(), true);
    let mut slot = None;
    let err = env.stage(&mut slot, &req(), 1).expect_err("refused");
    assert_eq!(err.code(), "audit_unavailable");
    assert!(env.poster.0.lock().expect("lock").is_empty());
}

#[test]
fn missing_config_refuses_and_posts_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    let env = env(home.path(), true);
    let err = env.stage(&mut None, &req(), 1).expect_err("refused");
    assert_eq!(err.code(), "config_unavailable");
    assert!(env.poster.0.lock().expect("lock").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn http_send_route_stages_and_refuses_without_config() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical");
    let state = Arc::new(crate::daemon::DaemonState::with_root_isolated_managed(root).await);
    let body = serde_json::json!({ "user": "bob-duetto", "text": TEXT });
    let request = Request::builder()
        .method("POST")
        .uri(SEND_ROUTE)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = router()
        .with_state(state)
        .oneshot(request)
        .await
        .expect("the router answers");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON body");
    assert_eq!(v["error"], "config_unavailable", "{v}");
    assert!(!v.to_string().contains(TEXT), "{v}");
}

/// #10423 regression: Approve resolves the route from the current
/// `orgs.toml`; a narrowed `secrets` scope refuses the staged send.
#[test]
fn approve_after_scope_change_is_refused_and_posts_nothing() {
    let home = configured();
    let env = env(home.path(), true);
    let mut slot = None;
    let staged = env.stage(&mut slot, &req(), 1).expect("staged");
    let narrowed = INITIAL_TOML.replace(
        "secrets = \"duettoresearch\"",
        "secrets = \"duettoresearch/mpm\"",
    );
    std::fs::write(orgs_path(home.path()), narrowed).expect("write");
    let err = env
        .decide(&mut slot, OWNER, staged.id, &approve(), 2)
        .expect_err("refused");
    assert_eq!(err.code(), "route_changed");
    assert_eq!(env.poster.sends_to_users(), 0);
    let table = slot.as_ref().expect("loaded");
    assert_eq!(
        table.get(staged.id).map(|s| &s.state),
        Some(&SendState::Staged)
    );
}
