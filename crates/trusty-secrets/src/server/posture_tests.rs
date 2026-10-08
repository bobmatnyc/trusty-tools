//! Value writes into the `file` backend on a Keychain build (#7524 H1).
//!
//! A child of `server_tests`, so it shares that module's fixture: every path
//! is under a `TempDir`, `keychain` is an in-memory double, and `file` is a
//! value-file backend in the temp dir. No test reaches the OS Keychain or
//! reads the account's own machine config; [`start_on_build`] picks whether
//! the server acts as a Keychain build, and [`start_with_consent`] where the
//! account's file consent config sits.
//!
//! Test: itself.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::store::config::MACHINE_CONFIG_SUBPATH;
use crate::store::platform;

/// The wire kind of the #7524 H1 refusal.
const FILE_NOT_SELECTED: &str = "file_backend_not_selected";

/// Start a server for `fx` that acts as a build with (`keychain: true`) or
/// without a Keychain backend. The fixture's machine config is also the
/// file consent config.
async fn start_on_build(fx: &Fixture, backends: BackendFactory, keychain: bool) -> Running {
    let mut state = fx.state(backends);
    state.keychain_compiled = keychain;
    fx.start_state(state).await
}

/// Start a Keychain-build server for `fx` whose file consent config is
/// `consent`, apart from the machine config in `fx.settings`.
///
/// Why: #7524 H1 — `--machine-config` and `$HOME` are the spawner's choice;
/// only the account's own config, which `consent` stands in for, consents.
async fn start_with_consent(fx: &Fixture, backends: BackendFactory, consent: &Path) -> Running {
    let mut state = fx.state(backends);
    state.keychain_compiled = true;
    state.file_consent_config = Some(consent.to_path_buf());
    fx.start_state(state).await
}

/// `backends`, counting every backend it opens.
// #7524 H1: a refused write opens no backend, so it reads no source value.
fn counting(backends: BackendFactory) -> (BackendFactory, Arc<AtomicUsize>) {
    let opens = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&opens);
    let factory: BackendFactory = Arc::new(move |id: &BackendId| {
        seen.fetch_add(1, Ordering::SeqCst);
        backends(id)
    });
    (factory, opens)
}

/// Point the machine config at `backend`; the server rereads it per request.
fn select_backend(fx: &Fixture, backend: &str) {
    std::fs::write(
        &fx.settings.machine_config,
        format!("secrets:\n  default_backend: {backend}\n"),
    )
    .unwrap();
}

/// Put [`VALUE`] under `name` in the Keychain double, with no index row.
fn seed_keychain(fx: &Fixture, name: &str) {
    fx.keychain
        .set(
            &vault("trusty/acme/web"),
            &key(name),
            &SecretValue::new(VALUE),
        )
        .unwrap();
}

/// `secrets.copy` of `keys` from the Keychain double into `file`.
async fn copy_to_file(fx: &Fixture, keys: &[&str]) -> RpcResponse {
    call(
        &fx.settings.socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain", "to_backend": "file",
               "keys": keys}),
    )
    .await
}

/// Every record in the fixture's audit log, and the log's raw text.
fn audit(fx: &Fixture) -> (Vec<AuditRecord>, String) {
    let text = match std::fs::read_to_string(&fx.settings.audit_log) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => panic!("{e}"),
    };
    let records = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (records, text)
}

/// Why: #7524 H1, owner ruling item 74 — the Keychain ACL is a boundary
/// against same-user callers, so on a Keychain build `copy` may not move a
/// value into the 0600 plaintext `file` backend unless the untracked machine
/// config selected `file`. The refusal names the remedy, opens no backend (so
/// it never reads the source), writes no value and no index row, and leaves
/// exactly one deny record carrying no value.
/// Red before the fix: the copy succeeds and `file` holds the value.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_to_file_is_refused_on_a_keychain_build_without_machine_selection() {
    let fx = fixture();
    seed_keychain(&fx, "A");
    let (factory, file) = with_file_backend(&fx);
    let (factory, opens) = counting(factory);
    let server = start_on_build(&fx, factory, true).await;
    let refused = copy_to_file(&fx, &["A"]).await;
    server.stop().await;
    // #7524 H1: refused before the source or the destination is opened.
    assert_eq!(opens.load(Ordering::SeqCst), 0, "no backend is opened");

    let text = wire(&refused);
    assert!(!text.contains(VALUE), "{text}");
    assert!(text.contains("secrets.default_backend: file"), "{text}");
    assert_eq!(
        fixed_error(&refused, method::COPY).as_str(),
        FILE_NOT_SELECTED
    );
    let project = vault("trusty/acme/web");
    assert!(file.get(&project, &key("A")).unwrap().is_none());
    assert!(!file.root().exists(), "no value file or directory is made");
    let index = NamesIndex::at(&fx.settings.index_root);
    assert!(index.list(&project).unwrap().is_empty(), "no index row");
    assert_eq!(fx.keychain.len(), 1, "the source is untouched");

    let (records, log) = audit(&fx);
    assert!(!log.contains(VALUE), "{log}");
    assert_eq!(records.len(), 1, "{log}");
    let record = &records[0];
    assert_eq!(
        (record.method, record.decision),
        (AuditMethod::Copy, AuditDecision::Deny)
    );
    assert_eq!(
        record.reason.as_ref().map(AuditReason::as_str),
        Some(FILE_NOT_SELECTED)
    );
    assert_eq!(record.backend, Some(BackendId::file()));
    assert_eq!(record.vault, Some(project));
    assert_eq!(record.key, None);
}

/// Why: #7524 H1 — the untracked machine config is the operator's consent:
/// with `secrets.default_backend: file` a Keychain build still copies into
/// `file`, and each key is one allow record.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_to_file_is_allowed_when_the_machine_config_selects_file() {
    let fx = fixture();
    select_backend(&fx, "file");
    seed_keychain(&fx, "A");
    let (factory, file) = with_file_backend(&fx);
    let server = start_on_build(&fx, factory, true).await;
    let copied = copy_to_file(&fx, &["A"]).await;
    server.stop().await;

    assert!(!wire(&copied).contains(VALUE));
    assert_eq!(ok(copied), json!({"copied": ["A"], "failed": []}));
    let project = vault("trusty/acme/web");
    assert_eq!(
        file.get(&project, &key("A")).unwrap().unwrap().expose(),
        VALUE
    );
    let (records, _) = audit(&fx);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].decision, AuditDecision::Allow);
}

/// Why: #7524 H1 — a build without a Keychain has no ACL boundary to keep:
/// `file` is its default store, and `copy` into it stays allowed whatever
/// the machine config names.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_to_file_is_allowed_on_a_build_without_a_keychain() {
    let fx = fixture();
    seed_keychain(&fx, "A");
    let (factory, file) = with_file_backend(&fx);
    let server = start_on_build(&fx, factory, false).await;
    let copied = copy_to_file(&fx, &["A"]).await;
    server.stop().await;

    assert_eq!(ok(copied), json!({"copied": ["A"], "failed": []}));
    assert_eq!(
        file.get(&vault("trusty/acme/web"), &key("A"))
            .unwrap()
            .unwrap()
            .expose(),
        VALUE
    );
}

/// Why: #7524 H1 refuses writes into `file`, not reads or deletes: on a
/// Keychain build, a `file` copy left from before the rule (or before the
/// machine config changed) is still cleared by #7519's delete sweep.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_delete_sweeps_a_file_copy_on_a_keychain_build() {
    let fx = fixture();
    let (factory, file) = with_file_backend(&fx);
    let server = start_on_build(&fx, factory, true).await;
    let project = vault("trusty/acme/web");
    let target = json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "A"});
    let mut set = target.clone();
    set["value"] = json!(VALUE);
    ok(call(&fx.settings.socket, method::SET, set).await);
    file.set(&project, &key("A"), &SecretValue::new(VALUE))
        .unwrap();

    let deleted = call(&fx.settings.socket, method::DELETE, target).await;
    server.stop().await;
    assert_eq!(ok(deleted), json!({"removed": true}));
    assert!(fx.keychain.is_empty());
    assert!(file.get(&project, &key("A")).unwrap().is_none());
}

/// Why: #7524 H1 — `set` is the other path that writes a value into `file`.
/// On a Keychain build a tracked `backend: file` stops at the tracked-config
/// check (`TrackedBackendRefused`) and writes nothing; the machine selection
/// then writes. This test never reaches `FileBackendNotSelected`;
/// `server_set_into_file_is_refused_when_only_a_spawner_chosen_config_selects_it`
/// does.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_set_writes_file_only_when_the_machine_config_selects_it() {
    let fx = fixture();
    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "secrets:\n  backend: file\n").unwrap();
    let (factory, file) = with_file_backend(&fx);
    let server = start_on_build(&fx, factory, true).await;
    let params =
        json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "A", "value": VALUE});
    let tracked = call(&fx.settings.socket, method::SET, params.clone()).await;
    let project = vault("trusty/acme/web");
    assert_eq!(
        fixed_error(&tracked, method::SET),
        ErrorKind::TrackedBackendRefused
    );
    assert!(file.get(&project, &key("A")).unwrap().is_none());
    assert!(fx.keychain.is_empty(), "never a silent switch");

    std::fs::remove_file(&config).unwrap();
    select_backend(&fx, "file");
    ok(call(&fx.settings.socket, method::SET, params).await);
    server.stop().await;
    assert_eq!(
        file.get(&project, &key("A")).unwrap().unwrap().expose(),
        VALUE
    );
}

/// The two account configs that do not consent: absent, and naming `keychain`.
fn non_consenting(fx: &Fixture) -> [PathBuf; 2] {
    let absent = fx
        .tmp
        .path()
        .join("absent-home")
        .join(MACHINE_CONFIG_SUBPATH);
    let keychain = fx
        .tmp
        .path()
        .join("keychain-home")
        .join(MACHINE_CONFIG_SUBPATH);
    std::fs::create_dir_all(keychain.parent().unwrap()).unwrap();
    std::fs::write(&keychain, "secrets:\n  default_backend: keychain\n").unwrap();
    [absent, keychain]
}

/// Why: #7524 H1, Architect ruling on item 74 — only the account's own
/// machine config may consent to `file` writes. `--machine-config` on any
/// socket, or a redirected `$HOME`, is the spawner's choice: a config there
/// that selects `file` must not let `copy` move Keychain values into 0600
/// plaintext. With the account config absent or naming `keychain`, the copy
/// is `file_backend_not_selected`, opens no backend, writes no value file and
/// no index row, and leaves one deny record carrying no value.
/// Red before the fix: the server read consent from `settings.machine_config`,
/// so the copy succeeded and `file` held the value.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_to_file_is_refused_when_only_a_spawner_chosen_config_selects_it() {
    for case in 0..2 {
        let fx = fixture();
        let consent = non_consenting(&fx)[case].clone();
        select_backend(&fx, "file");
        seed_keychain(&fx, "A");
        let (factory, file) = with_file_backend(&fx);
        let (factory, opens) = counting(factory);
        let server = start_with_consent(&fx, factory, &consent).await;
        let refused = copy_to_file(&fx, &["A"]).await;
        server.stop().await;

        assert!(!wire(&refused).contains(VALUE), "case {case}");
        assert_eq!(
            fixed_error(&refused, method::COPY).as_str(),
            FILE_NOT_SELECTED,
            "case {case}"
        );
        assert_eq!(opens.load(Ordering::SeqCst), 0, "case {case}: no open");
        let project = vault("trusty/acme/web");
        assert!(!file.root().exists(), "case {case}: no value file");
        let index = NamesIndex::at(&fx.settings.index_root);
        assert!(index.list(&project).unwrap().is_empty(), "case {case}");
        assert_eq!(fx.keychain.len(), 1, "case {case}: source untouched");

        let (records, log) = audit(&fx);
        assert!(!log.contains(VALUE), "case {case}: {log}");
        assert_eq!(records.len(), 1, "case {case}: {log}");
        assert_eq!(
            (records[0].method, records[0].decision),
            (AuditMethod::Copy, AuditDecision::Deny)
        );
        assert_eq!(
            records[0].reason.as_ref().map(AuditReason::as_str),
            Some(FILE_NOT_SELECTED)
        );
    }
}

/// Why: #7524 H1 — the same route through `set`. A spawner-chosen machine
/// config that selects `file` makes `file` the resolved backend, so `set`
/// reaches the write check itself (not the tracked-config refusal) and is
/// `file_backend_not_selected`: no backend opened, no value written, no
/// index row, one deny record without the value.
/// Red before the fix: the set succeeded into `file`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_set_into_file_is_refused_when_only_a_spawner_chosen_config_selects_it() {
    for case in 0..2 {
        let fx = fixture();
        let consent = non_consenting(&fx)[case].clone();
        select_backend(&fx, "file");
        let (factory, file) = with_file_backend(&fx);
        let (factory, opens) = counting(factory);
        let server = start_with_consent(&fx, factory, &consent).await;
        let params = json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "A",
                            "value": VALUE});
        let refused = call(&fx.settings.socket, method::SET, params).await;
        server.stop().await;

        assert!(!wire(&refused).contains(VALUE), "case {case}");
        assert_eq!(
            fixed_error(&refused, method::SET),
            ErrorKind::FileBackendNotSelected,
            "case {case}"
        );
        assert_eq!(opens.load(Ordering::SeqCst), 0, "case {case}: no open");
        let project = vault("trusty/acme/web");
        assert!(!file.root().exists(), "case {case}: no value file");
        assert!(fx.keychain.is_empty(), "case {case}: never a silent switch");
        let index = NamesIndex::at(&fx.settings.index_root);
        assert!(index.list(&project).unwrap().is_empty(), "case {case}");

        let (records, log) = audit(&fx);
        assert!(!log.contains(VALUE), "case {case}: {log}");
        assert_eq!(records.len(), 1, "case {case}: {log}");
        assert_eq!(
            (records[0].method, records[0].decision),
            (AuditMethod::Set, AuditDecision::Deny)
        );
        assert_eq!(
            records[0].reason.as_ref().map(AuditReason::as_str),
            Some(FILE_NOT_SELECTED)
        );
        assert_eq!(records[0].backend, Some(BackendId::file()));
        assert_eq!(records[0].key, Some(key("A")));
    }
}

/// Why: #7524 H1 — every other test aims the consent seam at a temp dir, so
/// this one pins the production default: the password database's home for
/// this uid joined with the machine-config subpath, whatever
/// `settings.machine_config` says. It reads no file. A host with no
/// password entry for the test uid (a stock container) takes the
/// fail-closed branch: no consent config, so `file` writes are refused.
/// Test: itself.
#[test]
fn server_file_consent_defaults_to_the_account_home_config() {
    let fx = fixture();
    let state = State::new(fx.settings.clone(), fx.backends());
    // #7524: `State::new` has no home seam, so this test pins both outcomes.
    match platform::account_home_dir() {
        Ok(home) => {
            assert!(home.is_absolute(), "{}", home.display());
            assert_eq!(
                state.file_consent_config,
                Some(home.join(MACHINE_CONFIG_SUBPATH))
            );
        }
        Err(err) => {
            assert!(matches!(err, SecretsError::HomeUnavailable), "{err:?}");
            assert_eq!(state.file_consent_config, None);
        }
    }
    assert_ne!(
        state.file_consent_config.as_deref(),
        Some(fx.settings.machine_config.as_path())
    );
}
