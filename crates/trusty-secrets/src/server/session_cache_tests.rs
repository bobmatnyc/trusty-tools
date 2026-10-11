//! The per-session cache of CLI-backed values (#7520, S1 and S2).
//!
//! A child of `wire_tests`, so it shares that module's fixture and helpers:
//! every path is under a `TempDir`, the backends are in memory, and the
//! caller is this test process, placed in a fake process table. Most tests
//! call the method bodies on one [`State`], which is one server session;
//! the first goes over a real socket.
//!
//! Test: itself.

use std::sync::atomic::{AtomicBool, AtomicU64};
use std::time::{Instant, SystemTime};

use super::*;
use crate::server::exec;
use crate::server::methods::{self, Caller};
use crate::store::SecretCache;

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

/// Why: #7520 AC3 — the cache shows in `State`'s `Debug` as an entry count
/// (and, since S2, its max age), never a value.
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
        assert!(compact.contains("SecretCache{entries:1,"), "{debug}");
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
    std::fs::write(&fx.settings.machine_config, onepassword_account(account)).unwrap();
}

/// The machine config text selecting `onepassword` under `account`.
fn onepassword_account(account: &str) -> String {
    format!("secrets:\n  default_backend: onepassword\n  onepassword:\n    account: {account}\n")
}

/// Two `onepassword` doubles, accounts A and B, each holding its own
/// `API_KEY` in the project vault.
fn two_accounts(fx: &Fixture) -> (Arc<Counting>, Arc<Counting>) {
    let account_a = Counting::new(BackendId::onepassword());
    let account_b = Counting::new(BackendId::onepassword());
    put(fx, &account_a, PROJECT_VAULT, "API_KEY", "account-a-value");
    put(fx, &account_b, PROJECT_VAULT, "API_KEY", "account-b-value");
    (account_a, account_b)
}

/// Which double the account machine config at `config` names.
fn account_named(
    config: &Path,
    a: &Arc<Counting>,
    b: &Arc<Counting>,
) -> Result<Arc<dyn SecretBackend>, SecretsError> {
    let machine = crate::store::config::load_machine_at(config)?.unwrap_or_default();
    let account = machine.onepassword.and_then(|section| section.account);
    match account.as_deref() {
        Some("a.1password.com") => Ok(Arc::clone(a) as Arc<dyn SecretBackend>),
        Some("b.1password.com") => Ok(Arc::clone(b) as Arc<dyn SecretBackend>),
        _ => Err(SecretsError::BackendNotEnabled {
            backend: BackendId::ONEPASSWORD.to_string(),
        }),
    }
}

/// Replace `path` with a new file holding `text`, by rename, as an editor
/// that saves atomically does.
fn replace_by_rename(path: &Path, text: &str) {
    let staged = path.with_extension("staged");
    std::fs::write(&staged, text).unwrap();
    std::fs::rename(&staged, path).unwrap();
}

/// Rewrite the file the symlink `config` points at, in place, to account B,
/// open B's double, then rewrite it to account A again; the link is not
/// touched.
///
/// What: same length, same inode, so only the target's mtime and ctime
/// change. The pause first puts the rewrite in a later timestamp tick than
/// the stamp taken before the open, on filesystems with coarse timestamps.
fn flip_target_in_place(
    config: &Path,
    a: &Arc<Counting>,
    b: &Arc<Counting>,
) -> Result<Arc<dyn SecretBackend>, SecretsError> {
    let target = std::fs::read_link(config).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    std::fs::write(&target, onepassword_account("b.1password.com")).unwrap();
    let opened = account_named(config, a, b);
    std::fs::write(&target, onepassword_account("a.1password.com")).unwrap();
    opened
}

/// A state whose factory opens `onepassword` from the account machine
/// config, as the production factory does: account A or account B. Selects
/// account A.
///
/// What: when `flip` is set, the next open clears it, replaces the config
/// with account B, opens B's double, then replaces the config with account
/// A's text again — the A→B→A flip of #7520 S2. A's first file is kept
/// linked aside, so the restored file is a new inode.
fn accounts_state(
    fx: &Fixture,
    account_a: &Arc<Counting>,
    account_b: &Arc<Counting>,
    flip: Option<Arc<AtomicBool>>,
) -> State {
    let config = fx.settings.machine_config.clone();
    let (a, b) = (Arc::clone(account_a), Arc::clone(account_b));
    let factory: BackendFactory = Arc::new(move |_: &BackendId| {
        if !flip
            .as_ref()
            .is_some_and(|flip| flip.swap(false, Ordering::SeqCst))
        {
            return account_named(&config, &a, &b);
        }
        if config.is_symlink() {
            return flip_target_in_place(&config, &a, &b);
        }
        std::fs::hard_link(&config, config.with_extension("first")).unwrap();
        replace_by_rename(&config, &onepassword_account("b.1password.com"));
        let opened = account_named(&config, &a, &b);
        replace_by_rename(&config, &onepassword_account("a.1password.com"));
        opened
    });
    select_onepassword_account(fx, "a.1password.com");
    let mut state = fx.state(factory);
    state.grants = Arc::new(GrantRegistry::new(
        procs_with_me(),
        Arc::new(FakeClock::at(T0)),
        DEFAULT_MAX_TTL,
    ));
    state
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
    let (account_a, account_b) = two_accounts(&fx);
    let state = accounts_state(&fx, &account_a, &account_b, None);
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

/// Why: #7520 S2 — the account config is read before and after the backend
/// opens, and the open reads it a third time. If the file goes A→B→A in
/// that window, the backend opened under B but both reads see A; B's value
/// must not be filed under A. A stat fingerprint of the file closes this.
/// Red while the before/after comparison looks at the settings alone (the
/// second resolve answers B's value from the cache).
/// Test: itself.
#[test]
fn cache_not_filled_across_account_file_flip() {
    let fx = fixture();
    let (account_a, account_b) = two_accounts(&fx);
    let flip = Arc::new(AtomicBool::new(false));
    let state = accounts_state(&fx, &account_a, &account_b, Some(Arc::clone(&flip)));
    let token = grant_in(&state, &fx.project(), &["API_KEY"]);

    flip.store(true, Ordering::SeqCst);
    assert_eq!(value_of(&state, &token, "API_KEY"), "account-b-value");
    assert!(!flip.load(Ordering::SeqCst), "the flip did not run");
    assert_eq!(
        value_of(&state, &token, "API_KEY"),
        "account-a-value",
        "a value read under account B answered under account A"
    );
    assert_eq!(value_of(&state, &token, "API_KEY"), "account-a-value");
    assert_eq!(account_a.gets(), 1, "account A's value was not cached");
}

/// Why: #7520 M2 — the write-ahead audit record precedes the cache, so a
/// warm cache with an audit log that cannot be written refuses the resolve
/// and serves nothing; once the log is writable the cache answers again.
/// Pins behaviour that already holds at S1 (b774b6feea): a later move of
/// the cache lookup ahead of `write_ahead` would serve unaudited values.
/// Test: itself.
#[cfg(unix)]
#[test]
fn warm_cache_with_unwritable_audit_serves_nothing() {
    let fx = fixture();
    let op = Counting::new(BackendId::onepassword());
    put(&fx, &op, PROJECT_VAULT, "API_KEY", SENTINEL_VALUE);
    let state = cache_state(&fx, std::slice::from_ref(&op), procs_with_me());
    let token = grant_in(&state, &fx.project(), &["API_KEY"]);
    assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
    assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
    assert_eq!(op.gets(), 1, "the cache was not warm");

    let dir = fx.settings.audit_log.parent().unwrap().to_path_buf();
    set_mode(&dir, 0o755);
    let restore = RestoreMode(dir);
    let refused = resolve_in(&state, &token, "API_KEY");
    drop(restore);
    assert_eq!(refused, Err(ErrorKind::AuditUnavailable));
    assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
    assert_eq!(op.gets(), 1, "the cache did not answer after the refusal");
}

/// Why: #7520 M2 — a one-shot grant is spent by a resolve the cache
/// answers, exactly as by one that reads the backend: the second use is
/// refused.
/// Pins behaviour that already holds at S1 (b774b6feea): the spend follows
/// the read whichever path served it, and a hit that skipped the spend
/// would make a one-shot grant reusable.
/// Test: itself.
#[test]
fn one_shot_grant_is_spent_by_a_cache_hit() {
    let fx = fixture();
    let op = Counting::new(BackendId::onepassword());
    put(&fx, &op, PROJECT_VAULT, "API_KEY", SENTINEL_VALUE);
    let state = cache_state(&fx, std::slice::from_ref(&op), procs_with_me());
    let warm = grant_in(&state, &fx.project(), &["API_KEY"]);
    assert_eq!(value_of(&state, &warm, "API_KEY"), SENTINEL_VALUE);

    let params = json!({"project": fx.project(), "child_pid": me(), "keys": ["API_KEY"],
        "ttl_secs": 600, "one_shot": true});
    let answer = exec::grant(&state, caller(), params).unwrap();
    let once = answer["token"].as_str().unwrap().to_owned();
    assert_eq!(value_of(&state, &once, "API_KEY"), SENTINEL_VALUE);
    assert_eq!(op.gets(), 1, "the one-shot resolve was not a cache hit");
    let spent = resolve_in(&state, &once, "API_KEY");
    assert_eq!(
        spent,
        Err(ErrorKind::GrantRefused),
        "a cache hit spent nothing"
    );
}

/// Why: #7520 S2 — idle exit alone does not bound how long a value is
/// served, since a busy server never goes idle. An entry as old as the
/// configured max age is not served: the next resolve reads the backend
/// again and refills it. `State::new` takes the max age from the settings.
/// Red while entries never expire (one read for the whole run).
/// Test: itself.
#[test]
fn entry_expires_after_max_age() {
    let mut fx = fixture();
    fx.settings.cache_max_age = Duration::from_secs(5);
    let op = Counting::new(BackendId::onepassword());
    put(&fx, &op, PROJECT_VAULT, "API_KEY", SENTINEL_VALUE);
    let mut state = cache_state(&fx, std::slice::from_ref(&op), procs_with_me());
    let debug = format!("{state:?}");
    assert!(debug.contains("max_age: 5s"), "{debug}");
    let (base, elapsed) = (Instant::now(), Arc::new(AtomicU64::new(0)));
    let (wall, clock, wall_clock) = (
        SystemTime::now(),
        Arc::clone(&elapsed),
        Arc::clone(&elapsed),
    );
    state.cache = Arc::new(SecretCache::with_clocks(
        state.settings.cache_max_age,
        Arc::new(move || base + Duration::from_secs(clock.load(Ordering::SeqCst))),
        Arc::new(move || wall + Duration::from_secs(wall_clock.load(Ordering::SeqCst))),
    ));
    let token = grant_in(&state, &fx.project(), &["API_KEY"]);

    for (at, reads) in [(0, 1), (4, 1), (5, 2), (9, 2), (10, 3)] {
        elapsed.store(at, Ordering::SeqCst);
        assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
        assert_eq!(op.gets(), reads, "backend reads after the resolve at {at}s");
    }
}

/// Why: #7520 S2 — the account config may be a symlink. A flip of the file
/// it points at, rewritten in place behind an unchanged link, must count as
/// a change, so the fingerprint stamps the target (`stat`) as well as the
/// link (`lstat`).
/// Passes on the fixed code; red when the target stamp is dropped (both
/// stamps from `lstat`), shown by a mutation run (#7520 S2 fix round).
/// Test: itself.
#[cfg(unix)]
#[test]
fn cache_not_filled_across_symlink_target_flip() {
    let fx = fixture();
    let (account_a, account_b) = two_accounts(&fx);
    let flip = Arc::new(AtomicBool::new(false));
    let state = accounts_state(&fx, &account_a, &account_b, Some(Arc::clone(&flip)));
    let config = &fx.settings.machine_config;
    let target = config.with_extension("target");
    std::fs::rename(config, &target).unwrap();
    std::os::unix::fs::symlink(&target, config).unwrap();
    let token = grant_in(&state, &fx.project(), &["API_KEY"]);

    flip.store(true, Ordering::SeqCst);
    assert_eq!(value_of(&state, &token, "API_KEY"), "account-b-value");
    assert!(!flip.load(Ordering::SeqCst), "the flip did not run");
    assert!(config.is_symlink(), "the link itself was replaced");
    assert_eq!(
        value_of(&state, &token, "API_KEY"),
        "account-a-value",
        "a value read under account B answered under account A"
    );
    assert_eq!(account_a.gets(), 1);
}
