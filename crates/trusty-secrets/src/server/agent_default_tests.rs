//! Agents use keys by default; operator-only methods stay refused (#10386).
//!
//! A child of `wire_tests`, so it shares that module's fixture and helpers:
//! every path is under a `TempDir`, the backends are in memory, and the
//! socket peer of every call is this test process, placed in a fake process
//! table whose shared parent 10 is marked Claude Code where a test says so.
//!
//! Test: itself.

use super::*;
use crate::server::methods::{self, Caller};

/// `procs_with_me` with the shared parent 10 marked as Claude Code.
fn agent_procs() -> Arc<FakeProcs> {
    let procs = procs_with_me();
    procs.set_agent(10);
    procs
}

/// Store `name` = `value` in the project vault with nothing else written.
fn plain_set(fx: &Fixture, name: &str, value: &str) {
    SecretStore::new(
        Arc::clone(&fx.keychain) as Arc<dyn SecretBackend>,
        NamesIndex::at(&fx.settings.index_root),
    )
    .set(&vault(PROJECT_VAULT), &key(name), &SecretValue::new(value))
    .unwrap();
}

fn set_params(fx: &Fixture, name: &str) -> Value {
    json!({"project": fx.project(), "vault": PROJECT_VAULT, "key": name, "value": OPEN_VALUE})
}

fn delete_params(fx: &Fixture, name: &str) -> Value {
    json!({"project": fx.project(), "vault": PROJECT_VAULT, "key": name})
}

/// Why: #10386 regression (Bob D69, D6a) — a registrar under Claude Code
/// grants an in-scope key nobody flagged or denied, and its process resolves
/// it. Red on origin/main 362e0fd6db: the grant was `agent_use_refused`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grant_and_resolve_by_agent_ancestor_of_an_unflagged_key_succeed() {
    let fx = fixture();
    plain_set(&fx, "PLAIN_KEY", OPEN_VALUE);
    let (state, _grants) = state_with(&fx, agent_procs());
    let server = fx.start_state(state).await;
    let token = granted(&fx, me(), &["PLAIN_KEY"]).await;
    let value = ok(call(
        &fx.settings.socket,
        method::RESOLVE,
        resolve_params(&token, "PLAIN_KEY"),
    )
    .await);
    server.stop().await;
    assert_eq!(value["value"], OPEN_VALUE);
}

/// Why: #10386 audit ruling — an agent's grant and resolve allowed by the
/// absence of a deny say so (`default_policy: true`); a person's records and
/// every deny record do not. Red when the field is dropped or set on a deny.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_allow_records_carry_default_policy() {
    let fx = fixture();
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let procs = agent_procs();
    let (state, _grants) = state_with(&fx, Arc::clone(&procs));
    let server = fx.start_state(state).await;
    let token = granted(&fx, me(), &["OPEN_KEY"]).await;
    ok(call(
        &fx.settings.socket,
        method::RESOLVE,
        resolve_params(&token, "OPEN_KEY"),
    )
    .await);
    let refused = call(
        &fx.settings.socket,
        method::GRANT,
        grant_params(&fx, me(), &["API_KEY"]),
    )
    .await;
    server.stop().await;
    assert_eq!(
        fixed_error(&refused, method::GRANT),
        ErrorKind::AgentUseRefused
    );

    let all = records(&fx);
    let policy: Vec<_> = all
        .iter()
        .map(|r| (r.method, r.decision, r.default_policy))
        .collect();
    assert_eq!(
        policy,
        [
            (AuditMethod::Grant, AuditDecision::Allow, Some(true)),
            (AuditMethod::Resolve, AuditDecision::Allow, Some(true)),
            (AuditMethod::Grant, AuditDecision::Deny, None),
        ]
    );
}

/// Why: #10386 Q2 — `set` carries a value an agent would hold in its
/// transcript, so a caller under Claude Code is refused before any backend
/// is touched, with one deny record naming the verdict. Red when `set` has
/// no ancestry check or runs it after the write.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_by_agent_ancestor_is_refused_before_the_backend() {
    let fx = fixture();
    let (state, _grants) = state_with(&fx, agent_procs());
    let server = fx.start_state(state).await;
    let refused = call(&fx.settings.socket, method::SET, set_params(&fx, "NEW_KEY")).await;
    server.stop().await;

    assert_eq!(
        fixed_error(&refused, method::SET),
        ErrorKind::AgentUseRefused
    );
    assert!(fx.keychain.is_empty(), "nothing was written");
    let all = records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(
        shape(&all[0]),
        (
            AuditMethod::Set,
            AuditDecision::Deny,
            Some("agent_use_refused"),
            Some("NEW_KEY")
        )
    );
    assert_eq!(all[0].agent_parent, Some(true));
}

/// Why: #10386 Q2 — `delete` is an irreversible loss, so a caller under
/// Claude Code is refused before any backend is touched; the value and its
/// row stay. Red when `delete` has no ancestry check.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_by_agent_ancestor_is_refused_before_the_backend() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, true);
    let (state, _grants) = state_with(&fx, agent_procs());
    let server = fx.start_state(state).await;
    let refused = call(
        &fx.settings.socket,
        method::DELETE,
        delete_params(&fx, "API_KEY"),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&refused, method::DELETE),
        ErrorKind::AgentUseRefused
    );
    assert!(!fx.keychain.is_empty(), "the value stays");
    let index = NamesIndex::at(&fx.settings.index_root);
    assert!(
        index
            .get(&vault(PROJECT_VAULT), &key("API_KEY"))
            .unwrap()
            .is_some()
    );
    let all = records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].decision, AuditDecision::Deny);
}

/// Why: Fail-Open Check — an ancestry the server cannot read, or a call
/// with no peer pid, is never "no agent": `set` and `delete` refuse with
/// `grant_refused` and change nothing. Red when a read error is `false`.
/// Test: itself.
#[test]
fn set_and_delete_with_unreadable_ancestry_or_no_peer_are_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, true);
    let procs = procs_with_me();
    procs.set_unreadable();
    let (state, _grants) = state_with(&fx, procs);
    for caller in [Caller::new(Some(me())), Caller::new(None)] {
        let set = methods::set(&state, caller, set_params(&fx, "NEW_KEY"));
        assert_eq!(set, Err(ErrorKind::GrantRefused));
        let delete = methods::delete(&state, caller, delete_params(&fx, "API_KEY"));
        assert_eq!(delete, Err(ErrorKind::GrantRefused));
    }
    let index = NamesIndex::at(&fx.settings.index_root);
    assert!(
        index
            .get(&vault(PROJECT_VAULT), &key("NEW_KEY"))
            .unwrap()
            .is_none()
    );
    assert!(
        index
            .get(&vault(PROJECT_VAULT), &key("API_KEY"))
            .unwrap()
            .is_some()
    );
}

/// Why: #10386 — consumer suites store keys through a real server; under an
/// agent session the host table would refuse their `set`. The test-support
/// server judges every caller a person, so a `set` from this process (which
/// may itself run under Claude Code) is stored. Red when the replacement
/// table is dropped and the host runs under Claude Code.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_without_agent_callers_stores_a_key_for_any_caller() {
    let fx = fixture();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(crate::server::serve_without_agent_callers(
        fx.settings.clone(),
        fx.backends(),
        async move {
            let _ = rx.await;
        },
    ));
    wait_serving(&fx.settings.socket).await;
    ok(call(&fx.settings.socket, method::SET, set_params(&fx, "NEW_KEY")).await);
    let _ = tx.send(());
    task.await.unwrap().unwrap();
    assert!(
        fx.keychain
            .get(&vault(PROJECT_VAULT), &key("NEW_KEY"))
            .unwrap()
            .is_some()
    );
}

fn copy_params(fx: &Fixture, to: &str) -> Value {
    json!({"project": fx.project(), "from_backend": "keychain", "to_backend": to, "keys": ["API_KEY"]})
}

/// The deny-change records: (key, `agents_allowed`).
fn deny_changes(fx: &Fixture) -> Vec<(Option<String>, Option<bool>)> {
    records(fx)
        .into_iter()
        .filter(|r| r.method == AuditMethod::SetAgentsMayUse)
        .map(|r| (r.key.map(|k| k.to_string()), r.agents_allowed))
        .collect()
}

/// Why: code-critic finding 1 (CRITICAL) on 90558c5cbc — `secrets.copy` never
/// judged its caller, so an agent could move a denied key into a backend
/// that cannot hold the deny. A copy is operator-only, like `set`. Red when
/// `copy` has no ancestry check.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_by_agent_ancestor_is_refused_and_writes_nothing() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, true);
    let (state, _grants) = state_with(&fx, agent_procs());
    let server = fx.start_state(state).await;
    let refused = call(&fx.settings.socket, method::COPY, copy_params(&fx, "spare")).await;
    server.stop().await;

    assert_eq!(
        fixed_error(&refused, method::COPY),
        ErrorKind::AgentUseRefused
    );
    assert!(fx.spare.is_empty(), "nothing was copied");
    let all = records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].decision, AuditDecision::Deny);
    assert_eq!(all[0].agent_parent, Some(true));
}

/// Why: code-critic finding 1 — a denied key copied into a backend that
/// cannot hold a deny (1Password, Keeper) read as open under `OpenAll`.
/// Any caller's copy of a denied key there is refused, and nothing lands.
/// Red when the copy writes the value without carrying the deny.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_of_a_denied_key_into_a_no_controls_backend_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let nocontrols = Arc::new(MemoryBackend::without_agent_controls());
    let base = fx.backends();
    let target = Arc::clone(&nocontrols);
    let factory: BackendFactory = Arc::new(move |id: &BackendId| match id.as_str() {
        "nocontrols" => Ok(Arc::clone(&target) as Arc<dyn SecretBackend>),
        _ => base(id),
    });
    let mut state = fx.state(factory);
    state.grants = Arc::new(GrantRegistry::new(
        procs_with_me(),
        Arc::new(FakeClock::at(T0)),
        DEFAULT_MAX_TTL,
    ));
    let server = fx.start_state(state).await;
    let answer = ok(call(
        &fx.settings.socket,
        method::COPY,
        copy_params(&fx, "nocontrols"),
    )
    .await);
    server.stop().await;

    assert_eq!(answer, json!({"copied": [], "failed": ["API_KEY"]}));
    assert!(nocontrols.is_empty(), "nothing was written");
    let failed: Vec<_> = records(&fx)
        .into_iter()
        .filter(|r| r.method == AuditMethod::Copy)
        .map(|r| r.reason.map(|reason| reason.as_str().to_owned()))
        .collect();
    assert_eq!(failed, [Some("unsupported".to_string())]);
}

/// Why: code-critic findings 1 and 3 — a copy into a backend that can hold a
/// deny carries the source's deny, written (with its write-ahead record)
/// before the value. Red when the destination key reads as open.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_of_a_denied_key_carries_the_deny() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    let answer = ok(call(&fx.settings.socket, method::COPY, copy_params(&fx, "spare")).await);
    server.stop().await;

    assert_eq!(answer, json!({"copied": ["API_KEY"], "failed": []}));
    assert!(
        fx.spare
            .control_item(
                ControlItem::AgentDeny,
                &vault(PROJECT_VAULT),
                Some(&key("API_KEY"))
            )
            .unwrap(),
        "the deny travelled with the key"
    );
    assert_eq!(
        deny_changes(&fx),
        [(Some("API_KEY".to_string()), Some(false))]
    );
}

/// Why: Architect acceptance criterion, code-critic finding 3 — a `delete`
/// that removes a deny leaves a write-ahead record of it, ahead of the
/// delete's own record. Red when the removal is unrecorded.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_delete_records_the_deny_it_removes() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    ok(call(
        &fx.settings.socket,
        method::DELETE,
        delete_params(&fx, "API_KEY"),
    )
    .await);
    server.stop().await;

    let methods: Vec<_> = records(&fx).iter().map(|r| r.method).collect();
    assert_eq!(methods, [AuditMethod::SetAgentsMayUse, AuditMethod::Delete]);
    assert_eq!(
        deny_changes(&fx),
        [(Some("API_KEY".to_string()), Some(true))]
    );
    assert!(
        !fx.keychain
            .control_item(
                ControlItem::AgentDeny,
                &vault(PROJECT_VAULT),
                Some(&key("API_KEY"))
            )
            .unwrap()
    );
}
