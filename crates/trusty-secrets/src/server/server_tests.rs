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
use trusty_common::uds::server::RpcResponse;
use trusty_common::uds::{UdsSecurityError, send_framed_request, socket_is_serving};

use super::*;
use crate::api::methods::method;
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};
use crate::store::{MemoryBackend, NamesIndex, SecretBackend, SecretStore, mask_secret};

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
        // #4567: the audit log stays in the temp dir too.
        audit_log: tmp.path().join("audit").join("audit.jsonl"),
        audit_max_bytes: DEFAULT_AUDIT_MAX_BYTES,
        // #7519: template files and the startup sweep stay in the temp dir.
        template_root: tmp.path().join("tmp"),
    };
    // #9326: the build default is `file` where no Keychain is compiled; pin
    // `keychain` (the in-memory double here) so every host runs one path.
    std::fs::write(
        &settings.machine_config,
        "secrets:\n  default_backend: keychain\n",
    )
    .unwrap();
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
        self.start_state(self.state(backends)).await
    }

    /// The state [`Self::start_with`] serves.
    ///
    /// What: the fixture's machine config is also the account's own file
    /// consent config, as in production when no flag or `$HOME` moves it.
    // #7524 H1: never the real account home's config in a test.
    fn state(&self, backends: BackendFactory) -> State {
        let mut state = State::new(self.settings.clone(), backends);
        state.file_consent_config = Some(self.settings.machine_config.clone());
        state
    }

    /// Serve `state` on the fixture's socket.
    async fn start_state(&self, state: State) -> Running {
        let (tx, rx) = oneshot::channel::<()>();
        let task = tokio::spawn(router::serve_state(state, async move {
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

/// [`ServerSettings::from_args`] with a fixed account home that is never
/// `$HOME` and holds no socket a test names.
// #7524: no test reads the real password database; a host without a row for
// the test uid would fail closed and read every socket as the default.
fn parse_settings(
    args: impl IntoIterator<Item = std::ffi::OsString>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<ServerSettings, SettingsError> {
    ServerSettings::from_args_with(args, env, || Some(PathBuf::from("/account-home-7524")))
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

const ALL_KINDS: [ErrorKind; 34] = ErrorKind::ALL;

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
    // #9070: list reports the flag the backend holds, not an index field.
    fx.keychain
        .set_agents_may_use(&vault("trusty/acme/web"), &key("API_KEY"), true)
        .unwrap();
    let list = ok(call(
        socket,
        method::LIST,
        json!({"project": fx.project(), "vault": "trusty/acme/web"}),
    )
    .await);
    assert_eq!(list["keys"][0]["agents_may_use"], true);

    let deleted = ok(call(socket, method::DELETE, target).await);
    assert_eq!(deleted, json!({"removed": true}));
    assert!(fx.keychain.is_empty());
    assert!(
        !fx.keychain
            .agents_may_use(&vault("trusty/acme/web"), &key("API_KEY"))
            .unwrap(),
        "the flag goes with the key"
    );
    let list = ok(call(
        socket,
        method::LIST,
        json!({"project": fx.project(), "vault": "trusty/acme"}),
    )
    .await);
    assert_eq!(list["keys"], json!([]));
    server.stop().await;
}

/// Why: #9070 — list read only the index before the flag moved to the
/// backend, so a backend that does not open must not fail it. Every open
/// failure lists the index rows with the flag OFF, even when the backend
/// holds the flag ON. Red when only `UnknownBackend` fell back.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_list_lists_flags_off_when_the_backend_does_not_open() {
    let open_failures: [fn() -> SecretsError; 3] = [
        || SecretsError::BackendNotEnabled {
            backend: "keychain".to_string(),
        },
        || SecretsError::CliNotInstalled {
            program: "op".to_string(),
            hint: "install it",
        },
        || SecretsError::HomeUnavailable,
    ];
    for failure in open_failures {
        let fx = fixture();
        let store = SecretStore::new(
            Arc::clone(&fx.keychain) as Arc<dyn SecretBackend>,
            NamesIndex::at(&fx.settings.index_root),
        );
        let (web, api_key) = (vault("trusty/acme/web"), key("API_KEY"));
        store.set(&web, &api_key, &SecretValue::new(VALUE)).unwrap();
        store.set_agents_may_use(&web, &api_key, true).unwrap();
        let shown = format!("{:?}", failure());
        let factory: BackendFactory = Arc::new(move |_: &BackendId| Err(failure()));
        let server = fx.start_with(factory).await;
        let list = call(
            &fx.settings.socket,
            method::LIST,
            json!({"project": fx.project(), "vault": "trusty/acme/web"}),
        )
        .await;
        assert!(list.error.is_none(), "{shown}: {:?}", list.error);
        let list = ok(list);
        assert_eq!(list["keys"][0]["name"], "API_KEY", "{shown}");
        assert_eq!(list["keys"][0]["agents_may_use"], false, "{shown}");
        server.stop().await;
    }
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
    // #7519 P4: every row names why it is unavailable; the CLI-backed rows
    // are listed on every build.
    let cli = if crate::store::cli_backends().is_empty() {
        Unavailable::NotCompiled
    } else {
        Unavailable::NotEnabled
    };
    let rows: Vec<_> = bare
        .backends
        .iter()
        .map(|b| {
            (
                b.id.as_str(),
                b.available,
                b.capabilities.join(","),
                b.reason,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("keychain", true, "READ,WRITE".to_string(), None),
            // #9326: listed beside the Keychain; this fixture maps no `file`.
            ("file", false, String::new(), Some(Unavailable::NotCompiled)),
            ("onepassword", false, String::new(), Some(cli)),
            ("keeper", false, String::new(), Some(cli)),
        ]
    );
    assert!(
        bare.backends
            .iter()
            .all(|b| b.available == b.detail.is_none())
    );
    assert_eq!(bare.posture, Some(StoragePosture::Keychain));

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

/// A factory for `fx` that also maps `file` to a value-file backend under
/// the fixture's temp dir.
fn with_file_backend(fx: &Fixture) -> (BackendFactory, crate::store::FileBackend) {
    let file = crate::store::FileBackend::at(fx.tmp.path().join("values"));
    let base = fx.backends();
    let shared = file.clone();
    let factory: BackendFactory = Arc::new(move |id: &BackendId| match id.as_str() {
        "file" => Ok(Arc::new(shared.clone()) as Arc<dyn SecretBackend>),
        _ => base(id),
    });
    (factory, file)
}

/// Why: #9326 AC4, owner ruling f5 — doctor reports the file backend as a
/// degraded posture whether config chose it or the build default did.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_doctor_reports_the_file_posture() {
    let fx = fixture();
    let (factory, _file) = with_file_backend(&fx);
    std::fs::write(
        &fx.settings.machine_config,
        "secrets:\n  default_backend: file\n",
    )
    .unwrap();
    let server = fx.start_with(factory).await;
    let doctor: DoctorResponse =
        serde_json::from_value(ok(call(&fx.settings.socket, DOCTOR, Value::Null).await)).unwrap();
    assert_eq!(doctor.selected_backend, BackendId::file());
    assert_eq!(doctor.posture, Some(StoragePosture::FileDegraded));
    let file_row = doctor
        .backends
        .iter()
        .find(|row| row.id == BackendId::file())
        .unwrap();
    assert!(file_row.available);
    assert_eq!(file_row.capabilities, ["READ", "WRITE", "LIST_NAMES"]);

    // No config at all: the build default, which is `file` off macOS.
    std::fs::remove_file(&fx.settings.machine_config).unwrap();
    let bare: DoctorResponse =
        serde_json::from_value(ok(call(&fx.settings.socket, DOCTOR, Value::Null).await)).unwrap();
    let default = crate::store::default_backend();
    assert_eq!(bare.selected_backend, default);
    assert_eq!(bare.posture, Some(StoragePosture::of(&default)));
    #[cfg(not(target_os = "macos"))]
    assert_eq!(bare.posture, Some(StoragePosture::FileDegraded));
    #[cfg(target_os = "macos")]
    assert_eq!(bare.posture, Some(StoragePosture::Keychain));
    server.stop().await;
}

/// Why: #9326 — a client older than a new posture variant still decodes the
/// doctor answer, reading the unknown posture as `Other`.
/// Red without `#[serde(other)]` on `StoragePosture::Other`.
/// Test: itself.
#[test]
fn server_unknown_posture_decodes_as_other() {
    let posture: StoragePosture = serde_json::from_value(json!("hsm_sealed")).unwrap();
    assert_eq!(posture, StoragePosture::Other);
    let known: StoragePosture = serde_json::from_value(json!("file_degraded")).unwrap();
    assert_eq!(known, StoragePosture::FileDegraded);
}

/// Why: #9326 AC1 — set, list, delete and copy work through the file
/// backend, and neither the index nor any answer carries a value.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_file_backend_set_list_delete_and_copy() {
    let fx = fixture();
    // #9326: only the untracked machine config may select `file` everywhere.
    std::fs::write(
        &fx.settings.machine_config,
        "secrets:\n  default_backend: file\n",
    )
    .unwrap();
    let (factory, file) = with_file_backend(&fx);
    let server = fx.start_with(factory).await;
    let socket = &fx.settings.socket;
    let project = vault("trusty/acme/web");

    let set = call(
        socket,
        method::SET,
        json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "A", "value": VALUE}),
    )
    .await;
    assert!(!wire(&set).contains(VALUE));
    ok(set);
    assert_eq!(fx.keychain.len(), 0, "the machine config chose `file`");
    assert_eq!(
        file.get(&project, &key("A")).unwrap().unwrap().expose(),
        VALUE
    );
    let list = call(
        socket,
        method::LIST,
        json!({"project": fx.project(), "vault": "trusty/acme/web"}),
    )
    .await;
    assert!(!wire(&list).contains(VALUE));
    assert_eq!(ok(list)["keys"][0]["name"], "A");

    // file -> keychain, then keychain -> file for a key only the Keychain has.
    let out = ok(call(
        socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "file", "to_backend": "keychain"}),
    )
    .await);
    assert_eq!(out, json!({"copied": ["A"], "failed": []}));
    assert_eq!(
        fx.keychain
            .get(&project, &key("A"))
            .unwrap()
            .unwrap()
            .expose(),
        VALUE
    );
    fx.keychain
        .set(&project, &key("B"), &SecretValue::new(SENTINEL))
        .unwrap();
    let back = call(
        socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain", "to_backend": "file",
               "keys": ["B"]}),
    )
    .await;
    assert!(!wire(&back).contains(SENTINEL));
    assert_eq!(ok(back), json!({"copied": ["B"], "failed": []}));
    assert_eq!(
        file.get(&project, &key("B")).unwrap().unwrap().expose(),
        SENTINEL
    );

    for entry in std::fs::read_dir(&fx.settings.index_root).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains(VALUE) && !text.contains(SENTINEL), "{text}");
    }

    ok(call(
        socket,
        method::DELETE,
        json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "A"}),
    )
    .await);
    assert!(file.get(&project, &key("A")).unwrap().is_none());
    assert_eq!(file.list_names(&project).unwrap(), [key("B")]);
    server.stop().await;
}

/// Why: #9326, Architect ruling (basis ruling 06 R2) — on a Keychain build a
/// tracked project config may not move values to plaintext files. The
/// refusal is a fixed kind that names the machine key and echoes nothing
/// from the repository; nothing is written. Off macOS `file` stays allowed.
/// Red when `check_project_backend` lets the project `file` through.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_tracked_file_backend_is_refused_on_a_keychain_build() {
    let fx = fixture();
    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        format!("# {SENTINEL}\nsecrets:\n  backend: file\n"),
    )
    .unwrap();
    let (factory, file) = with_file_backend(&fx);
    let server = fx.start_with(factory).await;
    let set = call(
        &fx.settings.socket,
        method::SET,
        json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "A", "value": VALUE}),
    )
    .await;
    let project = vault("trusty/acme/web");
    if cfg!(target_os = "macos") {
        let text = wire(&set);
        assert!(!text.contains(SENTINEL) && !text.contains(VALUE), "{text}");
        assert!(text.contains("secrets.default_backend"), "{text}");
        assert_eq!(
            fixed_error(&set, method::SET),
            ErrorKind::TrackedBackendRefused
        );
        assert!(file.get(&project, &key("A")).unwrap().is_none());
        assert_eq!(
            fx.keychain.len(),
            0,
            "never a silent switch to the Keychain"
        );
    } else {
        ok(set);
        assert_eq!(
            file.get(&project, &key("A")).unwrap().unwrap().expose(),
            VALUE
        );
    }
    server.stop().await;
}

/// Why: #7519, owner ruling 2026-10-07 — a tracked project config may not
/// set a CLI `account` or `config_path`, on any build. The refusal is a fixed
/// kind that echoes nothing from the repository, and nothing is written.
/// Red when `check_project_backend` lets the setting through.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_tracked_cli_setting_is_refused_on_every_build() {
    let fx = fixture();
    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, format!("secrets:\n  account: {SENTINEL}\n")).unwrap();
    let server = fx.start().await;
    let set = call(
        &fx.settings.socket,
        method::SET,
        json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "A", "value": VALUE}),
    )
    .await;
    let text = wire(&set);
    assert!(!text.contains(SENTINEL) && !text.contains(VALUE), "{text}");
    assert_eq!(
        fixed_error(&set, method::SET),
        ErrorKind::TrackedCliSettingRefused
    );
    assert_eq!(fx.keychain.len(), 0, "nothing is written");
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
    seed_victim(&fx);
    let server = fx.start().await;
    for (name, params) in [
        // #9328: R1 — the same check a pinned `secret://` reference passes.
        (
            method::LIST,
            json!({"project": fx.project(), "vault": "trusty/victim/prod-repo"}),
        ),
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
        assert!(!wire(&response).contains("DB_URL"));
    }
    assert_eq!(fx.keychain.len(), 1, "only the seeded victim entry");
    server.stop().await;
}

/// Index and store one key in another project's vault, `trusty/victim/prod-repo`.
fn seed_victim(fx: &Fixture) {
    let store = SecretStore::new(
        Arc::clone(&fx.keychain) as Arc<dyn SecretBackend>,
        NamesIndex::at(&fx.settings.index_root),
    );
    store
        .set(
            &vault("trusty/victim/prod-repo"),
            &key("DB_URL"),
            &SecretValue::new(SENTINEL),
        )
        .unwrap();
}

/// Why: #9328 vector (b), owner ruling 06 R2 — a tracked
/// `secrets.vault: trusty/victim/prod-repo` made the victim vault this
/// project's own, so list, set and delete reached it. Every method now
/// answers `vault_out_of_scope` in fixed text and the victim entry is left
/// alone; the same vault named in the untracked machine config is honoured.
/// Red on the unfixed code: `secrets.scopes` answers the victim vault.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_tracked_vault_override_outside_the_owner_is_refused() {
    let fx = fixture();
    seed_victim(&fx);
    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "secrets:\n  vault: trusty/victim/prod-repo\n").unwrap();
    let server = fx.start().await;
    let victim = "trusty/victim/prod-repo";
    for (name, params) in [
        (method::SCOPES, json!({"project": fx.project()})),
        (
            method::LIST,
            json!({"project": fx.project(), "vault": victim}),
        ),
        (
            method::SET,
            json!({"project": fx.project(), "vault": victim, "key": "DB_URL", "value": VALUE}),
        ),
        (
            method::DELETE,
            json!({"project": fx.project(), "vault": victim, "key": "DB_URL"}),
        ),
    ] {
        let response = call(&fx.settings.socket, name, params).await;
        assert_eq!(fixed_error(&response, name), ErrorKind::VaultOutOfScope);
        let text = wire(&response);
        assert!(
            !text.contains(SENTINEL) && !text.contains("victim"),
            "{text}"
        );
    }
    let victim_vault = vault(victim);
    assert_eq!(
        fx.keychain
            .get(&victim_vault, &key("DB_URL"))
            .unwrap()
            .map(|v| v.expose().to_string()),
        Some(SENTINEL.to_string()),
        "the victim entry is untouched"
    );

    std::fs::write(
        &fx.settings.machine_config,
        "secrets:\n  project_vaults:\n    acme/web: trusty/victim/prod-repo\n",
    )
    .unwrap();
    let scopes = ok(call(
        &fx.settings.socket,
        method::SCOPES,
        json!({"project": fx.project()}),
    )
    .await);
    assert_eq!(scopes["scopes"][0]["vault"], victim);
    assert_eq!(scopes["scopes"][1]["vault"], "trusty/acme");
    server.stop().await;
}

/// Why: #9328 vector (c), owner ruling 06 R3 — a non-github.com remote is a
/// fixed `remote_host_unsupported` error that names neither the host nor the
/// path; github.com in https and ssh forms still resolves, and github.com
/// over any other scheme or a `<helper>::` prefix is the same error.
/// Red on the unfixed code: `secrets.scopes` answers `trusty/acme/web` for
/// the `evil.example` remote.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_non_github_remote_is_a_fixed_error() {
    let fx = fixture();
    let server = fx.start().await;
    let checkout = |name: &str, url: &str| {
        let dir = fx.tmp.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["remote", "add", "origin", url]);
        dir.display().to_string()
    };
    let evil = checkout("evil", "https://evil.example/Acme/Web.git");
    let response = call(
        &fx.settings.socket,
        method::SCOPES,
        json!({"project": evil}),
    )
    .await;
    assert_eq!(
        fixed_error(&response, method::SCOPES),
        ErrorKind::RemoteHostUnsupported
    );
    assert!(!wire(&response).contains("evil"), "{}", wire(&response));

    // #9328: github.com over a scheme DOC-74 §15.3 does not accept.
    for (name, url) in [
        ("file", "file://github.com/acme/app"),
        ("git", "git://github.com/acme/app"),
        ("http", "http://github.com/acme/app"),
        ("helper", "x::https://github.com/acme/app"),
    ] {
        let dir = checkout(name, url);
        let response = call(&fx.settings.socket, method::SCOPES, json!({"project": dir})).await;
        assert_eq!(
            fixed_error(&response, method::SCOPES),
            ErrorKind::RemoteHostUnsupported,
            "{url}"
        );
        assert!(!wire(&response).contains("acme/app"), "{}", wire(&response));
    }

    for (name, url) in [
        ("https", "https://github.com/Acme/Web.git"),
        ("ssh", "ssh://git@github.com/acme/web.git"),
    ] {
        let dir = checkout(name, url);
        let scopes = ok(call(&fx.settings.socket, method::SCOPES, json!({"project": dir})).await);
        assert_eq!(scopes["scopes"][0]["vault"], "trusty/acme/web", "{url}");
    }
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

/// Why: #9328 — a kind missing from `ErrorKind::ALL` reads as `None` on the
/// client. The match below is exhaustive, so a new variant fails to compile
/// until it gets the next index; `ARMS` is that index plus one.
/// Test: itself.
#[test]
fn error_kind_all_lists_every_variant_once() {
    const ARMS: usize = 34;
    fn index(kind: ErrorKind) -> usize {
        match kind {
            ErrorKind::InvalidParams => 0,
            ErrorKind::ProjectInvalid => 1,
            ErrorKind::ProjectUnresolved => 2,
            ErrorKind::VaultOutOfScope => 3,
            ErrorKind::InvalidValue => 4,
            ErrorKind::NotFound => 5,
            ErrorKind::Unsupported => 6,
            ErrorKind::UnknownBackend => 7,
            ErrorKind::BackendFailed => 8,
            ErrorKind::OrphanedBackendEntry => 9,
            ErrorKind::IndexCorrupt => 10,
            ErrorKind::IndexBusy => 11,
            ErrorKind::StorageUnavailable => 12,
            ErrorKind::ConfigInvalid => 13,
            ErrorKind::HomeUnavailable => 14,
            ErrorKind::SameBackend => 15,
            ErrorKind::AgentUseRefused => 16,
            ErrorKind::InvalidEnvEntry => 17,
            ErrorKind::EnvResolutionFailed => 18,
            ErrorKind::DotenvSyntax => 19,
            ErrorKind::RemoteHostUnsupported => 20,
            ErrorKind::StorageRefused => 21,
            ErrorKind::TrackedBackendRefused => 22,
            ErrorKind::AuditUnavailable => 23,
            ErrorKind::TrackedAuditRefused => 24,
            // #7519: the tracked CLI-setting refusal and the CLI backends' kinds.
            ErrorKind::TrackedCliSettingRefused => 25,
            ErrorKind::CliNotInstalled => 26,
            ErrorKind::BackendLocked => 27,
            // #7524 H1: a write into `file` the machine config did not select.
            ErrorKind::FileBackendNotSelected => 28,
            ErrorKind::Internal => 29,
            // #7519: after `Internal`, so no existing discriminant moves.
            ErrorKind::BackendNotEnabled => 30,
            // #7524 P2-M1: after `BackendNotEnabled`.
            ErrorKind::DeadlineExceeded => 31,
            // #7524 P2-M3 fix round: after `DeadlineExceeded`.
            ErrorKind::VaultNotVisible => 32,
            // #7524 P2-L7: after `VaultNotVisible`.
            ErrorKind::BackendTimeout => 33,
        }
    }
    assert_eq!(ErrorKind::ALL.len(), ARMS);
    for (i, kind) in ErrorKind::ALL.into_iter().enumerate() {
        assert_eq!(index(kind), i, "{kind:?} is out of place in ErrorKind::ALL");
        assert_eq!(ErrorKind::from_wire(kind.as_str()), Some(kind));
    }
}

/// Why: `ErrorKind`'s discriminants are a public contract (#7524). For this
/// enum the trusty-secrets 0.1.2 accepted-break declaration covers only
/// `Internal` 25 -> 29. A variant reordered, or inserted before `Internal`,
/// moves a published value.
/// What: `kind as i32` equals its pinned value for all 34 variants, and the
/// table names every kind in `ErrorKind::ALL` exactly once.
/// Test: itself.
#[test]
fn error_kind_discriminants_are_pinned() {
    const PINNED: [(ErrorKind, i32); 34] = [
        (ErrorKind::InvalidParams, 0),
        (ErrorKind::ProjectInvalid, 1),
        (ErrorKind::ProjectUnresolved, 2),
        (ErrorKind::VaultOutOfScope, 3),
        (ErrorKind::InvalidValue, 4),
        (ErrorKind::NotFound, 5),
        (ErrorKind::Unsupported, 6),
        (ErrorKind::UnknownBackend, 7),
        (ErrorKind::BackendFailed, 8),
        (ErrorKind::OrphanedBackendEntry, 9),
        (ErrorKind::IndexCorrupt, 10),
        (ErrorKind::IndexBusy, 11),
        (ErrorKind::StorageUnavailable, 12),
        (ErrorKind::ConfigInvalid, 13),
        (ErrorKind::HomeUnavailable, 14),
        (ErrorKind::SameBackend, 15),
        (ErrorKind::AgentUseRefused, 16),
        (ErrorKind::InvalidEnvEntry, 17),
        (ErrorKind::EnvResolutionFailed, 18),
        (ErrorKind::DotenvSyntax, 19),
        (ErrorKind::RemoteHostUnsupported, 20),
        (ErrorKind::StorageRefused, 21),
        (ErrorKind::TrackedBackendRefused, 22),
        (ErrorKind::AuditUnavailable, 23),
        (ErrorKind::TrackedAuditRefused, 24),
        (ErrorKind::TrackedCliSettingRefused, 25),
        (ErrorKind::CliNotInstalled, 26),
        (ErrorKind::BackendLocked, 27),
        (ErrorKind::FileBackendNotSelected, 28),
        (ErrorKind::Internal, 29),
        (ErrorKind::BackendNotEnabled, 30),
        (ErrorKind::DeadlineExceeded, 31),
        (ErrorKind::VaultNotVisible, 32),
        (ErrorKind::BackendTimeout, 33),
    ];
    for (kind, value) in PINNED {
        assert_eq!(kind as i32, value, "{kind:?} moved from its pinned value");
    }
    for kind in ErrorKind::ALL {
        let rows = PINNED.iter().filter(|(k, _)| *k == kind).count();
        assert_eq!(rows, 1, "{kind:?} must be pinned exactly once");
    }
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

/// Why: #9073 — the client's error is this crate's `RpcFailure`, not
/// trusty-common's `RpcError`, so every kind must survive the conversion, an
/// unknown or absent kind must read as `None`, and the `Display` text must
/// not change.
/// Test: itself.
#[test]
fn client_rpc_failure_reads_every_kind_from_the_wire() {
    for kind in ALL_KINDS {
        let failure = RpcFailure::from_wire(kind.to_rpc(method::LIST));
        assert_eq!(failure.kind, Some(kind));
        assert_eq!(failure.code, kind.code());
        assert_eq!(
            failure.message,
            format!("{}: {}", method::LIST, kind.text())
        );
        assert_eq!(
            ClientError::Rpc(failure).to_string(),
            format!("[{}] {}: {}", kind.code(), method::LIST, kind.text())
        );
    }
    let newer = trusty_common::uds::server::RpcError::new(-32099, "secrets.list: newer")
        .with_data(json!({ "kind": "a_kind_from_a_newer_server" }));
    assert_eq!(RpcFailure::from_wire(newer).kind, None);
    let bare = trusty_common::uds::server::RpcError::new(-32601, "method not found");
    assert_eq!(
        RpcFailure::from_wire(bare),
        RpcFailure::new(-32601, "method not found", None)
    );
    let transport = ClientError::Transport(Box::new(std::io::Error::other("io detail")));
    assert_eq!(transport.to_string(), "io detail");
}

/// Why: #7519 — a client branches on the wire code of a refused CLI
/// backend, so `backend_not_enabled` keeps -32078 (-32077 is #7524's); the
/// uniqueness check alone would let it move to any unused code.
/// Test: itself.
#[test]
fn server_backend_not_enabled_is_wire_code_32078() {
    let kind = ErrorKind::from(SecretsError::BackendNotEnabled {
        backend: SENTINEL.into(),
    });
    assert_eq!(kind, ErrorKind::BackendNotEnabled);
    assert_eq!(kind.code(), -32078);
    assert_eq!(kind.as_str(), "backend_not_enabled");
    let failure = RpcFailure::from_wire(kind.to_rpc(method::SET));
    assert_eq!(
        (failure.code, failure.kind),
        (-32078, Some(ErrorKind::BackendNotEnabled))
    );
    assert!(!failure.message.contains(SENTINEL), "{}", failure.message);
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
    let out_of_scope = || SecretsError::VaultOutOfScope {
        vault: SENTINEL.into(),
        reason: SENTINEL,
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
        // #9328: out-of-scope stays a refusal even when `resolve_env` wraps it.
        (out_of_scope(), ErrorKind::VaultOutOfScope),
        (wrapped(out_of_scope()), ErrorKind::VaultOutOfScope),
        (
            SecretsError::UnsupportedRemoteHost {
                dir: SENTINEL.into(),
            },
            ErrorKind::RemoteHostUnsupported,
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
    // #9326: the socket accepts before the first binder drops its bind lock,
    // so under load the second binder can meet `BindInProgress`, a transient
    // refusal (#8759). Retry inside a bound; the assertion below is unchanged.
    let started = std::time::Instant::now();
    let second = loop {
        let attempt = serve(fx.settings.clone(), fx.backends(), std::future::ready(())).await;
        let in_progress = matches!(
            &attempt,
            Err(ServeError::Bind { source, .. })
                if matches!(
                    source.downcast_ref::<UdsSecurityError>(),
                    Some(UdsSecurityError::BindInProgress { .. })
                )
        );
        if !in_progress || started.elapsed() > Duration::from_secs(5) {
            break attempt;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        matches!(
            second,
            Err(ServeError::Bind { ref source, .. })
                if matches!(
                    source.downcast_ref::<UdsSecurityError>(),
                    Some(UdsSecurityError::AlreadyServing { .. })
                )
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
    let from_env = parse_settings(args(&["serve"]), env).unwrap();
    assert_eq!(from_env.socket, PathBuf::from("/env/s.sock"));
    assert_eq!(from_env.index_root, PathBuf::from("/env/index"));
    assert_eq!(from_env.idle_timeout, Duration::from_secs(7));

    let flags = parse_settings(
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
            "--audit-log",
            "/f/a.jsonl",
        ]),
        env,
    )
    .unwrap();
    assert_eq!(
        flags,
        ServerSettings::new(
            "/f/s.sock".into(),
            "/f/index".into(),
            "/f/m.yaml".into(),
            Duration::from_secs(2),
        )
        .with_audit_log("/f/a.jsonl".into())
    );

    if let Some(home) = dirs::home_dir() {
        let defaults = parse_settings(args(&["serve"]), |_| None).unwrap();
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
        parse_settings(
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

/// Why: #7524 P2-M1 — the client gave up at 30 s while the server went on
/// and committed the write. For every method the server's deadline must be
/// shorter than the client's wait, and a write's deadline must hold a
/// 1Password set's two CLI calls at their full 60 s timeout.
/// Test: itself.
#[test]
fn client_wait_exceeds_the_server_deadline_for_every_method() {
    use super::deadline::{client_wait, request_deadline};
    for (name, _) in router::METHODS {
        assert!(client_wait(name) > request_deadline(name), "{name}");
    }
    for name in [method::SET, method::DELETE, method::COPY] {
        assert!(request_deadline(name) >= Duration::from_secs(120), "{name}");
        assert!(client_wait(name) > Duration::from_secs(120), "{name}");
    }
    assert_eq!(
        ErrorKind::DeadlineExceeded.code(),
        -32079,
        "the next unused code"
    );
}

// #4567: the audit-trail tests share this module's fixture.
#[path = "audit_tests.rs"]
mod audit_tests;

// #7519: the delete-across-backends tests share this module's fixture.
#[path = "delete_tests.rs"]
mod delete_tests;

// #7519 P4: the doctor reason, readiness and wire tests share this fixture.
#[path = "doctor_tests.rs"]
mod doctor_tests;

// #7524: the Keychain-to-file write posture tests share this module's fixture.
#[path = "posture_tests.rs"]
mod posture_tests;

// #7519: the 1Password server-path tests share this module's fixture.
#[cfg(all(unix, feature = "cli-backends"))]
#[path = "onepassword_tests.rs"]
mod onepassword_tests;

// #7519 P3: the Keeper server-path tests share this module's fixture.
#[cfg(all(unix, feature = "cli-backends"))]
#[path = "keeper_tests.rs"]
mod keeper_tests;

/// Why: #7519 — the template directory holds values, so like the audit log
/// it follows only the `--index-dir` flag, never an environment variable.
/// Test: itself.
#[test]
fn settings_template_root_follows_the_index_flag_only() {
    let args = |v: &[&str]| {
        v.iter()
            .map(Into::into)
            .collect::<Vec<std::ffi::OsString>>()
    };
    let env = |name: &str| (name == INDEX_DIR_ENV).then(|| "/env/index".to_string());
    if let Some(home) = dirs::home_dir() {
        // #7519: through `parse_settings`, so a host with no password-database
        // row for the test uid (a Linux container) does not fail closed here.
        let flagged = parse_settings(args(&["serve", "--index-dir", "/f/index"]), env);
        assert_eq!(flagged.unwrap().template_root, PathBuf::from("/f/tmp"));
        // #7524: the environment moves the index only off the default socket.
        let from_env =
            parse_settings(args(&["serve", "--socket", "/s/secrets.sock"]), env).unwrap();
        assert_eq!(from_env.index_root, PathBuf::from("/env/index"));
        let default = settings::template_root_beside(&home.join(crate::store::INDEX_SUBDIR));
        assert_eq!(from_env.template_root, default);
        #[cfg(all(unix, feature = "cli-backends"))]
        assert_eq!(default, home.join(crate::store::cli::TMP_SUBDIR));
    }
    let built = ServerSettings::new(
        "/s".into(),
        "/x/index".into(),
        "/m".into(),
        DEFAULT_IDLE_TIMEOUT,
    );
    assert_eq!(built.template_root, PathBuf::from("/x/tmp"));
}

/// A backend whose every value call blocks until the test drops the sender
/// [`StuckBackend::new`] returns (#9572), then goes to an in-memory store.
///
/// Why: a backend call that never returns (a file backend on a hung mount)
/// must not hold its request past the deadline. A panicking test drops the
/// sender while it unwinds, so the blocked threads end and the test binary
/// exits instead of hanging.
#[derive(Debug)]
struct StuckBackend {
    inner: MemoryBackend,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl StuckBackend {
    fn new() -> (Arc<Self>, std::sync::mpsc::Sender<()>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let backend = Self {
            inner: MemoryBackend::new(),
            release: std::sync::Mutex::new(rx),
        };
        (Arc::new(backend), tx)
    }

    /// Block until the test drops its sender; nothing is ever sent.
    fn wait(&self) {
        if let Ok(rx) = self.release.lock() {
            let _ = rx.recv();
        }
    }
}

impl SecretBackend for StuckBackend {
    fn id(&self) -> BackendId {
        self.inner.id()
    }
    fn capabilities(&self) -> crate::store::Capabilities {
        self.wait();
        self.inner.capabilities()
    }
    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        self.wait();
        self.inner.get(vault, key)
    }
    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        self.wait();
        self.inner.set(vault, key, value)
    }
    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        self.wait();
        self.inner.delete(vault, key)
    }
    fn agents_may_use(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        self.wait();
        self.inner.agents_may_use(vault, key)
    }
    fn set_agents_may_use(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        allowed: bool,
    ) -> Result<(), SecretsError> {
        self.wait();
        self.inner.set_agents_may_use(vault, key, allowed)
    }
}

/// The request deadline the stuck-backend tests run under (#9572).
const STUCK_DEADLINE: Duration = Duration::from_secs(2);

/// How late past its deadline a reply may arrive and still pass (#9572).
const REPLY_MARGIN: Duration = Duration::from_secs(2);

/// The fixture's factory with `keychain`, the project's backend, mapped to
/// `stuck`.
fn stuck_factory(fx: &Fixture, stuck: &Arc<StuckBackend>) -> BackendFactory {
    let base = fx.backends();
    let stuck = Arc::clone(stuck);
    Arc::new(move |id: &BackendId| match id.as_str() {
        "keychain" => Ok(Arc::clone(&stuck) as Arc<dyn SecretBackend>),
        _ => base(id),
    })
}

/// Call `method` and fail the test, rather than hang it, when no reply comes
/// within the deadline plus [`REPLY_MARGIN`] (#9572).
async fn call_within_deadline(fx: &Fixture, method: &str, params: Value) -> RpcResponse {
    let watchdog = STUCK_DEADLINE + REPLY_MARGIN;
    tokio::time::timeout(watchdog, call(&fx.settings.socket, method, params))
        .await
        .unwrap_or_else(|_| panic!("no reply to {method} within {watchdog:?}"))
}

/// A `secrets.set` of `name` into the project vault, under the watchdog.
async fn set_within_deadline(fx: &Fixture, name: &str) -> RpcResponse {
    let params = json!({"project": fx.project(), "vault": "trusty/acme/web",
        "key": name, "value": VALUE});
    call_within_deadline(fx, method::SET, params).await
}

/// A `secrets.scopes` call, which touches no backend, under the watchdog.
async fn scopes_within_deadline(fx: &Fixture) -> RpcResponse {
    call_within_deadline(fx, method::SCOPES, json!({"project": fx.project()})).await
}

/// Why: #9572 — the router awaited a blocking body with no bound, so a
/// backend call that never returns held its request forever, and nothing
/// capped how many threads such calls could hold.
/// What: with a cap of 6, (a) a request to a stuck backend answers
/// `deadline_exceeded` within its deadline plus a margin; (b) with 6 calls
/// stuck, a 7th request that needs no backend waits for admission until its
/// deadline, then answers `deadline_exceeded`; (c) once the stuck calls are
/// released, a later request to the now working backend completes.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_stuck_backend_calls_do_not_starve_a_later_request() {
    let fx = fixture();
    let (stuck, release) = StuckBackend::new();
    let mut state = fx.state(stuck_factory(&fx, &stuck));
    state.deadline_override = Some(STUCK_DEADLINE);
    state.admission_cap_override = Some(6);
    let server = fx.start_state(state).await;

    // (a) the stuck call's request answers at its deadline.
    let started = std::time::Instant::now();
    let first = set_within_deadline(&fx, "STUCK_1").await;
    assert!(started.elapsed() < STUCK_DEADLINE + REPLY_MARGIN);
    assert_eq!(
        fixed_error(&first, method::SET),
        ErrorKind::DeadlineExceeded
    );

    // Five more stuck calls fill the cap of 6; each answers at its deadline.
    let more = tokio::join!(
        set_within_deadline(&fx, "STUCK_2"),
        set_within_deadline(&fx, "STUCK_3"),
        set_within_deadline(&fx, "STUCK_4"),
        set_within_deadline(&fx, "STUCK_5"),
        set_within_deadline(&fx, "STUCK_6"),
    );
    for response in [more.0, more.1, more.2, more.3, more.4] {
        assert_eq!(
            fixed_error(&response, method::SET),
            ErrorKind::DeadlineExceeded
        );
    }

    // (b) a 7th request is not admitted and gives up at its own deadline.
    let started = std::time::Instant::now();
    let seventh = scopes_within_deadline(&fx).await;
    let waited = started.elapsed();
    assert!(
        waited >= STUCK_DEADLINE - Duration::from_millis(50),
        "{waited:?}"
    );
    assert!(waited < STUCK_DEADLINE + REPLY_MARGIN, "{waited:?}");
    assert_eq!(
        fixed_error(&seventh, method::SCOPES),
        ErrorKind::DeadlineExceeded
    );

    // (c) released, the stuck threads finish and a later request completes.
    drop(release);
    let later = ok(set_within_deadline(&fx, "WORKING").await);
    assert_eq!(later["outcome"], json!("new"), "{later}");
    let stored = stuck.inner.get(&vault("trusty/acme/web"), &key("WORKING"));
    assert_eq!(stored.unwrap().unwrap().expose(), VALUE);
    server.stop().await;
}

/// Why: #9572 — the permit must be the stuck thread's, not the request's;
/// were it released when the request timed out, every timed-out request
/// would free a slot and stuck threads would pile up without bound.
/// What: with a cap of 1, a request to a stuck backend times out; a request
/// that needs no backend then still waits until its own deadline, because
/// the first request's thread holds the only permit; once the stuck call is
/// released, the same request is admitted and answers.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_timed_out_request_keeps_its_permit_until_its_call_returns() {
    let fx = fixture();
    let (stuck, release) = StuckBackend::new();
    let mut state = fx.state(stuck_factory(&fx, &stuck));
    state.deadline_override = Some(STUCK_DEADLINE);
    state.admission_cap_override = Some(1);
    let server = fx.start_state(state).await;

    let timed_out = set_within_deadline(&fx, "STUCK_1").await;
    assert_eq!(
        fixed_error(&timed_out, method::SET),
        ErrorKind::DeadlineExceeded
    );
    // The request has answered; its thread, still stuck, holds the permit.
    let refused = scopes_within_deadline(&fx).await;
    assert_eq!(
        fixed_error(&refused, method::SCOPES),
        ErrorKind::DeadlineExceeded
    );

    drop(release);
    let admitted = ok(scopes_within_deadline(&fx).await);
    assert_eq!(admitted["scopes"][0]["vault"], json!("trusty/acme/web"));
    server.stop().await;
}

/// A method body that records that it ran.
fn body_that_runs(_: &State, _: Value) -> Result<Value, ErrorKind> {
    Ok(json!("ran"))
}

/// Why: #9572 Fail-Open Check — an admission failure is never a success: a
/// closed semaphore answers `internal` and the body never runs.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_closed_admission_is_internal_and_never_runs_the_body() {
    let fx = fixture();
    let state = Arc::new(fx.state(fx.backends()));
    let admission = Arc::new(tokio::sync::Semaphore::new(1));
    admission.close();
    let deadline = std::time::Instant::now() + STUCK_DEADLINE;
    let answer = router::run_blocking(state, admission, deadline, body_that_runs, json!({})).await;
    assert_eq!(answer, Err(ErrorKind::Internal));
}
