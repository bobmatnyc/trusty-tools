//! `secrets.set_agents_may_use` over real sockets (#9070, S8 slice 3).
//!
//! A child of `wire_tests`, so it shares that module's fixture and helpers:
//! every path is under a `TempDir`, the backends are in memory, and the
//! socket peer of every call is this test process, placed in a fake process
//! table.
//!
//! Test: itself.

use std::path::PathBuf;

use super::*;
use crate::server::agents_flag;
use crate::server::methods::Caller;

/// `set_agents_may_use` params for `name` in the project vault.
fn flag_params(fx: &Fixture, name: &str, allowed: bool) -> Value {
    json!({"project": fx.project(), "vault": PROJECT_VAULT, "key": name, "allowed": allowed})
}

/// The flag as the fixture's project backend holds it.
fn flagged(fx: &Fixture, name: &str) -> bool {
    fx.keychain
        .agents_may_use(&vault(PROJECT_VAULT), &key(name))
        .unwrap()
}

/// `procs_with_me` with the shared parent 10 marked as Claude Code.
fn agent_procs() -> Arc<FakeProcs> {
    let procs = procs_with_me();
    procs.set_agent(10);
    procs
}

/// The flag-set records, in order.
fn flag_records(fx: &Fixture) -> Vec<AuditRecord> {
    records(fx)
        .into_iter()
        .filter(|r| r.method == AuditMethod::SetAgentsMayUse)
        .collect()
}

/// The project backend, wrapped so a flag write first counts the flag-set
/// records already in the audit log.
#[derive(Debug)]
struct ProbeBackend {
    inner: Arc<MemoryBackend>,
    audit_log: PathBuf,
    seen: Mutex<Option<usize>>,
}

impl SecretBackend for ProbeBackend {
    fn id(&self) -> BackendId {
        self.inner.id()
    }
    fn capabilities(&self) -> crate::store::Capabilities {
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
    fn agents_may_use(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        self.inner.agents_may_use(vault, key)
    }
    fn set_agents_may_use(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        allowed: bool,
    ) -> Result<(), SecretsError> {
        let text = std::fs::read_to_string(&self.audit_log).unwrap_or_default();
        let count = text
            .lines()
            .filter(|line| line.contains(method::SET_AGENTS_MAY_USE))
            .count();
        *self.seen.lock().unwrap() = Some(count);
        self.inner.set_agents_may_use(vault, key, allowed)
    }
}

/// Why: DOC-74 §15.8 — a caller under Claude Code cannot flag a key for
/// itself. The refusal comes before the backend call, so the flag stays OFF,
/// and it leaves one deny record naming the key, the peer and the verdict.
/// Red when the ancestry check is missing or runs after the backend call.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_flag_on_by_agent_ancestor_is_refused_and_audited() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, agent_procs());
    let server = fx.start_state(state).await;
    let refused = call(
        &fx.settings.socket,
        method::SET_AGENTS_MAY_USE,
        flag_params(&fx, "API_KEY", true),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&refused, method::SET_AGENTS_MAY_USE),
        ErrorKind::AgentUseRefused
    );
    assert!(refused.result.is_none());
    assert!(!flagged(&fx, "API_KEY"), "the flag stayed OFF");
    let all = records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(
        shape(&all[0]),
        (
            AuditMethod::SetAgentsMayUse,
            AuditDecision::Deny,
            Some("agent_use_refused"),
            Some("API_KEY")
        )
    );
    assert_eq!(all[0].caller_pid, Some(me()));
    assert_eq!(
        (all[0].agents_allowed, all[0].agent_parent),
        (Some(true), Some(true))
    );
}

/// Why: turning the flag OFF narrows access, so an agent caller may do it;
/// one allow record. Red on a blanket refusal of agent callers.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_flag_off_by_agent_ancestor_is_allowed() {
    let fx = fixture();
    seed(&fx, "OPEN_KEY", OPEN_VALUE, true);
    let (state, _grants) = state_with(&fx, agent_procs());
    let server = fx.start_state(state).await;
    let answer = ok(call(
        &fx.settings.socket,
        method::SET_AGENTS_MAY_USE,
        flag_params(&fx, "OPEN_KEY", false),
    )
    .await);
    server.stop().await;

    assert_eq!(answer["allowed"], false);
    assert!(!flagged(&fx, "OPEN_KEY"), "the flag went OFF");
    let flags = flag_records(&fx);
    let shapes: Vec<_> = flags.iter().map(shape).collect();
    assert_eq!(
        shapes,
        [(
            AuditMethod::SetAgentsMayUse,
            AuditDecision::Allow,
            None,
            Some("OPEN_KEY")
        )]
    );
}

/// Why: Architect ruling 2026-10-10 — the allow record is in the log before
/// the backend is touched, so no flag change can exist without its record.
/// A backend wrapper counts the records at the moment of its call.
/// Red when the record is appended after the backend call (`Recording::Once`).
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_flag_allow_record_is_written_before_the_backend_call() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let probe = Arc::new(ProbeBackend {
        inner: Arc::clone(&fx.keychain),
        audit_log: fx.settings.audit_log.clone(),
        seen: Mutex::new(None),
    });
    let shared = Arc::clone(&probe);
    let factory: BackendFactory =
        Arc::new(move |_: &BackendId| Ok(Arc::clone(&shared) as Arc<dyn SecretBackend>));
    let mut state = fx.state(factory);
    state.grants = Arc::new(GrantRegistry::new(
        procs_with_me(),
        Arc::new(FakeClock::at(T0)),
        DEFAULT_MAX_TTL,
    ));
    let server = fx.start_state(state).await;
    let answer = ok(call(
        &fx.settings.socket,
        method::SET_AGENTS_MAY_USE,
        flag_params(&fx, "API_KEY", true),
    )
    .await);
    server.stop().await;

    assert_eq!(answer["allowed"], true);
    assert!(flagged(&fx, "API_KEY"), "the flag went ON");
    assert_eq!(
        *probe.seen.lock().unwrap(),
        Some(1),
        "record before the call"
    );
    let all = flag_records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].decision, AuditDecision::Allow);
    assert_eq!(
        (all[0].agents_allowed, all[0].agent_parent),
        (Some(true), Some(false))
    );
}

/// Why: fail-closed — when the audit log cannot be opened, the flag is not
/// changed. Red when the backend call runs before the log is opened.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_flag_with_unwritable_audit_changes_nothing() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, procs_with_me());
    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    set_mode(&dir, 0o755);
    let _restore = RestoreMode(dir);
    let server = fx.start_state(state).await;
    let response = call(
        &fx.settings.socket,
        method::SET_AGENTS_MAY_USE,
        flag_params(&fx, "API_KEY", true),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&response, method::SET_AGENTS_MAY_USE),
        ErrorKind::AuditUnavailable
    );
    assert!(!flagged(&fx, "API_KEY"), "the flag stayed OFF");
}

/// Why: Architect Q1 — a backend call that fails after the allow record
/// leaves a second record, a deny with the failure's kind, so the trail
/// shows the intent and its failure. An unindexed key fails in the store.
/// Red when the failure leaves only the allow record.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_flag_backend_failure_leaves_a_deny_after_the_allow_record() {
    let fx = fixture();
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    let missing = call(
        &fx.settings.socket,
        method::SET_AGENTS_MAY_USE,
        flag_params(&fx, "MISSING", true),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&missing, method::SET_AGENTS_MAY_USE),
        ErrorKind::NotFound
    );
    use AuditDecision::{Allow, Deny};
    let flags = flag_records(&fx);
    let shapes: Vec<_> = flags.iter().map(shape).collect();
    assert_eq!(
        shapes,
        [
            (AuditMethod::SetAgentsMayUse, Allow, None, Some("MISSING")),
            (
                AuditMethod::SetAgentsMayUse,
                Deny,
                Some("not_found"),
                Some("MISSING")
            ),
        ]
    );
}

/// Why: fail-closed — an ancestry that cannot be read never counts as "no
/// agent". ON is refused with no change; OFF needs no ancestry and passes.
/// Red when a read error is treated as `false`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_flag_unreadable_ancestry_refuses_on() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let procs = procs_with_me();
    procs.set_unreadable();
    let (state, _grants) = state_with(&fx, procs);
    let server = fx.start_state(state).await;
    let socket = &fx.settings.socket;
    let on = call(
        socket,
        method::SET_AGENTS_MAY_USE,
        flag_params(&fx, "API_KEY", true),
    )
    .await;
    let off = call(
        socket,
        method::SET_AGENTS_MAY_USE,
        flag_params(&fx, "API_KEY", false),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&on, method::SET_AGENTS_MAY_USE),
        ErrorKind::GrantRefused
    );
    assert!(!flagged(&fx, "API_KEY"), "the flag stayed OFF");
    assert_eq!(ok(off)["allowed"], false);
}

/// Why: the caller is judged from the kernel's peer pid; with none there is
/// nothing to judge, so even OFF is refused, with one deny record.
/// Red when a missing peer pid skips the check.
/// Test: itself.
#[test]
fn set_flag_with_no_peer_pid_is_refused() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, procs_with_me());
    for allowed in [true, false] {
        let refused = agents_flag::set_agents_may_use(
            &state,
            Caller::new(None),
            flag_params(&fx, "API_KEY", allowed),
        );
        assert_eq!(refused, Err(ErrorKind::GrantRefused));
    }
    assert!(!flagged(&fx, "API_KEY"));
    let all = flag_records(&fx);
    assert_eq!(all.len(), 2, "{all:?}");
    assert!(all.iter().all(|r| r.decision == AuditDecision::Deny));
    assert!(all.iter().all(|r| r.caller_pid.is_none()));
}

/// Why: a vault outside the project's scope is refused before any backend
/// is opened, as `secrets.set` refuses it.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_flag_out_of_scope_vault_is_refused() {
    let fx = fixture();
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    let params = json!({"project": fx.project(), "vault": "trusty/other/repo",
        "key": "API_KEY", "allowed": true});
    let refused = call(&fx.settings.socket, method::SET_AGENTS_MAY_USE, params).await;
    server.stop().await;

    assert_eq!(
        fixed_error(&refused, method::SET_AGENTS_MAY_USE),
        ErrorKind::VaultOutOfScope
    );
    let all = flag_records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].decision, AuditDecision::Deny);
}

/// Why: end to end with slice 2 — a key a non-agent flagged on the socket
/// can then be granted by a registrar under Claude Code.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_flag_on_then_grant_by_agent_ancestor_succeeds() {
    let fx = fixture();
    seed(&fx, "API_KEY", OPEN_VALUE, false);
    let procs = procs_with_me();
    let (state, _grants) = state_with(&fx, Arc::clone(&procs));
    let server = fx.start_state(state).await;
    ok(call(
        &fx.settings.socket,
        method::SET_AGENTS_MAY_USE,
        flag_params(&fx, "API_KEY", true),
    )
    .await);
    procs.set_agent(10);
    let token = granted(&fx, me(), &["API_KEY"]).await;
    let value = ok(call(
        &fx.settings.socket,
        method::RESOLVE,
        resolve_params(&token, "API_KEY"),
    )
    .await);
    server.stop().await;
    assert_eq!(value["value"], OPEN_VALUE);
}
