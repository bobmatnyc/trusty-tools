//! Server tests over real Unix sockets in temp dirs, against `MemoryBackend`.
//!
//! No test here touches `~/.trusty-tools` or the OS keychain: every path is
//! under a `TempDir`, and the backend factory maps `keychain` and `spare` to
//! two in-memory backends.
//!
//! Test: itself.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use trusty_common::uds::server::{RpcResponse, ServeExit};
use trusty_common::uds::{UdsSecurityError, send_framed_request, socket_is_serving};

use super::*;
use crate::api::methods::method;
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};
use crate::store::{MemoryBackend, NamesIndex, SecretBackend, mask_secret};

const VALUE: &str = "sk-fake-server-0123456789abcdef";
const SENTINEL: &str = "SENTINEL-c0ffee-9065";
const SENTINEL_NUMBER: u64 = 42_424_242_429_065;

struct Fixture {
    tmp: TempDir,
    settings: ServerSettings,
    keychain: Arc<MemoryBackend>,
    spare: Arc<MemoryBackend>,
    repo: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn fixture() -> Fixture {
    fixture_with_idle(Duration::from_secs(60))
}

fn fixture_with_idle(idle_timeout: Duration) -> Fixture {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &["remote", "add", "origin", "git@github.com:Acme/Web.git"],
    );
    let settings = ServerSettings {
        socket: tmp.path().join("run").join("s.sock"),
        index_root: tmp.path().join("index"),
        machine_config: tmp.path().join("machine.yaml"),
        idle_timeout,
    };
    Fixture {
        tmp,
        settings,
        keychain: Arc::new(MemoryBackend::new()),
        spare: Arc::new(MemoryBackend::new()),
        repo,
    }
}

impl Fixture {
    fn backends(&self) -> BackendFactory {
        let keychain = Arc::clone(&self.keychain);
        let spare = Arc::clone(&self.spare);
        Arc::new(move |id: &BackendId| {
            let backend: Arc<dyn SecretBackend> = match id.as_str() {
                "keychain" => keychain.clone(),
                "spare" => spare.clone(),
                _ => {
                    return Err(SecretsError::UnknownBackend {
                        backend: id.to_string(),
                    });
                }
            };
            Ok(backend)
        })
    }

    fn project(&self) -> String {
        self.repo.display().to_string()
    }

    /// Start a server; returns its task and the shutdown trigger.
    async fn start(&self) -> Running {
        self.start_with(self.backends()).await
    }

    /// Start a server whose factory also maps `faulty` to `backend`.
    async fn start_with_faulty(&self, backend: Arc<dyn SecretBackend>) -> Running {
        let base = self.backends();
        let factory: BackendFactory = Arc::new(move |id: &BackendId| match id.as_str() {
            "faulty" => Ok(Arc::clone(&backend)),
            _ => base(id),
        });
        self.start_with(factory).await
    }

    async fn start_with(&self, backends: BackendFactory) -> Running {
        let (tx, rx) = oneshot::channel::<()>();
        let task = tokio::spawn(serve(self.settings.clone(), backends, async move {
            let _ = rx.await;
        }));
        wait_serving(&self.settings.socket).await;
        Running {
            task,
            shutdown: Some(tx),
        }
    }
}

struct Running {
    task: JoinHandle<Result<ServeExit, ServeError>>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl Running {
    async fn stop(mut self) -> ServeExit {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.task.await.unwrap().unwrap()
    }
}

async fn wait_serving(socket: &Path) {
    for _ in 0..200 {
        if socket_is_serving(socket, Duration::from_millis(200)).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("socket {} never served", socket.display());
}

async fn call(socket: &Path, method: &str, params: Value) -> RpcResponse {
    let request = json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params});
    send_framed_request(socket, &request, Duration::from_secs(10))
        .await
        .unwrap()
}

fn ok(response: RpcResponse) -> Value {
    assert!(response.error.is_none(), "{:?}", response.error);
    response.result.unwrap()
}

/// The error kind and message, after checking the message is the fixed text.
fn fixed_error(response: &RpcResponse, method: &'static str) -> ErrorKind {
    let error = response.error.as_ref().expect("an error response");
    let kind = error.data.as_ref().unwrap()["kind"].as_str().unwrap();
    let kind = ALL_KINDS
        .into_iter()
        .find(|k| k.as_str() == kind)
        .unwrap_or_else(|| panic!("unknown kind {kind}"));
    assert_eq!(error.message, format!("{method}: {}", kind.text()));
    assert_eq!(error.code, kind.code());
    kind
}

const ALL_KINDS: [ErrorKind; 21] = [
    ErrorKind::InvalidParams,
    ErrorKind::ProjectInvalid,
    ErrorKind::ProjectUnresolved,
    ErrorKind::VaultOutOfScope,
    ErrorKind::InvalidValue,
    ErrorKind::NotFound,
    ErrorKind::Unsupported,
    ErrorKind::UnknownBackend,
    ErrorKind::BackendFailed,
    ErrorKind::OrphanedBackendEntry,
    ErrorKind::IndexCorrupt,
    ErrorKind::IndexBusy,
    ErrorKind::StorageUnavailable,
    ErrorKind::ConfigInvalid,
    ErrorKind::HomeUnavailable,
    ErrorKind::SameBackend,
    ErrorKind::AgentUseRefused,
    ErrorKind::InvalidEnvEntry,
    ErrorKind::EnvResolutionFailed,
    ErrorKind::DotenvSyntax,
    ErrorKind::Internal,
];

fn wire(response: &RpcResponse) -> String {
    serde_json::to_string(response).unwrap()
}

fn vault(name: &str) -> VaultName {
    VaultName::new(name).unwrap()
}

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).unwrap()
}

/// Why: `secrets.scopes` derives the project and owner vaults from the
/// project's git remote, project first.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_scopes_round_trip_over_a_real_socket() {
    let fx = fixture();
    let server = fx.start().await;
    let result = ok(call(
        &fx.settings.socket,
        method::SCOPES,
        json!({"project": fx.project()}),
    )
    .await);
    assert_eq!(
        result,
        json!({"scopes": [
            {"kind": "project", "vault": "trusty/acme/web"},
            {"kind": "owner", "vault": "trusty/acme"},
        ]})
    );
    // A subdirectory of the checkout names the same project.
    let sub = fx.repo.join("src");
    std::fs::create_dir(&sub).unwrap();
    let nested = ok(call(
        &fx.settings.socket,
        method::SCOPES,
        json!({"project": sub.display().to_string()}),
    )
    .await);
    assert_eq!(nested, result);
    server.stop().await;
}

/// Why: set answers only the masked confirmation; list answers metadata;
/// delete removes from backend and index. No response carries the value.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_set_list_delete_round_trip_over_a_real_socket() {
    let fx = fixture();
    let server = fx.start().await;
    let socket = &fx.settings.socket;
    let target = json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "API_KEY"});

    let mut set_params = target.clone();
    set_params["value"] = json!(VALUE);
    let set = call(socket, method::SET, set_params).await;
    assert!(!wire(&set).contains(VALUE));
    let set = ok(set);
    assert_eq!(set, json!({"outcome": "new", "masked": mask_secret(VALUE)}));
    let stored = fx
        .keychain
        .get(&vault("trusty/acme/web"), &key("API_KEY"))
        .unwrap()
        .unwrap();
    assert_eq!(stored.expose(), VALUE);

    let list = call(
        socket,
        method::LIST,
        json!({"project": fx.project(), "vault": "trusty/acme/web"}),
    )
    .await;
    assert!(
        !wire(&list).contains(&VALUE[..8]),
        "list shows no characters"
    );
    let list = ok(list);
    assert_eq!(list["vault"], "trusty/acme/web");
    assert_eq!(list["keys"][0]["name"], "API_KEY");
    assert_eq!(list["keys"][0]["length"], VALUE.chars().count());
    assert_eq!(list["keys"][0]["agents_may_use"], false);
    assert!(list["keys"][0]["updated_at"].as_u64().unwrap() > 0);

    let deleted = ok(call(socket, method::DELETE, target).await);
    assert_eq!(deleted, json!({"removed": true}));
    assert!(fx.keychain.is_empty());
    let list = ok(call(
        socket,
        method::LIST,
        json!({"project": fx.project(), "vault": "trusty/acme"}),
    )
    .await);
    assert_eq!(list["keys"], json!([]));
    server.stop().await;
}

/// Why: DOC-74 §13 Q6 — copy moves this project's keys between its
/// backends, never across projects, and never returns a value.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_moves_keys_between_backends_in_one_project() {
    let fx = fixture();
    let server = fx.start().await;
    let socket = &fx.settings.socket;
    ok(call(
        socket,
        method::SET,
        json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "A", "value": VALUE}),
    )
    .await);
    // A key held by another project's vault in the same backend stays put.
    fx.keychain
        .set(
            &vault("trusty/other/app"),
            &key("B"),
            &SecretValue::new(VALUE),
        )
        .unwrap();

    let all = call(
        socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain", "to_backend": "spare"}),
    )
    .await;
    assert!(!wire(&all).contains(VALUE));
    assert_eq!(ok(all), json!({"copied": ["A"], "failed": []}));
    assert_eq!(fx.spare.len(), 1, "only the project's own key moved");
    let copied = fx
        .spare
        .get(&vault("trusty/acme/web"), &key("A"))
        .unwrap()
        .unwrap();
    assert_eq!(copied.expose(), VALUE);

    let named = ok(call(
        socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain", "to_backend": "spare",
               "keys": ["A", "B"]}),
    )
    .await);
    assert_eq!(named, json!({"copied": ["A"], "failed": ["B"]}));
    server.stop().await;
}

/// Why: a copy onto its own source is refused before any key moves, and an
/// unknown backend is a fixed error.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_refuses_the_same_backend_twice() {
    let fx = fixture();
    let server = fx.start().await;
    let same = call(
        &fx.settings.socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain", "to_backend": "keychain"}),
    )
    .await;
    assert_eq!(fixed_error(&same, method::COPY), ErrorKind::SameBackend);
    let unknown = call(
        &fx.settings.socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain", "to_backend": "vercel"}),
    )
    .await;
    assert_eq!(
        fixed_error(&unknown, method::COPY),
        ErrorKind::UnknownBackend
    );
    server.stop().await;
}

/// A memory backend that makes the index publish fail after it stores one
/// key, by setting the index root to 0500.
///
/// Why: #9065 — copy's index publish can fail after the destination write.
/// At 0500 the lock sidecar still opens, but the publish's scratch file
/// cannot be created: the fault `store_tests.rs` uses for `set`.
/// What: `set` of `poison` stores the value, then sets the root to 0500.
/// `delete` sets the root back to 0700, then deletes, or fails when
/// `undeletable`.
#[cfg(unix)]
#[derive(Debug)]
struct PublishFaultBackend {
    inner: MemoryBackend,
    index_root: PathBuf,
    poison: SecretKey,
    undeletable: bool,
}

#[cfg(unix)]
impl PublishFaultBackend {
    fn new(fx: &Fixture, poison: &str, undeletable: bool) -> Arc<Self> {
        Arc::new(Self {
            inner: MemoryBackend::new(),
            index_root: fx.settings.index_root.clone(),
            poison: key(poison),
            undeletable,
        })
    }

    fn holds(&self, name: &str) -> bool {
        self.inner
            .get(&vault("trusty/acme/web"), &key(name))
            .unwrap()
            .is_some_and(|stored| stored.expose() == VALUE)
    }
}

#[cfg(unix)]
fn set_mode(dir: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Sets the index root back to 0700 on drop, so a failed test still cleans up.
#[cfg(unix)]
struct RestoreMode(PathBuf);

#[cfg(unix)]
impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
    }
}

#[cfg(unix)]
impl SecretBackend for PublishFaultBackend {
    fn id(&self) -> BackendId {
        BackendId::new("faulty").unwrap()
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
        self.inner.set(vault, key, value)?;
        if *key == self.poison {
            set_mode(&self.index_root, 0o500);
        }
        Ok(())
    }
    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        set_mode(&self.index_root, 0o700);
        if self.undeletable {
            return Err(SecretsError::Backend {
                backend: "faulty".into(),
                vault: vault.to_string(),
                key: key.to_string(),
                reason: "delete refused".into(),
            });
        }
        self.inner.delete(vault, key)
    }
}

/// Put `names` in the source backend under the project vault, unindexed.
#[cfg(unix)]
fn seed_source(fx: &Fixture, names: &[&str]) {
    for name in names {
        fx.keychain
            .set(
                &vault("trusty/acme/web"),
                &key(name),
                &SecretValue::new(VALUE),
            )
            .unwrap();
    }
}

/// Why: #9065 — copy wrote the destination before the index and never
/// undid it, so a failed publish left a destination entry no index row
/// lists. Copy now writes through `SecretStore::set`, whose compensation
/// deletes the new entry; the key is `failed` and later keys still copy.
/// Test: itself.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_compensates_a_key_whose_index_publish_fails() {
    let fx = fixture();
    let _restore = RestoreMode(fx.settings.index_root.clone());
    let faulty = PublishFaultBackend::new(&fx, "POISON", false);
    let server = fx
        .start_with_faulty(Arc::clone(&faulty) as Arc<dyn SecretBackend>)
        .await;
    seed_source(&fx, &["GOOD1", "POISON", "GOOD2"]);

    let response = call(
        &fx.settings.socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain", "to_backend": "faulty",
               "keys": ["GOOD1", "POISON", "GOOD2"]}),
    )
    .await;
    assert!(!wire(&response).contains(VALUE));
    assert_eq!(
        ok(response),
        json!({"copied": ["GOOD1", "GOOD2"], "failed": ["POISON"]})
    );
    assert!(
        !faulty.holds("POISON"),
        "compensation must delete the entry"
    );
    assert!(faulty.holds("GOOD1") && faulty.holds("GOOD2"));
    let list = ok(call(
        &fx.settings.socket,
        method::LIST,
        json!({"project": fx.project(), "vault": "trusty/acme/web"}),
    )
    .await);
    let names: Vec<&str> = list["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["GOOD1", "GOOD2"]);
    server.stop().await;
}

/// Why: #9065 — when the publish fails and compensation fails too, the
/// destination holds an entry no index row lists. Folding that into
/// `failed` hid it; copy now aborts with `orphaned_backend_entry` and no
/// copied list. Keys copied before it stay visible through `secrets.list`.
/// Test: itself.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_aborts_with_orphaned_backend_entry_when_compensation_fails() {
    let fx = fixture();
    let _restore = RestoreMode(fx.settings.index_root.clone());
    let faulty = PublishFaultBackend::new(&fx, "POISON", true);
    let server = fx
        .start_with_faulty(Arc::clone(&faulty) as Arc<dyn SecretBackend>)
        .await;
    seed_source(&fx, &["GOOD1", "POISON", "GOOD2"]);

    let response = call(
        &fx.settings.socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain", "to_backend": "faulty",
               "keys": ["GOOD1", "POISON", "GOOD2"]}),
    )
    .await;
    assert!(!wire(&response).contains(VALUE));
    assert!(
        response.result.is_none(),
        "an orphan returns no copied list"
    );
    assert_eq!(
        fixed_error(&response, method::COPY),
        ErrorKind::OrphanedBackendEntry
    );
    assert!(
        faulty.holds("POISON"),
        "the orphan the error reports is real"
    );
    assert!(!faulty.holds("GOOD2"), "the copy stops at the orphan");
    let list = ok(call(
        &fx.settings.socket,
        method::LIST,
        json!({"project": fx.project(), "vault": "trusty/acme/web"}),
    )
    .await);
    assert_eq!(list["keys"].as_array().unwrap().len(), 1);
    assert_eq!(list["keys"][0]["name"], "GOOD1");
    server.stop().await;
}

/// Why: doctor reports availability and paths only.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_doctor_reports_backends_and_paths_only() {
    let fx = fixture();
    let server = fx.start().await;
    let bare: DoctorResponse =
        serde_json::from_value(ok(call(&fx.settings.socket, DOCTOR, Value::Null).await)).unwrap();
    assert_eq!(bare.socket, fx.settings.socket);
    assert_eq!(bare.index_root, fx.settings.index_root);
    assert_eq!(bare.project_config, None);
    assert_eq!(bare.selected_backend, BackendId::keychain());
    assert_eq!(
        bare.backends,
        [BackendStatus {
            id: BackendId::keychain(),
            available: true,
            capabilities: vec!["READ".to_string(), "WRITE".to_string()],
        }]
    );

    let with_project: DoctorResponse = serde_json::from_value(ok(call(
        &fx.settings.socket,
        DOCTOR,
        json!({"project": fx.project()}),
    )
    .await))
    .unwrap();
    let root = with_project.project_root.unwrap();
    assert_eq!(
        with_project.project_config,
        Some(root.join(PROJECT_CONFIG_SUBPATH))
    );
    server.stop().await;
}

/// Why: DOC-74 §6.1 — the project's tracked config may name a backend and a
/// vault override; both take effect.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_project_config_overrides_the_project_vault() {
    let fx = fixture();
    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        "secrets:\n  backend: spare\n  vault: trusty/acme/shared\n",
    )
    .unwrap();
    let server = fx.start().await;
    let scopes = ok(call(
        &fx.settings.socket,
        method::SCOPES,
        json!({"project": fx.project()}),
    )
    .await);
    assert_eq!(scopes["scopes"][0]["vault"], "trusty/acme/shared");
    ok(call(
        &fx.settings.socket,
        method::SET,
        json!({"project": fx.project(), "vault": "trusty/acme/shared", "key": "K", "value": VALUE}),
    )
    .await);
    assert_eq!((fx.keychain.len(), fx.spare.len()), (0, 1));

    std::fs::write(&config, "secrets: [not, a, mapping]\n").unwrap();
    let broken = call(
        &fx.settings.socket,
        method::SCOPES,
        json!({"project": fx.project()}),
    )
    .await;
    assert_eq!(
        fixed_error(&broken, method::SCOPES),
        ErrorKind::ConfigInvalid
    );
    server.stop().await;
}

/// Why: the project path decides the scopes, so a vault outside them —
/// another repository's — is refused and nothing is written.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_vault_outside_the_project_is_refused() {
    let fx = fixture();
    let server = fx.start().await;
    for (name, params) in [
        (
            method::SET,
            json!({"project": fx.project(), "vault": "trusty/acme/other", "key": "K", "value": VALUE}),
        ),
        (
            method::LIST,
            json!({"project": fx.project(), "vault": "trusty/elsewhere"}),
        ),
        (
            method::DELETE,
            json!({"project": fx.project(), "vault": "trusty/acme/other", "key": "K"}),
        ),
    ] {
        let response = call(&fx.settings.socket, name, params).await;
        assert_eq!(fixed_error(&response, name), ErrorKind::VaultOutOfScope);
    }
    assert!(fx.keychain.is_empty());
    server.stop().await;
}

/// Why: a checkout with no `origin` and no override, or a directory outside
/// any checkout, has no scopes — a fixed error, never a guessed vault.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_project_without_a_remote_is_a_fixed_error() {
    let fx = fixture();
    let bare_repo = fx.tmp.path().join("bare");
    std::fs::create_dir(&bare_repo).unwrap();
    git(&bare_repo, &["init", "-q"]);
    let outside = fx.tmp.path().join("plain");
    std::fs::create_dir(&outside).unwrap();
    let server = fx.start().await;
    for dir in [&bare_repo, &outside] {
        let response = call(
            &fx.settings.socket,
            method::SCOPES,
            json!({"project": dir.display().to_string()}),
        )
        .await;
        assert_eq!(
            fixed_error(&response, method::SCOPES),
            ErrorKind::ProjectUnresolved
        );
        assert!(!wire(&response).contains(&dir.display().to_string()));
    }
    server.stop().await;
}

/// Why: `project` must be an absolute directory; a relative path would
/// resolve against the server's own working directory.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_project_path_must_be_an_absolute_directory() {
    let fx = fixture();
    let file = fx.tmp.path().join("file.txt");
    std::fs::write(&file, "x").unwrap();
    let server = fx.start().await;
    for project in ["repo".to_string(), file.display().to_string()] {
        let response = call(
            &fx.settings.socket,
            method::SCOPES,
            json!({ "project": project }),
        )
        .await;
        assert_eq!(
            fixed_error(&response, method::SCOPES),
            ErrorKind::ProjectInvalid
        );
    }
    server.stop().await;
}

/// Why: DOC-74 §15.6 — every failure is fixed text per method and kind,
/// never the router's `params do not decode: {e}`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_error_text_is_fixed_per_method_and_kind() {
    let codes: std::collections::BTreeSet<i64> = ALL_KINDS.iter().map(|k| k.code()).collect();
    let names: std::collections::BTreeSet<&str> = ALL_KINDS.iter().map(|k| k.as_str()).collect();
    assert_eq!(
        (codes.len(), names.len()),
        (ALL_KINDS.len(), ALL_KINDS.len())
    );

    let fx = fixture();
    let server = fx.start().await;
    for (name, _) in router::METHODS {
        let response = call(&fx.settings.socket, name, json!(SENTINEL)).await;
        assert_eq!(fixed_error(&response, name), ErrorKind::InvalidParams);
        let text = wire(&response);
        assert!(!text.contains(SENTINEL), "{name}: {text}");
        assert!(!text.contains("decode"), "{name}: {text}");
    }
    server.stop().await;
}

/// Why: a key left in the backend with no index row needs reconciling, so it
/// must not read as success, `NotFound`, or a plain backend failure, and its
/// key, vault and causes must stay off the wire (#9065).
/// Test: itself.
#[test]
fn server_orphaned_backend_entry_has_its_own_wire_kind() {
    let cause = |reason: &str| {
        Box::new(SecretsError::Backend {
            backend: SENTINEL.into(),
            vault: SENTINEL.into(),
            key: SENTINEL.into(),
            reason: reason.into(),
        })
    };
    let orphan = SecretsError::OrphanedBackendEntry {
        backend: SENTINEL.into(),
        vault: SENTINEL.into(),
        key: SENTINEL.into(),
        source: cause(SENTINEL),
        cleanup: cause(SENTINEL),
    };
    let kind = ErrorKind::from(orphan);
    assert_eq!(kind, ErrorKind::OrphanedBackendEntry);
    for other in [ErrorKind::NotFound, ErrorKind::BackendFailed] {
        assert_ne!(kind.code(), other.code());
        assert_ne!(kind.as_str(), other.as_str());
    }

    let sent = RpcResponse::failure(json!(1), kind.to_rpc(method::SET));
    let text = wire(&sent);
    assert!(!text.contains(SENTINEL), "{text}");
    let received: RpcResponse = serde_json::from_str(&text).unwrap();
    assert!(received.result.is_none(), "{text}");
    assert_eq!(
        fixed_error(&received, method::SET),
        ErrorKind::OrphanedBackendEntry
    );
}

/// Why: the #7525 resolver errors each need a kind of their own, an
/// agent-parent refusal must read as a refusal even when `resolve_env` wraps
/// it, and no key, vault, name or reference may reach the wire.
/// Test: itself.
#[test]
fn server_resolver_errors_have_their_own_wire_kinds() {
    let refused = || SecretsError::AgentUseRefused {
        key: SENTINEL.into(),
        vault: SENTINEL.into(),
    };
    let wrapped = |source: SecretsError| SecretsError::EnvResolution {
        name: SENTINEL.into(),
        reference: SENTINEL.into(),
        source: Box::new(source),
    };
    let missing = SecretsError::NotFound {
        key: SENTINEL.into(),
        searched: SENTINEL.into(),
    };
    let cases = [
        (refused(), ErrorKind::AgentUseRefused),
        (wrapped(refused()), ErrorKind::AgentUseRefused),
        (wrapped(missing), ErrorKind::EnvResolutionFailed),
        (
            SecretsError::InvalidEnvEntry {
                position: 1,
                reason: SENTINEL,
            },
            ErrorKind::InvalidEnvEntry,
        ),
        (
            SecretsError::DotenvSyntax {
                line: 1,
                reason: SENTINEL,
            },
            ErrorKind::DotenvSyntax,
        ),
    ];
    for (error, expected) in cases {
        let kind = ErrorKind::from(error);
        assert_eq!(kind, expected);
        let sent = RpcResponse::failure(json!(1), kind.to_rpc(method::LIST));
        let text = wire(&sent);
        assert!(!text.contains(SENTINEL), "{text}");
        let received: RpcResponse = serde_json::from_str(&text).unwrap();
        assert_eq!(fixed_error(&received, method::LIST), expected);
    }
    for other in [
        ErrorKind::NotFound,
        ErrorKind::VaultOutOfScope,
        ErrorKind::BackendFailed,
        ErrorKind::EnvResolutionFailed,
    ] {
        assert_ne!(ErrorKind::AgentUseRefused.code(), other.code());
        assert_ne!(ErrorKind::AgentUseRefused.as_str(), other.as_str());
    }
}

/// Why: a malformed `set` must not echo its value. serde's own message for
/// these params quotes the value — the first assertion proves it — so a
/// server that passed decode errors through would leak it.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_malformed_set_never_echoes_its_value() {
    let fx = fixture();
    let server = fx.start().await;
    let numeric = json!({"vault": "trusty/acme/web", "key": "K", "value": SENTINEL_NUMBER});
    let serde_text = serde_json::from_value::<crate::api::methods::SetRequest>(numeric.clone())
        .unwrap_err()
        .to_string();
    assert!(serde_text.contains(&SENTINEL_NUMBER.to_string()));

    let mut with_project = numeric;
    with_project["project"] = json!(fx.project());
    let cases = [
        with_project,
        json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "K",
               "value": SENTINEL, "extra": SENTINEL}),
        json!({"project": fx.project(), "vault": "trusty/acme/web",
               "key": format!("bad key {SENTINEL}"), "value": SENTINEL}),
        json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "K",
               "value": {"nested": SENTINEL}}),
        json!({"project": SENTINEL_NUMBER, "vault": "trusty/acme/web", "key": "K",
               "value": SENTINEL}),
    ];
    for params in cases {
        let response = call(&fx.settings.socket, method::SET, params).await;
        assert_eq!(
            fixed_error(&response, method::SET),
            ErrorKind::InvalidParams
        );
        let text = wire(&response);
        assert!(!text.contains(SENTINEL), "{text}");
        assert!(!text.contains(&SENTINEL_NUMBER.to_string()), "{text}");
    }
    // An empty value is refused by the store, with its own fixed text.
    let empty = call(
        &fx.settings.socket,
        method::SET,
        json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "K", "value": ""}),
    )
    .await;
    assert_eq!(fixed_error(&empty, method::SET), ErrorKind::InvalidValue);
    assert!(fx.keychain.is_empty());
    server.stop().await;
}

/// Why: a corrupt index is a fixed error that names neither the file nor its
/// content, and the file is left untouched.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_corrupt_index_is_a_fixed_error() {
    let fx = fixture();
    let index = NamesIndex::at(&fx.settings.index_root);
    let path = index.path_for(&vault("trusty/acme/web"));
    std::fs::create_dir_all(&fx.settings.index_root).unwrap();
    let garbage = format!("{{\"version\": 1, \"{SENTINEL}\"");
    std::fs::write(&path, &garbage).unwrap();
    let server = fx.start().await;
    for (name, params) in [
        (
            method::LIST,
            json!({"project": fx.project(), "vault": "trusty/acme/web"}),
        ),
        (
            method::SET,
            json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "K", "value": VALUE}),
        ),
    ] {
        let response = call(&fx.settings.socket, name, params).await;
        assert_eq!(fixed_error(&response, name), ErrorKind::IndexCorrupt);
        let text = wire(&response);
        assert!(
            !text.contains(SENTINEL) && !text.contains(".json"),
            "{text}"
        );
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), garbage);
    server.stop().await;
}

/// Why: ruling 28 — the server exits when idle and removes its socket.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_exits_when_idle_and_removes_its_socket() {
    let fx = fixture_with_idle(Duration::from_secs(1));
    let (_tx, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(serve(fx.settings.clone(), fx.backends(), async move {
        let _ = rx.await;
    }));
    wait_serving(&fx.settings.socket).await;
    ok(call(&fx.settings.socket, DOCTOR, Value::Null).await);
    let exit = tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .expect("server exits inside the bound")
        .unwrap()
        .unwrap();
    assert_eq!(exit, ServeExit::Idle);
    assert!(!fx.settings.socket.exists(), "socket file removed");
}

/// Why: a second instance must not clobber a live one; the first keeps
/// serving and keeps its socket.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_second_instance_is_refused_and_the_first_keeps_serving() {
    let fx = fixture();
    let first = fx.start().await;
    let second = serve(fx.settings.clone(), fx.backends(), std::future::ready(())).await;
    assert!(
        matches!(
            second,
            Err(ServeError::Bind {
                source: UdsSecurityError::AlreadyServing { .. },
                ..
            })
        ),
        "{second:?}"
    );
    ok(call(&fx.settings.socket, DOCTOR, Value::Null).await);
    assert_eq!(first.stop().await, ServeExit::Shutdown);
    assert!(!fx.settings.socket.exists());
}

/// Why: a bind that fails — here the path holds a regular file — is
/// reported and leaves the occupant on disk.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_bind_failure_is_reported_and_the_occupant_kept() {
    let fx = fixture();
    std::fs::create_dir_all(fx.settings.socket.parent().unwrap()).unwrap();
    std::fs::write(&fx.settings.socket, "occupant").unwrap();
    let result = serve(fx.settings.clone(), fx.backends(), std::future::ready(())).await;
    assert!(matches!(result, Err(ServeError::Bind { .. })), "{result:?}");
    assert_eq!(
        std::fs::read_to_string(&fx.settings.socket).unwrap(),
        "occupant"
    );
}

/// Why: the peer check is the trust boundary. A foreign-uid peer needs a
/// second account to produce, so this proves the two halves this crate
/// controls: the socket is 0600 in a 0700 directory, and a same-uid peer is
/// accepted by trusty-common's `ensure_peer_is_self`, which
/// `handle_connection` runs on every connection before reading a byte.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_socket_is_owner_only_and_checks_its_peer() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture();
    let server = fx.start().await;
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&fx.settings.socket), 0o600);
    assert_eq!(mode(fx.settings.socket.parent().unwrap()), 0o700);
    let stream = trusty_common::uds::connect_hardened(&fx.settings.socket)
        .await
        .unwrap();
    trusty_common::uds::ensure_peer_is_self(&stream).unwrap();
    drop(stream);
    server.stop().await;
}

/// Why: flag beats environment beats default, for every location.
/// Test: itself.
#[test]
fn settings_flags_beat_env_beat_defaults() {
    let args = |v: &[&str]| {
        v.iter()
            .map(Into::into)
            .collect::<Vec<std::ffi::OsString>>()
    };
    let env = |name: &str| match name {
        SOCKET_ENV => Some("/env/s.sock".to_string()),
        INDEX_DIR_ENV => Some("/env/index".to_string()),
        IDLE_TIMEOUT_ENV => Some("7".to_string()),
        _ => None,
    };
    let from_env = ServerSettings::from_args(args(&["serve"]), env).unwrap();
    assert_eq!(from_env.socket, PathBuf::from("/env/s.sock"));
    assert_eq!(from_env.index_root, PathBuf::from("/env/index"));
    assert_eq!(from_env.idle_timeout, Duration::from_secs(7));

    let flags = ServerSettings::from_args(
        args(&[
            "serve",
            "--socket",
            "/f/s.sock",
            "--index-dir",
            "/f/index",
            "--machine-config",
            "/f/m.yaml",
            "--idle-timeout-secs",
            "2",
        ]),
        env,
    )
    .unwrap();
    assert_eq!(
        flags,
        ServerSettings {
            socket: "/f/s.sock".into(),
            index_root: "/f/index".into(),
            machine_config: "/f/m.yaml".into(),
            idle_timeout: Duration::from_secs(2),
        }
    );

    if let Some(home) = dirs::home_dir() {
        let defaults = ServerSettings::from_args(args(&["serve"]), |_| None).unwrap();
        assert_eq!(defaults.socket, home.join(SOCKET_SUBPATH));
        assert_eq!(defaults.idle_timeout, DEFAULT_IDLE_TIMEOUT);
    }
}

/// Why: there is no "never exit" value — `0` and garbage in the environment
/// fall back to 60 s rather than keeping the process resident.
/// Test: itself.
#[test]
fn settings_idle_env_falls_back_on_garbage_and_zero() {
    for raw in [None, Some(""), Some("0"), Some("-3"), Some("soon")] {
        assert_eq!(
            settings::idle_from_env(raw),
            DEFAULT_IDLE_TIMEOUT,
            "{raw:?}"
        );
    }
    assert_eq!(settings::idle_from_env(Some(" 5 ")), Duration::from_secs(5));
}

/// Why: a typo on the command line fails the start instead of being ignored.
/// Test: itself.
#[test]
fn settings_reject_unknown_and_incomplete_flags() {
    let parse = |v: &[&str]| {
        ServerSettings::from_args(
            v.iter()
                .map(Into::into)
                .collect::<Vec<std::ffi::OsString>>(),
            |_| Some("/x".to_string()),
        )
    };
    assert!(matches!(parse(&[]), Err(SettingsError::Usage)));
    assert!(matches!(parse(&["run"]), Err(SettingsError::Usage)));
    assert!(matches!(
        parse(&["serve", "--sokcet", "/a"]),
        Err(SettingsError::UnknownArgument)
    ));
    assert!(matches!(
        parse(&["serve", "--socket"]),
        Err(SettingsError::MissingValue { flag: "--socket" })
    ));
    for bad in ["0", "x"] {
        assert!(matches!(
            parse(&["serve", "--idle-timeout-secs", bad]),
            Err(SettingsError::InvalidIdleTimeout)
        ));
    }
}
