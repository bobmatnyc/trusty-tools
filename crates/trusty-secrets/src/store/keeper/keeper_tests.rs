//! Tests for [`super::KeeperBackend`] (#7519 P3) against [`KeeperShim`], a
//! fake `keeper` run by absolute path. No test runs the real `keeper`,
//! reads the Keychain, or changes `PATH` or any other process-global
//! environment. Every value-bearing test asserts that neither the value
//! nor its base64 form reaches argv or the environment.
//!
//! Test: itself.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::shim::KeeperShim;
use super::{KeeperBackend, KeeperSettings, markers, open, record};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};
use crate::store::cli::test_shim::{install_script, relative_to_cwd};
use crate::store::config::{CliSettings, MachineSecretsConfig};
use crate::store::{Capabilities, SecretBackend};

const VALUE: &str = "kp-canary-7519-0123456789abcdef";
const NOT_LOGGED_IN: &str = "Not logged in. Login with `login` first.";
const FOLDER: &str = "trusty/acme/web";

fn vault() -> VaultName {
    VaultName::new(FOLDER).unwrap()
}

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).unwrap()
}

fn value() -> SecretValue {
    SecretValue::new(VALUE)
}

/// `VALUE` as the batch carries it.
fn encoded() -> String {
    record::base64(VALUE.as_bytes())
}

/// `err` rendered every way a caller could log it.
fn shown(err: &SecretsError) -> String {
    format!("{err} {err:?}")
}

/// Neither the value nor its base64 reached argv or the environment.
fn assert_off_argv_and_env(shim: &KeeperShim) {
    let (calls, env) = (shim.calls(), shim.env_log());
    for secret in [VALUE.to_string(), encoded()] {
        assert!(!calls.contains(&secret), "argv carried a value: {calls}");
        assert!(!env.contains(&secret), "the env carried a value");
    }
}

/// The subcommand of each logged call, global flags removed.
fn subcommands(shim: &KeeperShim) -> Vec<String> {
    shim.calls()
        .lines()
        .map(|line| {
            let (_, rest) = line.split_once(" --batch-mode ").expect(line);
            rest.to_string()
        })
        .collect()
}

fn backend_error(err: &SecretsError, reason_part: &str) -> bool {
    matches!(err, SecretsError::Backend { backend, reason, .. }
        if backend == "keeper" && reason.contains(reason_part))
}

/// Why: ruling 1 — a new key is one `record-add` line on stdin, the value
/// only as `$BASE64:`; an existing key is a `record-update`, never a
/// delete-then-add. Ruling 3 — each write is confirmed by a listing and a
/// read-back. Ruling 4 — the record goes in the vault's own folder.
/// Test: itself.
#[test]
fn keeper_set_writes_through_a_stdin_batch_only() {
    let shim = KeeperShim::new();
    let backend = shim.backend();
    backend.set(&vault(), &key("API_KEY"), &value()).unwrap();
    let uid = shim.records()[0].0.clone();
    let get = format!("get --format json -- {uid}");
    assert_eq!(
        subcommands(&shim),
        [
            "ls --format json trusty/acme/web",
            "-",
            "ls --format json trusty/acme/web",
            get.as_str(),
        ]
    );
    let add = format!(
        "record-add --folder=trusty/acme/web --title=API_KEY --record-type=login password=$BASE64:{}\n",
        encoded()
    );
    assert_eq!(shim.stdin_log(), add);
    let calls = shim.calls();
    assert!(calls.lines().all(|l| l.contains("--config /")), "{calls}");
    assert!(!calls.contains("API_KEY"), "the key reached argv: {calls}");
    assert_off_argv_and_env(&shim);
    assert_eq!(
        shim.records(),
        [(uid.clone(), "API_KEY".into(), VALUE.into())]
    );
    let read = backend.get(&vault(), &key("API_KEY")).unwrap().unwrap();
    assert_eq!(read.expose(), VALUE);

    // An existing key: `record-update` of the listed uid, nothing added.
    let newer = SecretValue::new("kp-canary-7519-rotated");
    backend.set(&vault(), &key("API_KEY"), &newer).unwrap();
    let update = format!(
        "record-update --record={uid} password=$BASE64:{}\n",
        record::base64(b"kp-canary-7519-rotated")
    );
    assert!(shim.stdin_log().ends_with(&update), "{}", shim.stdin_log());
    assert_eq!(
        shim.records(),
        [(uid, "API_KEY".into(), "kp-canary-7519-rotated".into())]
    );
    assert!(!shim.calls().contains("kp-canary-7519-rotated"));
}

/// Why: ruling 1 — `$BASE64:` keeps a value with spaces, quotes, `$`, `#`
/// and `;` one batch word, and the encoder matches RFC 4648. An empty
/// value or a hostile uid is refused before anything runs.
/// Test: itself.
#[test]
fn keeper_batch_commands_carry_the_value_only_as_base64() {
    let vectors = [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ];
    for (plain, b64) in vectors {
        assert_eq!(record::base64(plain.as_bytes()), b64, "{plain:?}");
    }
    let batch = record::add_command(&vault(), &key("K"), &value()).unwrap();
    assert!(!batch.text.expose().contains(VALUE));
    assert_eq!(batch.encoded.expose(), encoded());
    assert!(record::add_command(&vault(), &key("K"), &SecretValue::new("")).is_err());
    assert!(record::update_command("a b", &value()).is_err());
    assert!(record::update_command("x\nrecord-add", &value()).is_err());

    let shim = KeeperShim::new();
    let awkward = "it's $HOME #x; y  z";
    let backend = shim.backend();
    backend
        .set(&vault(), &key("K"), &SecretValue::new(awkward))
        .unwrap();
    let read = backend.get(&vault(), &key("K")).unwrap().unwrap();
    assert_eq!(read.expose(), awkward);
    assert!(!shim.calls().contains("$HOME"), "{}", shim.calls());
}

/// Why: ruling 3 — a miss comes only from a successful listing. An empty
/// folder, and a folder a successful listing from the root does not show,
/// are misses; a failed listing of a folder that exists, a failed `get`
/// and a failed `rm` are errors whatever their stderr says. Red when
/// `lookup` reads a failed listing as an absent folder without the walk.
/// Test: itself.
#[test]
fn keeper_misses_come_only_from_a_successful_listing() {
    // An empty folder: one listing, and a miss.
    let shim = KeeperShim::new();
    let backend = shim.backend();
    assert!(backend.get(&vault(), &key("K")).unwrap().is_none());
    assert!(!backend.delete(&vault(), &key("K")).unwrap());
    assert_eq!(subcommands(&shim).len(), 2);

    // No folder: the walk from the root finds `acme` missing.
    let shim = KeeperShim::new();
    shim.set_folders(&["trusty"]);
    let backend = shim.backend();
    assert!(backend.get(&vault(), &key("K")).unwrap().is_none());
    assert_eq!(
        subcommands(&shim),
        [
            "ls --format json trusty/acme/web",
            "ls --format json /",
            "ls --format json trusty",
        ]
    );
    assert!(!backend.delete(&vault(), &key("K")).unwrap());
    let err = backend.set(&vault(), &key("K"), &value()).unwrap_err();
    assert!(backend_error(&err, "no folder"), "{err:?}");
    assert!(!shim.stdin_log().contains("record-add"));

    // The folder exists but its listing fails: an error, not a miss.
    let shim = KeeperShim::new();
    shim.fail_path(FOLDER);
    let backend = shim.backend();
    for err in [
        backend.get(&vault(), &key("K")).unwrap_err(),
        backend.delete(&vault(), &key("K")).unwrap_err(),
    ] {
        assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");
    }

    // Every listing fails with "not found" text: still an error.
    let shim = KeeperShim::new();
    shim.fail("ls", "ls: folder not found");
    let err = shim.backend().get(&vault(), &key("K")).unwrap_err();
    assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");

    // A listed record whose `get` or `rm` says "not found" is an error.
    let shim = KeeperShim::new();
    shim.seed("rec1", FOLDER, "K", "login", VALUE);
    shim.fail("get", "get: record not found");
    shim.fail("rm", "rm: record not found");
    let backend = shim.backend();
    let get = backend.get(&vault(), &key("K")).unwrap_err();
    let delete = backend.delete(&vault(), &key("K")).unwrap_err();
    for err in [get, delete] {
        assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");
        assert!(!shown(&err).contains(VALUE));
    }
    assert_eq!(shim.records().len(), 1);
}

/// Why: ruling 5 — `rm` moves a record to Keeper's trash, which satisfies
/// the delete; every copy titled with the key goes, and the delete is
/// confirmed by a listing. A uid starting with `-` is passed after `--`.
/// Test: itself.
#[test]
fn keeper_delete_moves_every_copy_to_the_trash() {
    let shim = KeeperShim::new();
    shim.seed("-dash7519", FOLDER, "K", "login", VALUE);
    shim.seed("rec2", FOLDER, "K", "login", "other");
    shim.seed("rec3", FOLDER, "OTHER", "login", "keep");
    shim.seed("rec4", "trusty/acme", "K", "login", "owner-vault");
    let backend = shim.backend();
    assert!(backend.delete(&vault(), &key("K")).unwrap());
    assert_eq!(shim.trash(), ["-dash7519", "rec2"]);
    let left: Vec<String> = shim.records().into_iter().map(|r| r.0).collect();
    assert_eq!(left, ["rec3", "rec4"]);
    let calls = subcommands(&shim);
    assert!(
        calls.contains(&"rm --force -- -dash7519".to_string()),
        "{calls:?}"
    );
    assert_eq!(calls.last().unwrap(), "ls --format json trusty/acme/web");
    assert!(!backend.delete(&vault(), &key("K")).unwrap());

    let shim = KeeperShim::new();
    shim.seed("-dash7519", FOLDER, "K", "login", VALUE);
    let read = shim.backend().get(&vault(), &key("K")).unwrap().unwrap();
    assert_eq!(read.expose(), VALUE);
}

/// Why: rulings 4 and 6 — two `login` records with the key's title, or
/// two folders with one name on the vault's path, are ambiguous; a record
/// of another type is left alone; a uid outside Keeper's alphabet never
/// reaches argv. Each is an error, never a pick.
/// Test: itself.
#[test]
fn keeper_ambiguous_foreign_and_hostile_rows_are_refused() {
    let shim = KeeperShim::new();
    shim.seed("rec1", FOLDER, "K", "login", "a");
    shim.seed("rec2", FOLDER, "K", "login", "b");
    let backend = shim.backend();
    let get = backend.get(&vault(), &key("K")).unwrap_err();
    let set = backend.set(&vault(), &key("K"), &value()).unwrap_err();
    for err in [get, set] {
        assert!(
            backend_error(&err, "more than one Keeper record"),
            "{err:?}"
        );
    }
    assert!(shim.stdin_log().is_empty());

    let shim = KeeperShim::new();
    shim.seed("rec1", FOLDER, "K", "databaseCredentials", "db");
    let backend = shim.backend();
    let errors = [
        backend.get(&vault(), &key("K")).unwrap_err(),
        backend.set(&vault(), &key("K"), &value()).unwrap_err(),
        backend.delete(&vault(), &key("K")).unwrap_err(),
    ];
    for err in errors {
        assert!(backend_error(&err, "not a login record"), "{err:?}");
    }
    assert_eq!(shim.records()[0].2, "db");
    assert!(shim.trash().is_empty());

    let shim = KeeperShim::new();
    shim.set_folders(&["trusty", "trusty"]);
    let err = shim.backend().get(&vault(), &key("K")).unwrap_err();
    assert!(
        backend_error(&err, "more than one Keeper folder"),
        "{err:?}"
    );

    let shim = KeeperShim::new();
    shim.seed("a;b", FOLDER, "K", "login", VALUE);
    let err = shim.backend().get(&vault(), &key("K")).unwrap_err();
    assert!(backend_error(&err, "uid alphabet"), "{err:?}");
    assert!(!subcommands(&shim).iter().any(|c| c.starts_with("get")));
}

/// Why: ruling 6 — a logged-out or unapproved `keeper` is `BackendLocked`,
/// whether it says so on stderr with a failing exit or on stdout with exit
/// 0, and the error names the human step: device approval and persistent
/// login. Nothing falls back. Red without the `markers_in` stdout check.
/// Test: itself.
#[test]
fn keeper_locked_output_is_backend_locked_and_names_device_approval() {
    let stderr = KeeperShim::new();
    stderr.locked(NOT_LOGGED_IN);
    let stdout = KeeperShim::new();
    stdout.stdout_only("ls", "Device approval is required for this device.\n");
    for shim in [stderr, stdout] {
        let backend = shim.backend();
        let errors = [
            backend.get(&vault(), &key("K")).unwrap_err(),
            backend.set(&vault(), &key("K"), &value()).unwrap_err(),
            backend.delete(&vault(), &key("K")).unwrap_err(),
        ];
        for err in errors {
            assert!(matches!(err, SecretsError::BackendLocked { .. }), "{err:?}");
            let text = err.to_string();
            assert!(
                text.contains("approve this device") && text.contains("persistent login"),
                "{text}"
            );
        }
        assert!(shim.stdin_log().is_empty(), "a locked keeper got a batch");
        assert_off_argv_and_env(&shim);
    }
}

/// Why: ruling 3 — exit 0 with an error on stdout is a failure. A batch or
/// an `rm` that prints an error and exits 0 is caught by the read-back or
/// the listing after it; a listing or `get` that prints one does not parse.
/// Red without the confirmation steps in `set` and `delete`.
/// Test: itself.
#[test]
fn keeper_exit_zero_with_an_error_on_stdout_is_a_failure() {
    let shim = KeeperShim::new();
    shim.stdout_only("batch", "Error: record-add: folder is read-only\n");
    let backend = shim.backend();
    let err = backend.set(&vault(), &key("K"), &value()).unwrap_err();
    assert!(backend_error(&err, "did not confirm the write"), "{err:?}");
    shim.seed("rec1", FOLDER, "K", "login", "old");
    let err = backend.set(&vault(), &key("K"), &value()).unwrap_err();
    assert!(backend_error(&err, "did not confirm the write"), "{err:?}");
    assert_eq!(shim.records()[0].2, "old");

    let shim = KeeperShim::new();
    shim.seed("rec1", FOLDER, "K", "login", VALUE);
    shim.stdout_only("rm", "Error: rm: permission denied\n");
    let err = shim.backend().delete(&vault(), &key("K")).unwrap_err();
    assert!(backend_error(&err, "did not confirm the delete"), "{err:?}");
    assert_eq!(shim.records().len(), 1);

    for cmd in ["ls", "get"] {
        let shim = KeeperShim::new();
        shim.seed("rec1", FOLDER, "K", "login", VALUE);
        shim.stdout_only(cmd, "Error: something went wrong\n");
        let err = shim.backend().get(&vault(), &key("K")).unwrap_err();
        assert!(
            matches!(err, SecretsError::Backend { .. }),
            "{cmd}: {err:?}"
        );
    }
}

/// Why: A2 — no value, raw or encoded, appears in an error or `Debug`,
/// even when `keeper` echoes its batch to stderr or prints every stored
/// value and then fails.
/// Test: itself.
#[test]
fn keeper_errors_never_carry_the_value() {
    let shim = KeeperShim::new();
    shim.fail("batch", "Error: batch failed");
    let err = shim
        .backend()
        .set(&vault(), &key("K"), &value())
        .unwrap_err();
    assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");

    let reader = KeeperShim::new();
    reader.seed("rec1", FOLDER, "K", "login", VALUE);
    reader.fail("get", "Error: get failed");
    let read = reader.backend().get(&vault(), &key("K")).unwrap_err();
    for err in [err, read] {
        let text = shown(&err);
        assert!(
            !text.contains(VALUE) && !text.contains(&encoded()),
            "{text}"
        );
    }
    assert_off_argv_and_env(&shim);
}

/// A temp dir with a 0600 Commander config file and an installed shim.
struct Machine {
    tmp: TempDir,
    config: PathBuf,
    program: PathBuf,
}

impl Machine {
    fn new(shim: &KeeperShim) -> Self {
        let tmp = TempDir::new().unwrap();
        let config = tmp.path().join("keeper-config.json");
        std::fs::write(&config, "{}").unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        let program = shim.install_in(&tmp.path().join("bin"));
        Self {
            tmp,
            config,
            program,
        }
    }

    fn section(&self) -> CliSettings {
        CliSettings {
            config_path: Some(self.config.clone()),
            program: Some(self.program.clone()),
            ..CliSettings::default()
        }
    }

    fn settings(
        &self,
        edit: impl FnOnce(&mut CliSettings),
    ) -> Result<KeeperSettings, SecretsError> {
        let mut section = self.section();
        edit(&mut section);
        let machine = MachineSecretsConfig {
            keeper: Some(section),
            ..MachineSecretsConfig::default()
        };
        KeeperSettings::from_machine(&machine, &self.tmp.path().join("machine.yaml"))
    }
}

fn config_error(result: Result<KeeperSettings, SecretsError>, part: &str) -> bool {
    matches!(result, Err(SecretsError::Config { reason, .. }) if reason.contains(part))
}

/// Why: ruling 7 — the Commander config file holds the device token, so it
/// must be named by the machine config, absolute, a regular file and not a
/// link, mode 0600 and owned by this user; `account` is refused, because
/// the config file decides the account. Every check runs before a spawn.
/// Test: itself.
#[test]
fn keeper_machine_settings_are_checked_before_any_spawn() {
    let shim = KeeperShim::new();
    let m = Machine::new(&shim);
    let settings = m.settings(|_| {}).unwrap();
    assert_eq!(settings.config_path, m.config);
    assert_eq!(settings.program, m.program.clone().into_os_string());

    assert!(config_error(
        m.settings(|s| s.config_path = None),
        "must name"
    ));
    assert!(config_error(
        m.settings(|s| s.config_path = Some("keeper.json".into())),
        "absolute"
    ));
    assert!(config_error(
        m.settings(|s| s.config_path = Some(m.tmp.path().join("absent.json"))),
        "names no file"
    ));
    assert!(config_error(
        m.settings(|s| s.account = Some("me@example.com".into())),
        "account"
    ));
    std::fs::set_permissions(&m.config, std::fs::Permissions::from_mode(0o644)).unwrap();
    let err = m.settings(|_| {}).unwrap_err();
    assert!(
        matches!(err, SecretsError::StorageRefused { .. }),
        "{err:?}"
    );
    std::fs::set_permissions(&m.config, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = m.tmp.path().join("link.json");
    std::os::unix::fs::symlink(&m.config, &link).unwrap();
    let err = m.settings(|s| s.config_path = Some(link)).unwrap_err();
    assert!(
        matches!(err, SecretsError::StorageRefused { .. }),
        "{err:?}"
    );
    assert!(!shim.spawned(), "a refused setting ran keeper");

    // The accepted settings reach argv as `--config <file> --batch-mode`.
    KeeperBackend::new(settings)
        .get(&vault(), &key("K"))
        .unwrap();
    let expected = format!("--config {} --batch-mode ls", m.config.display());
    assert!(shim.calls().starts_with(&expected), "{}", shim.calls());
}

/// Why: ruling 74 — the program is a machine pin, absolute and executable;
/// there is no `PATH` search. A relative program is never spawned, even
/// when it names a real file. Red without the `is_absolute` check in
/// `command`.
/// Test: itself.
#[test]
fn keeper_program_must_be_an_absolute_executable_machine_pin() {
    let shim = KeeperShim::new();
    let m = Machine::new(&shim);
    let marker = m.tmp.path().join("planted-ran");
    let planted_dir = m.tmp.path().join("planted");
    install_script(
        &planted_dir,
        "keeper",
        &format!(": > '{}'\nexit 0", marker.display()),
    );
    let relative = relative_to_cwd(&planted_dir).join("keeper");

    for program in [PathBuf::from("keeper"), relative.clone()] {
        let backend = KeeperBackend::new(KeeperSettings::new(program.clone(), m.config.clone()));
        let err = backend.get(&vault(), &key("K")).unwrap_err();
        assert!(
            matches!(err, SecretsError::CliNotInstalled { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("secrets.keeper.program"), "{err}");
    }
    assert!(!marker.exists(), "a relative program ran");

    assert!(config_error(
        m.settings(|s| s.program = Some(relative)),
        "absolute"
    ));
    let err = m.settings(|s| s.program = None).unwrap_err();
    assert!(
        matches!(err, SecretsError::CliNotInstalled { .. }),
        "{err:?}"
    );
    let not_exec = m.tmp.path().join("keeper-noexec");
    std::fs::write(&not_exec, "#!/bin/sh\n").unwrap();
    let err = m.settings(|s| s.program = Some(not_exec)).unwrap_err();
    assert!(
        matches!(err, SecretsError::CliNotInstalled { .. }),
        "{err:?}"
    );

    // The installed pin runs.
    let backend = KeeperBackend::new(m.settings(|_| {}).unwrap());
    assert!(backend.get(&vault(), &key("K")).unwrap().is_none());
    assert!(shim.spawned());
}

/// Why: #7519 P1 carry-over (a) — Keeper opens only when the machine
/// config enables it, through a `keeper` section or `default_backend`.
/// Opening spawns nothing.
/// Test: itself.
#[test]
fn keeper_open_requires_machine_enablement() {
    let shim = KeeperShim::new();
    let m = Machine::new(&shim);
    let path = m.tmp.path().join("machine.yaml");
    let not_enabled = |path: &Path| matches!(open(path), Err(SecretsError::BackendNotEnabled { backend }) if backend == "keeper");
    assert!(not_enabled(&path), "no file");
    std::fs::write(&path, "secrets:\n  onepassword: {}\n").unwrap();
    assert!(not_enabled(&path), "another backend's section");
    std::fs::write(&path, "secrets:\n  default_backend: keeper\n").unwrap();
    assert!(matches!(open(&path), Err(SecretsError::Config { .. })));
    let yaml = format!(
        "secrets:\n  keeper:\n    program: {}\n    config_path: {}\n",
        m.program.display(),
        m.config.display()
    );
    std::fs::write(&path, yaml).unwrap();
    let opened = open(&path).unwrap();
    assert_eq!(opened.id(), BackendId::keeper());
    assert!(!shim.spawned(), "opening ran keeper");
}

/// Why: A6 — the names-only index lists keys, so the backend is `READ |
/// WRITE` and `list_names` spawns nothing.
/// Test: itself.
#[test]
fn keeper_capabilities_are_read_write_and_list_names_spawns_nothing() {
    let shim = KeeperShim::new();
    let backend = shim.backend();
    assert_eq!(backend.id(), BackendId::keeper());
    assert_eq!(
        backend.capabilities(),
        Capabilities::READ | Capabilities::WRITE
    );
    assert!(matches!(
        backend.list_names(&vault()),
        Err(SecretsError::Unsupported { .. })
    ));
    assert!(!shim.spawned());
}

/// Why: ruling 3 — no output phrase is ever a miss, so `MISSING` is empty;
/// the locked phrases are lowercase and none is generic error text.
/// Test: itself.
#[test]
fn keeper_marker_table_is_narrow_and_has_no_miss_phrases() {
    assert!(markers::MISSING.is_empty());
    for marker in markers::LOCKED {
        assert_eq!(*marker, marker.to_ascii_lowercase());
        for generic in ["not found", "error", "failed", "denied"] {
            assert!(!marker.contains(generic), "{marker}");
        }
    }
}

/// Why: ruling 3 — output this backend cannot read exactly is an error:
/// an empty or non-array listing, an unknown row type, a row missing a
/// field, and a record answer for another uid, title or type, or without
/// one password string. Uids outside Keeper's alphabet are refused.
/// Test: itself.
#[test]
fn keeper_listing_and_record_parsers_fail_closed() {
    for bad in [
        "",
        "Error: no",
        "{}",
        r#"[{"type":"secret","uid":"a"}]"#,
        r#"[{"type":"record","uid":"a","title":"K"}]"#,
        r#"[{"name":"trusty"}]"#,
    ] {
        assert!(record::parse_listing(bad).is_err(), "{bad:?}");
    }
    let rows = record::parse_listing(
        r#"[{"type":"folder","uid":"f","name":"web"},{"type":"record","uid":"a","title":"K","record_type":"login"}]"#,
    )
    .unwrap();
    assert_eq!(record::folders_named(&rows, "web"), 1);
    assert_eq!(record::titled(rows, &key("K")).len(), 1);

    let good = r#"{"record_uid":"a","title":"K","type":"login","fields":[{"type":"password","value":["v"]}]}"#;
    assert_eq!(
        record::password_from(good, "a", &key("K"))
            .unwrap()
            .expose(),
        "v"
    );
    for (stdout, uid, name) in [
        (good, "b", "K"),
        (good, "a", "J"),
        (&good.replace("login", "file") as &str, "a", "K"),
        (
            r#"{"record_uid":"a","title":"K","type":"login","fields":[]}"#,
            "a",
            "K",
        ),
        (
            r#"{"record_uid":"a","title":"K","type":"login","fields":[{"type":"password","value":["v"]},{"type":"password","value":["w"]}]}"#,
            "a",
            "K",
        ),
        (
            r#"{"record_uid":"a","title":"K","type":"login","fields":[{"type":"password","value":[1]}]}"#,
            "a",
            "K",
        ),
        ("Error: no", "a", "K"),
    ] {
        assert!(
            record::password_from(stdout, uid, &key(name)).is_err(),
            "{stdout}"
        );
    }
    let long = "a".repeat(65);
    for bad in ["", "a b", "a/b", "a;b", "..", long.as_str()] {
        assert!(record::checked_uid(bad).is_err(), "{bad:?}");
    }
    for good in ["-leading7519", "AbC_09-x", "a"] {
        assert_eq!(record::checked_uid(good), Ok(good));
    }
}
