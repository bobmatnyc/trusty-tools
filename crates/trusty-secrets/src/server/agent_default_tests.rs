//! Agents use keys by default; operator-only methods stay refused (#10386).
//! #10459: agents may also write (`set`, local `copy`) a key with no deny.
//!
//! A child of `wire_tests`, so it shares that module's fixture and helpers:
//! every path is under a `TempDir`, the backends are in memory, and the
//! socket peer of every call is this test process, placed in a fake process
//! table whose shared parent 10 is marked Claude Code where a test says so.
//!
//! Test: itself.

use super::*;
use crate::server::methods::{self, Caller};
use crate::store::Capabilities;

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
        ErrorKind::AgentKeyDenied
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

/// Why: #10459 — consumers test the agent arm through
/// `serve_with_agent_callers`; it must judge this process an agent, so a
/// denied key is refused. Red when its table answers "not an agent".
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_with_agent_callers_refuses_a_denied_key() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(crate::server::serve_with_agent_callers(
        fx.settings.clone(),
        fx.backends(),
        async move {
            let _ = rx.await;
        },
    ));
    wait_serving(&fx.settings.socket).await;
    let refused = call(&fx.settings.socket, method::SET, set_params(&fx, "API_KEY")).await;
    let _ = tx.send(());
    task.await.unwrap().unwrap();
    assert_eq!(
        fixed_error(&refused, method::SET),
        ErrorKind::AgentKeyDenied
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

// #10459: agents may write a key with no deny (Bob 2026-10-29, reverses
// #10386 Q2 for `set` and a local `copy`).

/// A backend over [`MemoryBackend`] that, on each per-key deny lookup,
/// either fails or records whether the index lock is held at that moment.
#[derive(Debug)]
struct DenyProbe {
    inner: MemoryBackend,
    index_root: PathBuf,
    fail_deny: bool,
    held: std::sync::Mutex<Vec<bool>>,
}

impl DenyProbe {
    fn new(index_root: &Path, fail_deny: bool) -> Self {
        Self {
            inner: MemoryBackend::new(),
            index_root: index_root.to_path_buf(),
            fail_deny,
            held: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Whether another thread finds the vault's index lock taken.
    fn lock_held(&self, vault: &VaultName) -> bool {
        let probe = NamesIndex::at(&self.index_root).with_lock_timeout(Duration::from_millis(100));
        std::thread::scope(|s| {
            let taken = s.spawn(|| probe.remove(vault, &key("PROBE_KEY")));
            matches!(taken.join(), Ok(Err(SecretsError::LockTimeout { .. })))
        })
    }
}

impl SecretBackend for DenyProbe {
    fn id(&self) -> BackendId {
        self.inner.id()
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        self.inner.get(vault, key)
    }
    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        self.inner.set(vault, key, value)
    }
    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        self.inner.delete(vault, key)
    }
    fn holds_agent_controls(&self) -> bool {
        true
    }
    fn control_item(
        &self,
        item: ControlItem,
        vault: &VaultName,
        key: Option<&SecretKey>,
    ) -> Result<bool, SecretsError> {
        if item == ControlItem::AgentDeny {
            if self.fail_deny {
                return Err(SecretsError::Backend {
                    backend: "probe".into(),
                    vault: vault.to_string(),
                    key: String::new(),
                    reason: "deny lookup failed".into(),
                });
            }
            let held = self.lock_held(vault);
            self.held.lock().unwrap().push(held);
        }
        self.inner.control_item(item, vault, key)
    }
    fn set_control_item(
        &self,
        item: ControlItem,
        vault: &VaultName,
        key: Option<&SecretKey>,
        present: bool,
    ) -> Result<(), SecretsError> {
        self.inner.set_control_item(item, vault, key, present)
    }
}

fn set_value(fx: &Fixture, name: &str, value: &str) -> Value {
    json!({"project": fx.project(), "vault": PROJECT_VAULT, "key": name, "value": value})
}

/// Why: #10459 regression (Bob 2026-10-29, "the guard is too restrictive") —
/// a caller under Claude Code stores a key with no deny, and the one allow
/// record names the agent verdict, the default policy, the pid and the key,
/// with no value character. Red on 5ca52c46ad: `agent_use_refused`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_by_agent_ancestor_of_an_undenied_key_succeeds() {
    let fx = fixture();
    let (state, _grants) = state_with(&fx, agent_procs());
    let server = fx.start_state(state).await;
    let answer = call(&fx.settings.socket, method::SET, set_params(&fx, "NEW_KEY")).await;
    server.stop().await;

    let answer = ok(answer);
    assert_eq!(answer["outcome"], "new");
    let stored = fx
        .keychain
        .get(&vault(PROJECT_VAULT), &key("NEW_KEY"))
        .unwrap()
        .map(|v| v.expose() == OPEN_VALUE);
    assert_eq!(stored, Some(true), "the value was stored");
    let index = NamesIndex::at(&fx.settings.index_root);
    assert!(
        index
            .get(&vault(PROJECT_VAULT), &key("NEW_KEY"))
            .unwrap()
            .is_some()
    );
    let all = records(&fx);
    assert_eq!(all.len(), 1, "one record");
    assert_eq!(
        shape(&all[0]),
        (
            AuditMethod::Set,
            AuditDecision::Allow,
            None,
            Some("NEW_KEY")
        )
    );
    assert_eq!(all[0].agent_parent, Some(true));
    assert_eq!(all[0].default_policy, Some(true));
    assert_eq!(all[0].caller_pid, Some(me()));
    let raw = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(!raw.contains(OPEN_VALUE), "the record holds no value");
}

/// Why: #10459 — an agent's confirmation lands in its transcript, so it is
/// `[N chars]` only; a person's keeps the `mask_secret` form. Red on
/// 5ca52c46ad (the agent's `set` was refused) and when the agent arm uses
/// `mask_secret`.
/// Test: itself.
#[test]
fn set_by_agent_confirmation_shows_length_only() {
    let fx = fixture();
    let (agent, _a) = state_with(&fx, agent_procs());
    let (person, _p) = state_with(&fx, procs_with_me());
    let caller = Caller::new(Some(me()));
    for (n, value) in [
        (5, "q7Lm2"),
        (12, "q7Lm2Rx9Tz4W"),
        (40, "q7Lm2Rx9Tz4Wp3Ks8Vb1Nc6Hd0Fj5Gy2Ue7Ia4Ox"),
    ] {
        let answer = methods::set(&agent, caller, set_value(&fx, "AGENT_KEY", value)).unwrap();
        assert_eq!(answer["masked"], format!("[{n} chars]"), "agent, {n} chars");
        let answer = methods::set(&person, caller, set_value(&fx, "PERSON_KEY", value)).unwrap();
        let masked = answer["masked"].as_str().map(str::to_owned);
        assert_eq!(masked, Some(mask_secret(value)), "person, {n} chars");
    }
}

/// Why: #10459 — the length-only mask shows no character of the value.
/// Test: itself.
#[test]
fn mask_length_only_shows_no_value_character() {
    for (value, masked) in [
        ("", "[0 chars]"),
        ("Q7XZ", "[4 chars]"),
        ("Q7XZ-WVKP-0123456789-JMNB", "[25 chars]"),
        ("日本語のひみつ", "[7 chars]"),
    ] {
        assert_eq!(crate::api::mask_length_only(value), masked);
    }
}

/// Why: #10459 Fail-Open Check — a key with an operator deny stays shut to
/// an agent's write: `agent_key_denied`, the stored value unchanged, one
/// deny record. Red when the deny check is absent from the agent write.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_by_agent_of_a_denied_key_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, agent_procs());
    let server = fx.start_state(state).await;
    let refused = call(&fx.settings.socket, method::SET, set_params(&fx, "API_KEY")).await;
    server.stop().await;

    assert_eq!(
        fixed_error(&refused, method::SET),
        ErrorKind::AgentKeyDenied
    );
    let kept = fx
        .keychain
        .get(&vault(PROJECT_VAULT), &key("API_KEY"))
        .unwrap()
        .map(|v| v.expose() == SENTINEL_VALUE);
    assert_eq!(kept, Some(true), "the stored value is unchanged");
    let all = records(&fx);
    let sets: Vec<_> = all
        .iter()
        .filter(|r| r.method == AuditMethod::Set)
        .collect();
    assert_eq!(sets.len(), 1);
    assert_eq!(
        shape(sets[0]),
        (
            AuditMethod::Set,
            AuditDecision::Deny,
            Some("agent_key_denied"),
            Some("API_KEY")
        )
    );
    assert_eq!(sets[0].default_policy, None);
}

/// Why: #10459 — the deny lookup runs under the key's index lock, the lock
/// every deny change takes, so no deny can land between the check and the
/// write. Red when the check moves out of the locked section.
/// Test: itself.
#[test]
fn set_by_agent_rechecks_the_deny_under_the_index_lock() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("index");
    let probe = Arc::new(DenyProbe::new(&root, false));
    let store = SecretStore::new(
        Arc::clone(&probe) as Arc<dyn SecretBackend>,
        NamesIndex::at(&root),
    );
    let response = store.set_for_agent(
        &vault(PROJECT_VAULT),
        &key("NEW_KEY"),
        &SecretValue::new(OPEN_VALUE),
    );
    assert!(response.is_ok(), "{:?}", response.err());
    assert_eq!(*probe.held.lock().unwrap(), [true]);
}

/// Why: #10459 Fail-Open Check — a deny lookup that fails is never "no
/// deny": the agent write fails, nothing reaches the backend, no index row
/// is written. Red when the lookup error is read as "not denied".
/// Test: itself.
#[test]
fn set_by_agent_fails_closed_when_the_deny_lookup_fails() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("index");
    let probe = Arc::new(DenyProbe::new(&root, true));
    let index = NamesIndex::at(&root);
    let store = SecretStore::new(Arc::clone(&probe) as Arc<dyn SecretBackend>, index.clone());
    let (web, name) = (vault(PROJECT_VAULT), key("NEW_KEY"));
    let response = store.set_for_agent(&web, &name, &SecretValue::new(OPEN_VALUE));
    assert!(
        matches!(response, Err(SecretsError::Backend { .. })),
        "{:?}",
        response.map(|r| r.outcome)
    );
    assert!(probe.inner.is_empty(), "nothing was written");
    assert!(index.get(&web, &name).unwrap().is_none(), "no index row");
}

/// Why: #10459 Fail-Open Check — an agent's allowed write still needs its
/// audit record first: with the sink unwritable, `audit_unavailable` and no
/// value stored. Red when the agent arm writes before `gate.admit`.
/// Test: itself.
#[cfg(unix)]
#[test]
fn set_by_agent_with_unwritable_audit_changes_nothing() {
    let fx = fixture();
    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    set_mode(&dir, 0o755);
    let _restore = RestoreMode(dir);
    let (state, _grants) = state_with(&fx, agent_procs());
    let set = methods::set(&state, Caller::new(Some(me())), set_params(&fx, "NEW_KEY"));
    assert_eq!(set, Err(ErrorKind::AuditUnavailable));
    assert!(fx.keychain.is_empty(), "nothing was written");
}

/// Why: #10459 — a local copy by an agent moves keys with no deny, each
/// allow record marked `default_policy`. Red on 5ca52c46ad, where every
/// agent copy was `agent_use_refused`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_by_agent_of_undenied_keys_copies_them() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, true);
    let (state, _grants) = state_with(&fx, agent_procs());
    let server = fx.start_state(state).await;
    let answer = call(&fx.settings.socket, method::COPY, copy_params(&fx, "spare")).await;
    server.stop().await;

    assert_eq!(ok(answer), json!({"copied": ["API_KEY"], "failed": []}));
    assert!(
        fx.spare
            .get(&vault(PROJECT_VAULT), &key("API_KEY"))
            .unwrap()
            .is_some()
    );
    let copies: Vec<_> = records(&fx)
        .into_iter()
        .filter(|r| r.method == AuditMethod::Copy)
        .map(|r| (r.decision, r.agent_parent, r.default_policy))
        .collect();
    assert_eq!(copies, [(AuditDecision::Allow, Some(true), Some(true))]);
}

/// Why: #10459 Fail-Open Check — an agent's copy of a key denied in the
/// source is a denied read (no value read, no deny carried), and a copy
/// over a key denied in the destination is a denied write; both fail and
/// nothing lands. Red when either check is absent from the agent arm.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_by_agent_of_a_denied_source_or_destination_key_writes_nothing() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let web = vault(PROJECT_VAULT);
    fx.spare
        .set_control_item(ControlItem::AgentDeny, &web, Some(&key("OPEN_KEY")), true)
        .unwrap();
    let (state, _grants) = state_with(&fx, agent_procs());
    let server = fx.start_state(state).await;
    let params = json!({"project": fx.project(), "from_backend": "keychain",
        "to_backend": "spare", "keys": ["API_KEY", "OPEN_KEY"]});
    let answer = call(&fx.settings.socket, method::COPY, params).await;
    server.stop().await;

    assert_eq!(
        ok(answer),
        json!({"copied": [], "failed": ["API_KEY", "OPEN_KEY"]})
    );
    for name in ["API_KEY", "OPEN_KEY"] {
        assert!(fx.spare.get(&web, &key(name)).unwrap().is_none(), "{name}");
    }
    assert!(
        !fx.spare
            .control_item(ControlItem::AgentDeny, &web, Some(&key("API_KEY")))
            .unwrap(),
        "no deny was carried for a refused key"
    );
    assert_eq!(deny_changes(&fx), []);
    let reasons: Vec<_> = records(&fx)
        .into_iter()
        .filter(|r| r.method == AuditMethod::Copy)
        .map(|r| r.reason.map(|reason| reason.as_str().to_owned()))
        .collect();
    let denied = Some("agent_key_denied".to_string());
    assert_eq!(reasons, [denied.clone(), denied]);
}
