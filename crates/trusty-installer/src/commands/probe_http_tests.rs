//! Unit tests for the `probe_http` HTTP `/health` transport (#4246).
//!
//! Why: kept in a sibling file so `probe_http.rs` stays under the 500-SLOC
//! production cap (CLAUDE.md / `scripts/check_line_cap.sh`) — mirrors the
//! `verify_tail.rs` / `verify_tail_tests.rs` and `install.rs` /
//! `install_tests.rs` splits.
//!
//! These are the tests that PROVE #4246 is fixed rather than moved. Before this
//! module, `probe_member_health`'s launchd arm was never executed by any test in
//! a crate with 483 green ones, and two of those tests actively asserted the bug
//! was correct behaviour.
//!
//! Test: `cargo test -p trusty-installer` runs everything here.

use super::*;
use crate::commands::test_support::{dead_addr, stub_hang, stub_once, ENV_TEST_LOCK};

/// The status line for a plain 200.
const OK_LINE: &str = "HTTP/1.1 200 OK";

// ── The six REAL `/health` payloads ─────────────────────────────────────────
//
// Captured verbatim off a live stack (`curl --noproxy '*'
// http://127.0.0.1:<port>/health`) while `tctl status` was reporting every one
// of these daemons as `down`. They are the ground truth this fix is measured
// against: any classifier that reads ANY of them as `down` has reproduced #4246.

/// trusty-memory, port 7070 — carries the `daemon_state` readiness sub-field.
const REAL_MEMORY: &str = r#"{"status":"ok","version":"0.21.0","rss_mb":3255,"disk_bytes":288721838,"cpu_pct":0.027932314,"uptime_secs":241434,"addr":"127.0.0.1:7070","open_fds":55,"fd_soft_limit":8192,"daemon_state":"ready"}"#;

/// trusty-console, port 7788 — the minimal shared-handler envelope.
const REAL_CONSOLE: &str = r#"{"status":"ok","version":"0.4.0"}"#;

/// trusty-search, port 7878 — carries the `embedder` readiness sub-field, and is
/// the daemon whose exit-0 CLI payload (`{"daemon":"running",…}`, no `status`
/// key) the old `classify_health_json` read as `down`.
const REAL_SEARCH: &str = r#"{"status":"ok","version":"0.39.0","indexes":11,"uptime_secs":164212,"embedder":"ready","embedder_recent_timeout_count":0,"rss_mb":779,"rss_limit_mb":16384,"disk_bytes":719841751,"cpu_pct":4.8715587,"embedder_info":{"dimension":384,"provider":"MPS","quantized":false,"model":"all-MiniLM-L6-v2","backend":"python"},"background_reindex_queue_depth":0,"update_available":"0.39.1","indexes_kg_disabled":0,"indexes_vector_disabled":0,"embedder_bootstrap":"ready"}"#;

/// trusty-analyze, port 7879 — the daemon that answers 503 when search is down.
const REAL_ANALYZE: &str = r#"{"status":"ok","version":"0.7.4","search_reachable":true}"#;

/// trusty-mpm, port 7880 — the only payload with NO `version` field.
const REAL_MPM: &str = r#"{"status":"ok","catalog_stale":true,"catalog_unknown":false,"catalog_changes":["agent dart-engineer: new"],"supervised":false}"#;

/// trusty-review, port 7891 — answers 200 + `degraded` when search is down.
const REAL_REVIEW: &str = r#"{"status":"ok","version":"0.10.1","dry_run":true,"reviewer_model":"us.anthropic.claude-sonnet-4-6","inference":"ok","deps":{"trusty_search":{"required":true,"reachable":true,"state":"ok"}}}"#;

/// Every real payload, paired with the daemon it came from.
fn real_payloads() -> Vec<(&'static str, &'static str)> {
    vec![
        ("trusty-memory", REAL_MEMORY),
        ("trusty-console", REAL_CONSOLE),
        ("trusty-search", REAL_SEARCH),
        ("trusty-analyze", REAL_ANALYZE),
        ("trusty-mpm", REAL_MPM),
        ("trusty-review", REAL_REVIEW),
    ]
}

// ── The ground-truth table ──────────────────────────────────────────────────

/// Why: THE #4246 acceptance test. Six daemons answering `/health` with
/// `status:"ok"` were all reported `down`; classifying any of their REAL payloads
/// as `down` means the bug is back. Also asserts the negative that matters
/// operationally: none of them may be `is_confirmed_down`, because that is what
/// authorises `launchctl kickstart -k`.
/// What: table over the six verbatim payloads at HTTP 200; asserts each is
/// `Serving`, renders `healthy`, and is not confirmed-down.
/// Test: This is the test.
#[test]
fn real_daemon_payloads_are_never_down() {
    for (daemon, body) in real_payloads() {
        let outcome = classify_response(200, body.as_bytes());
        assert!(
            matches!(outcome, ProbeOutcome::Serving { .. }),
            "{daemon}'s real /health payload must classify as Serving, got {outcome:?}"
        );
        assert_eq!(
            outcome.health_string(),
            "healthy",
            "{daemon} must render `healthy`, not `down`"
        );
        assert!(
            !outcome.is_confirmed_down(),
            "{daemon} must never authorise a kickstart"
        );
    }
}

/// Why: the `version` column of `tctl status` / the verify rows reads the
/// envelope's version, and trusty-mpm's payload has none — an absent version must
/// be `None`, never a reason to downgrade the health verdict (that conflation is
/// the version-skew trap deliberately left out of #4246's scope).
/// What: asserts search reports its version and mpm reports `None`, both still
/// `Serving`.
/// Test: This is the test.
#[test]
fn missing_version_does_not_degrade_the_verdict() {
    let search = classify_response(200, REAL_SEARCH.as_bytes());
    assert_eq!(
        search,
        ProbeOutcome::Serving {
            status: "ok".to_owned(),
            version: Some("0.39.0".to_owned()),
        }
    );
    let mpm = classify_response(200, REAL_MPM.as_bytes());
    assert_eq!(
        mpm,
        ProbeOutcome::Serving {
            status: "ok".to_owned(),
            version: None,
        }
    );
}

// ── Body-over-status-code classification ────────────────────────────────────

/// Why: trusty-analyze answers **HTTP 503** with `status:"degraded"` when
/// trusty-search is unreachable (`service/routes.rs`), while trusty-review
/// answers **200** with the same `degraded` for the identical condition. A
/// 2xx-only liveness check (`ensure::daemon::health_ok`, which this module
/// deliberately does NOT reuse) reads one as degraded and the other as `down` —
/// and pre-gate would have kickstarted analyze every time search was slow. Prove
/// the transport accepts the non-2xx and reads the body.
/// What: a stub answering `503` + `{"status":"degraded",…}` must yield
/// `Serving{degraded}`, render `stale` (which `VerifyTailReport::build`
/// tolerates), and NOT be confirmed-down.
/// Test: This is the test.
#[tokio::test]
async fn probe_accepts_503_degraded() {
    let addr = stub_once(
        "HTTP/1.1 503 Service Unavailable",
        r#"{"status":"degraded","version":"0.7.4","search_reachable":false}"#,
    )
    .await;
    let client = build_probe_client().expect("probe client builds");
    let outcome = probe_bases(&client, Some(format!("http://{addr}")), None).await;

    assert_eq!(
        outcome,
        ProbeOutcome::Serving {
            status: "degraded".to_owned(),
            version: Some("0.7.4".to_owned()),
        },
        "503 with a usable envelope must read `degraded`, never `down`"
    );
    assert_eq!(outcome.health_string(), "stale");
    assert!(
        !outcome.is_confirmed_down(),
        "a degraded-but-answering daemon must never be kickstarted"
    );
}

/// Why: the envelope check is what stops a stale `http_addr` (or a squatter on a
/// documented default port — the #3364 collision class) reading as a healthy
/// trusty daemon. It must also NOT be treated as a down observation: something
/// IS listening, so restarting the member is a guess, not a repair.
/// What: a stub answering 200 + `{"hello":"world"}` must yield `BadEnvelope`,
/// must not render `healthy`, and must not be confirmed-down.
/// Test: This is the test.
#[tokio::test]
async fn probe_rejects_non_trusty_envelope() {
    let addr = stub_once(OK_LINE, r#"{"hello":"world"}"#).await;
    let client = build_probe_client().expect("probe client builds");
    let outcome = probe_bases(&client, Some(format!("http://{addr}")), None).await;

    assert!(
        matches!(outcome, ProbeOutcome::BadEnvelope { .. }),
        "a body with no string `status` is not a trusty health envelope, got {outcome:?}"
    );
    assert_ne!(outcome.health_string(), "healthy");
    assert!(
        !outcome.is_confirmed_down(),
        "a schema/envelope problem must NEVER authorise a kickstart — that is the \
         #4246 regression, in which an unimplemented contract looked like a dead process"
    );
}

/// Why: when a non-2xx body is ALSO unusable, the status code is the most
/// informative thing available and must survive into the outcome for diagnosis —
/// but still without becoming a confirmed-down.
/// What: `classify_response(500, "<html>…")` → `HttpError { status: 500 }`;
/// `classify_response(200, "<html>…")` → `BadEnvelope`.
/// Test: This is the test.
#[test]
fn classify_response_prefers_status_code_for_unusable_5xx() {
    let five = classify_response(500, b"<html>Bad Gateway</html>");
    assert_eq!(five, ProbeOutcome::HttpError { status: 500 });
    assert!(!five.is_confirmed_down());

    let two = classify_response(200, b"<html>squatter</html>");
    assert!(matches!(two, ProbeOutcome::BadEnvelope { .. }));
}

/// Why: the `BadEnvelope` sample exists to make a misconfiguration diagnosable,
/// not to paste a megabyte of a squatter's HTML into a status row.
/// What: a body far longer than the sample bound is truncated to it.
/// Test: This is the test.
#[test]
fn bad_envelope_sample_is_bounded() {
    let long = "x".repeat(10_000);
    let ProbeOutcome::BadEnvelope { got } = classify_response(200, long.as_bytes()) else {
        panic!("a long non-JSON body must be a BadEnvelope");
    };
    assert_eq!(got.chars().count(), ENVELOPE_SAMPLE_LEN);
}

// ── Readiness sub-fields ────────────────────────────────────────────────────

/// Why: `/health` returning 200 `{"status":"ok"}` does not mean the daemon can
/// serve — trusty-memory reports `daemon_state:"warming"` and trusty-search
/// reports `embedder:"initializing"`/`"stalled"`/`"error"` alongside a cheerful
/// `status:"ok"`. Reporting such a daemon fully serving hides a real capability
/// gap; reporting it `down` would be worse (it IS up, and a kickstart would only
/// restart the warm-up). `degraded` is the honest middle.
/// What: table over the readiness sub-fields and states; a non-ready value
/// degrades an `ok` envelope, `ready`/`ok` leaves it alone, a non-string
/// sub-field is ignored, and an already-degraded verdict is never upgraded.
/// Test: This is the test.
#[test]
fn warming_daemon_is_degraded_not_serving() {
    for key in ["daemon_state", "embedder", "embedder_status"] {
        for state in ["warming", "initializing", "stalled", "error"] {
            let v = serde_json::json!({ "status": "ok", key: state });
            assert_eq!(
                effective_status(&v),
                "degraded",
                "`{key}: {state}` must degrade an `ok` envelope"
            );
        }
        for state in ["ready", "ok", "OK"] {
            let v = serde_json::json!({ "status": "ok", key: state });
            assert_eq!(
                effective_status(&v),
                "ok",
                "`{key}: {state}` must leave a serving verdict alone"
            );
        }
    }

    // A non-string sub-field carries no readiness signal; it must be ignored.
    let obj = serde_json::json!({ "status": "ok", "embedder": { "model": "mini" } });
    assert_eq!(effective_status(&obj), "ok");

    // An already-degraded envelope is never upgraded by a ready sub-field.
    let already = serde_json::json!({ "status": "degraded", "daemon_state": "ready" });
    assert_eq!(effective_status(&already), "degraded");
}

// ── The dual-resolution precedence rule ─────────────────────────────────────

/// Why: THE rule that stops the fix re-shipping the bug. trusty-memory port-walks
/// `7070..=7079`, so a daemon that found 7070 taken and bound 7071 has a
/// `Serving` `http_addr` leg and a `Refused` fixed-port leg *while being
/// perfectly healthy*. Treating that refusal as authoritative would hard-restart
/// it. This test drives BOTH legs over the real HTTP transport — a live stub on
/// the recorded address, a genuinely-refusing address on the documented port.
/// What: asserts the reconciled verdict is `Serving`, renders `healthy`, and —
/// the operationally load-bearing part — is NOT confirmed-down, so
/// `needs_kickstart` cannot fire.
/// Test: This is the test.
#[tokio::test]
async fn probe_port_walked_daemon_is_healthy() {
    let walked = stub_once(OK_LINE, r#"{"status":"ok","version":"0.21.0"}"#).await;
    let documented = dead_addr();
    let client = build_probe_client().expect("probe client builds");

    let outcome = probe_bases(
        &client,
        Some(format!("http://{walked}")),
        Some(format!("http://{documented}")),
    )
    .await;

    assert!(
        matches!(outcome, ProbeOutcome::Serving { .. }),
        "a port-walked daemon is healthy; got {outcome:?}"
    );
    assert_eq!(outcome.health_string(), "healthy");
    assert!(
        !outcome.is_confirmed_down(),
        "a Refused on the documented-port leg must NEVER feed needs_kickstart \
         while the other leg is Serving — that would kickstart a healthy daemon"
    );
}

/// Why: pins the precedence rule as a pure function so the postconditions in
/// `reconcile`'s doc are executable, in both leg orders (a `min_by_key` that
/// happened to favour the first element would pass one order and fail the other).
/// What: `Serving` beats `Refused`/`Timeout` regardless of position; a healthy
/// `Serving` beats a degraded one.
/// Test: This is the test.
#[test]
fn reconcile_serving_leg_beats_refused_leg() {
    let serving = ProbeOutcome::Serving {
        status: "ok".to_owned(),
        version: None,
    };
    for other in [ProbeOutcome::Refused, ProbeOutcome::Timeout] {
        assert_eq!(
            reconcile(vec![serving.clone(), other.clone()]),
            serving,
            "serving-first must win"
        );
        assert_eq!(
            reconcile(vec![other, serving.clone()]),
            serving,
            "serving-second must win too"
        );
    }

    let degraded = ProbeOutcome::Serving {
        status: "degraded".to_owned(),
        version: None,
    };
    assert_eq!(
        reconcile(vec![degraded.clone(), serving.clone()]),
        serving,
        "a fully-serving leg outranks a degraded one"
    );
    assert!(!reconcile(vec![degraded, ProbeOutcome::Refused]).is_confirmed_down());
}

/// Why: `something answered, but with garbage` still means a process is
/// listening, so it must outrank the other leg's refusal — restarting on it would
/// be a guess, and (for a squatter on a documented default port) would restart
/// the wrong member's neighbour for no reason.
/// What: `BadEnvelope` and `HttpError` both beat `Refused`, and neither is
/// confirmed-down.
/// Test: This is the test.
#[test]
fn reconcile_answered_leg_beats_refused_leg() {
    for answered in [
        ProbeOutcome::BadEnvelope {
            got: "squatter".to_owned(),
        },
        ProbeOutcome::HttpError { status: 500 },
    ] {
        let verdict = reconcile(vec![ProbeOutcome::Refused, answered.clone()]);
        assert_eq!(verdict, answered);
        assert!(
            !verdict.is_confirmed_down(),
            "an answering leg must suppress the other leg's confirmed-down"
        );
    }
}

/// Why: the mirror safety property — the fix must not become "never repair
/// anything". When NO leg saw a peer at all, the verdict must stay a
/// confirmed-down so `verify_tail` still kickstarts a genuinely dead daemon (the
/// #2498 failure signature the machinery exists for).
/// What: all-refused and refused+timeout both stay confirmed-down; an empty leg
/// set is `NoAddress` and is NOT confirmed-down (nothing was observed).
/// Test: This is the test.
#[test]
fn reconcile_all_refused_stays_refused() {
    assert_eq!(
        reconcile(vec![ProbeOutcome::Refused, ProbeOutcome::Refused]),
        ProbeOutcome::Refused
    );
    assert!(reconcile(vec![ProbeOutcome::Refused, ProbeOutcome::Timeout]).is_confirmed_down());
    assert_eq!(reconcile(Vec::new()), ProbeOutcome::NoAddress);
    assert!(!ProbeOutcome::NoAddress.is_confirmed_down());
}

// ── The failure taxonomy ────────────────────────────────────────────────────

/// Why: THE reason `ProbeOutcome` exists. Before #4246 every one of these four
/// situations was literally the string `"down"`, and `launchctl kickstart -k`
/// fired on all of them — so an unimplemented CLI contract was indistinguishable
/// from a dead process. Each must now be its OWN variant, and only the two
/// transport-level ones may authorise a repair.
/// What: table over a refusing address, a 500 with an unusable body, a 200 with a
/// non-JSON body, and a peer that accepts then goes silent; asserts four distinct
/// variants and the correct confirmed-down flag for each.
/// Test: This is the test.
#[tokio::test]
async fn probe_distinguishes_failure_causes() {
    // Bounds well under the production 5s so the Timeout case stays cheap, but
    // with the CONNECT bound deliberately an order of magnitude SHORTER than the
    // whole-request bound. That ordering is load-bearing for what the `silent`
    // case below proves: the peer completes the TCP handshake and then goes
    // quiet, so the deadline that must fire is the REQUEST one, reached while
    // waiting to READ. The previous bounds were inverted (connect 500ms >
    // request 400ms), which made the connect bound unreachable dead config and
    // left the case unable to distinguish a read timeout from a connect timeout
    // — the two are indistinguishable downstream, because
    // `classify_transport_error` tests `is_timeout()` before `is_connect()` and
    // hyper-util reports a connect timeout as `io::ErrorKind::TimedOut`, so both
    // land on `Timeout`. A loopback connect completes in microseconds, so 250ms
    // is ~1000x headroom while 2s leaves the read path an 8x margin over it.
    // `build_probe_client_with` keeps `.no_proxy()`.
    let client = build_probe_client_with(Duration::from_millis(250), Duration::from_secs(2))
        .expect("probe client builds");

    let refused = dead_addr();
    let five_hundred = stub_once("HTTP/1.1 500 Internal Server Error", r#"{"error":"boom"}"#).await;
    let garbage = stub_once(OK_LINE, "not json at all").await;
    let silent = stub_hang().await;

    let cases: Vec<(&str, String, ProbeOutcome, bool)> = vec![
        (
            "nothing listening",
            refused,
            ProbeOutcome::Refused,
            true, // a genuine TCP observation — may repair
        ),
        (
            "answered 500 with an unusable body",
            five_hundred,
            ProbeOutcome::HttpError { status: 500 },
            false,
        ),
        (
            "answered 200 with a non-JSON body",
            garbage,
            ProbeOutcome::BadEnvelope {
                got: "not json at all".to_owned(),
            },
            false,
        ),
        (
            "accepted then never answered",
            silent,
            ProbeOutcome::Timeout,
            true, // a wedged daemon is not serving — may repair
        ),
    ];

    for (label, addr, want, confirmed_down) in cases {
        let got = probe_bases(&client, Some(format!("http://{addr}")), None).await;
        assert_eq!(got, want, "case `{label}` classified wrongly");
        assert_eq!(
            got.is_confirmed_down(),
            confirmed_down,
            "case `{label}`: wrong kickstart authorisation"
        );
    }
}

// ── Proxy immunity ──────────────────────────────────────────────────────────

/// Why: `.no_proxy()` is the single most load-bearing line in this module and
/// there is no other `.no_proxy()` anywhere in the workspace. reqwest 0.12
/// honours `HTTP_PROXY`/`http_proxy`/`ALL_PROXY` for `127.0.0.1` — hyper-util's
/// proxy matcher has no runtime loopback exemption — so a developer with a proxy
/// exported reproduces the identical #4246 false `down` through the NEW
/// transport, and (pre-gate) gets every healthy daemon kickstarted.
///
/// This asserts BOTH halves, so it fails loudly if someone deletes the flag as
/// "hygiene": a client built WITHOUT it does not reach the stub under an exported
/// proxy, and the production client does.
/// What: exports `HTTP_PROXY` pointing at a dead address, probes a live stub
/// twice — once with a proxy-honouring client, once with
/// [`build_probe_client`] — and restores the environment. Holds
/// [`ENV_TEST_LOCK`] because `HTTP_PROXY` is process-global.
/// Test: This is the test.
///
/// If a future reqwest DOES exempt loopback from proxies, the first assertion
/// below becomes obsolete and should be deleted — but `.no_proxy()` itself must
/// stay, because the crate cannot pin every consumer's reqwest patch level.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn probe_ignores_http_proxy_env() {
    let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let addr = stub_once(OK_LINE, r#"{"status":"ok","version":"0.39.0"}"#).await;
    let base = format!("http://{addr}");
    let dead_proxy = dead_addr();
    let previous = std::env::var("HTTP_PROXY").ok();
    unsafe {
        // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
        std::env::set_var("HTTP_PROXY", format!("http://{dead_proxy}"));
    }

    // A proxy-honouring client reproduces the bug: loopback goes to the dead proxy.
    let leaky = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_millis(500))
        .build()
        .expect("client builds");
    let leaked = probe_bases(&leaky, Some(base.clone()), None).await;

    // The production client must reach the stub regardless of the environment.
    let guarded = build_probe_client().expect("probe client builds");
    let guarded_outcome = probe_bases(&guarded, Some(base), None).await;

    unsafe {
        // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
        match previous {
            Some(v) => std::env::set_var("HTTP_PROXY", v),
            None => std::env::remove_var("HTTP_PROXY"),
        }
    }

    assert!(
        !matches!(leaked, ProbeOutcome::Serving { .. }),
        "a client WITHOUT .no_proxy() is expected to be diverted through HTTP_PROXY \
         (this is the #4246 mechanism); got {leaked:?}"
    );
    assert!(
        matches!(guarded_outcome, ProbeOutcome::Serving { .. }),
        ".no_proxy() must make the probe immune to an exported HTTP_PROXY; got \
         {guarded_outcome:?}"
    );
}

// ── Resolution ──────────────────────────────────────────────────────────────

/// Why: the fixed-port leg is transcribed from
/// `docs/architecture/port-assignments.md`, which is the workspace's
/// single cross-cutting inventory and has already been the subject of three
/// collision incidents. Pin every value so a silent drift is a test failure
/// rather than a probe pointed at another daemon.
/// What: asserts the six stable-set daemon ports and that a non-member resolves
/// to `None` (so no address is ever guessed).
/// Test: This is the test.
#[test]
fn fixed_ports_match_port_assignments_doc() {
    assert_eq!(fixed_port_for("trusty-memory"), Some(7070));
    assert_eq!(fixed_port_for("trusty-console"), Some(7788));
    assert_eq!(fixed_port_for("trusty-search"), Some(7878));
    assert_eq!(fixed_port_for("trusty-analyze"), Some(7879));
    assert_eq!(fixed_port_for("trusty-mpm"), Some(7880));
    assert_eq!(fixed_port_for("trusty-review"), Some(7891));
    // Never guess: `tga` is not a daemon, and `trusty-installer` binds nothing.
    assert_eq!(fixed_port_for("tga"), None);
    assert_eq!(fixed_port_for("trusty-installer"), None);
    // 7881 (mpm supervisor metrics) and 7882 (tcode) belong to other processes
    // and must not be probed as stable-set members.
    assert!(!matches!(fixed_port_for("trusty-code"), Some(7882)));
}

/// Why: `http_addr` is the PRIMARY discovery path — it is what survives a
/// `--port` override and auto-port-walking — so the resolver must read the real
/// file written by `trusty_common::write_daemon_addr`, not a mock.
/// What: plants an `http_addr` under a stubbed data dir and asserts both legs
/// resolve as documented (recorded from the file, fixed from the table).
/// Test: This is the test.
#[test]
fn resolve_probe_bases_reads_http_addr() {
    let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = crate::commands::test_support::stub_data_dir("trusty-memory", "127.0.0.1:7071");
    let (recorded, fixed) = resolve_probe_bases("trusty-memory", "trusty-memory");
    crate::commands::test_support::clear_data_dir_override(&dir);

    assert_eq!(recorded.as_deref(), Some("http://127.0.0.1:7071"));
    assert_eq!(fixed.as_deref(), Some("http://127.0.0.1:7070"));
}

/// Why: a member with neither a recorded address nor a documented default must
/// yield `NoAddress` — a clean "nothing to probe" — rather than a guessed port.
/// `NoAddress` is deliberately NOT confirmed-down: we observed nothing, so there
/// is no evidence to restart on.
/// What: an empty data dir plus a binary absent from the port table resolves to
/// two `None`s, and `probe_bases` on them is `NoAddress`.
/// Test: This is the test.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn probe_no_address_when_nothing_resolves() {
    let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = crate::commands::test_support::stub_empty_data_dir("probe-noaddr");
    let (recorded, fixed) = resolve_probe_bases("nobody-home-4246", "nobody-home-4246");
    assert_eq!(recorded, None);
    assert_eq!(fixed, None);

    let client = build_probe_client().expect("probe client builds");
    let outcome = probe_bases(&client, recorded, fixed).await;
    crate::commands::test_support::clear_data_dir_override(&dir);

    assert_eq!(outcome, ProbeOutcome::NoAddress);
    assert!(!outcome.is_confirmed_down());
}

/// Why: the end-to-end async entry point must actually wire resolution to the
/// transport — the two halves being individually correct does not prove
/// `probe_daemon_http` composes them. Uses a binary absent from the port table so
/// the ONLY leg is the `http_addr` the test plants, making the assertion
/// independent of whatever daemons are running on the developer's machine.
/// What: plants an `http_addr` pointing at a live stub and asserts
/// `probe_daemon_http` reports it serving.
/// Test: This is the test.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::await_holding_lock)]
async fn probe_uses_http_addr_when_fixed_port_unknown() {
    let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let addr = stub_once(OK_LINE, r#"{"status":"ok","version":"1.2.3"}"#).await;
    let app = "probe-http-e2e-4246";
    let dir = crate::commands::test_support::stub_data_dir(app, &addr);
    let outcome = probe_daemon_http(app, app).await;
    crate::commands::test_support::clear_data_dir_override(&dir);

    assert_eq!(
        outcome,
        ProbeOutcome::Serving {
            status: "ok".to_owned(),
            version: Some("1.2.3".to_owned()),
        }
    );
}

// ── The derived views ───────────────────────────────────────────────────────

/// Why: six existing call sites render `health_string()` into human tables and
/// `--json` contracts, and `VerifyTailReport::build` / `status`'s exit code both
/// branch on the exact words. Pin every variant's mapping so the vocabulary
/// cannot drift silently.
///
/// The `BadEnvelope` → `down` choice is deliberate and load-bearing: mapping it
/// to `unknown` (which `verified` tolerates) would print `VERIFIED` / exit 0 for
/// a stack about which we hold no health information at all.
/// What: asserts the string for each variant.
/// Test: This is the test.
#[test]
fn health_string_maps_every_variant() {
    assert_eq!(ProbeOutcome::NotInstalled.health_string(), "not_installed");
    assert_eq!(ProbeOutcome::Unprobeable.health_string(), "unknown");
    assert_eq!(ProbeOutcome::NoAddress.health_string(), "down");
    assert_eq!(ProbeOutcome::Refused.health_string(), "down");
    assert_eq!(ProbeOutcome::Timeout.health_string(), "down");
    assert_eq!(
        ProbeOutcome::HttpError { status: 502 }.health_string(),
        "down"
    );
    assert_eq!(
        ProbeOutcome::BadEnvelope {
            got: "?".to_owned()
        }
        .health_string(),
        "down",
        "a bad envelope must not be laundered into a tolerated `unknown`"
    );
    assert_eq!(
        ProbeOutcome::ProbeFailed {
            detail: "?".to_owned()
        }
        .health_string(),
        "down"
    );
    assert_eq!(
        ProbeOutcome::Serving {
            status: "ok".to_owned(),
            version: None
        }
        .health_string(),
        "healthy"
    );
    assert_eq!(
        ProbeOutcome::Serving {
            status: "degraded".to_owned(),
            version: None
        }
        .health_string(),
        "stale"
    );
}

/// Why: `tctl up`'s `ensure_member` branches on `MemberHealth`, not on a string.
/// `Unprobeable` must map to `Down` (not to a health verdict) so trusty-mpm keeps
/// falling through to its idempotent `start`, exactly as before #4246.
/// What: asserts the `MemberHealth` for each variant.
/// Test: This is the test.
#[test]
fn member_health_maps_every_variant() {
    assert_eq!(
        ProbeOutcome::NotInstalled.member_health(),
        MemberHealth::NotInstalled
    );
    assert_eq!(
        ProbeOutcome::Unprobeable.member_health(),
        MemberHealth::Down,
        "mpm must keep falling through to `start`, as it did pre-#4246"
    );
    for down in [
        ProbeOutcome::NoAddress,
        ProbeOutcome::Refused,
        ProbeOutcome::Timeout,
        ProbeOutcome::HttpError { status: 500 },
        ProbeOutcome::BadEnvelope {
            got: "?".to_owned(),
        },
        ProbeOutcome::ProbeFailed {
            detail: "?".to_owned(),
        },
    ] {
        assert_eq!(down.member_health(), MemberHealth::Down, "{down:?}");
    }
    assert_eq!(
        ProbeOutcome::Serving {
            status: "running".to_owned(),
            version: None
        }
        .member_health(),
        MemberHealth::HealthyVersionOk
    );
}

/// Why: pins the deliberate ASYMMETRY between the display vocabulary and the
/// repair gate, which is otherwise a trap: `health_string()` and
/// `is_confirmed_down()` disagree by design, and a future caller who conflates
/// them ("it says `down`, so restart it") reintroduces #4246 in one line.
/// `NoAddress` is the sharpest case — it reports `down` on purpose, because
/// mapping it to `unknown` would make `VerifyTailReport::build` and `status`'s
/// exit code report `VERIFIED` for a member whose address never resolved, yet
/// nothing was ever observed about it so there is nothing to repair.
///
/// Asserting the asymmetry EXISTS (rather than only asserting each view
/// separately, as the two sibling tests do) is what stops it being quietly
/// "tidied" into equivalence: if someone makes the sets coincide, this fails.
/// What: asserts `NoAddress` specifically maps to `down` while not being
/// confirmed-down; then, over every `down`-rendering variant, that the
/// confirmed-down set is a STRICT subset — non-empty on both sides.
/// Test: This is the test.
#[test]
fn down_health_string_is_not_a_kickstart_licence() {
    // The named trap, spelled out: `down` for display, no repair authorisation.
    assert_eq!(ProbeOutcome::NoAddress.health_string(), "down");
    assert!(
        !ProbeOutcome::NoAddress.is_confirmed_down(),
        "NoAddress reports `down` but observed NOTHING — it must never authorise \
         a kickstart. If this ever flips, `tctl install` can hard-restart a member \
         whose address simply failed to resolve."
    );

    let down_renderers = [
        ProbeOutcome::NoAddress,
        ProbeOutcome::Refused,
        ProbeOutcome::Timeout,
        ProbeOutcome::HttpError { status: 502 },
        ProbeOutcome::BadEnvelope {
            got: "<html>squatter</html>".to_owned(),
        },
        ProbeOutcome::ProbeFailed {
            detail: "no runtime".to_owned(),
        },
    ];
    let (repairable, benign): (Vec<_>, Vec<_>) = down_renderers
        .iter()
        .inspect(|o| {
            assert_eq!(o.health_string(), "down", "{o:?} must render `down`");
        })
        .partition(|o| o.is_confirmed_down());

    assert!(
        !benign.is_empty(),
        "the invariant under test is that `down` does NOT imply confirmed-down; if \
         every down-rendering variant is confirmed-down, the display string has \
         become a repair authorisation and #4246's gate is gone"
    );
    assert!(
        !repairable.is_empty(),
        "the mirror property: some `down` MUST still authorise repair, or a \
         genuinely dead daemon is never kickstarted (#2498)"
    );
}

/// Why: THE gate. `launchctl kickstart -k` is destructive — no `ExitTimeOut` in
/// the shared plist renderer means launchd SIGKILLs 20s after SIGTERM, inside
/// trusty-search's ≥30s-per-index flush budget — so only an observation at the
/// TRANSPORT layer may authorise it. Everything else means *something* answered,
/// or that we never looked.
/// What: exhaustive over every variant: exactly `Refused` and `Timeout` are
/// confirmed-down.
/// Test: This is the test.
#[test]
fn only_transport_failures_are_confirmed_down() {
    assert!(ProbeOutcome::Refused.is_confirmed_down());
    assert!(ProbeOutcome::Timeout.is_confirmed_down());
    for benign in [
        ProbeOutcome::NotInstalled,
        ProbeOutcome::Unprobeable,
        ProbeOutcome::NoAddress,
        ProbeOutcome::HttpError { status: 503 },
        ProbeOutcome::BadEnvelope {
            got: "clap: unexpected argument '--json'".to_owned(),
        },
        ProbeOutcome::ProbeFailed {
            detail: "no runtime".to_owned(),
        },
        ProbeOutcome::Serving {
            status: "ok".to_owned(),
            version: None,
        },
        ProbeOutcome::Serving {
            status: "degraded".to_owned(),
            version: None,
        },
    ] {
        assert!(
            !benign.is_confirmed_down(),
            "{benign:?} must never authorise a destructive kickstart"
        );
    }
}
