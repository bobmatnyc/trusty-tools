//! `secrets.delete` across every backend that may hold a key (#7519 A5).
//!
//! A child of `server_tests`, so it shares that module's fixture: every path
//! is under a `TempDir`, `keychain` is an in-memory double, and `file` is a
//! value-file backend in the temp dir or a double that refuses deletes. The
//! backend switch goes through the untracked machine config, the one place
//! #9326 lets select `file` on every host.
//!
//! Test: itself.

use super::*;
use crate::store::Capabilities;

/// Point the machine config at `backend`; the server rereads it per request.
fn select_backend(fx: &Fixture, backend: &str) {
    std::fs::write(
        &fx.settings.machine_config,
        format!("secrets:\n  default_backend: {backend}\n"),
    )
    .unwrap();
}

/// `{"project", "vault": "trusty/acme/web", "key"}` for `fx`.
fn target(fx: &Fixture, name: &str) -> Value {
    json!({"project": fx.project(), "vault": "trusty/acme/web", "key": name})
}

/// Set `name` to [`VALUE`] through the socket, under the selected backend.
async fn set_value(fx: &Fixture, name: &str) {
    let mut params = target(fx, name);
    params["value"] = json!(VALUE);
    ok(call(&fx.settings.socket, method::SET, params).await);
}

/// The key names `secrets.list` reports for the project vault.
async fn listed(fx: &Fixture) -> Value {
    let list = ok(call(
        &fx.settings.socket,
        method::LIST,
        json!({"project": fx.project(), "vault": "trusty/acme/web"}),
    )
    .await);
    list["keys"].clone()
}

/// A `file` backend whose `delete` fails: storage that refuses the removal.
///
/// What: reads and writes go to an in-memory map; every `delete` is a
/// [`SecretsError::Backend`], never a miss.
#[derive(Debug, Default)]
struct DeleteRefusedBackend {
    inner: MemoryBackend,
}

impl SecretBackend for DeleteRefusedBackend {
    fn id(&self) -> BackendId {
        BackendId::file()
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
        Err(SecretsError::Backend {
            backend: BackendId::FILE.to_string(),
            vault: vault.to_string(),
            key: key.to_string(),
            reason: "delete refused".to_string(),
        })
    }
}

/// A factory for `fx` that maps `file` to `backend`.
fn with_file_double(fx: &Fixture, backend: Arc<dyn SecretBackend>) -> BackendFactory {
    let base = fx.backends();
    Arc::new(move |id: &BackendId| match id.as_str() {
        BackendId::FILE => Ok(Arc::clone(&backend)),
        _ => base(id),
    })
}

/// Why: #7519 A5 — `delete` reached only the configured backend, so a key
/// set before a backend switch stayed in the old backend: a credential left
/// behind while the answer said it was removed.
/// Red on the unfixed code: the `file` entry survives the delete.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_delete_after_a_backend_switch_clears_the_old_backend() {
    let fx = fixture();
    select_backend(&fx, "file");
    let (factory, file) = with_file_backend(&fx);
    let server = fx.start_with(factory).await;
    let project = vault("trusty/acme/web");
    set_value(&fx, "A").await;
    assert!(file.get(&project, &key("A")).unwrap().is_some());

    select_backend(&fx, "keychain");
    let deleted = call(&fx.settings.socket, method::DELETE, target(&fx, "A")).await;
    assert!(!wire(&deleted).contains(VALUE));
    assert_eq!(ok(deleted), json!({"removed": true}));
    assert!(
        file.get(&project, &key("A")).unwrap().is_none(),
        "the old backend still holds the key"
    );
    assert!(fx.keychain.is_empty());
    assert_eq!(listed(&fx).await, json!([]));
    server.stop().await;
}

/// Why: #7519 A5 — a key `copy` placed in two backends is removed from both,
/// whichever one the project is configured for.
/// Red on the unfixed code: the `file` copy survives.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_delete_removes_a_key_held_by_two_backends() {
    let fx = fixture();
    let (factory, file) = with_file_backend(&fx);
    let server = fx.start_with(factory).await;
    let project = vault("trusty/acme/web");
    set_value(&fx, "A").await;
    let copied = ok(call(
        &fx.settings.socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain", "to_backend": "file",
               "keys": ["A"]}),
    )
    .await);
    assert_eq!(copied, json!({"copied": ["A"], "failed": []}));
    assert_eq!(fx.keychain.len(), 1);
    assert!(file.get(&project, &key("A")).unwrap().is_some());

    let deleted = ok(call(&fx.settings.socket, method::DELETE, target(&fx, "A")).await);
    assert_eq!(deleted, json!({"removed": true}));
    assert!(fx.keychain.is_empty(), "the configured backend is cleared");
    assert!(
        file.get(&project, &key("A")).unwrap().is_none(),
        "the second backend still holds the key"
    );
    assert_eq!(listed(&fx).await, json!([]));
    server.stop().await;
}

/// Why: #7519, Fail-Open Check — a backend that fails to delete may still
/// hold the value, so the call is an error, never success, and the index
/// row stays so `list` still shows the key. The error carries no value.
/// Red on the unfixed code: the delete answers `removed: true` and drops
/// the row while the old backend keeps the value.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_delete_failure_in_an_old_backend_is_an_error_and_keeps_the_row() {
    let fx = fixture();
    select_backend(&fx, "file");
    let refused = Arc::new(DeleteRefusedBackend::default());
    let factory = with_file_double(&fx, Arc::clone(&refused) as Arc<dyn SecretBackend>);
    let server = fx.start_with(factory).await;
    let project = vault("trusty/acme/web");
    set_value(&fx, "A").await;

    select_backend(&fx, "keychain");
    let deleted = call(&fx.settings.socket, method::DELETE, target(&fx, "A")).await;
    let text = wire(&deleted);
    assert!(!text.contains(VALUE), "{text}");
    assert_eq!(
        fixed_error(&deleted, method::DELETE),
        ErrorKind::BackendFailed
    );
    assert!(refused.inner.get(&project, &key("A")).unwrap().is_some());
    let keys = listed(&fx).await;
    assert_eq!(keys[0]["name"], "A", "the row stays while a value remains");
    server.stop().await;
}
