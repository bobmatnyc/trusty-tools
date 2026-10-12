//! #8454 VC: the egress guard's daemon verdict client and its fallbacks.
//!
//! Why: the client is a Fail-Open Check site. Each test goes red if a case
//! that must run the inline load serves a verdict, if an unusable answer
//! falls through to the inline load or to ready, or if an untainted session
//! queries the daemon.
//! What: a one-connection loopback HTTP server answers each canned reply;
//! a closed port stands for a daemon that is down. The inline load is a
//! closure that counts its runs, or panics where it must not run.
//! Test: this is the test module.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use trusty_mpm::core::channels_readiness::{HostBranch, NotReady, policy_ready};
use trusty_mpm::core::channels_taint::{TaintMark, taint_dir, taint_managed_session};
use trusty_mpm::core::egress_verdict::{Unserved, VERDICT_ROUTE};

use super::*;
use crate::commands::egress_guard::{Verdict, with_taint_async};
use crate::commands::egress_guard_taint::{POLICY_CHECK, TAINT_CHECK};

const SESSION: &str = "s-tainted";

/// The home the hook sends; the fake daemon never reads it.
fn home() -> PathBuf {
    PathBuf::from("/home/tester")
}

/// An HTTP/1.1 response with a JSON `body`.
fn http(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{body}",
        body.len()
    )
}

fn ok(body: &Value) -> String {
    http("200 OK", &body.to_string())
}

/// Read one request: the head, then `content-length` body bytes.
async fn read_request(sock: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = sock.read(&mut chunk).await.expect("read");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
        let len = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        if buf.len() >= end + 4 + len {
            break;
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// A loopback daemon that answers one request with `response`; the handle
/// yields the request it read. `None` holds the connection unanswered.
async fn serve_once(response: Option<String>) -> (String, tokio::task::JoinHandle<String>) {
    match response {
        Some(response) => serve_raw(response, false).await,
        None => serve_raw(String::new(), true).await,
    }
}

/// A loopback daemon that reads one request, writes `bytes` (any prefix of
/// a reply, or nothing), then holds the connection open when `stall` or
/// closes it.
async fn serve_raw(bytes: String, stall: bool) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");
        let request = read_request(&mut sock).await;
        sock.write_all(bytes.as_bytes()).await.expect("write");
        sock.flush().await.expect("flush");
        if stall {
            tokio::time::sleep(Duration::from_secs(10)).await;
        } else {
            let _ = sock.shutdown().await;
        }
        request
    });
    (url, server)
}

/// A 200 head promising a 100-byte body.
fn head_of_100() -> String {
    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\
     connection: close\r\n\r\n"
        .to_owned()
}

/// `cached_policy_ready` against a daemon writing `bytes` then stalling or
/// closing; the result and the inline run count (the inline load refuses
/// with `Stopped`).
async fn ready_against_raw(bytes: String, stall: bool) -> (Result<(), NotReady>, usize) {
    let (url, server) = serve_raw(bytes, stall).await;
    let mut runs = 0;
    let home = home();
    let result = cached_policy_ready(&url, Some(&home), || {
        runs += 1;
        Err(NotReady::Stopped)
    })
    .await;
    server.abort();
    (result, runs)
}

/// A URL on a loopback port nothing listens on: a daemon that is down.
fn down_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    format!("http://{addr}")
}

/// `cached_policy_ready` against a daemon answering `response`, with
/// `inline` as the inline load: the result and the inline run count.
async fn ready_against(
    response: Option<String>,
    inline: impl FnOnce() -> Result<(), NotReady>,
) -> (Result<(), NotReady>, usize) {
    let (url, server) = serve_once(response).await;
    let mut runs = 0;
    let home = home();
    let result = cached_policy_ready(&url, Some(&home), || {
        runs += 1;
        inline()
    })
    .await;
    server.abort();
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
        workspace: PathBuf::from("/"),
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

/// 🔴 #8454 VC (plan :131, R2): with the daemon down — a closed port, or the
/// sandbox's no-daemon URL — the inline load decides, once, and its refusal
/// stands. Through the taint arm, a missing `config.yaml` still denies as
/// `POLICY_CHECK`; nothing allows.
#[tokio::test]
async fn daemon_unreachable_falls_back_to_inline_load_never_allow() {
    let home = home();
    let down = down_url();
    let sandbox = trusty_mpm::core::discovery::ISOLATED_NO_DAEMON_URL;
    for url in [down.as_str(), sandbox] {
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
    let verdict = with_taint_async(Verdict::Allow, &read_call(SESSION), Some(at), ready).await;
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
    let (url, server) = serve_once(None).await;
    assert_eq!(
        verdict_query(&url, &home()).await,
        Err(QueryError::TimedOut(VERDICT_QUERY_TIMEOUT))
    );
    server.abort();
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
        let (result, runs) = ready_against(Some(http("200 OK", &body)), no_inline).await;
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

/// 🔴 #8454 VC (R2): a non-2xx status denies as not ready, a redirect too
/// (it is never followed), and the inline load never runs.
#[tokio::test]
async fn non_2xx_daemon_answer_denies() {
    let ready = json!({"answer": "ready", "fingerprint": 1, "age_ms": 1}).to_string();
    for (status, code) in [
        ("404 Not Found", 404),
        ("500 Internal Server Error", 500),
        ("503 Service Unavailable", 503),
    ] {
        let (result, runs) = ready_against(Some(http(status, &ready)), no_inline).await;
        match result {
            Err(NotReady::Unanswered { detail }) => {
                assert!(detail.contains(&code.to_string()), "{detail}");
            }
            other => panic!("{status}: {other:?}"),
        }
        assert_eq!(runs, 0, "{status}");
    }
    let (url, server) = serve_once(None).await;
    server.abort();
    let redirect = format!(
        "HTTP/1.1 307 Temporary Redirect\r\nlocation: {url}{VERDICT_ROUTE}\r\n\
         content-length: 0\r\nconnection: close\r\n\r\n"
    );
    let (result, runs) = ready_against(Some(redirect), no_inline).await;
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

/// 🔴 #8454 VC (R2): every reply that serves no verdict — a changed
/// fingerprint, a home mismatch, a stale or unloaded snapshot — runs the
/// inline load, whose refusal stands.
#[tokio::test]
async fn unserved_answers_fall_back_to_inline_load() {
    for why in [
        Unserved::FingerprintChanged,
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

/// 🔴 #8454 VC (R2): the query POSTs the hook's own home to the route.
#[tokio::test]
async fn verdict_query_sends_its_home() {
    let reply = json!({"answer": "unserved", "why": "home_mismatch"});
    let (url, server) = serve_once(Some(ok(&reply))).await;
    let verdict = verdict_query(&url, Path::new("/home/tester")).await;
    assert_eq!(verdict, Ok(DaemonVerdict::Unserved(Unserved::HomeMismatch)));
    let request = server.await.expect("server");
    assert!(
        request.starts_with(&format!("POST {VERDICT_ROUTE} HTTP/1.1")),
        "{request}"
    );
    assert!(request.ends_with(r#"{"home":"/home/tester"}"#), "{request}");
}

/// 🔴 #8454 VC (R4): an untainted session's call, and a call already
/// denied, never query the daemon or read the policy.
#[tokio::test]
async fn untainted_session_never_queries_the_daemon() {
    let (_tmp, tainted) = tainted_home();
    let at = Some(tainted.as_path());
    let never = || async { no_inline() };
    let clean = with_taint_async(Verdict::Allow, &read_call("s-clean"), at, never).await;
    assert_eq!(clean, Verdict::Allow);
    let first = Verdict::Deny {
        check: "egress-family",
        reason: "x".into(),
    };
    let kept = with_taint_async(first.clone(), &read_call(SESSION), at, never).await;
    assert_eq!(kept, first);
    // Control: the tainted session's call does ask.
    let asked =
        with_taint_async(Verdict::Allow, &read_call(SESSION), at, || async { Ok(()) }).await;
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

/// 🔴 #8454 VC (r-078): a body cut short of its `content-length` is a body
/// read error that is not a timeout: `Unanswered`, and no inline load.
#[tokio::test]
async fn daemon_cutting_the_body_short_denies() {
    let short = format!("{}{{\"answer\":", head_of_100());
    assert_eq!(short.len() - head_of_100().len(), 10, "fixture: 10 body bytes");
    let (result, runs) = ready_against_raw(short.clone(), false).await;
    assert!(
        matches!(result, Err(NotReady::Unanswered { .. })),
        "{result:?}"
    );
    assert_eq!(runs, 0);
    let (url, server) = serve_raw(short, false).await;
    assert_eq!(verdict_query(&url, &home()).await, Err(QueryError::Malformed));
    server.abort();
}

/// 🔴 #8454 VC (r-078): a daemon that sends its head and then stalls the
/// body past the query timeout has not answered: the inline load runs once.
#[tokio::test]
async fn daemon_stalling_the_body_falls_back_to_inline_load() {
    let started = Instant::now();
    let (result, runs) = ready_against_raw(head_of_100(), true).await;
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
        QueryError::Client => 0,
        QueryError::Unreachable => 1,
        QueryError::TimedOut(_) => 2,
        QueryError::Status(_) => 3,
        QueryError::Malformed => 4,
    }
}

/// 🔴 #8454 VC (r-078): every `QueryError` maps to its pinned outcome: no
/// daemon answered (client fault, unreachable, timeout) runs the inline load
/// once; an unusable answer (status, malformed) denies `Unanswered` without
/// it. The `Client` arm cannot be induced through a query, so it is pinned
/// here.
#[test]
fn every_query_error_maps_to_inline_or_deny() {
    let cases = [
        (QueryError::Client, true),
        (QueryError::Unreachable, true),
        (QueryError::TimedOut(VERDICT_QUERY_TIMEOUT), true),
        (QueryError::Status(503), false),
        (QueryError::Malformed, false),
    ];
    let mut rows: Vec<usize> = cases.iter().map(|(e, _)| row(e)).collect();
    rows.sort_unstable();
    assert_eq!(rows, [0, 1, 2, 3, 4], "every variant has exactly one row");
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
