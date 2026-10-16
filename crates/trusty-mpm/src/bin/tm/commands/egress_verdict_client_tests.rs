//! #8454 VC: the egress guard's daemon verdict client and its fallbacks.
//!
//! Why: the client is a Fail-Open Check site. Each test goes red if a case
//! that must run the inline load serves a verdict, if an unusable answer
//! falls through to the inline load or to ready, or if an untainted session
//! queries the daemon.
//! What: a one-connection daemon socket answers each canned frame (#6288: the
//! query travels over the socket only); an absent socket stands for a daemon
//! that is down. The inline load is a closure that counts its runs, or panics
//! where it must not run.
//! Test: this is the test module.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use trusty_mpm::core::channels_readiness::{HostBranch, NotReady, policy_ready};
use trusty_mpm::core::channels_taint::{TaintMark, taint_dir, taint_managed_session};
use trusty_mpm::core::egress_verdict::Unserved;

use super::*;
use crate::commands::egress_guard::{Verdict, with_taint_async};
use crate::commands::egress_guard_taint::{POLICY_CHECK, TAINT_CHECK};

const SESSION: &str = "s-tainted";

/// The home the hook sends; the fake daemon never reads it.
fn home() -> PathBuf {
    PathBuf::from("/home/tester")
}

/// One newline-terminated JSON-RPC frame.
fn frame(value: &Value) -> String {
    format!("{value}\n")
}

/// A result frame carrying `body`.
fn ok(body: &Value) -> String {
    frame(&json!({"jsonrpc": "2.0", "id": 1, "result": body}))
}

/// A result frame carrying `text` — as JSON when it parses, else as a string.
fn result_text(text: &str) -> String {
    let value = serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()));
    ok(&value)
}

/// An error frame: the refusal a daemon answers with `code`.
fn refusal(code: i64) -> String {
    frame(&json!({"jsonrpc": "2.0", "id": 1, "error": {"code": code, "message": "refused"}}))
}

/// A fake daemon on a hardened socket, and the locator a client dials.
struct Fake {
    url: String,
    server: tokio::task::JoinHandle<String>,
    _dir: tempfile::TempDir,
}

/// A daemon socket that answers one request with `response`; the handle
/// yields the request frame it read. `None` holds the connection unanswered.
async fn serve_once(response: Option<String>) -> Fake {
    match response {
        Some(response) => serve_raw(response, false).await,
        None => serve_raw(String::new(), true).await,
    }
}

/// A daemon socket that reads one request frame, writes `bytes` (any prefix
/// of a reply, or nothing), then holds the connection open when `stall` or
/// closes it.
async fn serve_raw(bytes: String, stall: bool) -> Fake {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("mpm.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind socket");
    let url = trusty_mpm::core::socket_locator(&socket);
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.expect("accept");
        let mut reader = BufReader::new(sock);
        let mut request = String::new();
        let _ = reader.read_line(&mut request).await;
        let mut sock = reader.into_inner();
        sock.write_all(bytes.as_bytes()).await.expect("write");
        sock.flush().await.expect("flush");
        if stall {
            tokio::time::sleep(Duration::from_secs(10)).await;
        } else {
            let _ = sock.shutdown().await;
        }
        request
    });
    Fake {
        url,
        server,
        _dir: dir,
    }
}

/// The first bytes of a result frame, cut before its end.
fn cut_frame() -> String {
    r#"{"jsonrpc":"2.0","id":1,"result":{"answer":"#.to_owned()
}

/// `cached_policy_ready` against a daemon writing `bytes` then stalling or
/// closing; the result and the inline run count (the inline load refuses
/// with `Stopped`).
async fn ready_against_raw(bytes: String, stall: bool) -> (Result<(), NotReady>, usize) {
    let fake = serve_raw(bytes, stall).await;
    let mut runs = 0;
    let home = home();
    let result = cached_policy_ready(&fake.url, Some(&home), || {
        runs += 1;
        Err(NotReady::Stopped)
    })
    .await;
    fake.server.abort();
    (result, runs)
}

/// The locator of a socket nothing listens on: a daemon that is down.
fn down_url() -> String {
    trusty_mpm::core::socket_locator(
        &std::env::temp_dir()
            .join("tm-6288-no-such-dir")
            .join("trusty-mpm.sock"),
    )
}

/// `cached_policy_ready` against a daemon answering `response`, with
/// `inline` as the inline load: the result and the inline run count.
async fn ready_against(
    response: Option<String>,
    inline: impl FnOnce() -> Result<(), NotReady>,
) -> (Result<(), NotReady>, usize) {
    let fake = serve_once(response).await;
    let mut runs = 0;
    let home = home();
    let result = cached_policy_ready(&fake.url, Some(&home), || {
        runs += 1;
        inline()
    })
    .await;
    fake.server.abort();
    (result, runs)
}

/// An inline load that must not run.
fn no_inline() -> Result<(), NotReady> {
    panic!("the inline policy load ran where the daemon's answer decides")
}

/// A canonical temp home with `SESSION` tainted under it (macOS tempdirs
/// sit under a symlink; the host read wants the real path).
fn tainted_home() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = std::fs::canonicalize(tmp.path()).expect("canonical");
    let mark = TaintMark {
        channel: "slack".into(),
        route: "owner-dm".into(),
        question_id: "Q-1".into(),
        tainted_at: chrono::Utc::now(),
        // #9923: scoped, so a session launched outside it stays untainted.
        workspace: home.join("ws"),
    };
    taint_managed_session(&taint_dir(&home), "m-1", SESSION, &mark).expect("taint");
    (tmp, home)
}

fn read_call(session: &str) -> Result<Value, crate::commands::hook_stdin::HookStdinError> {
    Ok(json!({
        "tool_name": "Read",
        "tool_input": { "file_path": "/etc/hosts" },
        "session_id": session,
    }))
}

/// 🔴 #8454 VC (plan :131, R2): with the daemon down — an absent socket, the
/// sandbox's no-daemon URL, or (#6288) a loopback HTTP URL, which is no socket
/// — the inline load decides, once, and its refusal
/// stands. Through the taint arm, a missing `config.yaml` still denies as
/// `POLICY_CHECK`; nothing allows.
#[tokio::test]
async fn daemon_unreachable_falls_back_to_inline_load_never_allow() {
    let home = home();
    let down = down_url();
    let sandbox = trusty_mpm::core::discovery::ISOLATED_NO_DAEMON_URL;
    for url in [down.as_str(), sandbox, "http://127.0.0.1:7880"] {
        assert_eq!(
            verdict_query(url, &home).await,
            Err(QueryError::Unreachable),
            "{url}"
        );
        let mut runs = 0;
        let refused = cached_policy_ready(url, Some(&home), || {
            runs += 1;
            Err(NotReady::NoRoutes)
        })
        .await;
        assert_eq!(refused, Err(NotReady::NoRoutes), "{url}");
        assert_eq!(runs, 1, "{url}: the inline load must run exactly once");
    }
    let (_tmp, tainted) = tainted_home();
    let at = tainted.as_path();
    let ready = || cached_policy_ready(&down, Some(at), || policy_ready(Some(at)));
    let verdict =
        with_taint_async(Verdict::Allow, &read_call(SESSION), Some(at), None, ready).await;
    match verdict {
        Verdict::Deny { check, reason } => {
            assert_eq!(check, POLICY_CHECK);
            assert!(reason.contains("config.yaml is missing"), "{reason}");
        }
        Verdict::Allow => panic!("a down daemon must never allow"),
    }
}

/// 🔴 #8454 VC (R2): a daemon that accepts but never answers costs at most
/// the query timeout, then the inline load decides.
#[tokio::test]
async fn daemon_timeout_falls_back_to_inline_load() {
    let started = Instant::now();
    let (result, runs) = ready_against(None, || Err(NotReady::Stopped)).await;
    assert_eq!(result, Err(NotReady::Stopped));
    assert_eq!(runs, 1);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let fake = serve_once(None).await;
    assert_eq!(
        verdict_query(&fake.url, &home()).await,
        Err(QueryError::TimedOut(VERDICT_QUERY_TIMEOUT))
    );
    fake.server.abort();
}

/// 🔴 #8454 VC (plan :132, R2): a 2xx reply that is not exactly one verdict
/// reply denies as not ready (`Unanswered`); the inline load never runs and
/// nothing reads it as ready.
#[tokio::test]
async fn malformed_daemon_answer_denies() {
    let bodies = [
        String::new(),
        "not json".to_owned(),
        "{}".to_owned(),
        json!({"answer": "allow", "fingerprint": 1, "age_ms": 1}).to_string(),
        json!({"answer": "ready", "fingerprint": 1}).to_string(),
        json!({"answer": "unserved", "why": "something_new"}).to_string(),
        json!({"answer": "not_ready", "slug": "egress-family", "not_ready": "NoRoutes",
               "fingerprint": 1, "age_ms": 1})
        .to_string(),
    ];
    for body in bodies {
        let (result, runs) = ready_against(Some(result_text(&body)), no_inline).await;
        assert!(
            matches!(result, Err(NotReady::Unanswered { .. })),
            "{body:?}: {result:?}"
        );
        assert_eq!(runs, 0, "{body:?}");
        assert_eq!(
            parse(body.as_bytes()),
            Err(QueryError::Malformed),
            "{body:?}"
        );
    }
}

/// 🔴 #8454 VC (R2): a refusal denies as not ready, and the inline load
/// never runs. #6288: over the socket a refusal is an RPC error code, which
/// projects onto the HTTP status it stands for.
#[tokio::test]
async fn non_2xx_daemon_answer_denies() {
    for (code, status) in [(-32004, 404), (-32603, 500), (-32002, 503)] {
        let (result, runs) = ready_against(Some(refusal(code)), no_inline).await;
        match result {
            Err(NotReady::Unanswered { detail }) => {
                assert!(detail.contains(&status.to_string()), "{detail}");
            }
            other => panic!("{code}: {other:?}"),
        }
        assert_eq!(runs, 0, "{code}");
    }
    // A daemon that predates the method: answered, unusably — deny.
    let (result, runs) = ready_against(Some(refusal(-32601)), no_inline).await;
    assert!(
        matches!(result, Err(NotReady::Unanswered { .. })),
        "{result:?}"
    );
    assert_eq!(runs, 0);
}

/// 🔴 #8454 VC (R2): a served verdict stands without the inline load —
/// ready is ready, and not ready keeps its branch.
#[tokio::test]
async fn ready_answer_skips_the_inline_load() {
    let ready = json!({"answer": "ready", "fingerprint": 9, "age_ms": 10});
    assert_eq!(
        ready_against(Some(ok(&ready)), no_inline).await,
        (Ok(()), 0)
    );
    let not_ready = json!({"answer": "not_ready", "slug": "egress-policy-not-ready",
                           "not_ready": "NoRoutes", "fingerprint": 9, "age_ms": 10});
    assert_eq!(
        ready_against(Some(ok(&not_ready)), no_inline).await,
        (Err(NotReady::NoRoutes), 0)
    );
}

/// 🔴 #8454 VC (plan :130, R2): a verdict older than max age is not used;
/// the inline load decides.
#[tokio::test]
async fn stale_daemon_answer_falls_back_to_inline_load() {
    let stale = json!({"answer": "ready", "fingerprint": 9, "age_ms": 60_001});
    let (result, runs) = ready_against(Some(ok(&stale)), || Err(NotReady::NoRoutes)).await;
    assert_eq!((result, runs), (Err(NotReady::NoRoutes), 1));
}

/// 🔴 #8454 VC (R1): a verdict for fingerprint 0 is not used; the inline
/// load decides.
#[tokio::test]
async fn zero_fingerprint_answer_falls_back_to_inline_load() {
    for answer in ["ready", "not_ready"] {
        let reply = json!({"answer": answer, "slug": "egress-policy-not-ready",
                           "not_ready": "NoRoutes", "fingerprint": 0, "age_ms": 1});
        let (result, runs) = ready_against(Some(ok(&reply)), || Err(NotReady::Stopped)).await;
        assert_eq!((result, runs), (Err(NotReady::Stopped), 1), "{answer}");
    }
}

/// 🔴 #8454 VC (R2): every reply that serves no verdict — a home mismatch,
/// a stale, unloaded or poisoned snapshot, no fingerprint — runs the inline
/// load, whose refusal stands.
#[tokio::test]
async fn unserved_answers_fall_back_to_inline_load() {
    for why in [
        Unserved::HomeMismatch,
        Unserved::Stale,
        Unserved::NeverLoaded,
        Unserved::Poisoned,
        Unserved::NoFingerprint,
        Unserved::Refused,
    ] {
        let reply = json!({"answer": "unserved", "why": why});
        let (result, runs) = ready_against(Some(ok(&reply)), || Err(NotReady::NoRoutes)).await;
        assert_eq!((result, runs), (Err(NotReady::NoRoutes), 1), "{why:?}");
    }
}

/// 🔴 #8454 VC (R2): the query sends the hook's own home to the route's
/// socket method (#6288).
#[tokio::test]
async fn verdict_query_sends_its_home() {
    let reply = json!({"answer": "unserved", "why": "home_mismatch"});
    let fake = serve_once(Some(ok(&reply))).await;
    let verdict = verdict_query(&fake.url, Path::new("/home/tester")).await;
    assert_eq!(verdict, Ok(DaemonVerdict::Unserved(Unserved::HomeMismatch)));
    let request: Value =
        serde_json::from_str(&fake.server.await.expect("server")).expect("a JSON frame");
    assert_eq!(request["method"], "mpm.channels.egress_verdict");
    assert_eq!(request["params"], json!({"home": "/home/tester"}));
}

/// 🔴 #8454 VC (R4): an untainted session's call, and a call already
/// denied, never query the daemon or read the policy.
#[tokio::test]
async fn untainted_session_never_queries_the_daemon() {
    let (_tmp, tainted) = tainted_home();
    let at = Some(tainted.as_path());
    let never = || async { no_inline() };
    // #9923: launched outside the mark's workspace, so no mark covers it.
    let clean = with_taint_async(Verdict::Allow, &read_call("s-clean"), at, at, never).await;
    assert_eq!(clean, Verdict::Allow);
    let first = Verdict::Deny {
        check: "egress-family",
        reason: "x".into(),
    };
    let kept = with_taint_async(first.clone(), &read_call(SESSION), at, None, never).await;
    assert_eq!(kept, first);
    // Control: the tainted session's call does ask.
    let asked = with_taint_async(Verdict::Allow, &read_call(SESSION), at, None, || async {
        Ok(())
    })
    .await;
    assert!(matches!(
        asked,
        Verdict::Deny {
            check: TAINT_CHECK,
            ..
        }
    ));
}

/// 🔴 #8454 VC (r-078): with no home there is nothing to ask under, so the
/// inline load runs once, even with a daemon URL that cannot answer, and its
/// `HomeUnknown` refusal stands.
#[tokio::test]
async fn no_home_runs_the_inline_load_once() {
    let mut runs = 0;
    let result = cached_policy_ready(&down_url(), None, || {
        runs += 1;
        policy_ready(None)
    })
    .await;
    assert!(
        matches!(
            result,
            Err(NotReady::Host {
                branch: HostBranch::HomeUnknown,
                ..
            })
        ),
        "{result:?}"
    );
    assert_eq!(runs, 1, "the inline load must run exactly once");
}

/// 🔴 #8454 VC (r-078): a daemon that reads the request and closes without
/// a reply answered, unusably: `Unanswered`, and the inline load never runs.
#[tokio::test]
async fn daemon_closing_without_a_reply_denies() {
    let (result, runs) = ready_against_raw(String::new(), false).await;
    assert!(
        matches!(result, Err(NotReady::Unanswered { .. })),
        "{result:?}"
    );
    assert_eq!(runs, 0);
}

/// 🔴 #8454 VC (r-078): a reply frame cut short is a read error that is not
/// a timeout: `Unanswered`, and no inline load.
#[tokio::test]
async fn daemon_cutting_the_body_short_denies() {
    let (result, runs) = ready_against_raw(cut_frame(), false).await;
    assert!(
        matches!(result, Err(NotReady::Unanswered { .. })),
        "{result:?}"
    );
    assert_eq!(runs, 0);
    let fake = serve_raw(cut_frame(), false).await;
    assert_eq!(
        verdict_query(&fake.url, &home()).await,
        Err(QueryError::Malformed)
    );
    fake.server.abort();
}

/// 🔴 #8454 VC (r-078): a daemon that sends part of its frame and then
/// stalls past the query timeout has not answered: the inline load runs once.
#[tokio::test]
async fn daemon_stalling_the_body_falls_back_to_inline_load() {
    let started = Instant::now();
    let (result, runs) = ready_against_raw(cut_frame(), true).await;
    assert_eq!((result, runs), (Err(NotReady::Stopped), 1));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// The table row of each [`QueryError`] variant. No wildcard: a new variant
/// fails to compile here until it has a row.
fn row(e: &QueryError) -> usize {
    match e {
        QueryError::Unreachable => 0,
        QueryError::TimedOut(_) => 1,
        QueryError::Status(_) => 2,
        QueryError::Malformed => 3,
    }
}

/// 🔴 #8454 VC (r-078): every `QueryError` maps to its pinned outcome: no
/// daemon answered (unreachable, timeout) runs the inline load once; an
/// unusable answer (status, malformed) denies `Unanswered` without it.
#[test]
fn every_query_error_maps_to_inline_or_deny() {
    let cases = [
        (QueryError::Unreachable, true),
        (QueryError::TimedOut(VERDICT_QUERY_TIMEOUT), true),
        (QueryError::Status(503), false),
        (QueryError::Malformed, false),
    ];
    let mut rows: Vec<usize> = cases.iter().map(|(e, _)| row(e)).collect();
    rows.sort_unstable();
    assert_eq!(rows, [0, 1, 2, 3], "every variant has exactly one row");
    for (e, inline) in cases {
        assert_eq!(e.asked_no_one(), inline, "{e:?}");
        let mut runs = 0;
        let detail = e.to_string();
        let result = e.clone().readiness(|| {
            runs += 1;
            Err(NotReady::Stopped)
        });
        if inline {
            assert_eq!((result, runs), (Err(NotReady::Stopped), 1), "{e:?}");
        } else {
            assert_eq!(
                (result, runs),
                (Err(NotReady::Unanswered { detail }), 0),
                "{e:?}"
            );
        }
    }
}
