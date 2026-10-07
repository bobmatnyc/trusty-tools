//! Unit tests for [`super::SecretStore`], capabilities, backend selection, and
//! `mask_secret`. Every test runs against [`MemoryBackend`] or a failing
//! double, with a temp-dir index.
//!
//! Test: itself.

use std::sync::Arc;

use tempfile::TempDir;

use super::platform;
use super::*;
use crate::api::methods::SetOutcome;
use crate::api::{BackendId, SecretKey, SecretRef, SecretValue, SecretsError, VaultName};

const FAKE_VALUE: &str = "sk-fake-store-1234567890";

fn project() -> VaultName {
    VaultName::new("trusty/acme/web").unwrap()
}

fn owner() -> VaultName {
    VaultName::new("trusty/acme").unwrap()
}

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).unwrap()
}

fn scopes() -> ScopeSet {
    ScopeSet::new(project(), Some(owner()))
}

fn fixture_with(backend: Arc<dyn SecretBackend>) -> (TempDir, SecretStore) {
    let tmp = TempDir::new().unwrap();
    let store = SecretStore::new(backend, NamesIndex::at(tmp.path().join("index")));
    (tmp, store)
}

fn fixture() -> (TempDir, Arc<MemoryBackend>, SecretStore) {
    let backend = Arc::new(MemoryBackend::new());
    let (tmp, store) = fixture_with(Arc::clone(&backend) as Arc<dyn SecretBackend>);
    (tmp, backend, store)
}

/// A backend whose every call fails, standing in for a locked keychain.
#[derive(Debug)]
struct FailingBackend;

impl FailingBackend {
    fn failure(vault: &VaultName, key: &SecretKey) -> SecretsError {
        SecretsError::Backend {
            backend: "failing".to_string(),
            vault: vault.to_string(),
            key: key.to_string(),
            reason: "storage is locked".to_string(),
        }
    }
}

impl SecretBackend for FailingBackend {
    fn id(&self) -> BackendId {
        BackendId::new("failing").unwrap()
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::READ | Capabilities::WRITE
    }
    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        Err(Self::failure(vault, key))
    }
    fn set(&self, vault: &VaultName, key: &SecretKey, _: &SecretValue) -> Result<(), SecretsError> {
        Err(Self::failure(vault, key))
    }
    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        Err(Self::failure(vault, key))
    }
}

/// Why: DOC-74 §15.6 — `set` reports new/updated and shows the mask once;
/// an empty value is refused before it reaches the backend.
/// Test: itself.
#[test]
fn store_set_reports_outcome_and_mask_once() {
    let (_tmp, backend, store) = fixture();
    let first = store
        .set(&project(), &key("API_KEY"), &SecretValue::new(FAKE_VALUE))
        .unwrap();
    assert_eq!(first.outcome, SetOutcome::New);
    assert_eq!(first.masked, "sk-fake-… [24 chars]");
    let second = store
        .set(&project(), &key("API_KEY"), &SecretValue::new("short"))
        .unwrap();
    assert_eq!(second.outcome, SetOutcome::Updated);
    assert_eq!(second.masked, "[5 chars]");

    let err = store
        .set(&project(), &key("EMPTY"), &SecretValue::new(""))
        .unwrap_err();
    assert!(matches!(err, SecretsError::InvalidValue { .. }), "{err:?}");
    assert_eq!(
        backend.len(),
        1,
        "a refused value never reaches the backend"
    );
}

/// Why: `list` carries length and `updated_at`, never characters, and reads
/// the index rather than values.
/// Test: itself.
#[test]
fn store_list_reports_length_and_time_never_characters() {
    let (_tmp, _backend, store) = fixture();
    store
        .set(&project(), &key("API_KEY"), &SecretValue::new(FAKE_VALUE))
        .unwrap();
    let rows = store.list(&project()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].length, FAKE_VALUE.chars().count());
    assert!(rows[0].updated_at > 0);
    let wire = serde_json::to_string(&rows).unwrap();
    assert!(
        !wire.contains(&FAKE_VALUE[..8]),
        "list leaked characters: {wire}"
    );
}

/// Why: removal must clear both records, or `list` reports a key that is gone.
/// Test: itself.
#[test]
fn store_delete_removes_entry_and_row() {
    let (_tmp, backend, store) = fixture();
    let value = SecretValue::new(FAKE_VALUE);
    store.set(&project(), &key("KEEP"), &value).unwrap();
    store.set(&project(), &key("DROP"), &value).unwrap();

    assert!(store.delete(&project(), &key("DROP")).unwrap().removed);
    let names: Vec<String> = store
        .list(&project())
        .unwrap()
        .into_iter()
        .map(|m| m.name.to_string())
        .collect();
    assert_eq!(names, ["KEEP"]);
    assert_eq!(backend.len(), 1);
    assert!(!store.delete(&project(), &key("DROP")).unwrap().removed);
}

/// Why: #7519 A5 — a key in a backend other than the configured one is
/// removed too; a key no backend holds is still `removed: false`.
/// Test: itself.
#[test]
fn store_delete_across_removes_the_key_from_every_backend() {
    let (_tmp, configured, store) = fixture();
    let old = Arc::new(MemoryBackend::new());
    let others = [Arc::clone(&old) as Arc<dyn SecretBackend>];
    let value = SecretValue::new(FAKE_VALUE);
    store.set(&project(), &key("BOTH"), &value).unwrap();
    old.set(&project(), &key("BOTH"), &value).unwrap();
    old.set(&project(), &key("ONLY_OLD"), &value).unwrap();

    let both = store.delete_across(&project(), &key("BOTH"), &others);
    assert!(both.unwrap().removed);
    let only_old = store.delete_across(&project(), &key("ONLY_OLD"), &others);
    assert!(
        only_old.unwrap().removed,
        "a value only the old backend held"
    );
    assert!(configured.is_empty() && old.is_empty());
    assert!(store.list(&project()).unwrap().is_empty());
    let none = store.delete_across(&project(), &key("NONE"), &others);
    assert!(!none.unwrap().removed);
}

/// Why: #7519, Fail-Open Check — a backend that fails to delete may still
/// hold the value, so the call is an error and the index row stays. The
/// other backends are still cleared, and the error carries no value.
/// Red when a failed delete is skipped or ends the loop early.
/// Test: itself.
#[test]
fn store_delete_across_keeps_the_row_when_any_backend_fails() {
    let (_tmp, configured, store) = fixture();
    let later = Arc::new(MemoryBackend::new());
    let others = [
        Arc::new(FailingBackend) as Arc<dyn SecretBackend>,
        Arc::clone(&later) as Arc<dyn SecretBackend>,
    ];
    let value = SecretValue::new(FAKE_VALUE);
    store.set(&project(), &key("API_KEY"), &value).unwrap();
    later.set(&project(), &key("API_KEY"), &value).unwrap();

    let err = store
        .delete_across(&project(), &key("API_KEY"), &others)
        .unwrap_err();
    assert!(
        matches!(&err, SecretsError::Backend { backend, .. } if backend == "failing"),
        "{err:?}"
    );
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains(FAKE_VALUE), "{shown}");
    assert!(configured.is_empty() && later.is_empty());
    let rows = store.list(&project()).unwrap();
    assert_eq!(rows.len(), 1, "the row stays while a value may remain");
}

/// A swept backend whose `delete` lets a second writer try a `set` first.
///
/// What: stands in for a concurrent `secrets.set` that lands while a delete
/// is sweeping. The racer's index never waits for the lock, so it either
/// writes at once or gets [`SecretsError::LockTimeout`]; `raced_in` records
/// which.
#[derive(Debug)]
struct RacingBackend {
    inner: MemoryBackend,
    racer: SecretStore,
    raced_in: std::sync::atomic::AtomicBool,
}

impl SecretBackend for RacingBackend {
    fn id(&self) -> BackendId {
        BackendId::new("racing").unwrap()
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        self.inner.get(vault, key)
    }
    fn set(&self, vault: &VaultName, key: &SecretKey, v: &SecretValue) -> Result<(), SecretsError> {
        self.inner.set(vault, key, v)
    }
    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        let raced = self
            .racer
            .set(vault, key, &SecretValue::new("sk-racer-7519"));
        self.raced_in
            .store(raced.is_ok(), std::sync::atomic::Ordering::SeqCst);
        self.inner.delete(vault, key)
    }
}

/// Why: #7519 P1 carry-over (b) — the sweep ran outside the index lock, so a
/// `set` landing between the sweep and the row removal wrote a value whose
/// row the delete then dropped: a stored credential `list` no longer shows.
/// The sweep and the row removal must be one `index.update`.
/// Red on the unfixed code: the racer's `set` lands, and the configured
/// backend keeps a value with no index row.
/// Test: itself.
#[test]
fn store_delete_across_holds_the_index_lock_through_the_sweep() {
    let (tmp, configured, store) = fixture();
    let racer_index =
        NamesIndex::at(tmp.path().join("index")).with_lock_timeout(std::time::Duration::ZERO);
    let racing = Arc::new(RacingBackend {
        inner: MemoryBackend::new(),
        racer: SecretStore::new(
            Arc::clone(&configured) as Arc<dyn SecretBackend>,
            racer_index,
        ),
        raced_in: std::sync::atomic::AtomicBool::new(false),
    });
    let others = [Arc::clone(&racing) as Arc<dyn SecretBackend>];
    store
        .set(&project(), &key("API_KEY"), &SecretValue::new(FAKE_VALUE))
        .unwrap();

    let deleted = store.delete_across(&project(), &key("API_KEY"), &others);
    assert!(deleted.unwrap().removed);
    assert!(
        !racing.raced_in.load(std::sync::atomic::Ordering::SeqCst),
        "a set landed between the sweep and the row removal"
    );
    let rows = store.list(&project()).unwrap().len();
    assert_eq!(
        (configured.len(), rows),
        (0, 0),
        "a backend holds a value no index row lists"
    );
}

/// Why: #7519 — `delete` sweeps every backend this build can write, and
/// none it cannot: off macOS every Keychain call fails closed, which would
/// fail every delete.
/// Test: itself.
#[test]
fn store_local_backends_follow_the_build() {
    let file: Vec<BackendId> = if cfg!(unix) {
        vec![BackendId::file()]
    } else {
        Vec::new()
    };
    let mut with_keychain = vec![BackendId::keychain()];
    with_keychain.extend(file.iter().cloned());
    assert_eq!(backend::local_backends_for(true), with_keychain);
    assert_eq!(backend::local_backends_for(false), file);
    assert_eq!(
        local_backends(),
        backend::local_backends_for(backend::KEYCHAIN_COMPILED)
    );
}

/// Why: DOC-74 §15.3 — a project key wins over an owner key with the same
/// name; explicit references read only the vault they name.
/// Test: itself.
#[test]
fn store_resolution_prefers_project_over_owner() {
    let (_tmp, _backend, store) = fixture();
    store
        .set(&owner(), &key("SHARED"), &SecretValue::new("owner-value"))
        .unwrap();
    store
        .set(
            &owner(),
            &key("ONLY_OWNER"),
            &SecretValue::new("owner-only"),
        )
        .unwrap();
    store
        .set(
            &project(),
            &key("SHARED"),
            &SecretValue::new("project-value"),
        )
        .unwrap();

    let read = |raw: &str| store.read(&SecretRef::parse(raw).unwrap(), &scopes());
    assert_eq!(read("secret://SHARED").unwrap().expose(), "project-value");
    assert_eq!(read("secret://ONLY_OWNER").unwrap().expose(), "owner-only");
    assert_eq!(
        read("secret://acme/SHARED").unwrap().expose(),
        "owner-value"
    );
    assert_eq!(
        read("secret://acme/web/SHARED").unwrap().expose(),
        "project-value"
    );
}

/// Why: a miss must be `NotFound`, never an empty value, and must name every
/// vault searched. An indexed key the backend lost is also `NotFound`.
/// Test: itself.
#[test]
fn store_resolution_miss_is_not_found() {
    let (_tmp, backend, store) = fixture();
    let err = store
        .read(&SecretRef::parse("secret://NOPE").unwrap(), &scopes())
        .unwrap_err();
    match err {
        SecretsError::NotFound { key, searched } => {
            assert_eq!(key, "NOPE");
            assert_eq!(searched, "trusty/acme/web, trusty/acme");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert!(matches!(
        store.read(
            &SecretRef::parse("secret://acme/web/NOPE").unwrap(),
            &scopes()
        ),
        Err(SecretsError::NotFound { .. })
    ));

    store
        .set(&project(), &key("LOST"), &SecretValue::new(FAKE_VALUE))
        .unwrap();
    backend.delete(&project(), &key("LOST")).unwrap();
    assert!(matches!(
        store.read(&SecretRef::parse("secret://LOST").unwrap(), &scopes()),
        Err(SecretsError::NotFound { .. })
    ));
}

/// Why: fail closed — a backend failure must propagate as a backend error on
/// every path, never read as a miss, and a refused write must not add an
/// index row.
/// Test: itself.
#[test]
fn store_backend_errors_are_never_downgraded() {
    let (_tmp, store) = fixture_with(Arc::new(FailingBackend));
    let err = store
        .set(&project(), &key("API_KEY"), &SecretValue::new(FAKE_VALUE))
        .unwrap_err();
    assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");
    assert!(
        store.list(&project()).unwrap().is_empty(),
        "no row on failure"
    );

    store
        .index()
        .upsert(&project(), &key("API_KEY"), 3, 1)
        .unwrap();
    let err = store
        .read(&SecretRef::parse("secret://API_KEY").unwrap(), &scopes())
        .unwrap_err();
    assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");

    let err = store.delete(&project(), &key("API_KEY")).unwrap_err();
    assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");
    assert_eq!(
        store.list(&project()).unwrap().len(),
        1,
        "row kept on failure"
    );
}

/// Why: a sync target is write-only (DOC-74 §15.4); reading it back must be
/// refused before the backend is called.
/// Test: itself.
#[test]
fn store_capabilities_gate_operations() {
    let sync = Capabilities::WRITE | Capabilities::SYNC_TARGET;
    assert!(sync.contains(Capabilities::WRITE));
    assert!(!sync.contains(Capabilities::READ));
    assert_eq!(format!("{sync:?}"), "Capabilities(WRITE | SYNC_TARGET)");

    let (_tmp, store) = fixture_with(Arc::new(MemoryBackend::with_capabilities(sync)));
    store
        .set(&project(), &key("API_KEY"), &SecretValue::new(FAKE_VALUE))
        .unwrap();
    let err = store
        .read(&SecretRef::parse("secret://API_KEY").unwrap(), &scopes())
        .unwrap_err();
    assert!(
        matches!(
            err,
            SecretsError::Unsupported {
                operation: "read",
                ..
            }
        ),
        "{err:?}"
    );
    let err = store.backend().list_names(&project()).unwrap_err();
    assert!(matches!(err, SecretsError::Unsupported { .. }), "{err:?}");

    let (_tmp, read_only) = fixture_with(Arc::new(MemoryBackend::with_capabilities(
        Capabilities::READ,
    )));
    let err = read_only
        .set(&project(), &key("API_KEY"), &SecretValue::new(FAKE_VALUE))
        .unwrap_err();
    assert!(matches!(err, SecretsError::Unsupported { .. }), "{err:?}");
}

/// Why: a configured backend this build does not implement must fail closed,
/// never fall back to the Keychain or to files.
/// Test: itself.
#[test]
fn store_open_backend_knows_keychain_and_file() {
    let keychain = open_backend(&BackendId::keychain()).unwrap();
    assert_eq!(keychain.id().as_str(), "keychain");
    // #9326: opening resolves `$HOME` only; no file is touched.
    #[cfg(unix)]
    {
        let file = open_backend(&BackendId::file()).unwrap();
        assert_eq!(file.id().as_str(), "file");
    }
    // #7519: with `cli-backends` it opens from the machine config under
    // `$HOME`, which no test reads; `server_backends_for_opens_onepassword_only_when_enabled`
    // covers that build.
    // #7519 P3: Keeper likewise.
    #[cfg(not(all(unix, feature = "cli-backends")))]
    for id in [BackendId::onepassword(), BackendId::keeper()] {
        let err = open_backend(&id).unwrap_err();
        assert!(
            matches!(err, SecretsError::UnknownBackend { .. }),
            "{err:?}"
        );
    }
    let err = open_backend(&BackendId::new("bitwarden").unwrap()).unwrap_err();
    assert!(
        matches!(err, SecretsError::UnknownBackend { .. }),
        "{err:?}"
    );
}

/// Why: #9326, owner ruling f5 — a Keychain error must surface as itself.
/// Falling through to the file backend would move secrets to plaintext
/// files with no one asking (Fail-Open Check).
/// Red when the `keychain` arm of `open_backend_from` falls back to `file`.
/// Test: itself.
#[test]
fn store_keychain_failure_never_falls_through_to_file() {
    let file_opened = std::cell::Cell::new(false);
    let result = backend::open_backend_from(
        &BackendId::keychain(),
        || {
            Err(SecretsError::Backend {
                backend: BackendId::KEYCHAIN.to_string(),
                vault: project().to_string(),
                key: "API_KEY".to_string(),
                reason: "secure storage is not accessible".to_string(),
            })
        },
        || {
            file_opened.set(true);
            Ok(Arc::new(MemoryBackend::new()) as Arc<dyn SecretBackend>)
        },
        || panic!("a Keychain id opened 1Password"),
        || panic!("a Keychain id opened Keeper"),
    );
    assert!(
        !file_opened.get(),
        "a failing Keychain opened the file backend"
    );
    match result {
        Err(SecretsError::Backend { backend, .. }) => assert_eq!(backend, "keychain"),
        Err(other) => panic!("the Keychain error was replaced: {other:?}"),
        Ok(opened) => panic!("a failing Keychain still opened {:?}", opened.id()),
    }
}

/// Why: #7519 — a failing 1Password open surfaces as itself; it never
/// reaches the Keychain or file opener, so a locked or disabled 1Password
/// never moves values to another backend.
/// Red when the `onepassword` arm falls to another opener or to
/// `UnknownBackend`.
/// Test: itself.
#[test]
fn store_onepassword_opens_only_through_its_own_opener() {
    let other_opened = std::cell::Cell::new(false);
    let result = backend::open_backend_from(
        &BackendId::onepassword(),
        || {
            other_opened.set(true);
            Ok(Arc::new(MemoryBackend::new()) as Arc<dyn SecretBackend>)
        },
        || {
            other_opened.set(true);
            Ok(Arc::new(MemoryBackend::new()) as Arc<dyn SecretBackend>)
        },
        || {
            Err(SecretsError::BackendLocked {
                backend: BackendId::ONEPASSWORD.to_string(),
                hint: "unlock it",
            })
        },
        || panic!("a 1Password id opened Keeper"),
    );
    assert!(!other_opened.get(), "1Password opened another backend");
    assert!(
        matches!(result, Err(SecretsError::BackendLocked { .. })),
        "{:?}",
        result.map(|b| b.id())
    );
}

/// Why: #7519 P3 — a failing Keeper open surfaces as itself; it never
/// reaches another opener, so a locked or disabled Keeper never moves
/// values to another backend. Red when the `keeper` arm falls to another
/// opener or to `UnknownBackend`.
/// Test: itself.
#[test]
fn store_keeper_opens_only_through_its_own_opener() {
    let other_opened = std::cell::Cell::new(false);
    let other = || {
        other_opened.set(true);
        Ok(Arc::new(MemoryBackend::new()) as Arc<dyn SecretBackend>)
    };
    let result = backend::open_backend_from(&BackendId::keeper(), other, other, other, || {
        Err(SecretsError::BackendNotEnabled {
            backend: BackendId::KEEPER.to_string(),
        })
    });
    assert!(!other_opened.get(), "Keeper opened another backend");
    assert!(
        matches!(result, Err(SecretsError::BackendNotEnabled { .. })),
        "{:?}",
        result.map(|b| b.id())
    );
}

/// Why: #9326 AC3, owner ruling f5 — the Keychain is the default wherever
/// one is compiled in; `file` is the default only where none is. An
/// explicit `keychain` stays `keychain` on every host.
/// Test: itself.
#[test]
fn store_default_backend_is_keychain_unless_none_is_compiled() {
    assert_eq!(backend::default_backend_for(true), BackendId::keychain());
    assert_eq!(backend::default_backend_for(false), BackendId::file());
    assert_eq!(config::resolve(None, None).backend, default_backend());
    #[cfg(target_os = "macos")]
    assert_eq!(default_backend(), BackendId::keychain());
    #[cfg(not(target_os = "macos"))]
    assert_eq!(default_backend(), BackendId::file());

    let explicit = config::ProjectSecretsConfig {
        backend: Some(BackendId::keychain()),
        ..config::ProjectSecretsConfig::default()
    };
    assert_eq!(
        config::resolve(Some(&explicit), None).backend,
        BackendId::keychain()
    );
}

/// Why: QA regression class from PR #2427 — `{:?}` of anything holding a
/// value must not render it.
/// Test: itself.
#[test]
fn store_debug_never_contains_a_value() {
    let (_tmp, backend, store) = fixture();
    store
        .set(&project(), &key("API_KEY"), &SecretValue::new(FAKE_VALUE))
        .unwrap();
    let shown = format!(
        "{store:?} {backend:?} {:?}",
        backend.get(&project(), &key("API_KEY"))
    );
    assert!(!shown.contains(FAKE_VALUE), "{shown}");
}

/// Why: owner ruling 2026-10-01 — ≤ 8 characters shows only the length;
/// longer shows the first 8 plus the length. Characters, not bytes.
/// Test: itself.
#[test]
fn mask_secret_table() {
    assert_eq!(mask_secret(""), "[0 chars]");
    assert_eq!(mask_secret("12345678"), "[8 chars]");
    assert_eq!(mask_secret("123456789"), "12345678… [9 chars]");
    assert_eq!(mask_secret("ééééééééé"), "éééééééé… [9 chars]");
    assert!(!mask_secret("12345678").contains('1'));
}

/// A store over a fresh memory backend whose index waits `lock_ms` at most.
fn fixture_with_lock_timeout(lock_ms: u64) -> (TempDir, Arc<MemoryBackend>, SecretStore) {
    let tmp = TempDir::new().unwrap();
    let backend = Arc::new(MemoryBackend::new());
    let index = NamesIndex::at(tmp.path().join("index"))
        .with_lock_timeout(std::time::Duration::from_millis(lock_ms));
    let store = SecretStore::new(Arc::clone(&backend) as Arc<dyn SecretBackend>, index);
    (tmp, backend, store)
}

/// Why: #9064 — `set` used to write the backend before reading or locking
/// the index, so a corrupt or locked index left an orphaned Keychain entry
/// while the caller was told the set failed. Both index failures must now
/// surface before the backend is touched.
/// Test: itself.
#[test]
fn store_set_fails_closed_on_the_index_before_the_backend_write() {
    let (_tmp, backend, store) = fixture_with_lock_timeout(60);
    let index_file = store.index().path_for(&project());
    std::fs::create_dir_all(store.index().root()).unwrap();
    std::fs::write(&index_file, "{ not json").unwrap();
    let err = store
        .set(&project(), &key("API_KEY"), &SecretValue::new(FAKE_VALUE))
        .unwrap_err();
    assert!(matches!(err, SecretsError::IndexCorrupt { .. }), "{err:?}");
    assert_eq!(
        backend.len(),
        0,
        "a corrupt index must stop the backend write"
    );

    std::fs::remove_file(&index_file).unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(platform::lock_path(&index_file))
        .unwrap();
    let mut lock = fd_lock::RwLock::new(held);
    let _guard = lock.try_write().unwrap();
    let err = store
        .set(&project(), &key("API_KEY"), &SecretValue::new(FAKE_VALUE))
        .unwrap_err();
    assert!(matches!(err, SecretsError::LockTimeout { .. }), "{err:?}");
    assert_eq!(backend.len(), 0, "a held lock must stop the backend write");
}

/// Why: #9064 — if the index publish fails after a NEW key reached the
/// backend, the backend entry is removed again so nothing is orphaned.
/// Test: itself.
#[cfg(unix)]
#[test]
fn store_set_compensates_a_new_key_when_the_index_publish_fails() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, backend, store) = fixture();
    let value = SecretValue::new(FAKE_VALUE);
    store.set(&project(), &key("FIRST"), &value).unwrap();

    // 0500: the lock sidecar opens, but the scratch file cannot be created.
    let root = store.index().root().to_path_buf();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = store.set(&project(), &key("SECOND"), &value);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(matches!(result, Err(SecretsError::Io { .. })), "{result:?}");
    assert_eq!(backend.len(), 1, "the new key must be deleted again");
    assert!(backend.get(&project(), &key("SECOND")).unwrap().is_none());
    assert!(backend.get(&project(), &key("FIRST")).unwrap().is_some());
}

/// A memory backend whose `delete` always fails, so a compensation fails.
#[derive(Debug, Default)]
struct UndeletableBackend(MemoryBackend);

impl SecretBackend for UndeletableBackend {
    fn id(&self) -> BackendId {
        BackendId::new("undeletable").unwrap()
    }
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }
    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        self.0.get(vault, key)
    }
    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        self.0.set(vault, key, value)
    }
    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        Err(FailingBackend::failure(vault, key))
    }
}

/// Why: #9064 — when the index publish fails and the compensating delete
/// fails too, the orphaned backend entry must be reported with its vault and
/// key, never swallowed, and the report must not carry the value.
/// Test: itself.
#[cfg(unix)]
#[test]
fn store_set_reports_an_orphan_when_compensation_fails() {
    use std::os::unix::fs::PermissionsExt;
    let backend = Arc::new(UndeletableBackend::default());
    let (_tmp, store) = fixture_with(Arc::clone(&backend) as Arc<dyn SecretBackend>);
    let value = SecretValue::new(FAKE_VALUE);
    store.set(&project(), &key("FIRST"), &value).unwrap();

    // 0500: the lock sidecar opens, but the scratch file cannot be created.
    let root = store.index().root().to_path_buf();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = store.set(&project(), &key("SECOND"), &value);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();

    let err = result.unwrap_err();
    let shown = format!("{err} / {err:?}");
    match &err {
        SecretsError::OrphanedBackendEntry {
            vault, key, source, ..
        } => {
            assert_eq!(vault, "trusty/acme/web");
            assert_eq!(key, "SECOND");
            assert!(matches!(**source, SecretsError::Io { .. }), "{source:?}");
        }
        other => panic!("expected OrphanedBackendEntry, got {other:?}"),
    }
    assert!(shown.contains("no index row"), "{shown}");
    assert!(
        !shown.contains(FAKE_VALUE),
        "the report leaked the value: {shown}"
    );
    assert!(backend.0.get(&project(), &key("SECOND")).unwrap().is_some());
}
