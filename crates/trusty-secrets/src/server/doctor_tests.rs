//! `secrets.doctor` reasons, headless readiness and wire shape (#7519 P4).
//!
//! A child of `server_tests`, so it shares that module's fixture: every path
//! is under a `TempDir` and `keychain` is an in-memory double. The account's
//! own machine config is a temp file, never the real one. CLI-backed rows
//! open through the production factory, `router::backends_with`; no test
//! runs the real `op` or `keeper`, and a planted program proves doctor runs
//! none at all.
//!
//! Test: itself.

use super::*;

/// A canary token; it must reach no doctor output.
const TOKEN: &str = "ops_doctor_canary_7519_p4_0123456789abcdef";

/// The rows of `doctor` by id: (available, reason, detail).
fn row(doctor: &DoctorResponse, id: &str) -> (bool, Option<Unavailable>, String) {
    let row = doctor
        .backends
        .iter()
        .find(|b| b.id.as_str() == id)
        .unwrap_or_else(|| panic!("no {id} row in {doctor:?}"));
    (
        row.available,
        row.reason,
        row.detail.clone().unwrap_or_default(),
    )
}

async fn doctor(fx: &Fixture, params: Value) -> DoctorResponse {
    serde_json::from_value(ok(call(&fx.settings.socket, DOCTOR, params).await)).unwrap()
}

/// Write the account's own machine config under the fixture's temp dir.
fn account(fx: &Fixture, yaml: &str) -> PathBuf {
    let path = fx.tmp.path().join("account").join("config.yaml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, yaml).unwrap();
    path
}

/// A server whose CLI-backed backends open through the production factory
/// with `account` as the account's own machine config; `keychain` stays the
/// fixture's double.
async fn start_with_account(fx: &Fixture, account: Option<PathBuf>) -> Running {
    let production = router::backends_with(account.clone(), &fx.settings, None, Vec::new());
    let base = fx.backends();
    let factory: BackendFactory = Arc::new(move |id: &BackendId| match id.as_str() {
        BackendId::ONEPASSWORD | BackendId::KEEPER => production(id),
        _ => base(id),
    });
    let mut state = fx.state(factory);
    state.file_consent_config = account;
    fx.start_state(state).await
}

/// `secrets.set` of `A` in `fx`'s project vault.
async fn set(fx: &Fixture) -> RpcResponse {
    let params = json!({"project": fx.project(), "vault": "trusty/acme/web", "key": "A",
                        "value": VALUE});
    call(&fx.settings.socket, method::SET, params).await
}

/// A planted program that creates `marker` if it ever runs.
#[cfg(feature = "cli-backends")]
fn planted(fx: &Fixture, marker: &Path) -> PathBuf {
    crate::store::onepassword::shim::plant_op(&fx.tmp.path().join("planted"), marker)
}

/// One account config case: name, YAML (`None` for no file), the 1Password
/// and Keeper reasons expected, and text the 1Password detail must carry.
#[cfg(feature = "cli-backends")]
type Case = (
    &'static str,
    Option<String>,
    Option<Unavailable>,
    Option<Unavailable>,
    &'static str,
);

/// Why: #7519 P4 A4 — each cause of an unavailable CLI row has its own
/// reason and fix: off in the account's config, an unreadable account
/// config, no account home, a missing CLI, a relative program pin, an
/// incomplete Keeper section. A pinned program is available and never run.
/// Red before the reason field: every row was a bare `available: false`.
/// Test: itself.
#[cfg(feature = "cli-backends")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_reasons_name_each_cause() {
    use Unavailable::*;
    let fx = fixture();
    let marker = fx.tmp.path().join("planted-ran");
    let program = planted(&fx, &marker);
    let pinned = format!(
        "secrets:\n  onepassword:\n    program: {}\n",
        program.display()
    );
    let cases: [Case; 6] = [
        (
            "missing",
            None,
            Some(NotEnabled),
            Some(NotEnabled),
            "does not enable it",
        ),
        (
            "off",
            Some("secrets:\n  default_backend: keychain\n".into()),
            Some(NotEnabled),
            Some(NotEnabled),
            "add a `secrets.onepassword:` section",
        ),
        (
            "no cli",
            Some("secrets:\n  onepassword: {}\n".into()),
            Some(CliNotInstalled),
            Some(NotEnabled),
            "install the 1Password CLI",
        ),
        (
            "relative",
            Some("secrets:\n  onepassword:\n    program: bin/op\n".into()),
            Some(ConfigInvalid),
            Some(NotEnabled),
            "must be an absolute path",
        ),
        ("pinned", Some(pinned), None, Some(NotEnabled), ""),
        (
            "keeper",
            Some("secrets:\n  keeper: {}\n".into()),
            Some(NotEnabled),
            Some(ConfigInvalid),
            "",
        ),
    ];
    for (name, yaml, onepassword, keeper, detail) in cases {
        let path = fx.tmp.path().join("account").join("config.yaml");
        let _ = std::fs::remove_file(&path);
        if let Some(yaml) = &yaml {
            account(&fx, yaml);
        }
        let server = start_with_account(&fx, Some(path.clone())).await;
        let report = doctor(&fx, Value::Null).await;
        server.stop().await;
        let op = row(&report, "onepassword");
        assert_eq!((op.0, op.1), (onepassword.is_none(), onepassword), "{name}");
        assert!(op.2.contains(detail), "{name}: {}", op.2);
        assert_eq!(row(&report, "keeper").1, keeper, "{name}");
        assert_eq!(report.account_config.as_ref(), Some(&path), "{name}");
    }

    // A directory where the file belongs: unreadable, which is not "off".
    let dir = fx.tmp.path().join("account-dir");
    std::fs::create_dir(&dir).unwrap();
    let server = start_with_account(&fx, Some(dir)).await;
    let report = doctor(&fx, Value::Null).await;
    server.stop().await;
    assert_eq!(row(&report, "onepassword").1, Some(ConfigInvalid));
    assert_eq!(row(&report, "keeper").1, Some(ConfigInvalid));

    let server = start_with_account(&fx, None).await;
    let report = doctor(&fx, Value::Null).await;
    server.stop().await;
    let op = row(&report, "onepassword");
    assert_eq!(op.1, Some(NotEnabled));
    assert!(op.2.contains("home directory is unknown"), "{}", op.2);
    assert_eq!(report.account_config, None);
    assert!(!marker.exists(), "doctor ran the pinned program");
}

/// Why: #7519 P4 — a build without `cli-backends` still lists 1Password and
/// Keeper, as not compiled, instead of dropping their rows.
/// Test: itself.
#[cfg(not(feature = "cli-backends"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_reasons_name_each_cause() {
    let fx = fixture();
    let path = account(
        &fx,
        "secrets:\n  default_backend: onepassword\n  keeper: {}\n",
    );
    let server = start_with_account(&fx, Some(path)).await;
    let report = doctor(&fx, Value::Null).await;
    server.stop().await;
    for id in ["onepassword", "keeper"] {
        let (available, reason, detail) = row(&report, id);
        assert_eq!((available, reason), (false, Some(Unavailable::NotCompiled)));
        assert!(detail.contains("--features cli-backends"), "{detail}");
    }
}

/// Why: #7519 P4 A4 — off macOS the Keychain backend opened and then failed
/// every call, so doctor showed a healthy Keychain on a Linux host with no
/// Keychain. The production factory now refuses it there, the row says
/// `not_compiled`, and a `file` selection keeps its degraded posture.
/// Red before `open_local`: the row was available on Linux.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_keychain_row_is_not_compiled_without_a_keychain() {
    let keychain = BackendId::keychain();
    let err = router::open_local(&keychain, false).unwrap_err();
    assert!(
        matches!(err, SecretsError::UnknownBackend { .. }),
        "{err:?}"
    );
    assert!(router::open_local(&keychain, true).is_ok());

    let fx = fixture();
    let (file, _values) = with_file_backend(&fx);
    let factory: BackendFactory = Arc::new(move |id: &BackendId| match id.as_str() {
        BackendId::KEYCHAIN => router::open_local(id, false),
        _ => file(id),
    });
    std::fs::write(
        &fx.settings.machine_config,
        "secrets:\n  default_backend: file\n",
    )
    .unwrap();
    let server = fx.start_with(factory).await;
    let report = doctor(&fx, Value::Null).await;
    server.stop().await;
    let keychain = row(&report, "keychain");
    assert_eq!(
        (keychain.0, keychain.1),
        (false, Some(Unavailable::NotCompiled))
    );
    assert!(row(&report, "file").0);
    assert_eq!(report.selected_backend, BackendId::file());
    assert_eq!(report.posture, Some(StoragePosture::FileDegraded));
}

/// Why: #7519 P4 — `--machine-config` (the spawner's) selects the backend
/// for every request, while only the account's own file enables a CLI
/// backend (ruling 74). Doctor's `selected` must be the backend a write
/// actually uses, and the CLI row must say whether the account enables it.
/// Case A: the spawner's file selects 1Password, the account's does not
/// enable it — doctor and `set` both pick 1Password, the row says why it
/// fails, and `set` fails the same way. Case B: the account's file selects
/// and enables 1Password, the spawner's selects the Keychain — doctor and
/// `set` both pick the Keychain.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_selected_is_the_backend_a_write_uses_when_the_configs_differ() {
    let compiled = !crate::store::cli_backends().is_empty();

    // Case A.
    let fx = fixture();
    std::fs::write(
        &fx.settings.machine_config,
        "secrets:\n  default_backend: onepassword\n",
    )
    .unwrap();
    let path = account(&fx, "secrets:\n  default_backend: keychain\n");
    let server = start_with_account(&fx, Some(path.clone())).await;
    for params in [Value::Null, json!({"project": fx.project()})] {
        let report = doctor(&fx, params).await;
        assert_eq!(report.selected_backend, BackendId::onepassword());
        assert_eq!(report.machine_config, fx.settings.machine_config);
        assert_eq!(report.account_config.as_ref(), Some(&path));
        let (available, reason, detail) = row(&report, "onepassword");
        assert!(!available);
        if compiled {
            assert_eq!(reason, Some(Unavailable::NotEnabled));
            assert!(detail.contains(&path.display().to_string()), "{detail}");
        } else {
            assert_eq!(reason, Some(Unavailable::NotCompiled));
        }
    }
    let refused = set(&fx).await;
    let expected = if compiled {
        ErrorKind::BackendNotEnabled
    } else {
        ErrorKind::UnknownBackend
    };
    assert_eq!(fixed_error(&refused, method::SET), expected);
    assert!(
        fx.keychain.is_empty(),
        "the write fell back to the Keychain"
    );
    server.stop().await;

    // Case B.
    let fx = fixture();
    #[cfg(feature = "cli-backends")]
    let marker = fx.tmp.path().join("planted-ran");
    #[cfg(feature = "cli-backends")]
    let yaml = format!(
        "secrets:\n  default_backend: onepassword\n  onepassword:\n    program: {}\n",
        planted(&fx, &marker).display()
    );
    #[cfg(not(feature = "cli-backends"))]
    let yaml = "secrets:\n  default_backend: onepassword\n".to_string();
    let path = account(&fx, &yaml);
    let server = start_with_account(&fx, Some(path)).await;
    let report = doctor(&fx, json!({"project": fx.project()})).await;
    assert_eq!(report.selected_backend, BackendId::keychain());
    assert_eq!(row(&report, "onepassword").0, compiled);
    ok(set(&fx).await);
    assert_eq!(fx.keychain.len(), 1, "the write used the selected Keychain");
    server.stop().await;
    #[cfg(feature = "cli-backends")]
    assert!(!marker.exists(), "doctor or set ran the 1Password program");
}

/// Why: #7519 P4 critic HIGH, #7524 H1 — on a Keychain build every value
/// write into `file` needs the account's own machine config to select it
/// (`check_value_write_for`). Case A: the spawner's file selects `file`
/// and the account's config is absent or keeps the Keychain — doctor's
/// selected `file` row is unavailable with the consent refusal as its
/// detail, and `set` is refused the same way. Case B: the account's config
/// selects `file` — the row is available and `set` writes the value.
/// Red before doctor applied the consent check: case A read available.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_file_row_is_unavailable_when_every_write_into_it_is_refused() {
    for account_yaml in [None, Some("secrets:\n  default_backend: keychain\n")] {
        let fx = fixture();
        std::fs::write(
            &fx.settings.machine_config,
            "secrets:\n  default_backend: file\n",
        )
        .unwrap();
        let consent = match account_yaml {
            Some(yaml) => account(&fx, yaml),
            None => fx.tmp.path().join("account").join("absent.yaml"),
        };
        let (factory, values) = with_file_backend(&fx);
        let mut state = fx.state(factory);
        state.keychain_compiled = true;
        state.file_consent_config = Some(consent);
        let server = fx.start_state(state).await;
        for params in [Value::Null, json!({"project": fx.project()})] {
            let report = doctor(&fx, params).await;
            assert_eq!(report.selected_backend, BackendId::file());
            let (available, reason, detail) = row(&report, "file");
            assert!(!available, "{account_yaml:?}: {report:?}");
            assert_eq!(reason, Some(Unavailable::NotEnabled), "{account_yaml:?}");
            assert_eq!(detail, SecretsError::FileBackendNotSelected.to_string());
        }
        let refused = set(&fx).await;
        assert_eq!(
            fixed_error(&refused, method::SET),
            ErrorKind::FileBackendNotSelected
        );
        server.stop().await;
        assert!(!values.root().exists(), "a value reached `file`");
    }

    // Case B: the account's own config selects `file`.
    let fx = fixture();
    std::fs::write(
        &fx.settings.machine_config,
        "secrets:\n  default_backend: file\n",
    )
    .unwrap();
    let consent = account(&fx, "secrets:\n  default_backend: file\n");
    let (factory, values) = with_file_backend(&fx);
    let mut state = fx.state(factory);
    state.keychain_compiled = true;
    state.file_consent_config = Some(consent);
    let server = fx.start_state(state).await;
    let report = doctor(&fx, json!({"project": fx.project()})).await;
    let (available, reason, _) = row(&report, "file");
    assert_eq!((available, reason), (true, None));
    ok(set(&fx).await);
    server.stop().await;
    assert!(
        values.root().exists(),
        "the value was not written to `file`"
    );
}

/// Why: #7519 P4 — a project whose tracked config sets what only the
/// machine config may is refused on every request. Doctor reports it on
/// the selected row, with the fix, instead of failing the call; the reply
/// names the file and the key, never the value.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_reports_a_refused_tracked_setting_on_the_selected_row() {
    let canary = "acct-canary-7519-p4";
    let fx = fixture();
    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let server = fx.start().await;
    for (yaml, needle) in [
        (
            format!("secrets:\n  onepassword:\n    account: {canary}\n"),
            "onepassword.account",
        ),
        (
            "secrets:\n  audit: false\n".to_string(),
            "turn the credential audit off",
        ),
    ] {
        std::fs::write(&config, &yaml).unwrap();
        let response = call(
            &fx.settings.socket,
            DOCTOR,
            json!({"project": fx.project()}),
        )
        .await;
        assert!(!wire(&response).contains(canary), "{}", wire(&response));
        let report: DoctorResponse = serde_json::from_value(ok(response)).unwrap();
        assert_eq!(report.selected_backend, BackendId::keychain());
        let root = report.project_root.clone().expect("the project root");
        assert_eq!(
            report.project_config,
            Some(root.join(PROJECT_CONFIG_SUBPATH))
        );
        let (available, reason, detail) = row(&report, "keychain");
        assert!(!available, "{yaml}");
        assert_eq!(reason, Some(Unavailable::TrackedSettingRefused), "{yaml}");
        assert!(detail.contains(needle), "{detail}");
        // A request in the project meets the refusal the row reports.
        let list = call(
            &fx.settings.socket,
            method::LIST,
            json!({"project": fx.project(), "vault": "trusty/acme/web"}),
        )
        .await;
        assert!(list.error.is_some());
    }
    server.stop().await;
}

/// Why: #7519 P4 A10, owner ruling Q1 — doctor says whether a 1Password
/// service-account token was present at start, yes or no, and the token
/// itself reaches no doctor reply, `Debug` output, audit record or error
/// text. With `cli-backends`, the 1Password backend holds the token and its
/// shim is never spawned, so no argv or env log carries it either.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_reports_token_presence_and_never_the_token() {
    let fx = fixture();
    let start = StartEnv::default().with_onepassword_token(true);
    assert!(!format!("{start:?}").contains(TOKEN));
    #[cfg(feature = "cli-backends")]
    let shim = crate::store::onepassword::shim::OpShim::new();
    #[cfg(feature = "cli-backends")]
    let factory = {
        let mut settings = shim.settings(&fx.settings.template_root);
        settings.token = Some(SecretValue::new(TOKEN));
        assert!(!format!("{settings:?}").contains(TOKEN));
        let backend: Arc<dyn SecretBackend> =
            Arc::new(crate::store::onepassword::OnePasswordBackend::new(settings));
        let base = fx.backends();
        let factory: BackendFactory = Arc::new(move |id: &BackendId| match id.as_str() {
            BackendId::ONEPASSWORD => Ok(Arc::clone(&backend)),
            _ => base(id),
        });
        factory
    };
    #[cfg(not(feature = "cli-backends"))]
    let factory = fx.backends();
    std::fs::write(
        &fx.settings.machine_config,
        "secrets:\n  default_backend: keychain\n  onepassword: {}\n",
    )
    .unwrap();
    let mut state = fx.state(factory);
    state.start = start;
    assert!(!format!("{state:?}").contains(TOKEN));
    let server = fx.start_state(state).await;

    let mut texts = Vec::new();
    for params in [Value::Null, json!({"project": fx.project()})] {
        let response = call(&fx.settings.socket, DOCTOR, params).await;
        texts.push(wire(&response));
        let report: DoctorResponse = serde_json::from_value(ok(response)).unwrap();
        assert_eq!(
            report.headless,
            Some(HeadlessReadiness {
                onepassword_token: true
            })
        );
        texts.push(format!("{report:?}"));
    }
    // A refused doctor call carries fixed text only.
    let refused = call(&fx.settings.socket, DOCTOR, json!({"unknown": TOKEN})).await;
    assert_eq!(fixed_error(&refused, DOCTOR), ErrorKind::InvalidParams);
    texts.push(wire(&refused));
    server.stop().await;
    texts.push(std::fs::read_to_string(&fx.settings.audit_log).unwrap_or_default());
    for text in &texts {
        assert!(!text.contains(TOKEN), "{text}");
    }
    #[cfg(feature = "cli-backends")]
    {
        assert!(!shim.spawned(), "doctor ran `op`:\n{}", shim.calls());
        assert!(!shim.env_log().contains(TOKEN) && !shim.calls().contains(TOKEN));
    }

    // No token at start reads `false`.
    let fx = fixture();
    let server = fx.start().await;
    let report = doctor(&fx, Value::Null).await;
    server.stop().await;
    assert_eq!(report.headless, Some(HeadlessReadiness::default()));
}

/// The doctor wire shape before #7519 P4, as an older client decodes it.
#[derive(Debug, serde::Deserialize)]
struct OldBackendStatus {
    id: BackendId,
    available: bool,
    capabilities: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
struct OldDoctorResponse {
    selected_backend: BackendId,
    backends: Vec<OldBackendStatus>,
    posture: Option<StoragePosture>,
}

/// Why: #7519 P4, owner ruling Q5 — the reason fields are additive: an
/// older client decodes a new server's reply, and a new client decodes an
/// older server's reply with every new field defaulted.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_answer_decodes_on_an_old_client_and_an_old_answer_on_a_new_one() {
    let fx = fixture();
    let server = fx.start().await;
    let new = ok(call(&fx.settings.socket, DOCTOR, Value::Null).await);
    server.stop().await;
    assert!(new["backends"][1]["reason"].is_string(), "{new}");
    let old: OldDoctorResponse = serde_json::from_value(new).unwrap();
    assert_eq!(old.selected_backend, BackendId::keychain());
    assert_eq!(old.posture, Some(StoragePosture::Keychain));
    let file = &old.backends[1];
    assert_eq!((file.id.as_str(), file.available), ("file", false));
    assert!(file.capabilities.is_empty());

    let older = json!({
        "socket": "/s", "index_root": "/i", "machine_config": "/m",
        "project_root": null, "project_config": null, "selected_backend": "keychain",
        "backends": [{"id": "keychain", "available": false, "capabilities": []}],
    });
    let decoded: DoctorResponse = serde_json::from_value(older).unwrap();
    assert_eq!(decoded.backends[0].reason, None);
    assert_eq!(decoded.backends[0].detail, None);
    assert_eq!(
        (decoded.posture, decoded.account_config, decoded.headless),
        (None, None, None)
    );
    assert!(decoded.tools.is_empty());
}

/// Why: #7519 P4, DOC-74 §7 and owner amendment 5 — doctor lists bw, vault,
/// pass, gopass, doppler and infisical as installed or not, from the
/// absolute entries of the `PATH` read at start, and runs none of them.
/// A relative entry and a file without an execute bit find nothing.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_detects_unsupported_tools_on_the_start_path_without_running_them() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture();
    let bin = fx.tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let marker = fx.tmp.path().join("tool-ran");
    for name in ["bw", "gopass", "pass"] {
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n: > '{}'\n", marker.display())).unwrap();
        let mode = if name == "pass" { 0o644 } else { 0o755 };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    // `doppler` only under a relative entry, which names the working directory.
    let search = std::env::join_paths([PathBuf::from("relative/bin"), bin.clone()]).unwrap();
    let mut state = fx.state(fx.backends());
    state.start = StartEnv::default().with_search_path(Some(search));
    let server = fx.start_state(state).await;
    let report = doctor(&fx, Value::Null).await;
    server.stop().await;
    let tools: Vec<_> = report
        .tools
        .iter()
        .map(|t| (t.id.as_str(), t.program.as_str(), t.installed, t.supported))
        .collect();
    assert_eq!(
        tools,
        [
            ("bitwarden", "bw", true, false),
            ("vault", "vault", false, false),
            ("pass", "pass", false, false),
            ("gopass", "gopass", true, false),
            ("doppler", "doppler", false, false),
            ("infisical", "infisical", false, false),
        ]
    );
    assert_eq!(report.tools[0].path, Some(bin.join("bw")));
    assert_eq!(report.tools[1].path, None);
    assert!(!marker.exists(), "doctor ran a detected tool");

    // No `PATH` at start finds nothing.
    let fx = fixture();
    let server = fx.start().await;
    let report = doctor(&fx, Value::Null).await;
    server.stop().await;
    assert_eq!(report.tools.len(), 6);
    assert!(report.tools.iter().all(|t| !t.installed));
}

/// Why: #7519 P4 — a reason added later decodes as `Other` on this client.
/// Red without `#[serde(other)]` on `Unavailable::Other`.
/// Test: itself.
#[test]
fn doctor_unknown_reason_decodes_as_other() {
    let reason: Unavailable = serde_json::from_value(json!("hsm_offline")).unwrap();
    assert_eq!(reason, Unavailable::Other);
    for known in [Unavailable::NotEnabled, Unavailable::TrackedSettingRefused] {
        let wire = serde_json::to_value(known).unwrap();
        assert_eq!(wire, json!(known.as_str()));
        assert_eq!(serde_json::from_value::<Unavailable>(wire).unwrap(), known);
    }
}
