//! The 1Password backend through the server (#7519 P2): the delete sweep,
//! scope refusals before any spawn, doctor, the token, the enablement
//! gate, and the startup template sweep.
//!
//! A child of `server_tests`, so it shares that module's fixture: every path
//! is under a `TempDir` and `keychain` is an in-memory double. `onepassword`
//! is a backend over [`OpShim`], a fake `op` run by absolute path; no test
//! runs the real `op` or changes the process environment.
//!
//! Test: itself.

use super::*;
use crate::store::Capabilities;
use crate::store::onepassword::OnePasswordBackend;
use crate::store::onepassword::shim::{OpShim, install_op, plant_op};

const TOKEN: &str = "ops_token_canary_7519_server_0123456789";

/// A machine config that keeps the Keychain and enables 1Password.
const ENABLED: &str = "secrets:\n  default_backend: keychain\n  onepassword: {}\n";

/// A machine config that selects 1Password, which also enables it.
const SELECTED: &str = "secrets:\n  default_backend: onepassword\n";

/// Replace the machine config; the server rereads it per request.
fn machine(fx: &Fixture, yaml: &str) {
    std::fs::write(&fx.settings.machine_config, yaml).unwrap();
}

/// A factory for `fx` that maps `onepassword` to a backend over `shim`.
fn with_onepassword(fx: &Fixture, shim: &OpShim, token: Option<&str>) -> BackendFactory {
    let mut settings = shim.settings(&fx.settings.template_root);
    settings.token = token.map(SecretValue::new);
    let backend: Arc<dyn SecretBackend> = Arc::new(OnePasswordBackend::new(settings));
    let base = fx.backends();
    Arc::new(move |id: &BackendId| match id.as_str() {
        BackendId::ONEPASSWORD => Ok(Arc::clone(&backend)),
        _ => base(id),
    })
}

/// `{"project", "vault": "trusty/acme/web", "key"}` for `fx`.
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

/// Why: #7519 P1 carry-over (a) — a key `copy` placed in 1Password is
/// removed from it by a delete, though the project is configured for the
/// Keychain. Red before the sweep reached past `local_backends()`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_delete_sweeps_onepassword_when_the_machine_enables_it() {
    let fx = fixture();
    machine(&fx, ENABLED);
    let shim = OpShim::new();
    let server = fx.start_with(with_onepassword(&fx, &shim, None)).await;
    ok(set(&fx, "A").await);
    let copied = ok(call(
        &fx.settings.socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain",
               "to_backend": "onepassword", "keys": ["A"]}),
    )
    .await);
    assert_eq!(copied, json!({"copied": ["A"], "failed": []}));
    let items = shim.items();
    assert_eq!((items.len(), items[0].2.as_str()), (1, VALUE));

    let deleted = delete(&fx, "A").await;
    assert!(!wire(&deleted).contains(VALUE));
    assert_eq!(ok(deleted), json!({"removed": true}));
    assert!(shim.items().is_empty(), "1Password still holds the key");
    assert!(fx.keychain.is_empty());
    assert_eq!(listed(&fx).await, json!([]));
    server.stop().await;
}

/// Why: the sweep's cost — one `op` listing per delete — is paid only on a
/// machine that enables 1Password; elsewhere a delete never runs `op`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_delete_skips_onepassword_when_the_machine_does_not_enable_it() {
    let fx = fixture();
    let shim = OpShim::new();
    let server = fx.start_with(with_onepassword(&fx, &shim, None)).await;
    ok(set(&fx, "A").await);
    assert_eq!(ok(delete(&fx, "A").await), json!({"removed": true}));
    assert!(!shim.spawned(), "a delete ran `op` on a machine without it");
    server.stop().await;
}

/// Why: #7519 A7, #9328 R1–R3 — every scope refusal happens before the
/// 1Password backend runs: an out-of-scope vault (R1), a tracked override
/// outside the owner (R2), and a non-github.com remote (R3) leave the shim's
/// call log empty.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_scope_refusals_spawn_no_onepassword_process() {
    let fx = fixture();
    machine(&fx, SELECTED);
    let shim = OpShim::new();
    let server = fx.start_with(with_onepassword(&fx, &shim, None)).await;
    let socket = &fx.settings.socket;

    let other = json!({"project": fx.project(), "vault": "trusty/acme/other", "key": "K"});
    let mut set_other = other.clone();
    set_other["value"] = json!(VALUE);
    for (name, params) in [(method::SET, set_other), (method::DELETE, other)] {
        let response = call(socket, name, params).await;
        assert_eq!(fixed_error(&response, name), ErrorKind::VaultOutOfScope);
    }

    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "secrets:\n  vault: trusty/victim/prod-repo\n").unwrap();
    let response = set(&fx, "K").await;
    assert_eq!(
        fixed_error(&response, method::SET),
        ErrorKind::VaultOutOfScope
    );
    std::fs::remove_file(&config).unwrap();

    let evil = fx.tmp.path().join("evil");
    std::fs::create_dir(&evil).unwrap();
    git(&evil, &["init", "-q"]);
    git(
        &evil,
        &["remote", "add", "origin", "git@gitlab.com:acme/web.git"],
    );
    let params = json!({"project": evil.display().to_string(), "vault": "trusty/acme/web",
                        "key": "K", "value": VALUE});
    let response = call(socket, method::SET, params).await;
    assert_eq!(
        fixed_error(&response, method::SET),
        ErrorKind::RemoteHostUnsupported
    );
    assert!(
        !shim.spawned(),
        "a refused request ran `op`:\n{}",
        shim.calls()
    );
    server.stop().await;
}

/// Why: #7519 A9 — doctor lists 1Password, and finding it available never
/// runs `op`, let alone `op read`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_doctor_lists_onepassword_without_spawning() {
    let fx = fixture();
    machine(&fx, ENABLED);
    let shim = OpShim::new();
    let server = fx.start_with(with_onepassword(&fx, &shim, None)).await;
    for params in [Value::Null, json!({"project": fx.project()})] {
        let doctor: DoctorResponse =
            serde_json::from_value(ok(call(&fx.settings.socket, DOCTOR, params).await)).unwrap();
        let row = doctor
            .backends
            .iter()
            .find(|b| b.id == BackendId::onepassword())
            .expect("a 1Password row");
        assert!(row.available);
        assert_eq!(row.capabilities, ["READ", "WRITE"]);
    }
    assert!(!shim.spawned(), "doctor ran `op`:\n{}", shim.calls());
    server.stop().await;
}

/// Why: #7519 A10 — the service-account token reaches `op` through the
/// environment overlay and nowhere else: not argv, not a response, not the
/// audit log, including when a call fails.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_onepassword_token_never_reaches_the_wire_or_audit() {
    let fx = fixture();
    machine(&fx, SELECTED);
    let shim = OpShim::new();
    shim.headless(TOKEN);
    let server = fx
        .start_with(with_onepassword(&fx, &shim, Some(TOKEN)))
        .await;
    let mut responses = vec![set(&fx, "A").await, delete(&fx, "A").await];
    assert!(responses.iter().all(|r| r.error.is_none()), "{responses:?}");
    shim.fail(
        "list",
        "[ERROR] dial tcp: lookup my.1password.com: no such host",
    );
    let failed = set(&fx, "B").await;
    assert_eq!(fixed_error(&failed, method::SET), ErrorKind::BackendFailed);
    responses.push(failed);
    for response in &responses {
        let text = wire(response);
        assert!(!text.contains(TOKEN) && !text.contains(VALUE), "{text}");
    }
    let audit = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(!audit.is_empty(), "the calls left no audit records");
    assert!(!audit.contains(TOKEN) && !audit.contains(VALUE), "{audit}");
    assert!(
        fx.keychain.is_empty(),
        "the value fell back to the Keychain"
    );
    let overlay = format!(
        "{}={TOKEN}",
        crate::store::onepassword::SERVICE_ACCOUNT_TOKEN_ENV
    );
    assert!(shim.env_log().contains(&overlay));
    assert!(!shim.calls().contains(TOKEN));
    server.stop().await;
}

/// Why: #7519 A4 — signed out and headless with no token, a set through a
/// 1Password project is a locked error: no prompt, nothing written to the
/// Keychain or the index instead.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_onepassword_locked_never_falls_back() {
    let fx = fixture();
    machine(&fx, SELECTED);
    let shim = OpShim::new();
    shim.headless(TOKEN);
    let server = fx.start_with(with_onepassword(&fx, &shim, None)).await;
    let response = set(&fx, "A").await;
    assert_eq!(
        fixed_error(&response, method::SET),
        ErrorKind::BackendLocked
    );
    let response = delete(&fx, "A").await;
    assert_eq!(
        fixed_error(&response, method::DELETE),
        ErrorKind::BackendLocked
    );
    assert!(fx.keychain.is_empty());
    assert_eq!(listed(&fx).await, json!([]));
    server.stop().await;
}

/// Why: #7519 — a tracked project file may not choose the program the
/// server runs as `op`. `onepassword.program` there is
/// `TrackedCliSettingRefused` before any backend runs: the shim and the
/// planted program never run, and the reply never echoes the path.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_tracked_onepassword_program_is_refused_before_any_spawn() {
    let fx = fixture();
    machine(&fx, SELECTED);
    let shim = OpShim::new();
    let marker = fx.tmp.path().join("planted-ran");
    let planted = plant_op(&fx.tmp.path().join("planted"), &marker);
    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let yaml = format!(
        "secrets:\n  backend: onepassword\n  onepassword:\n    program: '{}'\n",
        planted.display()
    );
    std::fs::write(&config, yaml).unwrap();
    let server = fx.start_with(with_onepassword(&fx, &shim, None)).await;
    let response = set(&fx, "A").await;
    let text = wire(&response);
    assert!(
        !text.contains(&planted.display().to_string()) && !text.contains(VALUE),
        "{text}"
    );
    assert_eq!(
        fixed_error(&response, method::SET),
        ErrorKind::TrackedCliSettingRefused
    );
    assert!(!shim.spawned(), "a refused request ran `op`");
    assert!(!marker.exists(), "the planted program ran");
    server.stop().await;
}

/// Why: #7519 P1 carry-over (a) — the production factory opens 1Password
/// only when the machine config enables it, so a tracked `backend:
/// onepassword` alone cannot aim the server at an account, and nothing is
/// written where the delete sweep does not reach. Opening spawns nothing;
/// #7524 P2-M2: it finds `op` in the directory list handed in, never a `PATH`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_backends_for_opens_onepassword_only_when_enabled() {
    let fx = fixture();
    let op_dir = fx.tmp.path().join("op-bin");
    install_op(&op_dir, "exit 1");
    let op_dirs = vec![op_dir];
    // The fixture's machine config is also the account's own file here.
    let account = Some(fx.settings.machine_config.clone());
    let factory = router::backends_with(
        account.clone(),
        &fx.settings,
        Some(SecretValue::new(TOKEN)),
        op_dirs.clone(),
    );
    let err = factory(&BackendId::onepassword()).unwrap_err();
    assert!(
        matches!(err, SecretsError::BackendNotEnabled { .. }),
        "{err:?}"
    );

    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "secrets:\n  backend: onepassword\n").unwrap();
    let server = fx
        .start_with(router::backends_with(account, &fx.settings, None, op_dirs))
        .await;
    let response = set(&fx, "A").await;
    assert_eq!(
        fixed_error(&response, method::SET),
        ErrorKind::BackendNotEnabled
    );
    server.stop().await;

    machine(&fx, ENABLED);
    let opened = factory(&BackendId::onepassword()).unwrap();
    assert_eq!(opened.id(), BackendId::onepassword());
    assert_eq!(
        opened.capabilities(),
        Capabilities::READ | Capabilities::WRITE
    );
    assert!(!format!("{opened:?}").contains(TOKEN));
}

/// A spawner-chosen machine config that enables 1Password and pins `program`
/// to a planted `op` that creates `marker` if it ever runs.
fn spawner_enables_planted_op(fx: &Fixture, marker: &Path) {
    let planted = plant_op(&fx.tmp.path().join("planted"), marker);
    machine(
        fx,
        &format!(
            "secrets:\n  default_backend: keychain\n  onepassword:\n    program: {}\n",
            planted.display()
        ),
    );
}

/// A server whose `onepassword` opens through the production factory with
/// `account` as the account's own machine config, and whose other backends
/// are the fixture's in-memory doubles.
async fn start_with_account(fx: &Fixture, account: Option<PathBuf>) -> Running {
    let production = router::backends_with(account.clone(), &fx.settings, None, Vec::new());
    let base = fx.backends();
    let factory: BackendFactory = Arc::new(move |id: &BackendId| match id.as_str() {
        BackendId::ONEPASSWORD => production(id),
        _ => base(id),
    });
    let mut state = fx.state(factory);
    state.file_consent_config = account;
    fx.start_state(state).await
}

/// Why: #7519, ruling 74 — `--machine-config` and `$HOME` are the spawner's
/// to choose, so a file there that enables 1Password and pins `program`
/// neither opens the backend nor runs that program; only the account's own
/// machine config may. Red before the factory and the delete sweep read
/// enablement from the account's file.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_onepassword_enablement_ignores_a_spawner_chosen_machine_config() {
    let fx = fixture();
    let marker = fx.tmp.path().join("planted-ran");
    spawner_enables_planted_op(&fx, &marker);
    let account = fx.tmp.path().join("account").join("config.yaml");
    std::fs::create_dir_all(account.parent().unwrap()).unwrap();
    std::fs::write(&account, "secrets:\n  default_backend: keychain\n").unwrap();

    let factory = router::backends_with(
        Some(account.clone()),
        &fx.settings,
        Some(SecretValue::new(TOKEN)),
        Vec::new(),
    );
    let err = factory(&BackendId::onepassword()).unwrap_err();
    assert!(
        matches!(err, SecretsError::BackendNotEnabled { .. }),
        "{err:?}"
    );

    let server = start_with_account(&fx, Some(account)).await;
    ok(set(&fx, "A").await);
    assert_eq!(ok(delete(&fx, "A").await), json!({"removed": true}));
    server.stop().await;
    assert!(!marker.exists(), "the spawner-pinned program ran");
}

/// Why: #7519, ruling 74 — an account machine config that cannot be read,
/// or no account home at all, leaves 1Password off whatever the spawner's
/// file says, and set and list still serve the other backends. A delete
/// refuses instead, before any backend is touched: 1Password may still hold
/// a copy from when the file was readable. Red while delete swept without
/// 1Password and removed the row.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_onepassword_is_off_when_the_account_config_is_unreadable() {
    let fx = fixture();
    let marker = fx.tmp.path().join("planted-ran");
    spawner_enables_planted_op(&fx, &marker);
    // A directory where the file belongs: present, but not readable as one.
    let account = fx.tmp.path().join("account-dir");
    std::fs::create_dir(&account).unwrap();
    assert!(crate::store::config::load_machine_at(&account).is_err());

    for config in [Some(account.clone()), None] {
        let factory = router::backends_with(config.clone(), &fx.settings, None, Vec::new());
        let err = factory(&BackendId::onepassword()).unwrap_err();
        assert!(
            matches!(err, SecretsError::BackendNotEnabled { .. }),
            "{config:?}: {err:?}"
        );
    }

    let server = start_with_account(&fx, Some(account)).await;
    ok(set(&fx, "A").await);
    assert_eq!(listed(&fx).await[0]["name"], json!("A"));
    // #7519: an unreadable account file refuses the delete; the row stays.
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
    assert!(!marker.exists(), "the spawner-pinned program ran");
}

/// Why: #7519 — an account machine config that does not parse refuses a
/// delete before any backend is touched, and the index row stays; a missing
/// one still means "skip 1Password", so the same delete then succeeds.
/// Red while delete read a parse error as "1Password off".
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_delete_refuses_when_the_account_config_does_not_parse() {
    let fx = fixture();
    let account = fx.tmp.path().join("account").join("config.yaml");
    std::fs::create_dir_all(account.parent().unwrap()).unwrap();
    std::fs::write(&account, "secrets: [unclosed\n").unwrap();
    assert!(crate::store::config::load_machine_at(&account).is_err());

    let server = start_with_account(&fx, Some(account.clone())).await;
    ok(set(&fx, "A").await);
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

    std::fs::remove_file(&account).unwrap();
    assert_eq!(ok(delete(&fx, "A").await), json!({"removed": true}));
    assert_eq!(listed(&fx).await, json!([]));
    server.stop().await;
}

/// Why: #7519 owner ruling — a crash skips the template guard's drop and
/// leaves a value on disk; server startup removes a dead process's
/// template directory and leaves a live one's alone.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_startup_sweeps_stale_template_dirs() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture();
    let root = fx.settings.template_root.clone();
    let private = |dir: &Path| {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    private(root.as_path());
    let mut child = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap();
    let dead = child.id();
    child.wait().unwrap();
    let stale = root.join(format!("tpl.{dead}.1.0"));
    let live = root.join(format!("tpl.{}.2.0", std::process::id()));
    for dir in [&stale, &live] {
        private(dir.as_path());
        std::fs::write(dir.join("template.json"), VALUE).unwrap();
    }
    let server = fx.start().await;
    assert!(
        !stale.exists(),
        "a dead process's template survived startup"
    );
    assert!(
        live.join("template.json").exists(),
        "a live template was removed"
    );
    server.stop().await;
}

/// Why: #7519 Fail-Open Check — a startup sweep failure is only reported,
/// and the server keeps serving, because the next template write refuses
/// the same directory. With a symlinked template root the server answers,
/// the leftover behind the link is not followed, and an edit through 1Password
/// is `StorageRefused` with no template written and no `op item edit` run.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_template_sweep_failure_keeps_serving_and_refuses_the_edit() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture();
    machine(&fx, SELECTED);
    let elsewhere = fx.tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = fx.settings.template_root.clone();
    std::fs::create_dir_all(root.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &root).unwrap();
    let mut child = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap();
    let dead = child.id();
    child.wait().unwrap();
    let stale = elsewhere.join(format!("tpl.{dead}.1.0"));
    std::fs::create_dir(&stale).unwrap();
    std::fs::write(stale.join("template.json"), VALUE).unwrap();

    let shim = OpShim::new();
    shim.seed("item1", "A", "old-value", "PASSWORD");
    let server = fx.start_with(with_onepassword(&fx, &shim, None)).await;
    assert_eq!(listed(&fx).await, json!([]), "the server stopped serving");
    assert!(stale.exists(), "the sweep followed the symlinked root");

    let response = set(&fx, "A").await;
    assert_eq!(
        fixed_error(&response, method::SET),
        ErrorKind::StorageRefused
    );
    assert!(!shim.calls().contains("item edit"), "{}", shim.calls());
    assert_eq!(shim.template_log(), "");
    let made: Vec<_> = std::fs::read_dir(&elsewhere)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(
        made,
        vec![stale.clone()],
        "a template landed behind the link"
    );
    server.stop().await;
}

/// A factory for `fx` that maps `onepassword` to a backend over `shim`
/// whose calls each get the production 60 s timeout.
fn with_slow_onepassword(fx: &Fixture, shim: &OpShim) -> BackendFactory {
    let mut settings = shim.settings(&fx.settings.template_root);
    settings.timeout = crate::store::onepassword::DEFAULT_TIMEOUT;
    let backend: Arc<dyn SecretBackend> = Arc::new(OnePasswordBackend::new(settings));
    let base = fx.backends();
    Arc::new(move |id: &BackendId| match id.as_str() {
        BackendId::ONEPASSWORD => Ok(Arc::clone(&backend)),
        _ => base(id),
    })
}

/// Why: #7524 P2-M1 — the client waited 30 s while one `op` call may take
/// 60 s, so a slow 1Password set was reported as a transport timeout and
/// then committed. Through the real client, a set whose `op item create`
/// takes 31 s now reports its real outcome, and the key is listed. Red
/// while the client's wait was 30 s.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_onepassword_set_slower_than_the_old_client_wait_reports_its_outcome() {
    let fx = fixture();
    machine(&fx, SELECTED);
    let shim = OpShim::new();
    shim.delay("create", 31);
    let server = fx.start_with(with_slow_onepassword(&fx, &shim)).await;
    let client = crate::server::OnDemandSecrets::at(&fx.settings.socket)
        .with_program(fx.tmp.path().join("no-such-trusty-secrets"));
    let mut params = target(&fx, "A");
    params["value"] = json!(VALUE);

    let started = std::time::Instant::now();
    let answer = client.call(method::SET, params).await;
    assert!(started.elapsed() >= Duration::from_secs(31));
    let result = answer.expect("the client gave up before the server answered");
    assert_eq!(result["outcome"], json!("new"), "{result}");
    assert!(!result.to_string().contains(VALUE));
    assert_eq!(shim.items().len(), 1);
    assert_eq!(listed(&fx).await[0]["name"], json!("A"));
    server.stop().await;
}

/// Why: #7524 P2-M1 — a request that runs past the server's deadline gets a
/// definite `deadline_exceeded` answer while the client still waits, and
/// nothing commits afterwards: the `op` it started is killed with its group,
/// so no item appears later, and no index row is written.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_request_past_its_deadline_is_a_definite_error_and_commits_nothing() {
    let fx = fixture();
    machine(&fx, SELECTED);
    let shim = OpShim::new();
    shim.delay("create", 4);
    let mut state = fx.state(with_slow_onepassword(&fx, &shim));
    state.deadline_override = Some(Duration::from_secs(1));
    let server = fx.start_state(state).await;

    let started = std::time::Instant::now();
    let response = set(&fx, "A").await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        fixed_error(&response, method::SET),
        ErrorKind::DeadlineExceeded
    );
    assert!(!wire(&response).contains(VALUE));
    assert!(
        wire(&response).contains("may have landed"),
        "{}",
        wire(&response)
    );

    // Past the shim's own sleep: the killed `op` never stored the item.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(shim.items().is_empty(), "the write landed after the reply");
    assert_eq!(listed(&fx).await, json!([]));
    server.stop().await;
}

/// Why: #7524 P2-M1 — a `copy` reaches a vendor CLI once per key, so it can
/// outlast any fixed wait. Past the deadline it starts no further key, and
/// the reply names every key that was not copied.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_past_its_deadline_starts_no_further_key() {
    let fx = fixture();
    machine(&fx, ENABLED);
    let shim = OpShim::new();
    shim.delay("create", 3);
    let mut state = fx.state(with_slow_onepassword(&fx, &shim));
    state.deadline_override = Some(Duration::from_secs(1));
    let server = fx.start_state(state).await;
    ok(set(&fx, "A").await);
    ok(set(&fx, "B").await);

    let copied = ok(call(
        &fx.settings.socket,
        method::COPY,
        json!({"project": fx.project(), "from_backend": "keychain",
               "to_backend": "onepassword", "keys": ["A", "B"]}),
    )
    .await);
    assert_eq!(copied, json!({"copied": [], "failed": ["A", "B"]}));
    let calls = shim.calls();
    assert_eq!(
        calls.lines().count(),
        2,
        "B started after the deadline: {calls}"
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(shim.items().is_empty(), "a copy landed after the reply");
    server.stop().await;
}
