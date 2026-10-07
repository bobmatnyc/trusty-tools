//! The Keeper backend through the server (#7519 P3): the delete sweep,
//! doctor, the locked reply, the tracked-setting refusal, and enablement
//! read only from the account's own machine config. Mirrors
//! `onepassword_tests.rs`.
//!
//! A child of `server_tests`, so it shares that module's fixture: every path
//! is under a `TempDir` and `keychain` is an in-memory double. `keeper` is a
//! backend over [`KeeperShim`], a fake `keeper` run by absolute path; no test
//! runs the real `keeper` or changes the process environment.
//!
//! Test: itself.

use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::store::Capabilities;
use crate::store::cli::test_shim::install_script;
use crate::store::keeper::shim::KeeperShim;

/// A machine config that keeps the Keychain and enables Keeper.
const ENABLED: &str = "secrets:\n  default_backend: keychain\n  keeper: {}\n";

/// A machine config that selects Keeper, which also enables it.
const SELECTED: &str = "secrets:\n  default_backend: keeper\n";

/// Replace the machine config; the server rereads it per request.
fn machine(fx: &Fixture, yaml: &str) {
    std::fs::write(&fx.settings.machine_config, yaml).unwrap();
}

/// A factory for `fx` that maps `keeper` to a backend over `shim`.
fn with_keeper(fx: &Fixture, shim: &KeeperShim) -> BackendFactory {
    let backend: Arc<dyn SecretBackend> = Arc::new(shim.backend());
    let base = fx.backends();
    Arc::new(move |id: &BackendId| match id.as_str() {
        BackendId::KEEPER => Ok(Arc::clone(&backend)),
        _ => base(id),
    })
}

fn target(fx: &Fixture, name: &str) -> Value {
    json!({"project": fx.project(), "vault": "trusty/acme/web", "key": name})
}

async fn set(fx: &Fixture, name: &str) -> RpcResponse {
    let mut params = target(fx, name);
    params["value"] = json!(VALUE);
    call(&fx.settings.socket, method::SET, params).await
}

async fn delete(fx: &Fixture, name: &str) -> RpcResponse {
    call(&fx.settings.socket, method::DELETE, target(fx, name)).await
}

async fn listed(fx: &Fixture) -> Value {
    let list = ok(call(
        &fx.settings.socket,
        method::LIST,
        json!({"project": fx.project(), "vault": "trusty/acme/web"}),
    )
    .await);
    list["keys"].clone()
}

/// A 0600 Commander config file and an executable `keeper` running `shim`,
/// under `fx`'s temp dir: what a production open needs.
fn provision(fx: &Fixture, shim: &KeeperShim) -> String {
    let config = fx.tmp.path().join("keeper-config.json");
    std::fs::write(&config, "{}").unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    let program = shim.install_in(&fx.tmp.path().join("keeper-bin"));
    format!(
        "secrets:\n  default_backend: keychain\n  keeper:\n    program: {}\n    config_path: {}\n",
        program.display(),
        config.display()
    )
}

/// Why: #7519 P3 ruling 5 — a key `copy` placed in Keeper is removed by a
/// delete, though the project is configured for the Keychain; the record
/// goes to Keeper's trash. The value never reaches argv or the env.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_delete_sweeps_keeper_when_the_machine_enables_it() {
    let fx = fixture();
    machine(&fx, ENABLED);
    let shim = KeeperShim::new();
    let server = fx.start_with(with_keeper(&fx, &shim)).await;
    ok(set(&fx, "A").await);
    let copied = ok(call(
        &fx.settings.socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain",
               "to_backend": "keeper", "keys": ["A"]}),
    )
    .await);
    assert_eq!(copied, json!({"copied": ["A"], "failed": []}));
    let records = shim.records();
    assert_eq!((records.len(), records[0].2.as_str()), (1, VALUE));
    assert!(!shim.calls().contains(VALUE) && !shim.env_log().contains(VALUE));

    let deleted = delete(&fx, "A").await;
    assert!(!wire(&deleted).contains(VALUE));
    assert_eq!(ok(deleted), json!({"removed": true}));
    assert!(shim.records().is_empty(), "Keeper still holds the key");
    assert_eq!(shim.trash().len(), 1);
    assert!(fx.keychain.is_empty());
    assert_eq!(listed(&fx).await, json!([]));
    server.stop().await;
}

/// Why: the sweep's cost is paid only on a machine that enables Keeper.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_delete_skips_keeper_when_the_machine_does_not_enable_it() {
    let fx = fixture();
    let shim = KeeperShim::new();
    let server = fx.start_with(with_keeper(&fx, &shim)).await;
    ok(set(&fx, "A").await);
    assert_eq!(ok(delete(&fx, "A").await), json!({"removed": true}));
    assert!(
        !shim.spawned(),
        "a delete ran `keeper` on a machine without it"
    );
    server.stop().await;
}

/// Why: A9 — doctor lists Keeper, and finding it available never runs
/// `keeper`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_doctor_lists_keeper_without_spawning() {
    let fx = fixture();
    machine(&fx, ENABLED);
    let shim = KeeperShim::new();
    let server = fx.start_with(with_keeper(&fx, &shim)).await;
    let doctor: DoctorResponse =
        serde_json::from_value(ok(call(&fx.settings.socket, DOCTOR, Value::Null).await)).unwrap();
    let row = doctor
        .backends
        .iter()
        .find(|b| b.id == BackendId::keeper())
        .expect("a Keeper row");
    assert!(row.available);
    assert_eq!(row.capabilities, ["READ", "WRITE"]);
    assert!(!shim.spawned(), "doctor ran `keeper`:\n{}", shim.calls());
    server.stop().await;
}

/// Why: ruling 6 — a logged-out or unapproved `keeper` is `backend_locked`
/// on the wire, the reply names the device-approval step, and nothing
/// falls back to the Keychain or the index.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_keeper_locked_never_falls_back() {
    let fx = fixture();
    machine(&fx, SELECTED);
    let shim = KeeperShim::new();
    shim.locked("Device is not approved. Approve it and log in.");
    let server = fx.start_with(with_keeper(&fx, &shim)).await;
    for (response, name) in [
        (set(&fx, "A").await, method::SET),
        (delete(&fx, "A").await, method::DELETE),
    ] {
        assert_eq!(fixed_error(&response, name), ErrorKind::BackendLocked);
        let text = wire(&response);
        assert!(
            text.contains("approve this device") && !text.contains(VALUE),
            "{text}"
        );
    }
    assert!(fx.keychain.is_empty());
    assert_eq!(listed(&fx).await, json!([]));
    server.stop().await;
}

/// Why: #7519 — a tracked project file may not choose the program run as
/// `keeper`, nor its config file: `TrackedCliSettingRefused` before any
/// backend runs, and the reply never echoes the path.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_tracked_keeper_settings_are_refused_before_any_spawn() {
    let fx = fixture();
    machine(&fx, SELECTED);
    let shim = KeeperShim::new();
    let marker = fx.tmp.path().join("planted-ran");
    let planted = install_script(
        &fx.tmp.path().join("planted"),
        "keeper",
        &format!(": > '{}'\nexit 0", marker.display()),
    );
    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let server = fx.start_with(with_keeper(&fx, &shim)).await;
    for setting in ["program", "config_path"] {
        let yaml = format!(
            "secrets:\n  backend: keeper\n  keeper:\n    {setting}: '{}'\n",
            planted.display()
        );
        std::fs::write(&config, yaml).unwrap();
        let response = set(&fx, "A").await;
        let text = wire(&response);
        assert!(!text.contains(&planted.display().to_string()), "{text}");
        assert_eq!(
            fixed_error(&response, method::SET),
            ErrorKind::TrackedCliSettingRefused
        );
    }
    assert!(!shim.spawned(), "a refused request ran `keeper`");
    assert!(!marker.exists(), "the planted program ran");
    server.stop().await;
}

/// Why: #7519 P1 carry-over (a), ruling 74 — the production factory opens
/// Keeper only when the account's machine config enables it, with the
/// program and Commander config file that file names; a tracked `backend:
/// keeper` alone is `backend_not_enabled`. Opening spawns nothing.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_backends_for_opens_keeper_only_when_enabled() {
    let fx = fixture();
    let shim = KeeperShim::new();
    let provisioned = provision(&fx, &shim);
    let account = Some(fx.settings.machine_config.clone());
    let factory = router::backends_with(account.clone(), &fx.settings, None, None);
    let err = factory(&BackendId::keeper()).unwrap_err();
    assert!(
        matches!(err, SecretsError::BackendNotEnabled { .. }),
        "{err:?}"
    );

    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "secrets:\n  backend: keeper\n").unwrap();
    let server = fx
        .start_with(router::backends_with(account, &fx.settings, None, None))
        .await;
    let response = set(&fx, "A").await;
    assert_eq!(
        fixed_error(&response, method::SET),
        ErrorKind::BackendNotEnabled
    );
    server.stop().await;

    machine(&fx, &provisioned);
    let opened = factory(&BackendId::keeper()).unwrap();
    assert_eq!(opened.id(), BackendId::keeper());
    assert_eq!(
        opened.capabilities(),
        Capabilities::READ | Capabilities::WRITE
    );
    assert!(!shim.spawned(), "opening ran `keeper`");
    let read = opened.get(&vault("trusty/acme/web"), &key("A")).unwrap();
    assert!(read.is_none());
    assert!(
        shim.calls()
            .contains("--batch-mode ls --format json trusty/acme/web")
    );
}

/// A server whose `keeper` opens through the production factory with
/// `account` as the account's own machine config.
async fn start_with_account(fx: &Fixture, account: Option<PathBuf>) -> Running {
    let production = router::backends_with(account.clone(), &fx.settings, None, None);
    let base = fx.backends();
    let factory: BackendFactory = Arc::new(move |id: &BackendId| match id.as_str() {
        BackendId::KEEPER => production(id),
        _ => base(id),
    });
    let mut state = fx.state(factory);
    state.file_consent_config = account;
    fx.start_state(state).await
}

/// Why: ruling 74 — `--machine-config` and `$HOME` are the spawner's to
/// choose, so a file there that enables Keeper and pins `program` neither
/// opens the backend nor runs that program; only the account's own file
/// may.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_keeper_enablement_ignores_a_spawner_chosen_machine_config() {
    let fx = fixture();
    let shim = KeeperShim::new();
    machine(&fx, &provision(&fx, &shim));
    let account = fx.tmp.path().join("account").join("config.yaml");
    std::fs::create_dir_all(account.parent().unwrap()).unwrap();
    std::fs::write(&account, "secrets:\n  default_backend: keychain\n").unwrap();

    let factory = router::backends_with(Some(account.clone()), &fx.settings, None, None);
    let err = factory(&BackendId::keeper()).unwrap_err();
    assert!(
        matches!(err, SecretsError::BackendNotEnabled { .. }),
        "{err:?}"
    );

    let server = start_with_account(&fx, Some(account)).await;
    ok(set(&fx, "A").await);
    assert_eq!(ok(delete(&fx, "A").await), json!({"removed": true}));
    server.stop().await;
    assert!(!shim.spawned(), "the spawner-pinned program ran");
}

/// Why: ruling 74 and de69ae008d — an account machine config that cannot be
/// read, or no account home, leaves Keeper off whatever the spawner's file
/// says; set and list still serve the other backends, and a delete refuses
/// before any backend is touched, since Keeper may hold a copy.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_keeper_is_off_when_the_account_config_is_unreadable() {
    let fx = fixture();
    let shim = KeeperShim::new();
    machine(&fx, &provision(&fx, &shim));
    let account = fx.tmp.path().join("account-dir");
    std::fs::create_dir(&account).unwrap();
    assert!(crate::store::config::load_machine_at(&account).is_err());

    for config in [Some(account.clone()), None] {
        let factory = router::backends_with(config.clone(), &fx.settings, None, None);
        let err = factory(&BackendId::keeper()).unwrap_err();
        assert!(
            matches!(err, SecretsError::BackendNotEnabled { .. }),
            "{config:?}: {err:?}"
        );
    }

    let server = start_with_account(&fx, Some(account)).await;
    ok(set(&fx, "A").await);
    assert_eq!(listed(&fx).await[0]["name"], json!("A"));
    let refused = delete(&fx, "A").await;
    assert_eq!(
        fixed_error(&refused, method::DELETE),
        ErrorKind::ConfigInvalid
    );
    assert_eq!(listed(&fx).await[0]["name"], json!("A"));
    assert!(
        !fx.keychain.is_empty(),
        "the refused delete touched a backend"
    );
    server.stop().await;
    assert!(!shim.spawned(), "the spawner-pinned program ran");
}

/// Why: ruling 1 — through the server, a set into Keeper keeps the value
/// out of argv, the env, every reply and the audit log, including when the
/// batch fails and `keeper` echoes it.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_keeper_value_never_reaches_argv_env_wire_or_audit() {
    let fx = fixture();
    machine(&fx, SELECTED);
    let shim = KeeperShim::new();
    let server = fx.start_with(with_keeper(&fx, &shim)).await;
    let mut responses = vec![set(&fx, "A").await];
    assert!(responses[0].error.is_none(), "{responses:?}");
    shim.fail("batch", "Error: batch failed");
    let failed = set(&fx, "B").await;
    assert_eq!(fixed_error(&failed, method::SET), ErrorKind::BackendFailed);
    responses.push(failed);
    responses.push(delete(&fx, "A").await);
    let audit = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    for text in responses
        .iter()
        .map(wire)
        .chain([audit, shim.calls(), shim.env_log()])
    {
        assert!(!text.contains(VALUE), "{text}");
    }
    assert!(shim.stdin_log().contains("password=$BASE64:"));
    assert!(
        fx.keychain.is_empty(),
        "the value fell back to the Keychain"
    );
    server.stop().await;
}
