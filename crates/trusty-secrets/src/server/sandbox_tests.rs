//! The sandbox consent override and backend refusal (#9793).
//!
//! A child of `server_tests`, so it shares that module's fixture. No test
//! reads the process environment or the password database: each builds a
//! [`SandboxEnv`] and an account home inside the fixture's `TempDir` and
//! hands both to `State::new_in`. `keychain` is the in-memory double, so a
//! test that counts its opens proves the factory was never asked for it.
//!
//! Test: itself.

use std::ffi::OsString;
use std::sync::Mutex;

use super::*;
use crate::server::sandbox::{SandboxEnv, file_consent_config_for, sandbox_consent_config};
use crate::store::config::MACHINE_CONFIG_SUBPATH;
use crate::store::{FileBackend, VALUES_SUBDIR};

/// One sandbox layout under a fixture: the account home, the sandbox home
/// and the data directory, all existing directories.
struct Layout {
    account: PathBuf,
    home: PathBuf,
    data: PathBuf,
}

impl Layout {
    fn new(fx: &Fixture) -> Self {
        let account = fx.tmp.path().join("account-home");
        let home = fx.tmp.path().join("sandbox").join("home");
        let data = fx.tmp.path().join("sandbox").join("data");
        for dir in [&account, &home, &data] {
            std::fs::create_dir_all(dir).unwrap();
        }
        Self {
            account,
            home,
            data,
        }
    }

    /// The variables the launcher pins: flag `1`, the data dir, the home.
    fn env(&self) -> SandboxEnv {
        SandboxEnv {
            flag: Some("1".into()),
            data_dir_override: Some(self.data.clone().into()),
            home: Some(self.home.clone().into()),
        }
    }

    /// The sandbox home's machine config, as the override names it.
    fn consent(&self) -> PathBuf {
        self.home
            .canonicalize()
            .unwrap()
            .join(MACHINE_CONFIG_SUBPATH)
    }

    /// The account home's machine config, the consent outside a sandbox.
    fn account_consent(&self) -> PathBuf {
        self.account.join(MACHINE_CONFIG_SUBPATH)
    }

    /// Write the sandbox home's machine config selecting `backend`.
    fn select(&self, backend: &str) {
        let path = self.home.join(MACHINE_CONFIG_SUBPATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("secrets:\n  default_backend: {backend}\n")).unwrap();
    }

    /// Remove the sandbox home's machine config.
    fn unselect(&self) {
        let _ = std::fs::remove_file(self.home.join(MACHINE_CONFIG_SUBPATH));
    }

    /// The `file` backend production opens under this sandbox home.
    fn file(&self) -> FileBackend {
        FileBackend::at(self.home.join(VALUES_SUBDIR))
    }
}

/// A Keychain-build state for `fx` under `env`, with `settings`.
fn sandbox_state(
    settings: ServerSettings,
    backends: BackendFactory,
    env: &SandboxEnv,
    layout: &Layout,
) -> State {
    let mut state = State::new_in(settings, backends, env, Some(&layout.account));
    state.keychain_compiled = true;
    state
}

/// Settings whose `--machine-config` is the sandbox home's file, as the
/// binary resolves it from `$HOME` in a sandbox.
fn home_settings(fx: &Fixture, layout: &Layout) -> ServerSettings {
    let mut settings = fx.settings.clone();
    settings.machine_config = layout.home.join(MACHINE_CONFIG_SUBPATH);
    settings
}

/// A factory mapping `keychain`, `onepassword` and `keeper` to in-memory
/// doubles and `file` to `file`, recording every id it is asked for.
fn recording(fx: &Fixture, file: FileBackend) -> (BackendFactory, Arc<Mutex<Vec<String>>>) {
    let opened = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&opened);
    let keychain = Arc::clone(&fx.keychain);
    let cli: Arc<dyn SecretBackend> = Arc::new(MemoryBackend::new());
    let factory: BackendFactory = Arc::new(move |id: &BackendId| {
        seen.lock().unwrap().push(id.to_string());
        let backend: Arc<dyn SecretBackend> = match id.as_str() {
            "keychain" => keychain.clone(),
            "file" => Arc::new(file.clone()),
            "onepassword" | "keeper" => cli.clone(),
            _ => {
                return Err(SecretsError::UnknownBackend {
                    backend: id.to_string(),
                });
            }
        };
        Ok(backend)
    });
    (factory, opened)
}

fn set_params(fx: &Fixture, name: &str) -> Value {
    json!({"project": fx.project(), "vault": "trusty/acme/web", "key": name, "value": VALUE})
}

/// Why: #9793, ruling Q1 — only `TRUSTY_SANDBOX` set to exactly `1` turns
/// the override on, read by trusty-common's `sandbox_flag_set`. Unset, `0`,
/// `true`, empty, padded and non-UTF-8 values each give no override, so
/// consent stays the account home's file. The `1` row is the control.
/// Red before the fix: the control gives no override.
/// Test: itself.
#[test]
fn sandbox_consent_is_none_without_the_flag() {
    use std::os::unix::ffi::OsStringExt;
    let fx = fixture();
    let layout = Layout::new(&fx);
    let account = Some(layout.account.as_path());
    let mut env = layout.env();
    assert_eq!(
        sandbox_consent_config(&env, account),
        Some(layout.consent()),
        "control: the launcher's variables give the override"
    );
    let bad: [Option<OsString>; 6] = [
        None,
        Some("0".into()),
        Some("true".into()),
        Some("".into()),
        Some(" 1".into()),
        Some(OsString::from_vec(vec![0x31, 0xff])),
    ];
    for flag in bad {
        env.flag = flag.clone();
        assert_eq!(sandbox_consent_config(&env, account), None, "{flag:?}");
        assert_eq!(
            file_consent_config_for(&env, account),
            Some(layout.account_consent()),
            "{flag:?}"
        );
    }
}

/// Why: #9793, ruling Q1 — `TRUSTY_DATA_DIR_OVERRIDE` must be set, absolute
/// and an existing directory. Unset, empty, relative, missing and a plain
/// file each give no override. The launcher's value is the control.
/// Red before the fix: the control gives no override.
/// Test: itself.
#[test]
fn sandbox_consent_is_none_without_a_data_dir_override() {
    let fx = fixture();
    let layout = Layout::new(&fx);
    let account = Some(layout.account.as_path());
    let mut env = layout.env();
    assert_eq!(
        sandbox_consent_config(&env, account),
        Some(layout.consent()),
        "control"
    );
    let plain = fx.tmp.path().join("plain-file");
    std::fs::write(&plain, "").unwrap();
    let bad: [Option<OsString>; 5] = [
        None,
        Some("".into()),
        Some("sandbox/data".into()),
        Some(fx.tmp.path().join("missing").into()),
        Some(plain.into()),
    ];
    for data in bad {
        env.data_dir_override = data.clone();
        assert_eq!(sandbox_consent_config(&env, account), None, "{data:?}");
        assert_eq!(
            file_consent_config_for(&env, account),
            Some(layout.account_consent()),
            "{data:?}"
        );
    }
}

/// Why: #9793, rulings Q1 and Q3 — with the flag and a data dir set but
/// `HOME` the account's own home (directly or through a symlink), or `HOME`
/// relative or missing, or no account home known, there is no override:
/// the real home is never a sandbox, and consent stays the account's file.
/// A distinct sandbox home is the control.
/// Red before the fix: the control gives no override. Mutation: drop the
/// `HOME` != account-home guard and the first case goes red.
/// Test: itself.
#[test]
fn sandbox_consent_is_none_when_home_is_the_account_home() {
    let fx = fixture();
    let layout = Layout::new(&fx);
    let account = Some(layout.account.as_path());
    let mut env = layout.env();
    assert_eq!(
        file_consent_config_for(&env, account),
        Some(layout.consent()),
        "control"
    );
    let link = fx.tmp.path().join("home-link");
    std::os::unix::fs::symlink(&layout.account, &link).unwrap();
    let bad: [Option<OsString>; 5] = [
        Some(layout.account.clone().into()),
        Some(link.into()),
        Some("sandbox/home".into()),
        Some(fx.tmp.path().join("no-such-home").into()),
        None,
    ];
    for home in bad {
        env.home = home.clone();
        assert_eq!(sandbox_consent_config(&env, account), None, "{home:?}");
        assert_eq!(
            file_consent_config_for(&env, account),
            Some(layout.account_consent()),
            "{home:?}"
        );
    }
    // No account home: no override and no consent at all.
    let env = layout.env();
    assert_eq!(sandbox_consent_config(&env, None), None);
    assert_eq!(file_consent_config_for(&env, None), None);
}

/// Why: #9793 — the point of the override. Under the flag, a sandbox home
/// config selecting `file` consents to `set` into `file` on a Keychain
/// build, and the value lands under the sandbox home.
/// Red before the fix: `file_backend_not_selected`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_set_into_file_is_allowed_under_the_sandbox_flag() {
    let fx = fixture();
    let layout = Layout::new(&fx);
    layout.select("file");
    let file = layout.file();
    let (factory, opened) = recording(&fx, file.clone());
    let state = sandbox_state(home_settings(&fx, &layout), factory, &layout.env(), &layout);
    assert_eq!(state.file_consent_config, Some(layout.consent()));
    let server = fx.start_state(state).await;
    let set = call(&fx.settings.socket, method::SET, set_params(&fx, "A")).await;
    server.stop().await;

    ok(set);
    let project = vault("trusty/acme/web");
    assert_eq!(
        file.get(&project, &key("A")).unwrap().unwrap().expose(),
        VALUE
    );
    assert!(file.root().starts_with(&layout.home));
    assert!(fx.keychain.is_empty());
    assert!(!opened.lock().unwrap().iter().any(|id| id == "keychain"));
}

/// Why: #9793 — the override moves consent, it does not grant it. Under the
/// flag, a sandbox config naming `keychain`, or none at all, still refuses
/// `set` into `file` (selected here by `--machine-config`) and writes
/// nothing. Selecting `file` in the sandbox home is the control.
/// Red before the fix: the control is refused too.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_set_into_file_is_still_refused_when_the_sandbox_config_says_keychain() {
    let fx = fixture();
    let layout = Layout::new(&fx);
    select_file_by_flag(&fx);
    let file = layout.file();
    let (factory, _) = recording(&fx, file.clone());
    let state = sandbox_state(fx.settings.clone(), factory, &layout.env(), &layout);
    let server = fx.start_state(state).await;
    for case in ["keychain", "absent"] {
        match case {
            "absent" => layout.unselect(),
            backend => layout.select(backend),
        }
        let refused = call(&fx.settings.socket, method::SET, set_params(&fx, "A")).await;
        assert_eq!(
            fixed_error(&refused, method::SET),
            ErrorKind::FileBackendNotSelected,
            "{case}"
        );
        assert!(!file.root().exists(), "{case}: no value file");
    }
    layout.select("file");
    let control = call(&fx.settings.socket, method::SET, set_params(&fx, "A")).await;
    server.stop().await;
    ok(control);
}

/// Point the fixture's `--machine-config` at `file`.
fn select_file_by_flag(fx: &Fixture) {
    std::fs::write(
        &fx.settings.machine_config,
        "secrets:\n  default_backend: file\n",
    )
    .unwrap();
}

/// Why: #9793, ruling Q2 — under the flag the login Keychain is refused for
/// every operation with `backend_not_enabled`, and the factory is never
/// asked for it: `set`, `list`, `delete`, `copy` from it, the resolve path
/// (`ProjectContext::backend`), doctor, and the build default with no
/// config at all. The delete sweep skips it rather than failing.
/// Red before the fix: the factory opens `keychain`. Mutation: drop the
/// refusal in `State::open` and this goes red.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_keychain_is_never_opened_under_the_sandbox_flag() {
    let fx = fixture();
    let layout = Layout::new(&fx);
    let (factory, opened) = recording(&fx, layout.file());
    let settings = home_settings(&fx, &layout);
    // The resolve path is not served without an exec grant; `probe` drives
    // `ProjectContext::backend`, the open `secrets.resolve` makes.
    let probe = sandbox_state(settings.clone(), factory.clone(), &layout.env(), &layout);
    let state = sandbox_state(settings, factory, &layout.env(), &layout);
    let server = fx.start_state(state).await;
    let socket = &fx.settings.socket;
    let target = json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "A"});
    let not_enabled = |response: &RpcResponse, method| {
        assert_eq!(
            fixed_error(response, method),
            ErrorKind::BackendNotEnabled,
            "{method}"
        );
    };

    // The Keychain named by config: every operation on it is refused.
    layout.select("keychain");
    not_enabled(
        &call(socket, method::SET, set_params(&fx, "A")).await,
        method::SET,
    );
    not_enabled(
        &call(socket, method::DELETE, target.clone()).await,
        method::DELETE,
    );
    let list = json!({"project": fx.project(), "vault": "trusty/acme/web"});
    ok(call(socket, method::LIST, list).await);
    let doctor: DoctorResponse =
        serde_json::from_value(ok(call(socket, DOCTOR, Value::Null).await)).unwrap();
    let row = doctor
        .backends
        .iter()
        .find(|row| row.id == BackendId::keychain());
    assert!(!row.unwrap().available, "{doctor:?}");
    let get = ProjectContext::resolve(&probe, &fx.repo).unwrap();
    assert!(
        matches!(get.backend(&probe), Err(ErrorKind::BackendNotEnabled)),
        "the resolve path"
    );

    // `file` selected: a copy out of the Keychain is refused, and the delete
    // sweep skips it.
    layout.select("file");
    ok(call(socket, method::SET, set_params(&fx, "A")).await);
    let copy = json!({"project": fx.project(), "from_backend": "keychain", "to_backend": "file",
                      "keys": ["A"]});
    not_enabled(&call(socket, method::COPY, copy).await, method::COPY);
    assert_eq!(
        ok(call(socket, method::DELETE, target).await),
        json!({"removed": true})
    );

    // No config at all: the build default never reaches the Keychain.
    layout.unselect();
    assert!(
        call(socket, method::SET, set_params(&fx, "B"))
            .await
            .error
            .is_some()
    );
    server.stop().await;
    let opened = opened.lock().unwrap();
    assert!(!opened.iter().any(|id| id == "keychain"), "{opened:?}");
    assert!(fx.keychain.is_empty());
}

/// Why: #9793, ruling Q6 — the flag never enables a CLI backend: a `copy`
/// into `onepassword` or `keeper` is `backend_not_enabled`, and the factory
/// is never asked for either.
/// Red before the fix: the factory opens both and the copies succeed.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_sandbox_flag_does_not_enable_cli_backends() {
    let fx = fixture();
    let layout = Layout::new(&fx);
    layout.select("file");
    let (factory, opened) = recording(&fx, layout.file());
    let state = sandbox_state(home_settings(&fx, &layout), factory, &layout.env(), &layout);
    let server = fx.start_state(state).await;
    for to in ["onepassword", "keeper"] {
        let copy = json!({"project": fx.project(), "from_backend": "file", "to_backend": to,
                          "keys": []});
        let refused = call(&fx.settings.socket, method::COPY, copy).await;
        assert_eq!(
            fixed_error(&refused, method::COPY),
            ErrorKind::BackendNotEnabled,
            "{to}"
        );
    }
    server.stop().await;
    let opened = opened.lock().unwrap();
    assert!(
        !opened
            .iter()
            .any(|id| id == "onepassword" || id == "keeper"),
        "{opened:?}"
    );
}

/// Why: #9793 — under the flag, consent is only the sandbox
/// `$HOME/.trusty-tools/trusty-common/config.yaml`. A spawner-chosen
/// `--machine-config`, a config planted under `TRUSTY_SECRETS_INDEX_DIR`'s
/// directory and one under the data dir each select `file` and consent to
/// nothing. Writing the sandbox home's file is the control.
/// Red before the fix: consent stays the account home, so the control fails.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_spawner_chosen_configs_are_not_consent_under_the_sandbox_flag() {
    let fx = fixture();
    let layout = Layout::new(&fx);
    select_file_by_flag(&fx);
    let planted = [
        fx.settings.index_root.join("config.yaml"),
        fx.settings.index_root.join(MACHINE_CONFIG_SUBPATH),
        layout.data.join(MACHINE_CONFIG_SUBPATH),
    ];
    for path in &planted {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "secrets:\n  default_backend: file\n").unwrap();
    }
    let file = layout.file();
    let (factory, _) = recording(&fx, file.clone());
    let state = sandbox_state(fx.settings.clone(), factory, &layout.env(), &layout);
    assert_eq!(state.file_consent_config, Some(layout.consent()));
    let server = fx.start_state(state).await;
    let refused = call(&fx.settings.socket, method::SET, set_params(&fx, "A")).await;
    assert_eq!(
        fixed_error(&refused, method::SET),
        ErrorKind::FileBackendNotSelected
    );
    assert!(!file.root().exists(), "no value file");

    layout.select("file");
    let control = call(&fx.settings.socket, method::SET, set_params(&fx, "A")).await;
    server.stop().await;
    ok(control);
}

/// Why: #9793, `doctor.rs` `rows` — under the flag, doctor reports the
/// file posture a write meets: the sandbox home's file as the account
/// config, `file` selected and available, and the Keychain not enabled.
/// Red before the fix: doctor names the account home and marks `file`
/// unavailable.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_doctor_reports_the_sandbox_file_posture_a_write_uses() {
    let fx = fixture();
    let layout = Layout::new(&fx);
    layout.select("file");
    let (factory, _) = recording(&fx, layout.file());
    let state = sandbox_state(home_settings(&fx, &layout), factory, &layout.env(), &layout);
    let server = fx.start_state(state).await;
    let doctor: DoctorResponse =
        serde_json::from_value(ok(call(&fx.settings.socket, DOCTOR, Value::Null).await)).unwrap();
    let write = call(&fx.settings.socket, method::SET, set_params(&fx, "A")).await;
    server.stop().await;

    assert_eq!(doctor.account_config, Some(layout.consent()));
    assert_eq!(doctor.selected_backend, BackendId::file());
    assert_eq!(doctor.posture, Some(StoragePosture::FileDegraded));
    let row = |id: BackendId| doctor.backends.iter().find(|row| row.id == id).unwrap();
    assert!(row(BackendId::file()).available, "{doctor:?}");
    let keychain = row(BackendId::keychain());
    assert!(!keychain.available);
    assert_eq!(keychain.reason, Some(Unavailable::NotEnabled));
    ok(write);
}

/// Why: #9793, ruling Q3 — the flag with the real home ignores the override
/// but still refuses the Keychain, without asking the factory. Without the
/// flag the Keychain opens as before (the control).
/// Red before the fix: the factory opens `keychain` under the flag.
/// Test: itself.
#[test]
fn sandbox_flag_with_the_real_home_still_refuses_the_keychain() {
    let fx = fixture();
    let layout = Layout::new(&fx);
    let mut env = layout.env();
    env.home = Some(layout.account.clone().into());
    let (factory, opened) = recording(&fx, layout.file());
    let state = sandbox_state(fx.settings.clone(), factory.clone(), &env, &layout);
    assert_eq!(state.file_consent_config, Some(layout.account_consent()));
    assert!(matches!(
        state.open(&BackendId::keychain()),
        Err(SecretsError::BackendNotEnabled { .. })
    ));
    assert!(opened.lock().unwrap().is_empty());

    env.flag = None;
    let state = sandbox_state(fx.settings.clone(), factory, &env, &layout);
    assert!(state.open(&BackendId::keychain()).is_ok(), "control");
}
