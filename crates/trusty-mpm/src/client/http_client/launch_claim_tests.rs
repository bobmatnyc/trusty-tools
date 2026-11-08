//! Tests for the launch name rule (#10101 D3c) and the TUI `/connect` and
//! `/launch` paths that read it.
//!
//! Why: the rule decides whether a PM launch runs, resumes or refuses; the
//! TUI paths must launch where the CLI does and never type into a session
//! they did not create.
//! What: pure checks of [`name_holder`] / [`legacy_name_holder`], then
//! [`DaemonClient`] calls against a stub daemon with a recording fake tmux.
//! Test: this file IS the test.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::*;

/// One managed summary as the daemon sends it.
fn summary(name: &str, state: &str, persisted: Option<&str>) -> ManagedSessionSummary {
    serde_json::from_value(serde_json::json!({
        "id": format!("{name}-{state}-id"),
        "name": name,
        "state": state,
        "persisted_state": persisted,
    }))
    .expect("summary")
}

/// #10101: a non-terminal record holds its name; a decommissioned or deleted
/// one does not; the persisted state decides; an unknown state holds.
#[test]
fn a_live_managed_record_holds_its_name() {
    let held = [summary("tm-proj", "active", None)];
    assert_eq!(
        name_holder(&held, "tm-proj").map(|s| s.id.as_str()),
        Some("tm-proj-active-id")
    );
    assert!(name_holder(&held, "tm-other").is_none());
    for state in ["decommissioned", "deleted"] {
        assert!(name_holder(&[summary("tm-proj", state, None)], "tm-proj").is_none());
    }
    let tombstone = [summary("tm-proj", "stopped", Some("decommissioned"))];
    assert!(
        name_holder(&tombstone, "tm-proj").is_none(),
        "persisted wins"
    );
    let stopped = [summary("tm-proj", "active", Some("stopped"))];
    assert!(
        name_holder(&stopped, "tm-proj").is_some(),
        "stopped still holds"
    );
    let unknown = [summary("tm-proj", "some-future-state", None)];
    assert!(
        name_holder(&unknown, "tm-proj").is_some(),
        "unknown fails closed"
    );
}

/// One legacy row as `GET /sessions` sends it.
fn legacy(name: &str, status: Option<&str>, workdir: &str) -> serde_json::Value {
    serde_json::json!({
        "id": format!("{name}-legacy-id"),
        "workdir": workdir,
        "tmux_name": name,
        "status": status,
    })
}

/// #10101 D3c: a legacy record holds its name unless it is `Stopped`; an
/// unknown or missing status holds (fail closed).
#[test]
fn a_live_legacy_record_holds_its_name_10101() {
    let rows = |v: serde_json::Value| -> Vec<LegacyRow> {
        serde_json::from_value(serde_json::json!([v])).expect("rows")
    };
    for status in [Some("Active"), Some("Starting"), Some("SomeFuture"), None] {
        let r = rows(legacy("tm-proj", status, "/w"));
        assert!(legacy_name_holder(&r, "tm-proj").is_some(), "{status:?}");
        assert!(legacy_name_holder(&r, "tm-other").is_none());
    }
    let r = rows(legacy("tm-proj", Some("Stopped"), "/w"));
    assert!(legacy_name_holder(&r, "tm-proj").is_none());
}

/// What one stub daemon saw.
#[derive(Default)]
struct Seen {
    /// `(path, body)` of every registration POST.
    posts: Vec<(String, serde_json::Value)>,
    /// Ids of every `DELETE /sessions/{id}`.
    deletes: Vec<String>,
    /// Ids of every managed resume.
    resumes: Vec<String>,
}

/// A stub daemon: `managed` and `legacy` lists, registration under the
/// posted name with a fixed id, delete, and a resume answering `resume`
/// (status, body).
async fn spawn_stub(
    managed: Vec<serde_json::Value>,
    legacy: Vec<serde_json::Value>,
    resume: (u16, serde_json::Value),
) -> (Arc<Mutex<Seen>>, String) {
    use axum::{
        Json, Router,
        extract::Path as P,
        http::{StatusCode, Uri},
        routing::{delete, get, post},
    };
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let (on_post, on_delete, on_resume) = (seen.clone(), seen.clone(), seen.clone());
    let managed = serde_json::json!({ "sessions": managed });
    let legacy = serde_json::json!({ "sessions": legacy });
    let register = move |uri: Uri, Json(body): Json<serde_json::Value>| {
        let seen = on_post.clone();
        async move {
            let name = body["name"].as_str().unwrap_or("tm-stub").to_owned();
            seen.lock()
                .expect("seen")
                .posts
                .push((uri.path().to_owned(), body));
            Json(serde_json::json!({
                "id": "33333333-3333-3333-3333-333333333333",
                "name": name,
            }))
        }
    };
    let router = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route(
            "/api/v1/sessions/managed",
            get(move || {
                let m = managed.clone();
                async move { Json(m) }
            }),
        )
        .route(
            "/sessions",
            get(move || {
                let l = legacy.clone();
                async move { Json(l) }
            })
            .post(register.clone()),
        )
        .route("/api/v1/sessions/connect", post(register))
        .route(
            "/sessions/{id}",
            delete(move |P(id): P<String>| {
                let seen = on_delete.clone();
                async move {
                    seen.lock().expect("seen").deletes.push(id.clone());
                    Json(serde_json::json!({ "removed": id }))
                }
            }),
        )
        .route(
            "/api/v1/sessions/managed/{id}/resume",
            post(move |P(id): P<String>| {
                let seen = on_resume.clone();
                let (code, body) = resume.clone();
                async move {
                    seen.lock().expect("seen").resumes.push(id);
                    (StatusCode::from_u16(code).expect("status"), Json(body))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });
    (seen, format!("http://{addr}"))
}

/// A fake `tmux` under `dir` logging each argv to `<dir>/argv.log`; a call
/// whose argv contains `fail_on` exits 1, every other call 0.
fn fake_tmux(dir: &Path, fail_on: &str) -> (PathBuf, PathBuf) {
    let log = dir.join("argv.log");
    let bin = dir.join("fake-tmux");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in *{fail_on}*) exit 1;; esac\nexit 0\n",
        log.display()
    );
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    (bin, log)
}

/// The logged tmux argv lines.
fn logged(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A resume reply: the summary the daemon answers with.
fn resumed(name: &str, state: &str) -> (u16, serde_json::Value) {
    (
        200,
        serde_json::json!({ "id": "held-id-10101", "name": name, "state": state }),
    )
}

/// #10101 D3c: `Stopped` and `Errored` holders are resumed; an `Active` or
/// unknown-state holder refuses, naming the holder.
#[tokio::test]
async fn a_stopped_holder_is_resumed_not_launched_10101() {
    let dir = Path::new("/w/proj");
    for (state, resumes) in [
        ("stopped", true),
        ("errored", true),
        ("active", false),
        ("provisioning", false),
        ("some-future-state", false),
    ] {
        let holder =
            serde_json::json!({ "id": "held-id-10101", "name": "tm-proj", "state": state });
        let (_seen, url) = spawn_stub(vec![holder], vec![], resumed("tm-proj", "active")).await;
        let claim = DaemonClient::new(url)
            .claim_launch_name("tm-proj", dir)
            .await;
        match (resumes, claim) {
            (true, Ok(LaunchClaim::Resume(h))) => assert_eq!(h.id, "held-id-10101"),
            (false, Err(e)) => assert!(e.to_string().contains("held-id-10101"), "{e}"),
            (_, other) => panic!("{state}: {other:?}"),
        }
    }
}

/// #10101 D3c: an unreachable daemon refuses the claim and the probe.
#[tokio::test]
async fn an_unreachable_daemon_refuses_the_claim_10101() {
    let client = DaemonClient::new("http://127.0.0.1:1");
    let err = client
        .claim_launch_name("tm-proj", Path::new("/w"))
        .await
        .expect_err("refused");
    assert!(err.to_string().contains("cannot be reached"), "{err}");
    let err = client.require_reachable().await.expect_err("refused");
    assert!(err.to_string().contains("cannot be reached"), "{err}");
}

/// Run `connect_session` (or `launch_session` when `launch`) for `project`
/// against `url`, every tmux call going to `bin`.
async fn tui(
    url: &str,
    project: &Path,
    home: &Path,
    bin: PathBuf,
    launch: bool,
) -> anyhow::Result<String> {
    let client = DaemonClient::new(url).with_home(home);
    let workdir = project.to_string_lossy().into_owned();
    crate::core::tmux::with_tmux_binary(bin, async {
        if launch {
            client.launch_session(&workdir).await
        } else {
            client.connect_session(&workdir).await
        }
    })
    .await
}

/// The workspace server the CLI rule names for `dir`, or `None` when this
/// environment opts out (`TM_WORKSPACE_TMUX=0`).
fn expected_server(dir: &Path) -> Option<(String, PathBuf)> {
    let opted_out = std::env::var("TM_WORKSPACE_TMUX").is_ok_and(|v| v.trim() == "0");
    (!opted_out).then(|| {
        let server = crate::core::tmux_server::workspace_server_name(dir);
        let socket = crate::core::tmux_server::ambient_socket_dir().join(&server);
        (server, socket)
    })
}

/// #10101 D3c: the TUI `/connect` creates the PM on its workspace server and
/// its registration carries that server's socket. Red before: no `-L`, no
/// socket.
#[serial_test::serial]
#[tokio::test]
async fn the_tui_connect_launches_on_the_workspace_server_10101() {
    let home = crate::test_support::hermetic_temp_dir();
    let project = crate::test_support::hermetic_temp_dir();
    let (bin, log) = fake_tmux(project.path(), "has-session");
    let (seen, url) = spawn_stub(vec![], vec![], resumed("x", "active")).await;
    tui(&url, project.path(), home.path(), bin, false)
        .await
        .expect("the fake accepts the launch");
    let posts = std::mem::take(&mut seen.lock().expect("seen").posts);
    assert_eq!(posts.len(), 1, "{posts:?}");
    let lines = logged(&log);
    assert!(lines.iter().any(|l| l.contains("send-keys")), "{lines:?}");
    match expected_server(project.path()) {
        Some((server, socket)) => {
            assert_eq!(posts[0].1["tmux_socket"], serde_json::json!(socket));
            assert!(
                lines
                    .iter()
                    .all(|l| l.starts_with(&format!("-L {server} "))),
                "every call must reach {server}: {lines:?}"
            );
        }
        None => assert!(posts[0].1["tmux_socket"].is_null(), "{posts:?}"),
    }
}

/// #10101 D3c: a TUI `/launch` whose session already exists on the server
/// never types the `claude` line into it and keeps the registration. Red
/// before: `launch_session` typed into the existing pane.
#[serial_test::serial]
#[tokio::test]
async fn the_tui_launch_never_types_into_a_session_it_did_not_create_10101() {
    let home = crate::test_support::hermetic_temp_dir();
    let project = crate::test_support::hermetic_temp_dir();
    // Every call succeeds: `has-session` answers that the session exists.
    let (bin, log) = fake_tmux(project.path(), "no-call-fails");
    let (seen, url) = spawn_stub(vec![], vec![], resumed("x", "active")).await;
    tui(&url, project.path(), home.path(), bin, true)
        .await
        .expect("an existing session is left in place");
    let lines = logged(&log);
    assert!(lines.iter().any(|l| l.contains("has-session")), "{lines:?}");
    assert!(
        !lines.iter().any(|l| l.contains("send-keys")),
        "typed into a session it did not create: {lines:?}"
    );
    assert!(seen.lock().expect("seen").deletes.is_empty());
}

/// #10101 D3c: a TUI create that fails after the registration removes it.
/// Red before: the record was left behind.
#[serial_test::serial]
#[tokio::test]
async fn a_failed_tui_create_removes_the_registration_10101() {
    let project = crate::test_support::hermetic_temp_dir();
    let home = crate::test_support::hermetic_temp_dir();
    let (bin, _log) = fake_tmux(project.path(), "-");
    let (seen, url) = spawn_stub(vec![], vec![], resumed("x", "active")).await;
    let err = tui(&url, project.path(), home.path(), bin, false)
        .await
        .expect_err("a failed create fails the connect");
    assert!(format!("{err:#}").contains("was removed"), "{err:#}");
    assert_eq!(
        seen.lock().expect("seen").deletes,
        ["33333333-3333-3333-3333-333333333333"]
    );
}

/// #10101 D3c: a TUI `/connect` under a name a live legacy record of another
/// directory holds is refused before any registration or tmux call. Red
/// before: the TUI registered a second record and launched.
#[serial_test::serial]
#[tokio::test]
async fn a_live_legacy_record_refuses_the_launch_10101() {
    let home = crate::test_support::hermetic_temp_dir();
    let project = crate::test_support::hermetic_temp_dir();
    let name = crate::core::names::name_from_dir(project.path());
    let (bin, log) = fake_tmux(project.path(), "has-session");
    let row = legacy(&name, Some("Active"), "/elsewhere");
    let (seen, url) = spawn_stub(vec![], vec![row], resumed("x", "active")).await;
    let err = tui(&url, project.path(), home.path(), bin, false)
        .await
        .expect_err("a held name is refused");
    assert!(err.to_string().contains("-legacy-id"), "{err}");
    assert!(seen.lock().expect("seen").posts.is_empty(), "registered");
    assert!(logged(&log).is_empty(), "tmux ran: {:?}", logged(&log));
}

/// #10101 D3c: a TUI `/connect` under a name a `Stopped` managed record
/// holds resumes that record (its id) and registers and creates nothing.
/// Red before: the TUI registered a new record and launched a second PM.
#[serial_test::serial]
#[tokio::test]
async fn the_tui_resumes_a_stopped_holder_10101() {
    let home = crate::test_support::hermetic_temp_dir();
    let project = crate::test_support::hermetic_temp_dir();
    let name = crate::core::names::name_from_dir(project.path());
    let (bin, log) = fake_tmux(project.path(), "has-session");
    let holder = serde_json::json!({ "id": "held-id-10101", "name": name, "state": "stopped" });
    let (seen, url) = spawn_stub(vec![holder], vec![], resumed(&name, "active")).await;
    let got = tui(&url, project.path(), home.path(), bin, false)
        .await
        .expect("resumed");
    assert_eq!(got, name);
    let seen = seen.lock().expect("seen");
    assert_eq!(seen.resumes, ["held-id-10101"]);
    assert!(seen.posts.is_empty(), "registered: {:?}", seen.posts);
    assert!(logged(&log).is_empty(), "tmux ran: {:?}", logged(&log));
}
