//! The per-session cache of CLI-backed values (#7520, S1).
//!
//! A child of `wire_tests`, so it shares that module's fixture and helpers:
//! every path is under a `TempDir`, the backends are in memory, and the
//! caller is this test process, placed in a fake process table. Most tests
//! call the method bodies on one [`State`], which is one server session;
//! the first goes over a real socket.
//!
//! Test: itself.

use super::*;
use crate::server::exec;
use crate::server::methods::{self, Caller};

/// The project vault of the second fixture repo, `Acme/Api`.
const API_VAULT: &str = "trusty/acme/api";

/// A backend double that answers to any id and counts `get` calls.
///
/// What: values and flags live in an inner [`MemoryBackend`]; `id` is the
/// one given, so it can stand in for `onepassword`, `keeper`, `keychain` or
/// `file`. `failing` makes that many next `get` calls fail.
#[derive(Debug)]
struct Counting {
    id: BackendId,
    inner: MemoryBackend,
    gets: AtomicUsize,
    failing: AtomicUsize,
}

impl Counting {
    fn new(id: BackendId) -> Arc<Self> {
        Arc::new(Self {
            id,
            inner: MemoryBackend::new(),
            gets: AtomicUsize::new(0),
            failing: AtomicUsize::new(0),
        })
    }

    fn gets(&self) -> usize {
        self.gets.load(Ordering::SeqCst)
    }
}

impl SecretBackend for Counting {
    fn id(&self) -> BackendId {
        self.id.clone()
    }
    fn capabilities(&self) -> crate::store::Capabilities {
        self.inner.capabilities()
    }
    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        let failing = self
            .failing
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
        if failing.is_ok() {
            return Err(SecretsError::Backend {
                backend: self.id.to_string(),
                vault: vault.to_string(),
                key: key.to_string(),
                reason: "the unlock was declined".to_string(),
            });
        }
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
        self.inner.set_agents_may_use(vault, key, allowed)
    }
}

/// Select `backend` as the machine's default; the server rereads it per
/// request.
fn select(fx: &Fixture, backend: &str) {
    std::fs::write(
        &fx.settings.machine_config,
        format!("secrets:\n  default_backend: {backend}\n"),
    )
    .unwrap();
}

/// A state whose factory serves `doubles` by id, then the fixture's own
/// backends, with a grant registry over `procs`. Selects the first double.
fn cache_state(fx: &Fixture, doubles: &[Arc<Counting>], procs: Arc<FakeProcs>) -> State {
    let doubles: Vec<Arc<Counting>> = doubles.to_vec();
    select(fx, doubles[0].id.as_str());
    let base = fx.backends();
    let factory: BackendFactory =
        Arc::new(
            move |id: &BackendId| match doubles.iter().find(|double| double.id == *id) {
                Some(double) => Ok(Arc::clone(double) as Arc<dyn SecretBackend>),
                None => base(id),
            },
        );
    let mut state = fx.state(factory);
    state.grants = Arc::new(GrantRegistry::new(
        procs,
        Arc::new(FakeClock::at(T0)),
        DEFAULT_MAX_TTL,
    ));
    state
}

/// Store `name` = `value` in `vault_name` of `backend`, with its index
/// row. Goes around the server, so it evicts nothing.
fn put(fx: &Fixture, backend: &Arc<Counting>, vault_name: &str, name: &str, value: &str) {
    let store = SecretStore::new(
        Arc::clone(backend) as Arc<dyn SecretBackend>,
        NamesIndex::at(&fx.settings.index_root),
    );
    store
        .set(&vault(vault_name), &key(name), &SecretValue::new(value))
        .unwrap();
}

/// Set the agents flag of `name` in the project vault of `backend`.
fn flag(fx: &Fixture, backend: &Arc<Counting>, name: &str, allowed: bool) {
    let store = SecretStore::new(
        Arc::clone(backend) as Arc<dyn SecretBackend>,
        NamesIndex::at(&fx.settings.index_root),
    );
    store
        .set_agents_may_use(&vault(PROJECT_VAULT), &key(name), allowed)
        .unwrap();
}

fn caller() -> Caller {
    Caller::new(Some(me()))
}

/// Grant `keys` to this process in the checkout at `project`.
fn grant_in(state: &State, project: &str, keys: &[&str]) -> String {
    let params = json!({"project": project, "child_pid": me(), "keys": keys, "ttl_secs": 600});
    let answer = exec::grant(state, caller(), params).unwrap();
    answer["token"].as_str().unwrap().to_owned()
}

/// Resolve `name` under `token` from this process.
fn resolve_in(state: &State, token: &str, name: &str) -> Result<Value, ErrorKind> {
    exec::resolve(state, caller(), resolve_params(token, name))
}

/// The value `resolve_in` answered.
fn value_of(state: &State, token: &str, name: &str) -> String {
    let answer = resolve_in(state, token, name).unwrap();
    answer["value"].as_str().unwrap().to_owned()
}

/// A second checkout, `Acme/Api`, beside the fixture's repo.
fn second_repo(fx: &Fixture) -> String {
    let repo = fx.tmp.path().join("api");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &["remote", "add", "origin", "git@github.com:Acme/Api.git"],
    );
    repo.display().to_string()
}

/// Why: #7520 AC1 — one server session reads a CLI-backed key from its
/// backend once; the second `secrets.resolve` is answered from the cache.
/// Red while every resolve reads the backend (two reads).
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_second_call_does_not_reread_cli_backend() {
    let fx = fixture();
    let op = Counting::new(BackendId::onepassword());
    put(&fx, &op, PROJECT_VAULT, "API_KEY", SENTINEL_VALUE);
    let state = cache_state(&fx, std::slice::from_ref(&op), procs_with_me());
    let server = fx.start_state(state).await;
    let token = granted(&fx, me(), &["API_KEY"]).await;
    let first = ok(resolve_now(&fx, &token, "API_KEY").await);
    let second = ok(resolve_now(&fx, &token, "API_KEY").await);
    server.stop().await;

    assert_eq!(first["value"], SENTINEL_VALUE);
    assert_eq!(second["value"], SENTINEL_VALUE);
    assert_eq!(op.gets(), 1, "the second resolve read the backend again");
    // Every call is still audited: two allow records.
    let allows = records(&fx)
        .iter()
        .filter(|r| r.method == AuditMethod::Resolve && r.decision == AuditDecision::Allow)
        .count();
    assert_eq!(allows, 2);
}

/// Why: #7520 AC2 — a cached value answers only its own (backend, vault,
/// key): one project's value never answers another project's lookup of the
/// same key, nor another CLI backend's.
/// Red while nothing is cached (each resolve reads, four reads).
/// Test: itself.
#[test]
fn cache_is_namespaced_per_vault() {
    let fx = fixture();
    let api = second_repo(&fx);
    let op = Counting::new(BackendId::onepassword());
    let keeper = Counting::new(BackendId::keeper());
    put(&fx, &op, PROJECT_VAULT, "API_KEY", "web-op-value");
    put(&fx, &op, API_VAULT, "API_KEY", "api-op-value");
    put(&fx, &keeper, PROJECT_VAULT, "API_KEY", "web-keeper-value");
    let state = cache_state(
        &fx,
        &[Arc::clone(&op), Arc::clone(&keeper)],
        procs_with_me(),
    );
    let web_token = grant_in(&state, &fx.project(), &["API_KEY"]);
    let api_token = grant_in(&state, &api, &["API_KEY"]);

    for _ in 0..2 {
        assert_eq!(value_of(&state, &web_token, "API_KEY"), "web-op-value");
        assert_eq!(value_of(&state, &api_token, "API_KEY"), "api-op-value");
    }
    assert_eq!(op.gets(), 2, "one read per vault, then cache hits");
    select(&fx, BackendId::KEEPER);
    assert_eq!(value_of(&state, &web_token, "API_KEY"), "web-keeper-value");
    assert_eq!(keeper.gets(), 1);
}

/// Why: #7520 AC3 — the cache shows in `State`'s `Debug` as an entry count,
/// never a value.
/// Red while `State` holds no cache (no count in its `Debug`).
/// Test: itself.
#[test]
fn cache_debug_never_prints_value() {
    let fx = fixture();
    let op = Counting::new(BackendId::onepassword());
    put(&fx, &op, PROJECT_VAULT, "API_KEY", SENTINEL_VALUE);
    let state = cache_state(&fx, std::slice::from_ref(&op), procs_with_me());
    let token = grant_in(&state, &fx.project(), &["API_KEY"]);
    assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);

    for debug in [format!("{state:?}"), format!("{state:#?}")] {
        assert!(!debug.contains(SENTINEL_VALUE), "{debug}");
        // `{:#?}` adds a trailing comma before each closing brace.
        let compact = debug
            .split_whitespace()
            .collect::<String>()
            .replace(",}", "}");
        assert!(compact.contains("SecretCache{entries:1}"), "{debug}");
    }
}

/// Why: DOC-74 §8.4, §15.5 — only CLI-backed sources are cached; Keychain
/// and file values are read from the backend on every resolve, while a
/// `onepassword` key in the same session is read once.
/// Red while nothing is cached (the `onepassword` control reads twice).
/// Test: itself.
#[test]
fn keychain_and_file_reads_are_never_cached() {
    let fx = fixture();
    let op = Counting::new(BackendId::onepassword());
    let keychain = Counting::new(BackendId::keychain());
    let file = Counting::new(BackendId::file());
    for double in [&op, &keychain, &file] {
        put(&fx, double, PROJECT_VAULT, "API_KEY", SENTINEL_VALUE);
    }
    let doubles = [Arc::clone(&op), Arc::clone(&keychain), Arc::clone(&file)];
    let state = cache_state(&fx, &doubles, procs_with_me());
    let token = grant_in(&state, &fx.project(), &["API_KEY"]);

    for double in &doubles {
        select(&fx, double.id.as_str());
        for _ in 0..2 {
            assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
        }
    }
    assert_eq!(op.gets(), 1, "the CLI-backed control was not cached");
    assert_eq!(keychain.gets(), 2, "a Keychain read was cached");
    assert_eq!(file.gets(), 2, "a file read was cached");
}

/// Why: #7520 — a value written through `secrets.set`, `secrets.delete` or
/// `secrets.copy` replaces what the cache held; the next resolve reads the
/// backend again. Each later value is written around the server, so only
/// the method's eviction can make it visible.
/// Red while nothing is cached (the warm-up reads twice).
/// Test: itself.
#[test]
fn set_delete_copy_evict_cached_value() {
    let fx = fixture();
    let op = Counting::new(BackendId::onepassword());
    put(&fx, &op, PROJECT_VAULT, "API_KEY", "v1");
    let state = cache_state(&fx, std::slice::from_ref(&op), procs_with_me());
    let token = grant_in(&state, &fx.project(), &["API_KEY"]);
    let target = json!({"project": fx.project(), "vault": PROJECT_VAULT, "key": "API_KEY"});
    assert_eq!(value_of(&state, &token, "API_KEY"), "v1");
    assert_eq!(value_of(&state, &token, "API_KEY"), "v1");
    assert_eq!(op.gets(), 1, "the cache was not warm");

    let mut set = target.clone();
    set["value"] = json!("v2");
    methods::set(&state, caller(), set).unwrap();
    assert_eq!(value_of(&state, &token, "API_KEY"), "v2", "set");

    methods::delete(&state, caller(), target).unwrap();
    put(&fx, &op, PROJECT_VAULT, "API_KEY", "v3");
    assert_eq!(value_of(&state, &token, "API_KEY"), "v3", "delete");

    fx.keychain
        .set(
            &vault(PROJECT_VAULT),
            &key("API_KEY"),
            &SecretValue::new("v4"),
        )
        .unwrap();
    let copy = json!({
        "project": fx.project(),
        "from_backend": "keychain",
        "to_backend": BackendId::ONEPASSWORD,
        "keys": ["API_KEY"],
    });
    let copied = methods::copy(&state, caller(), copy).unwrap();
    assert_eq!(copied["copied"], json!(["API_KEY"]));
    assert_eq!(value_of(&state, &token, "API_KEY"), "v4", "copy");
}

/// Why: #7520 — a hit skips only the backend read. With the cache warm, a
/// caller under Claude Code still has the key's agents flag read and judged
/// on every call; once the flag is OFF it gets no value.
/// Red while nothing is cached (the warm-up reads twice).
/// Test: itself.
#[test]
fn warm_cache_still_refuses_unflagged_key_for_agent() {
    let fx = fixture();
    let op = Counting::new(BackendId::onepassword());
    put(&fx, &op, PROJECT_VAULT, "API_KEY", SENTINEL_VALUE);
    flag(&fx, &op, "API_KEY", true);
    let procs = procs_with_me();
    procs.set_agent(10);
    let state = cache_state(&fx, std::slice::from_ref(&op), procs);
    let token = grant_in(&state, &fx.project(), &["API_KEY"]);
    assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
    assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
    assert_eq!(op.gets(), 1, "the cache was not warm");

    flag(&fx, &op, "API_KEY", false);
    let refused = resolve_in(&state, &token, "API_KEY");
    assert_eq!(refused, Err(ErrorKind::AgentUseRefused));
}

/// Why: #7520 — a hit skips only the backend read. With the cache warm, a
/// revoked token is refused like any other.
/// Red while nothing is cached (the warm-up reads twice).
/// Test: itself.
#[test]
fn warm_cache_still_refuses_revoked_token() {
    let fx = fixture();
    let op = Counting::new(BackendId::onepassword());
    put(&fx, &op, PROJECT_VAULT, "API_KEY", SENTINEL_VALUE);
    let state = cache_state(&fx, std::slice::from_ref(&op), procs_with_me());
    let token = grant_in(&state, &fx.project(), &["API_KEY"]);
    assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
    assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
    assert_eq!(op.gets(), 1, "the cache was not warm");

    let revoked = exec::revoke(&state, caller(), json!({"token": token})).unwrap();
    assert_eq!(revoked["revoked"], true);
    let refused = resolve_in(&state, &token, "API_KEY");
    assert_eq!(refused, Err(ErrorKind::GrantRefused));
}

/// Why: #7520, plan §6 point 2 — a failed read (a declined unlock) is not
/// cached: the next resolve reads the backend again, and its value is then
/// cached.
/// Red while nothing is cached (three reads).
/// Test: itself.
#[test]
fn failed_read_is_not_cached() {
    let fx = fixture();
    let op = Counting::new(BackendId::onepassword());
    put(&fx, &op, PROJECT_VAULT, "API_KEY", SENTINEL_VALUE);
    op.failing.store(1, Ordering::SeqCst);
    let state = cache_state(&fx, std::slice::from_ref(&op), procs_with_me());
    let token = grant_in(&state, &fx.project(), &["API_KEY"]);

    let failed = resolve_in(&state, &token, "API_KEY");
    assert_eq!(failed, Err(ErrorKind::BackendFailed));
    assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
    assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
    assert_eq!(op.gets(), 2, "a failure was cached, or a value was not");
}

/// Select `onepassword` under 1Password account `account` in the account
/// machine config, the file the server's factory opens CLI backends from.
fn select_onepassword_account(fx: &Fixture, account: &str) {
    std::fs::write(
        &fx.settings.machine_config,
        format!(
            "secrets:\n  default_backend: onepassword\n  onepassword:\n    account: {account}\n"
        ),
    )
    .unwrap();
}

/// Why: #7520 M1 — `backends_with` rereads the account machine config on
/// every open, so `onepassword.account` can change mid-session. A value
/// filled under the old account must not answer once the backend opens
/// under the new one. The factory here builds the backend from that file's
/// account, as the production factory does.
/// Red while entries are keyed by backend id alone (account A's value
/// answers after the switch to B).
/// Test: itself.
#[test]
fn cache_cleared_when_cli_account_settings_change() {
    let fx = fixture();
    let account_a = Counting::new(BackendId::onepassword());
    let account_b = Counting::new(BackendId::onepassword());
    put(&fx, &account_a, PROJECT_VAULT, "API_KEY", "account-a-value");
    put(&fx, &account_b, PROJECT_VAULT, "API_KEY", "account-b-value");
    let config = fx.settings.machine_config.clone();
    let (a, b) = (Arc::clone(&account_a), Arc::clone(&account_b));
    let factory: BackendFactory = Arc::new(move |id: &BackendId| {
        let machine = crate::store::config::load_machine_at(&config)?.unwrap_or_default();
        let account = machine.onepassword.and_then(|section| section.account);
        match account.as_deref() {
            Some("a.1password.com") => Ok(Arc::clone(&a) as Arc<dyn SecretBackend>),
            Some("b.1password.com") => Ok(Arc::clone(&b) as Arc<dyn SecretBackend>),
            _ => Err(SecretsError::BackendNotEnabled {
                backend: id.to_string(),
            }),
        }
    });
    select_onepassword_account(&fx, "a.1password.com");
    let mut state = fx.state(factory);
    state.grants = Arc::new(GrantRegistry::new(
        procs_with_me(),
        Arc::new(FakeClock::at(T0)),
        DEFAULT_MAX_TTL,
    ));
    let token = grant_in(&state, &fx.project(), &["API_KEY"]);
    assert_eq!(value_of(&state, &token, "API_KEY"), "account-a-value");
    assert_eq!(value_of(&state, &token, "API_KEY"), "account-a-value");
    assert_eq!(
        account_a.gets(),
        1,
        "the cache was not warm under account A"
    );

    select_onepassword_account(&fx, "b.1password.com");
    assert_eq!(
        value_of(&state, &token, "API_KEY"),
        "account-b-value",
        "a value filled under account A answered under account B"
    );
    assert_eq!(account_b.gets(), 1, "account B's backend was not read");
    assert_eq!(value_of(&state, &token, "API_KEY"), "account-b-value");
    assert_eq!(account_b.gets(), 1, "account B's value was not cached");
}
