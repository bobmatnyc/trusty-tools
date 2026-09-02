//! Contract tests for the core RPC methods (#6288 slice 2).
//!
//! Why these are PARITY tests rather than "the socket answers 200-shaped JSON":
//! this slice's whole claim is that a route reached over the socket and the same
//! route reached over HTTP give the same answer. A test that only exercises the
//! RPC side proves the method exists; it cannot fail when the two transports
//! drift, which is the failure the slice exists to prevent. So every `parity_*`
//! case builds the axum router AND the RPC router from ONE `Arc<DaemonState>`,
//! issues the equivalent request both ways, and compares the decoded JSON.
//!
//! ## The comparison allowlist
//!
//! Five things are dropped before comparing, all on `mpm.doctor`, and all
//! because the DAEMON varies them between two calls of the same code rather
//! than because the transports disagree:
//!
//! - `generated_at`, which `DoctorReport::from_checks` stamps with `Utc::now()`
//!   at every call (`core::doctor`).
//! - the `worktree_disk` check's `message`, which reports how much of the
//!   worktree tree the probe measured inside its 3-second deadline.
//! - the `worktrees` check's `message`, which reports how many worktrees the
//!   reconciled inventory classified — "14 live, 265 not reclaimable" on one
//!   call and "266" on the next, because every agent on the machine adds and
//!   removes worktrees while the test runs (#6358).
//! - the `session_store` check's `message`, which reports the live record count
//!   and byte length of `~/.trusty-mpm/session-manager/sessions.json` — "43
//!   session record(s), 59773 byte(s)" on one call and "44 … 60950" on the
//!   next, because a concurrent managed session writes that file between the two
//!   reads (#6490).
//! - the `pty_headroom` check's `message`, which reports the machine's live
//!   pseudo-terminal census — "82 of 511 pseudo-terminals allocated" on one call
//!   and "83 of 511" on the next, because any process on the box opens or closes
//!   a pty between the two reads (#6577).
//!
//! All four probes sample host state the daemon re-reads per call, which is
//! what makes their messages the only host-sampled strings in the report. Their
//! `name` and `status` are still compared, so a check that vanished or changed
//! verdict between the transports still fails — see
//! [`HOST_SAMPLED_MESSAGE_CHECKS`] and [`blank_host_sampled_messages`].
//!
//! Nothing else is excused. Every other method is compared whole, `pid`
//! included — the two transports run in one process, so a `pid` that differed
//! would be a real finding.
//!
//! Test: this file IS the test module for [`super`].

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use trusty_common::error_capture::CapturedError;
use trusty_common::uds::server::{
    CODE_INTERNAL_ERROR, CODE_INVALID_PARAMS, CODE_METHOD_NOT_FOUND, RpcResponse, RpcRouter,
};

use super::{METHODS, register};
use crate::core::paths::FrameworkPaths;
use crate::daemon::api;
use crate::daemon::bug_report;
use crate::daemon::error::{CODE_NOT_FOUND, CODE_UNAVAILABLE};
use crate::daemon::state::DaemonState;

/// One daemon state rooted at an empty temp directory, plus the directory.
///
/// Why hermetic: a developer machine with a live `overseer.toml` and an
/// `OPENROUTER_API_KEY` builds a REAL LLM overseer, and
/// `rpc_llm_chat_without_overseer_reports_unavailable` would then get an answer
/// instead of the refusal it asserts (#1523). An empty root builds the disabled
/// deterministic overseer. The caller holds the `TempDir` for the test's life.
fn hermetic() -> (Arc<DaemonState>, TempDir) {
    let dir = tempfile::tempdir().expect("temp dir for hermetic DaemonState");
    let paths = FrameworkPaths::under(dir.path());
    (Arc::new(DaemonState::with_paths(&paths)), dir)
}

/// The RPC router this slice registers, over `state`.
fn rpc_router(state: &Arc<DaemonState>) -> RpcRouter {
    register(RpcRouter::new(), state)
}

/// RAII guard restoring `$HOME` on drop, including on a panic-driven unwind —
/// mirrors `core::session_assets::tests::HomeGuard`.
struct HomeGuard(Option<std::ffi::OsString>);

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: every caller is `#[serial_test::serial]`, so no other test
        // thread reads or writes the environment concurrently.
        match self.0.take() {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

/// Pin `$HOME` at a fresh tempdir for the guard's lifetime (#6580).
///
/// Why: `core::host_state_gate` embeds the LIVE `$HOME` in the refusal it hands
/// every tmux route, so a test that reads that refusal twice gets two different
/// strings whenever a sibling moves `$HOME` between the reads. Pinning makes the
/// classification — and therefore the message — the same for both reads. Both
/// returned values must stay alive for the test's body: dropping the `TempDir`
/// removes the directory `$HOME` names.
/// Callers MUST be tagged `#[serial_test::serial]`.
fn pinned_home() -> (TempDir, HomeGuard) {
    let home = tempfile::tempdir().expect("temp dir for a pinned $HOME");
    let prior = std::env::var_os("HOME");
    // SAFETY: caller is `#[serial_test::serial]`.
    unsafe { std::env::set_var("HOME", home.path()) };
    (home, HomeGuard(prior))
}

/// Drive one HTTP request through the real daemon router and decode the answer.
///
/// Why the real router rather than calling a handler directly: the parity claim
/// is about the ROUTE, so the path, the method, and the extractors all have to
/// participate. Calling the handler would skip exactly the decoding the socket
/// has to reproduce.
async fn http(
    state: &Arc<DaemonState>,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(v) => request
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&v).expect("encode body")))
            .expect("build request"),
        None => request.body(Body::empty()).expect("build request"),
    };

    let response = api::router(Arc::clone(state))
        .oneshot(request)
        .await
        .expect("the router must answer");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the response body");
    // A `StatusCode`-only refusal has an empty body; report it as `null` rather
    // than failing the decode, so an error-parity case can still see the status.
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("the HTTP body must be JSON")
    };
    (status, value)
}

/// Dispatch one JSON-RPC call against `router` and return the whole frame.
///
/// `dispatch` takes a raw frame for the same reason the server does — the
/// envelope checks are part of what is under test.
async fn rpc_frame(router: &RpcRouter, method: &str, params: Value) -> Value {
    let frame = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let response: RpcResponse = router
        .dispatch(&serde_json::to_vec(&frame).expect("encode the request frame"))
        .await;
    serde_json::to_value(response).expect("the response frame must serialise")
}

/// The `result` half of a call that must succeed.
async fn rpc_ok(router: &RpcRouter, method: &str, params: Value) -> Value {
    let frame = rpc_frame(router, method, params).await;
    assert!(
        frame.get("error").is_none(),
        "{method} must succeed, got {frame}"
    );
    frame
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("{method} answered no result: {frame}"))
}

/// The `error` half of a call that must be refused.
async fn rpc_err(router: &RpcRouter, method: &str, params: Value) -> Value {
    let frame = rpc_frame(router, method, params).await;
    frame
        .get("error")
        .cloned()
        .unwrap_or_else(|| panic!("{method} must be refused, got {frame}"))
}

/// Assert the two transports answered the same JSON for one route.
///
/// `drop_fields` is the allowlist this module's doc records; it is empty for
/// every method but `mpm.doctor`.
fn assert_same(method: &str, mut http_body: Value, mut rpc_result: Value, drop_fields: &[&str]) {
    for field in drop_fields {
        if let Some(map) = http_body.as_object_mut() {
            map.remove(*field);
        }
        if let Some(map) = rpc_result.as_object_mut() {
            map.remove(*field);
        }
    }
    assert_eq!(
        http_body, rpc_result,
        "{method} must answer identically over HTTP and the socket"
    );
}

// ── The method table ─────────────────────────────────────────────────────────

/// Why: four crates outside this one will dial these names by literal once the
/// slice-7 client swap lands, with no compile-time link to this table. Pinning
/// the registered set here turns a rename into a failing assertion rather than
/// a consumer that silently reports `method_not_found`.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_router_registers_every_documented_method() {
    let (state, _dir) = hermetic();
    let router = rpc_router(&state);
    let registered: Vec<&str> = router.method_names().collect();

    let mut documented: Vec<&str> = METHODS.to_vec();
    documented.sort_unstable();

    assert_eq!(
        registered, documented,
        "the router and METHODS must name the same set"
    );
    assert_eq!(
        METHODS.len(),
        20,
        "slice 2 owns twenty routes; a new one needs a row in core.rs's table too"
    );
}

/// Why: slice 1's contract — an unregistered name answers with a coded frame
/// rather than a dropped connection — has to survive the router gaining methods.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_reports_method_not_found_for_an_unknown_method() {
    let (state, _dir) = hermetic();
    let error = rpc_err(&rpc_router(&state), "mpm.no.such.method", json!({})).await;
    assert_eq!(error["code"], json!(CODE_METHOD_NOT_FOUND), "{error}");
}

/// Why: `params` is absent on a well-formed no-argument call, and a plain unit
/// struct refuses `null` — which would make every health probe fail with
/// `invalid_params`. [`super::NoParams`] exists to prevent that, and this is
/// what proves it.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_health_answers_with_no_params() {
    let (state, _dir) = hermetic();
    let result = rpc_ok(&rpc_router(&state), "mpm.health", Value::Null).await;
    assert_eq!(result["status"], json!("ok"), "{result}");
}

/// Why: a no-argument method has nothing to get wrong, so refusing a stray
/// field would turn an additive client change into an outage.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_health_answers_with_a_stray_params_object() {
    let (state, _dir) = hermetic();
    let result = rpc_ok(&rpc_router(&state), "mpm.health", json!({"unknown": 1})).await;
    assert_eq!(result["status"], json!("ok"), "{result}");
}

/// Why: a method that DOES take arguments must refuse a payload it cannot
/// decode, with the reason, rather than running on a default.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_reports_invalid_params_for_an_undecodable_payload() {
    let (state, _dir) = hermetic();
    let error = rpc_err(
        &rpc_router(&state),
        "mpm.tmux.snapshot",
        json!({"name": 42}),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_INVALID_PARAMS), "{error}");
}

// ── Parity: one state, two transports, one answer ────────────────────────────

/// Why: `/health` is what `tm doctor` and every liveness probe read, so a drift
/// between the transports here is the one a consumer notices first.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_health_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let (status, body) = http(&state, "GET", "/health", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.health", Value::Null).await;
    assert_same("mpm.health", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_breakers_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let (status, body) = http(&state, "GET", "/breakers", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.breakers", Value::Null).await;
    assert_same("mpm.breakers", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_optimizer_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let (status, body) = http(&state, "GET", "/optimizer", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.optimizer", Value::Null).await;
    assert_same("mpm.optimizer", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_overseer_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let (status, body) = http(&state, "GET", "/overseer", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.overseer", Value::Null).await;
    assert_same("mpm.overseer", body, result, &[]);
}

/// One captured error written into the hermetic daemon's own store, returning
/// its fingerprint.
///
/// Why (#6505): without a store it controls, the errors parity case compares
/// whatever the developer's real `errors.jsonl` happened to hold at each of its
/// two reads — a value another test could change between them. Seeding one
/// record makes the expected answer a constant, and makes a regression of the
/// [`DaemonState::error_store_base`] seam visible: a body that went back to the
/// ambient data directory would not find this fingerprint.
/// What: writes a single JSONL line to the `trusty-mpm` store under the state's
/// pinned base, which for [`hermetic`] is the temp framework root.
fn seed_one_error(state: &Arc<DaemonState>) -> String {
    let base = state
        .error_store_base()
        .expect("a hermetic daemon must pin its error-store base");
    let path = bug_report::store_paths_under(base)
        .into_iter()
        .find(|p| p.ends_with("trusty-mpm/errors.jsonl"))
        .expect("the aggregated store set must include this crate's own daemon");
    std::fs::create_dir_all(path.parent().expect("the store path has a parent"))
        .expect("create the hermetic store directory");

    let record = CapturedError {
        timestamp_secs: 1_700_000_000,
        crate_target: "trusty_mpm::daemon::rpc".to_string(),
        crate_version: "0.0.0-test".to_string(),
        message: "seeded parity record".to_string(),
        fields: String::new(),
        file: Some("src/daemon/rpc/core_tests.rs".to_string()),
        line: Some(1),
        os: "test-os".to_string(),
        arch: "test-arch".to_string(),
        fingerprint: "6505000000000000000000000000000000000000000000000000000000000000".to_string(),
    };
    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string(&record).expect("encode record")
        ),
    )
    .expect("write the hermetic error store");
    record.fingerprint
}

/// Why the explicit `?limit=`: the HTTP route defaults an absent `limit` to 20
/// inside the shared body, and passing it on both sides proves the ARGUMENT
/// survives the transport change rather than only the default.
///
/// Why no `#[serial]` despite the sibling doctor case carrying one (#6505): this
/// body used to read `<data_dir>/<app>/errors.jsonl`, resolved through the
/// process-global `TRUSTY_DATA_DIR_OVERRIDE` on EVERY call, so a test that set
/// or cleared that variable between the two transport calls made them read
/// different directories — observed as `total: 5` over HTTP and `total: 0` over
/// the socket. The daemon now pins its store base at construction
/// ([`DaemonState::error_store_base`]), so both calls read the one temp
/// directory this test seeded and no process state can move it. A seam, not a
/// lock: `#[serial]` would only have serialised this binary's own tests, and
/// nextest gives every test its own process where that attribute serialises
/// nothing (#4162).
/// Test: this function IS the test.
#[tokio::test]
async fn parity_errors_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let fingerprint = seed_one_error(&state);

    let (status, body) = http(&state, "GET", "/api/v1/errors?limit=5", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.errors.list", json!({"limit": 5})).await;
    assert_eq!(body["limit"], json!(5), "the argument must reach the body");
    // The seeded record, and only it: reading the ambient data directory instead
    // would report the operator's real errors (or none of them).
    assert_eq!(
        body["total"],
        json!(1),
        "the hermetic store holds one: {body}"
    );
    assert_eq!(
        body["errors"][0]["fingerprint"],
        json!(fingerprint),
        "{body}"
    );
    assert_same("mpm.errors.list", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_tmux_sessions_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let (status, body) = http(&state, "GET", "/tmux/sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.tmux.sessions", Value::Null).await;
    assert_same("mpm.tmux.sessions", body, result, &[]);
}

/// Why `generated_at` is dropped: `DoctorReport::from_checks` stamps it with
/// `Utc::now()` on every call, so two calls differ there by construction. The
/// two [`HOST_SAMPLED_MESSAGE_CHECKS`] messages are blanked for the same
/// reason. Every other field — the overall status, and every check's name,
/// status and message — is compared whole.
///
/// Why serial: the agent, asset-tier and hooks checks resolve paths under
/// `$HOME`, and several tests in this binary move `$HOME` process-wide through
/// `test_support`'s `override_home`. Without the crate-wide serial group one of
/// them can land BETWEEN this test's two calls, so the HTTP report reads a temp
/// home and the socket report reads the real one — which fails the comparison
/// while proving nothing about the transports. `HomeOverride`'s doc records why
/// that group has to be crate-wide rather than a local lock.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn parity_doctor_agrees_across_transports() {
    let (state, dir) = hermetic();
    let project = dir.path().display().to_string();
    let (status, body) = http(
        &state,
        "GET",
        &format!("/api/v1/doctor?project={project}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.doctor",
        json!({"project": project}),
    )
    .await;
    assert!(
        body.get("generated_at").is_some(),
        "the allowlist entry must name a field that exists: {body}"
    );
    assert_same(
        "mpm.doctor",
        blank_host_sampled_messages(body),
        blank_host_sampled_messages(result),
        &["generated_at"],
    );
}

/// The doctor checks whose `message` samples live host state, by name (#6358,
/// #6490, #6577).
///
/// The membership rule, so the next such check need not flake first: a check
/// belongs here when its message embeds a number or a name it RE-SAMPLES FROM
/// THE HOST on every call. A message built from config on disk, or from the
/// daemon's own state, does not.
///
/// Why exactly these four: each puts live host state the daemon re-reads per
/// call into its message. `worktree_disk` reports how far it got against a
/// 3-second deadline ("6.2 GiB … 268 worktree(s) went unmeasured" on one call,
/// "6.8 GiB … 267" on the next) and `worktrees` reports the reconciled
/// inventory's counts ("14 live, 265 not reclaimable" against "266"), both
/// changing whenever an agent elsewhere on the machine adds or removes a
/// worktree between this test's two calls. `session_store` reports the record
/// count and byte length of `sessions.json` ("43 session record(s), 59773
/// byte(s)" against "44 … 60950"), which change whenever a concurrent managed
/// session writes that file between the two reads (#6490). `pty_headroom`
/// reports a live census of allocated pseudo-terminals ("82 of 511
/// pseudo-terminals allocated" against "83 of 511"), which changes whenever any
/// process on this machine opens or closes a pty between the two reads (#6577).
/// All four are the
/// PROBE varying, not the transports disagreeing. Only the MESSAGE is host-
/// sampled: each check's `status` is a stable verdict (`session_store` stays
/// `Ok` while only its counts churn), so it is still compared — a store that
/// went `Fail` over one transport but not the other is a real finding this
/// test must still catch. Every other check's message reads config on disk or a
/// daemon's own answer, and is compared whole.
/// What: the names [`blank_host_sampled_messages`] blanks.
/// Test: [`parity_doctor_agrees_across_transports`],
/// [`session_store_message_is_excluded_from_parity_but_its_status_is_not`].
const HOST_SAMPLED_MESSAGE_CHECKS: &[&str] = &[
    "worktree_disk",
    "worktrees",
    // #6490: session_store's message samples sessions.json's live record count.
    "session_store",
    // #6577: pty_headroom's message samples this machine's live pty census.
    "pty_headroom",
];

/// Blank each [`HOST_SAMPLED_MESSAGE_CHECKS`] message, keeping name and status.
///
/// Why the presence assertion: a blank-by-name pass over a report that no
/// longer contains the name would silently exclude nothing, and the test would
/// go back to flaking on a message it believed it had dropped. Requiring every
/// named check to appear turns a rename into a failure that says so.
fn blank_host_sampled_messages(mut report: Value) -> Value {
    let mut blanked: Vec<String> = Vec::new();
    if let Some(checks) = report.get_mut("checks").and_then(Value::as_array_mut) {
        for check in checks {
            let Some(name) = check.get("name").and_then(Value::as_str).map(str::to_owned) else {
                continue;
            };
            if HOST_SAMPLED_MESSAGE_CHECKS.contains(&name.as_str())
                && let Some(map) = check.as_object_mut()
            {
                map.insert("message".into(), json!("<host-sampled>"));
                blanked.push(name);
            }
        }
    }
    for name in HOST_SAMPLED_MESSAGE_CHECKS {
        assert!(
            blanked.iter().any(|seen| seen == name),
            "the allowlist names the `{name}` check, which this report does not \
             contain — a renamed check must fail here rather than silently stop \
             being excluded"
        );
    }
    report
}

/// Pin what adding `session_store` to the allowlist bought (#6490): its live-
/// sampled MESSAGE is dropped from the parity comparison, while its STATUS is
/// not — so the count/bytes churn that flaked the test is excused, but a genuine
/// cross-transport verdict divergence still fails.
///
/// Why a hand-built pair rather than the live parity test: the flake needs
/// another process to write `sessions.json` between the two reads, which cannot
/// be forced deterministically. Feeding [`blank_host_sampled_messages`] two
/// reports directly reproduces both the churn (message differs, status same) and
/// the real break (status differs) with no host dependency, and proves the
/// exclusion is not vacuous — the exact bar #6358's fix was held to.
/// Test: this function IS the test.
#[test]
fn session_store_message_is_excluded_from_parity_but_its_status_is_not() {
    // Every host-sampled check is present, so the presence assertion inside
    // `blank_host_sampled_messages` is satisfied and only the exclusion is tested.
    let report = |session_store_status: &str, session_store_message: &str| {
        json!({
            "checks": [
                {"name": "worktree_disk", "status": "ok", "message": "6.2 GiB measured"},
                {"name": "worktrees", "status": "ok", "message": "14 live, 265 not reclaimable"},
                {"name": "session_store", "status": session_store_status, "message": session_store_message},
                // #6577: present so the presence assertion is satisfied.
                {"name": "pty_headroom", "status": "ok", "message": "82 of 511 pseudo-terminals allocated"},
            ]
        })
    };

    // The #6490 case: a concurrent session grew `sessions.json` between the two
    // reads, so only the message differs. After blanking, the transports agree.
    let http = blank_host_sampled_messages(report(
        "ok",
        "…/sessions.json loads cleanly — 43 session record(s), 59773 byte(s)",
    ));
    let socket = blank_host_sampled_messages(report(
        "ok",
        "…/sessions.json loads cleanly — 44 session record(s), 60950 byte(s)",
    ));
    assert_eq!(
        http, socket,
        "a session_store message that churned between the reads must not fail parity"
    );

    // Non-vacuous: a real verdict divergence survives the blanking, so the
    // parity assertion still fails on it. A store one transport called healthy
    // and the other called corrupt is a finding, not noise.
    let http = blank_host_sampled_messages(report("ok", "loads cleanly"));
    let socket = blank_host_sampled_messages(report("fail", "loads cleanly"));
    assert_ne!(
        http, socket,
        "a session_store STATUS divergence must still fail parity"
    );
}

/// Why an unknown fingerprint: it exercises the whole route — argument decode,
/// the store scan, the response shape — while filing nothing and spending
/// nothing, which is what makes it safe to run in a unit suite.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_report_bug_preview_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let payload = json!({
        "fingerprint": "0".repeat(64),
        "confirm": false,
    });
    let (status, body) = http(&state, "POST", "/api/v1/report-bug", Some(payload.clone())).await;
    assert_eq!(status, StatusCode::OK, "this route is always 200");
    assert_eq!(body["filed"], json!(false), "nothing may be filed: {body}");
    let result = rpc_ok(&rpc_router(&state), "mpm.report_bug", payload).await;
    assert_same("mpm.report_bug", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_claude_config_get_agrees_across_transports() {
    let (state, dir) = hermetic();
    let project = dir.path().display().to_string();
    let (status, body) = http(
        &state,
        "GET",
        &format!("/claude-config?project={project}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.claude_config.get",
        json!({"project": project}),
    )
    .await;
    assert_same("mpm.claude_config.get", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_claude_config_checkpoints_agrees_across_transports() {
    let (state, dir) = hermetic();
    let project = dir.path().display().to_string();
    let (status, body) = http(
        &state,
        "GET",
        &format!("/claude-config/checkpoints?project={project}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.claude_config.checkpoints.list",
        json!({"project": project}),
    )
    .await;
    assert_same("mpm.claude_config.checkpoints.list", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_claude_config_profiles_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let (status, body) = http(&state, "GET", "/claude-config/profiles", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.claude_config.profiles",
        Value::Null,
    )
    .await;
    assert_same("mpm.claude_config.profiles", body, result, &[]);
}

// ── Error parity: one failure, two renderings ────────────────────────────────

/// Why 503 rather than any refusal: an unconfigured capability is the one error
/// class on this slice that a caller can act on (set a key), so losing its
/// distinctness in the move to the socket would cost real information.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_llm_chat_without_overseer_reports_unavailable() {
    let (state, _dir) = hermetic();
    let payload = json!({"message": "hello", "history": []});

    let (status, body) = http(&state, "POST", "/llm/chat", Some(payload.clone())).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    let error = rpc_err(&rpc_router(&state), "mpm.llm.chat", payload).await;
    assert_eq!(error["code"], json!(CODE_UNAVAILABLE), "{error}");
    assert_eq!(
        error["message"], body["error"],
        "the socket must carry the HTTP body's message verbatim"
    );
}

/// Why the unknown profile name: it is the only 404 on this slice that fires
/// deterministically on any machine — no tmux, no config, no network.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_claude_config_deploy_unknown_profile_reports_not_found() {
    let (state, dir) = hermetic();
    let payload = json!({
        "project": dir.path().display().to_string(),
        "profile_name": "no-such-profile-xyz",
    });

    let (status, _body) = http(
        &state,
        "POST",
        "/claude-config/deploy",
        Some(payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "HTTP must still 404");

    let error = rpc_err(&rpc_router(&state), "mpm.claude_config.deploy", payload).await;
    assert_eq!(error["code"], json!(CODE_NOT_FOUND), "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no-such-profile-xyz"),
        "the refusal must name what was not found: {error}"
    );
}

/// Why: the recommendation id is the second 404 on this slice, and it fires on
/// an empty project directory — which analyses to a recommendation set that
/// certainly does not contain this id.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_claude_config_apply_unknown_recommendation_reports_not_found() {
    let (state, dir) = hermetic();
    let payload = json!({
        "project": dir.path().display().to_string(),
        "recommendation_id": "no-such-recommendation-xyz",
    });

    let (status, _body) = http(
        &state,
        "POST",
        "/claude-config/apply",
        Some(payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "HTTP must still 404");

    let error = rpc_err(&rpc_router(&state), "mpm.claude_config.apply", payload).await;
    assert_eq!(error["code"], json!(CODE_NOT_FOUND), "{error}");
}

/// Why this one asserts the DERIVED code rather than a literal: what tmux does
/// on a machine with no tmux binary and on one with tmux but no such session are
/// different failure kinds, and pinning either would make the test a report on
/// the machine rather than on the transport. What must hold either way is that
/// the socket's code is the code the HTTP status maps to and its message is the
/// HTTP body's message — which is the whole of requirement 3.
///
/// Why the pinned `$HOME` and the serial group (#6580): the refusal both
/// transports carry comes from `core::host_state_gate`, whose message names the
/// LIVE `$HOME` ("$HOME is /var/folders/… but this user's real home is
/// /Users/…"). Several tests in this binary move `$HOME` process-wide, and one
/// landing between the two calls below left the HTTP read and the socket read
/// quoting different homes — the verbatim-message assertion then failed while
/// proving nothing about the transports. `#[serial_test::serial]` keeps those
/// tests out, and [`pinned_home`] fixes what the gate classifies for both reads.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn rpc_tmux_snapshot_unknown_session_reports_a_coded_error() {
    let (_home, _home_guard) = pinned_home();
    let (state, _dir) = hermetic();

    let (status, body) = http(
        &state,
        "GET",
        "/tmux/sessions/no-such-session-xyz/snapshot",
        None,
    )
    .await;
    assert!(
        status.is_client_error() || status.is_server_error(),
        "{status}"
    );

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.tmux.snapshot",
        json!({"name": "no-such-session-xyz"}),
    )
    .await;
    assert_eq!(
        error["message"], body["error"],
        "the socket must carry the HTTP body's message verbatim"
    );
    let expected = match status {
        StatusCode::NOT_FOUND => CODE_NOT_FOUND,
        StatusCode::INTERNAL_SERVER_ERROR => trusty_common::uds::server::CODE_INTERNAL_ERROR,
        other => panic!("unexpected status for a missing tmux session: {other}"),
    };
    assert_eq!(error["code"], json!(expected), "{error}");
}

/// Why an adopt of a session that is not there: it is the write-shaped tmux
/// route, and it must refuse identically over both transports rather than
/// reporting success on one.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_tmux_adopt_unknown_session_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let payload = json!({"session": "no-such-session-xyz"});

    let (status, body) = http(&state, "POST", "/tmux/adopt", Some(payload.clone())).await;
    assert!(
        status.is_client_error() || status.is_server_error(),
        "{status}"
    );

    let error = rpc_err(&rpc_router(&state), "mpm.tmux.adopt", payload).await;
    assert_eq!(
        error["message"], body["error"],
        "the socket must carry the HTTP body's message verbatim"
    );
}

// ── The four write-shaped claude-config methods ──────────────────────────────
//
// Why these get their own section: `checkpoints.create`, `checkpoints.delete`,
// `restore` and `restart` all WRITE (or drive tmux), so a whole-body comparison
// against an HTTP call would perform the side effect twice and — for `create`,
// whose id embeds a timestamp and four random characters — compare two ids that
// can never be equal. Each therefore asserts the same persisted effect its HTTP
// test asserts, driven over the socket, or the failure both transports must
// agree on. Every one dials a registered method and asserts a code OTHER than
// `method_not_found`, so deleting the registration line in `core.rs::register`
// fails the test rather than silently passing it.

/// Why: `create` is the only method on this slice whose success is a WRITE, and
/// its HTTP test (`create_checkpoint_returns_id`) proves it by listing the
/// checkpoint back. This proves the socket performs the same write, with both
/// halves over the socket — so a `create` registered but wired to the wrong body
/// lists nothing.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_claude_config_checkpoint_create_then_list_sees_it() {
    let (state, _dir) = hermetic();
    let project = tempfile::tempdir().expect("project dir");
    let project_path = project.path().display().to_string();
    let router = rpc_router(&state);

    let created = rpc_ok(
        &router,
        "mpm.claude_config.checkpoints.create",
        json!({"project": project_path, "label": "from-the-socket"}),
    )
    .await;
    let id = created["id"]
        .as_str()
        .expect("create returns an id")
        .to_string();
    assert!(!id.is_empty(), "the id must not be empty: {created}");

    let listed = rpc_ok(
        &router,
        "mpm.claude_config.checkpoints.list",
        json!({"project": project_path}),
    )
    .await;
    assert!(
        checkpoint_ids(&listed).contains(&id),
        "the checkpoint written over the socket must be listed back: {listed}"
    );
}

/// Why: `delete`'s effect is a removal, so the only assertion that proves it ran
/// is that a checkpoint which WAS listed no longer is. Creating it over the
/// socket first keeps the whole round trip on the transport under test.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_claude_config_checkpoint_delete_removes_a_created_checkpoint() {
    let (state, _dir) = hermetic();
    let project = tempfile::tempdir().expect("project dir");
    let project_path = project.path().display().to_string();
    let router = rpc_router(&state);

    let created = rpc_ok(
        &router,
        "mpm.claude_config.checkpoints.create",
        json!({"project": project_path}),
    )
    .await;
    let id = created["id"]
        .as_str()
        .expect("create returns an id")
        .to_string();
    assert!(
        checkpoint_ids(
            &rpc_ok(
                &router,
                "mpm.claude_config.checkpoints.list",
                json!({"project": project_path}),
            )
            .await
        )
        .contains(&id),
        "the checkpoint must exist before the delete is meaningful"
    );

    let deleted = rpc_ok(
        &router,
        "mpm.claude_config.checkpoints.delete",
        json!({"project": project_path, "id": id}),
    )
    .await;
    assert_eq!(
        deleted["deleted"],
        json!(id),
        "delete must name the checkpoint it removed: {deleted}"
    );

    let listed = rpc_ok(
        &router,
        "mpm.claude_config.checkpoints.list",
        json!({"project": project_path}),
    )
    .await;
    assert!(
        !checkpoint_ids(&listed).contains(&id),
        "the deleted checkpoint must be gone from the listing: {listed}"
    );
}

/// The `id` of every checkpoint in a `checkpoints.list` result.
fn checkpoint_ids(listed: &Value) -> Vec<String> {
    listed["checkpoints"]
        .as_array()
        .expect("checkpoints is an array")
        .iter()
        .filter_map(|c| c["id"].as_str().map(str::to_owned))
        .collect()
}

/// Why: a checkpoint id that was never written is the one `delete` failure both
/// transports reach with no setup, and HTTP answers 404 for it
/// (`delete_unknown_checkpoint_is_404`). The socket must answer the code that
/// 404 maps to, not the `method_not_found` a missing registration would give.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_claude_config_delete_unknown_checkpoint_reports_not_found() {
    let (state, _dir) = hermetic();
    let project = tempfile::tempdir().expect("project dir");
    let project_path = project.path().display().to_string();

    let (status, _body) = http(
        &state,
        "DELETE",
        &format!("/claude-config/checkpoints/no-such-checkpoint?project={project_path}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "HTTP must still 404");

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.claude_config.checkpoints.delete",
        json!({"project": project_path, "id": "no-such-checkpoint"}),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_NOT_FOUND), "{error}");
}

/// Why: restoring an id that was never written is `restore`'s reachable failure,
/// and HTTP answers 500 for it (`restore_unknown_checkpoint_is_500`) — a missing
/// checkpoint and a failed rewrite are deliberately the same status there. This
/// pins that the socket agrees rather than inventing a 404.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_claude_config_restore_unknown_checkpoint_reports_internal_error() {
    let (state, _dir) = hermetic();
    let project = tempfile::tempdir().expect("project dir");
    let payload = json!({
        "project": project.path().display().to_string(),
        "checkpoint_id": "no-such-checkpoint",
    });

    let (status, _body) = http(
        &state,
        "POST",
        "/claude-config/restore",
        Some(payload.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "HTTP must still 500"
    );

    let error = rpc_err(&rpc_router(&state), "mpm.claude_config.restore", payload).await;
    assert_eq!(error["code"], json!(CODE_INTERNAL_ERROR), "{error}");
}

/// Why: `restart` drives tmux, so what it does on a machine with no tmux and on
/// one with tmux but no such session are different failure kinds — pinning
/// either would make this a report on the machine. What holds either way is that
/// the socket's code is the one the observed HTTP status maps to, which is also
/// what rules out `method_not_found`.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_claude_config_restart_unknown_session_reports_a_coded_error() {
    let (state, _dir) = hermetic();
    let payload = json!({"tmux_session": "no-such-session-xyz"});

    let (status, _body) = http(
        &state,
        "POST",
        "/claude-config/restart",
        Some(payload.clone()),
    )
    .await;
    assert!(
        status.is_client_error() || status.is_server_error(),
        "restarting a session that is not there must fail: {status}"
    );

    let error = rpc_err(&rpc_router(&state), "mpm.claude_config.restart", payload).await;
    let expected = match status {
        StatusCode::NOT_FOUND => CODE_NOT_FOUND,
        StatusCode::INTERNAL_SERVER_ERROR => CODE_INTERNAL_ERROR,
        other => panic!("unexpected status for a missing tmux session: {other}"),
    };
    assert_eq!(error["code"], json!(expected), "{error}");
}
