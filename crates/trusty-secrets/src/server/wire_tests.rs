//! The exec-grant methods over real sockets (#9070, S8 slice 2).
//!
//! A child of `server_tests`, so it shares that module's fixture: every path
//! is under a `TempDir`, including the audit log, and the backends are
//! in-memory. The socket peer of every call is this test process, so a test
//! that needs a process tree puts this pid into a fake process table.
//!
//! Test: itself.

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::sync::{Mutex, Once};

use trusty_common::uds::server::ServeExit as UdsServeExit;

use super::*;
use crate::server::grant_fakes::{FakeClock, FakeProcs};
use crate::server::router::serving_ends;

// #9070: S8 slice 3, the agents flag; it shares this module's helpers.
#[path = "flag_tests.rs"]
mod flag_tests;

/// The project vault of the fixture's `Acme/Web` remote.
const PROJECT_VAULT: &str = "trusty/acme/web";
/// A value whose text appears nowhere else in the crate.
const SENTINEL_VALUE: &str = "SENTINEL-VALUE-9070-zq4Rk8Lw2Pn6";
/// A flagged key's value.
const OPEN_VALUE: &str = "open-value-9070-Hc3Vt";
const T0: u64 = 1_000_000;

fn me() -> u32 {
    std::process::id()
}

/// Seed `name` = `value` in the project vault, flagged "agents may use"
/// when `flagged`.
fn seed(fx: &Fixture, name: &str, value: &str, flagged: bool) {
    let store = SecretStore::new(
        Arc::clone(&fx.keychain) as Arc<dyn SecretBackend>,
        NamesIndex::at(&fx.settings.index_root),
    );
    let (web, name) = (vault(PROJECT_VAULT), key(name));
    store.set(&web, &name, &SecretValue::new(value)).unwrap();
    if flagged {
        store.set_agents_may_use(&web, &name, true).unwrap();
    }
}

/// `1 -> 10 -> { me, 20 (start 200) -> 30 }`: this test process is a
/// sibling of the child 20.
fn procs_with_me() -> Arc<FakeProcs> {
    let procs = FakeProcs::default();
    procs
        .add(1, 0, 1)
        .add(10, 1, 100)
        .add(me(), 10, 4242)
        .add(20, 10, 200)
        .add(30, 20, 300);
    Arc::new(procs)
}

/// The fixture's state with a grant registry over `procs` and a fake clock.
fn state_with(fx: &Fixture, procs: Arc<FakeProcs>) -> (State, Arc<GrantRegistry>) {
    let mut state = fx.state(fx.backends());
    let grants = Arc::new(GrantRegistry::new(
        procs,
        Arc::new(FakeClock::at(T0)),
        DEFAULT_MAX_TTL,
    ));
    state.grants = Arc::clone(&grants);
    (state, grants)
}

fn grant_params(fx: &Fixture, child: u32, keys: &[&str]) -> Value {
    json!({"project": fx.project(), "child_pid": child, "keys": keys, "ttl_secs": 600})
}

fn resolve_params(token: &str, name: &str) -> Value {
    json!({"token": token, "key": name})
}

/// Grant `keys` to `child` and return the token.
async fn granted(fx: &Fixture, child: u32, keys: &[&str]) -> String {
    let answer = ok(call(
        &fx.settings.socket,
        method::GRANT,
        grant_params(fx, child, keys),
    )
    .await);
    answer["token"].as_str().unwrap().to_owned()
}

/// Every record in the fixture's audit log; none when it is absent.
fn records(fx: &Fixture) -> Vec<AuditRecord> {
    match std::fs::read_to_string(&fx.settings.audit_log) {
        Ok(text) => text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("{e}"),
    }
}

/// A record's (method, decision, reason, key) shape.
fn shape(record: &AuditRecord) -> (AuditMethod, AuditDecision, Option<&str>, Option<&str>) {
    (
        record.method,
        record.decision,
        record.reason.as_ref().map(AuditReason::as_str),
        record.key.as_ref().map(SecretKey::as_str),
    )
}

/// The value text must be absent from a reply.
fn assert_no_value(response: &RpcResponse) {
    assert!(response.result.is_none(), "{:?}", response.result);
    assert!(!wire(response).contains(SENTINEL_VALUE));
}

/// Every tracing event the test binary emits once [`capture_logs`] ran.
static LOGS: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static INSTALL: Once = Once::new();

struct LogSink;

impl Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        LOGS.lock()
            .map_err(|_| io::Error::other("log capture poisoned"))?
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Install a global TRACE subscriber writing into [`LOGS`].
fn capture_logs() {
    INSTALL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(|| LogSink)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

fn captured() -> String {
    String::from_utf8_lossy(&LOGS.lock().unwrap()).into_owned()
}

/// Why: AC 1 — a resolve with no grant answers the one fixed refusal, no
/// value, and leaves exactly one deny record naming the key and the peer.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_without_grant_returns_no_value_and_one_deny_record() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, true);
    let server = fx.start().await;
    let forged = "0".repeat(64);
    let response = call(
        &fx.settings.socket,
        method::RESOLVE,
        resolve_params(&forged, "API_KEY"),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&response, method::RESOLVE),
        ErrorKind::GrantRefused
    );
    assert_no_value(&response);
    let all = records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(
        shape(&all[0]),
        (
            AuditMethod::Resolve,
            AuditDecision::Deny,
            Some("grant_refused"),
            Some("API_KEY")
        )
    );
    assert_eq!(all[0].caller_pid, Some(me()));
}

/// Why: AC 3 — a registrar with a Claude Code ancestor (judged on the fake
/// table from its peer pid) cannot grant an unflagged key; nothing is
/// minted and the refusal is audited with the key.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grant_by_agent_ancestor_cannot_name_unflagged_key() {
    let fx = fixture();
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let procs = procs_with_me();
    procs.set_agent(10);
    let (state, grants) = state_with(&fx, procs);
    let server = fx.start_state(state).await;
    let refused = call(
        &fx.settings.socket,
        method::GRANT,
        grant_params(&fx, me(), &["OPEN_KEY", "API_KEY"]),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&refused, method::GRANT),
        ErrorKind::AgentUseRefused
    );
    assert!(refused.result.is_none());
    assert_eq!(grants.has_unexpired(), Ok(false), "nothing was minted");
    let all = records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(
        shape(&all[0]),
        (
            AuditMethod::Grant,
            AuditDecision::Deny,
            Some("agent_use_refused"),
            Some("API_KEY")
        )
    );
    assert_eq!(all[0].caller_pid, Some(me()));
}

/// Why: AC 3, allow arm — the same registrar may grant a flagged key, and
/// its own process then resolves it.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grant_by_agent_ancestor_may_name_a_flagged_key() {
    let fx = fixture();
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let procs = procs_with_me();
    procs.set_agent(10);
    let (state, _grants) = state_with(&fx, procs);
    let server = fx.start_state(state).await;
    let token = granted(&fx, me(), &["OPEN_KEY"]).await;
    let value = ok(call(
        &fx.settings.socket,
        method::RESOLVE,
        resolve_params(&token, "OPEN_KEY"),
    )
    .await);
    server.stop().await;
    assert_eq!(value["value"], OPEN_VALUE);
}

/// Why: AC 1 — a sibling of the granted child holding the valid token is
/// refused with the same error an unknown token gets, and sees no value.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_from_sibling_with_valid_token_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    let token = granted(&fx, 20, &["API_KEY"]).await;
    let socket = &fx.settings.socket;
    let sibling = call(socket, method::RESOLVE, resolve_params(&token, "API_KEY")).await;
    let unknown = call(
        socket,
        method::RESOLVE,
        resolve_params(&"f".repeat(64), "API_KEY"),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&sibling, method::RESOLVE),
        ErrorKind::GrantRefused
    );
    assert_no_value(&sibling);
    let (a, b) = (sibling.error.unwrap(), unknown.error.unwrap());
    assert_eq!((a.code, a.message, a.data), (b.code, b.message, b.data));
    let resolves: Vec<_> = records(&fx)
        .into_iter()
        .filter(|r| r.method == AuditMethod::Resolve)
        .collect();
    assert_eq!(resolves.len(), 2);
    assert!(
        resolves
            .iter()
            .all(|r| r.reason.as_ref().map(AuditReason::as_str) == Some("grant_refused"))
    );
}

/// Why: AC 5 — an allowed resolve whose audit record cannot be written
/// returns no value (`FAIL_CLOSED_ON_ALLOW`).
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_allow_with_unwritable_audit_returns_no_value() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    let token = granted(&fx, me(), &["API_KEY"]).await;
    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    set_mode(&dir, 0o755);
    let _restore = RestoreMode(dir);
    let response = call(
        &fx.settings.socket,
        method::RESOLVE,
        resolve_params(&token, "API_KEY"),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&response, method::RESOLVE),
        ErrorKind::AuditUnavailable
    );
    assert_no_value(&response);
}

/// Why: AC 5 for `grant` — no token leaves without its records, and the
/// minted grant is removed again.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grant_with_unwritable_audit_returns_no_token() {
    let fx = fixture();
    let (state, grants) = state_with(&fx, procs_with_me());
    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    set_mode(&dir, 0o755);
    let _restore = RestoreMode(dir);
    let server = fx.start_state(state).await;
    let response = call(
        &fx.settings.socket,
        method::GRANT,
        grant_params(&fx, me(), &["API_KEY"]),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&response, method::GRANT),
        ErrorKind::AuditUnavailable
    );
    assert!(response.result.is_none());
    assert_eq!(grants.has_unexpired(), Ok(false), "the grant was revoked");
}

/// Why: AC 8 — ruling 31's idle exit is deferred while a grant is live and
/// comes back once it expires or is revoked; a registry error exits.
/// Proven on the loop's own decision with a fake clock, no sleeps.
/// Test: itself.
#[test]
fn idle_exit_deferred_while_grant_live() {
    let clock = Arc::new(FakeClock::at(T0));
    let grants = GrantRegistry::new(procs_with_me(), clock.clone(), DEFAULT_MAX_TTL);
    let keys: BTreeSet<SecretKey> = [key("API_KEY")].into();
    let request = || GrantRequest::new(keys.clone(), 20, Duration::from_secs(60));
    assert!(serving_ends(UdsServeExit::Idle, &grants), "no grant");

    grants.mint(request()).unwrap();
    assert!(!serving_ends(UdsServeExit::Idle, &grants), "live grant");
    assert!(serving_ends(UdsServeExit::Shutdown, &grants), "shutdown");
    clock.set(T0 + 60);
    assert!(serving_ends(UdsServeExit::Idle, &grants), "expired");

    let token = grants.mint(request()).unwrap().token;
    assert!(!serving_ends(UdsServeExit::Idle, &grants), "live again");
    assert_eq!(grants.revoke(&token), Ok(true));
    assert!(serving_ends(UdsServeExit::Idle, &grants), "revoked");

    grants.mint(request()).unwrap();
    clock.fail();
    assert!(
        serving_ends(UdsServeExit::Idle, &grants),
        "clock error exits"
    );
}

/// Why: AC 6 — a resolved sentinel value, and the grant token, appear in
/// neither the audit file nor any captured log line. Also covers revoke:
/// one record, and the token stops working.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_sentinel_never_reaches_the_audit_file_or_logs() {
    capture_logs();
    tracing::info!("wire-tests capture check");
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    let socket = &fx.settings.socket;
    let token = granted(&fx, me(), &["API_KEY"]).await;
    assert_eq!(token.len(), 64);
    let resolved = ok(call(socket, method::RESOLVE, resolve_params(&token, "API_KEY")).await);
    let outside = call(socket, method::RESOLVE, resolve_params(&token, "OTHER")).await;
    let revoked = ok(call(socket, method::REVOKE, json!({"token": token})).await);
    let after = call(socket, method::RESOLVE, resolve_params(&token, "API_KEY")).await;
    server.stop().await;

    assert_eq!(resolved["value"], SENTINEL_VALUE, "the value was resolved");
    assert_eq!(resolved["key"], "API_KEY");
    assert_eq!(revoked["revoked"], true);
    for refused in [&outside, &after] {
        assert_eq!(
            fixed_error(refused, method::RESOLVE),
            ErrorKind::GrantRefused
        );
        assert_no_value(refused);
    }
    let audit = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(!audit.contains(SENTINEL_VALUE), "value in the audit file");
    assert!(!audit.contains(&token), "token in the audit file");
    let logs = captured();
    assert!(logs.contains("wire-tests capture check"), "capture is live");
    assert!(!logs.contains(SENTINEL_VALUE), "value in a log line");
    assert!(!logs.contains(&token), "token in a log line");

    use AuditDecision::{Allow, Deny};
    use AuditMethod::{Grant, Resolve, Revoke};
    let all = records(&fx);
    let shapes: Vec<_> = all.iter().map(shape).collect();
    assert_eq!(
        shapes,
        [
            (Grant, Allow, None, Some("API_KEY")),
            (Resolve, Allow, None, Some("API_KEY")),
            (Resolve, Deny, Some("grant_refused"), Some("OTHER")),
            (Revoke, Allow, None, None),
            (Resolve, Deny, Some("grant_refused"), Some("API_KEY")),
        ]
    );
    assert!(all.iter().all(|r| r.caller_pid == Some(me())));
}

/// Why: #9070 slice 3, the #9629 review's finding (A) — a non-agent may
/// register a grant for an agent's pid. The resolving caller's own ancestry
/// counts too, so that agent reads no unflagged key.
/// Red when `resolve` uses only the grant's `agent_parent`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_by_agent_descendant_of_unflagged_grant_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let procs = procs_with_me();
    let (state, _grants) = state_with(&fx, Arc::clone(&procs));
    let server = fx.start_state(state).await;
    let token = granted(&fx, me(), &["API_KEY"]).await;
    procs.set_agent(10);
    let response = call(
        &fx.settings.socket,
        method::RESOLVE,
        resolve_params(&token, "API_KEY"),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&response, method::RESOLVE),
        ErrorKind::AgentUseRefused
    );
    assert_no_value(&response);
    let resolves: Vec<_> = records(&fx)
        .into_iter()
        .filter(|r| r.method == AuditMethod::Resolve)
        .collect();
    assert_eq!(resolves.len(), 1, "{resolves:?}");
    assert_eq!(resolves[0].agent_parent, Some(true));
}

/// Why: #9070 slice 3, the #9629 QA finding — a grant record says whether
/// the registrar had an agent ancestor, so a live check observes the
/// decision instead of inferring it.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grant_records_carry_the_agent_parent_verdict() {
    let fx = fixture();
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let procs = procs_with_me();
    let (state, _grants) = state_with(&fx, Arc::clone(&procs));
    let server = fx.start_state(state).await;
    granted(&fx, me(), &["OPEN_KEY"]).await;
    procs.set_agent(10);
    granted(&fx, me(), &["OPEN_KEY"]).await;
    server.stop().await;

    let verdicts: Vec<_> = records(&fx)
        .into_iter()
        .filter(|r| r.method == AuditMethod::Grant)
        .map(|r| (r.decision, r.agent_parent))
        .collect();
    assert_eq!(
        verdicts,
        [
            (AuditDecision::Allow, Some(false)),
            (AuditDecision::Allow, Some(true))
        ]
    );
}

/// Why: #9629 review, MEDIUM — the token check, proven where no other check
/// would refuse: the caller is the granted process itself and the key is
/// seeded, so only the wrong token stands between it and the value.
/// Red when `authorize_scoped` accepts any token.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_with_wrong_token_from_inside_the_tree_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    let token = granted(&fx, me(), &["API_KEY"]).await;
    let wrong: String = token
        .chars()
        .map(|c| if c == '0' { '1' } else { '0' })
        .collect();
    let response = call(
        &fx.settings.socket,
        method::RESOLVE,
        resolve_params(&wrong, "API_KEY"),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&response, method::RESOLVE),
        ErrorKind::GrantRefused
    );
    assert_no_value(&response);
}

/// Why: #9629 review, MEDIUM — the key check, proven on a key that is
/// seeded in the project vault, so a missing check would return its value
/// instead of failing with `not_found`.
/// Red when `authorize_scoped` skips the key-in-grant check.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_of_seeded_key_outside_the_grant_is_refused() {
    let fx = fixture();
    seed(&fx, "OTHER_KEY", OPEN_VALUE, false);
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    let token = granted(&fx, me(), &["OTHER_KEY"]).await;
    let response = call(
        &fx.settings.socket,
        method::RESOLVE,
        resolve_params(&token, "API_KEY"),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&response, method::RESOLVE),
        ErrorKind::GrantRefused
    );
    assert_no_value(&response);
}

/// Why: #9070 slice 3, fail-closed — an ancestry `resolve` cannot read never
/// counts as "no agent". The caller's parent is re-pointed after the grant
/// to a pid the table does not hold, so the descendant check still passes
/// (the caller is the granted process) and only the agent check can fail.
/// Red when `resolve` treats a read error as `false`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_with_unreadable_caller_ancestry_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let procs = procs_with_me();
    let (state, _grants) = state_with(&fx, Arc::clone(&procs));
    let server = fx.start_state(state).await;
    let token = granted(&fx, me(), &["API_KEY"]).await;
    procs.add(me(), 99, 4242);
    let response = call(
        &fx.settings.socket,
        method::RESOLVE,
        resolve_params(&token, "API_KEY"),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&response, method::RESOLVE),
        ErrorKind::GrantRefused
    );
    assert_no_value(&response);
    let resolves: Vec<_> = records(&fx)
        .into_iter()
        .filter(|r| r.method == AuditMethod::Resolve)
        .collect();
    assert_eq!(resolves.len(), 1, "{resolves:?}");
    assert_eq!(
        shape(&resolves[0]),
        (
            AuditMethod::Resolve,
            AuditDecision::Deny,
            Some("grant_refused"),
            Some("API_KEY")
        )
    );
}
