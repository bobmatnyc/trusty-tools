//! Tests for [`super::OnePasswordBackend`] (#7519 P2) against [`OpShim`],
//! a fake `op` run by absolute path. No test runs the real `op`, reads the
//! Keychain, or changes `PATH` or any other process-global environment.
//!
//! Test: itself.

use std::ffi::OsString;
use std::path::PathBuf;

use tempfile::TempDir;

use super::shim::OpShim;
use super::{
    OnePasswordBackend, OnePasswordSettings, SERVICE_ACCOUNT_TOKEN_ENV, inherited_op_vars, item,
    markers, open, token_from,
};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};
use crate::store::config::{CliSettings, MachineSecretsConfig};
use crate::store::{Capabilities, NamesIndex, SecretBackend, SecretStore};

const VALUE: &str = "sk-op-canary-7519-0123456789abcdef";
const TOKEN: &str = "ops_token_canary_7519_fedcba9876543210";
const SIGNED_OUT: &str = "[ERROR] 2026/10/07 You are not currently signed in.";

fn vault() -> VaultName {
    VaultName::new("trusty/acme/web").unwrap()
}

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).unwrap()
}

fn value() -> SecretValue {
    SecretValue::new(VALUE)
}

/// A shim, a temp dir for templates, and a backend over both.
struct Fx {
    shim: OpShim,
    tmp: TempDir,
    backend: OnePasswordBackend,
}

impl Fx {
    fn templates(&self) -> PathBuf {
        self.tmp.path().join("tmp")
    }

    /// Template guard directories left under the template root.
    fn leftover_templates(&self) -> usize {
        std::fs::read_dir(self.templates())
            .map(|dir| dir.filter_map(Result::ok).count())
            .unwrap_or(0)
    }
}

fn fx_with(edit: impl FnOnce(&mut OnePasswordSettings)) -> Fx {
    let shim = OpShim::new();
    let tmp = TempDir::new().unwrap();
    let mut settings = shim.settings(&tmp.path().join("tmp"));
    edit(&mut settings);
    let backend = OnePasswordBackend::new(settings);
    Fx { shim, tmp, backend }
}

fn new_fx() -> Fx {
    fx_with(|_| {})
}

/// `err` rendered every way a caller could log it.
fn shown(err: &SecretsError) -> String {
    format!("{err} {err:?}")
}

/// Why: A1 and owner ruling 2026-10-07 — a new key is `op item create -`
/// with the value only inside the stdin template; argv and env never hold
/// it, and the key never reaches argv at all.
/// Test: itself.
#[test]
fn onepassword_set_creates_with_the_value_on_stdin_only() {
    let fx = new_fx();
    fx.backend.set(&vault(), &key("API_KEY"), &value()).unwrap();
    let calls = fx.shim.calls();
    assert_eq!(
        calls.lines().collect::<Vec<_>>(),
        [
            "item list --vault trusty/acme/web --format json",
            "item create --vault trusty/acme/web -",
        ]
    );
    let stdin = fx.shim.stdin_log();
    assert!(stdin.contains(VALUE) && stdin.contains(r#""title":"API_KEY""#));
    assert!(!calls.contains(VALUE) && !fx.shim.env_log().contains(VALUE));

    let read = fx.backend.get(&vault(), &key("API_KEY")).unwrap().unwrap();
    assert_eq!(read.expose(), VALUE);
    let calls = fx.shim.calls();
    assert!(
        calls
            .lines()
            .last()
            .unwrap()
            .starts_with("read --no-newline op://vault7519/new"),
        "{calls}"
    );
    assert!(!calls.contains("API_KEY"), "the key reached argv: {calls}");
}

/// Why: A1 and owner ruling 2026-10-07 — an existing key is `op item edit
/// <id> --template <file>`: the value is only in the template file, which
/// is gone afterwards. Never delete-then-create.
/// Test: itself.
#[test]
fn onepassword_set_edits_through_a_template_file_that_is_removed() {
    let fx = new_fx();
    fx.shim.seed("item1", "API_KEY", "old-value", "PASSWORD");
    fx.backend.set(&vault(), &key("API_KEY"), &value()).unwrap();
    let calls = fx.shim.calls();
    let edit = calls.lines().nth(1).unwrap();
    assert!(
        edit.starts_with("item edit item1 --vault trusty/acme/web --template /"),
        "{calls}"
    );
    assert_eq!(calls.lines().count(), 2, "no create and no delete: {calls}");
    assert!(fx.shim.template_log().contains(VALUE));
    assert!(fx.shim.stdin_log().is_empty());
    assert!(!calls.contains(VALUE) && !fx.shim.env_log().contains(VALUE));
    assert_eq!(fx.leftover_templates(), 0, "the template file was left");
    let items = fx.shim.items();
    let expected = (
        "item1".to_string(),
        "API_KEY".to_string(),
        VALUE.to_string(),
    );
    assert_eq!(items, [expected]);
}

/// Why: A2 — no value appears in an error or `Debug`, even when `op`
/// echoes its stdin or template to stderr, or prints a value and then
/// fails. A failed edit still removes its template file.
/// Test: itself.
#[test]
fn onepassword_errors_never_carry_the_value() {
    let fx = new_fx();
    fx.shim.fail("create", "[ERROR] could not create item");
    let err = fx.backend.set(&vault(), &key("A"), &value()).unwrap_err();
    assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");
    assert!(!shown(&err).contains(VALUE), "{}", shown(&err));

    let fx = new_fx();
    fx.shim.seed("item1", "A", "old-value", "PASSWORD");
    fx.shim.fail("edit", "[ERROR] could not edit item");
    let err = fx.backend.set(&vault(), &key("A"), &value()).unwrap_err();
    assert!(!shown(&err).contains(VALUE), "{}", shown(&err));
    assert_eq!(fx.leftover_templates(), 0, "a failed edit left its file");

    let fx = new_fx();
    fx.shim.seed("item1", "A", VALUE, "PASSWORD");
    fx.shim.fail("read", "[ERROR] network unreachable");
    let err = fx.backend.get(&vault(), &key("A")).unwrap_err();
    assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");
    assert!(!shown(&err).contains(VALUE), "{}", shown(&err));
    assert!(!format!("{:?}", fx.backend).contains(VALUE));
}

/// Why: A3 — a missing item or vault is a miss; a locked, signed-out or
/// unknown failure is an error and never a miss.
/// Test: itself.
#[test]
fn onepassword_missing_is_none_and_failures_are_errors() {
    let (v, k) = (vault(), key("API_KEY"));
    let fx = new_fx();
    assert!(fx.backend.get(&v, &k).unwrap().is_none(), "empty vault");
    assert!(!fx.backend.delete(&v, &k).unwrap());

    let fx = new_fx();
    fx.shim.fail(
        "list",
        "[ERROR] \"trusty/acme/web\" isn't a vault in this account.",
    );
    assert!(fx.backend.get(&v, &k).unwrap().is_none(), "no such vault");
    assert!(!fx.backend.delete(&v, &k).unwrap());
    let err = fx.backend.set(&v, &k, &value()).unwrap_err();
    assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");

    let fx = new_fx();
    fx.shim.seed("item1", "API_KEY", VALUE, "PASSWORD");
    fx.shim.fail("read", "[ERROR] \"item1\" isn't an item.");
    assert!(fx.backend.get(&v, &k).unwrap().is_none(), "gone mid-read");

    for (stderr, locked) in [
        (SIGNED_OUT, true),
        ("[ERROR] session expired, sign in again", true),
        ("[ERROR] authorization prompt dismissed", true),
        (
            "[ERROR] dial tcp: lookup my.1password.com: no such host",
            false,
        ),
        ("[ERROR] something new", false),
    ] {
        let fx = new_fx();
        fx.shim.seed("item1", "API_KEY", VALUE, "PASSWORD");
        fx.shim.fail("list", stderr);
        let errors = [
            fx.backend.get(&v, &k).map(drop).unwrap_err(),
            fx.backend.set(&v, &k, &value()).unwrap_err(),
            fx.backend.delete(&v, &k).map(drop).unwrap_err(),
        ];
        for err in errors {
            let is_locked = matches!(err, SecretsError::BackendLocked { .. });
            let is_backend = matches!(err, SecretsError::Backend { .. });
            assert!(
                if locked { is_locked } else { is_backend },
                "{stderr}: {err:?}"
            );
        }
        assert_eq!(fx.shim.items().len(), 1, "{stderr}: the item was touched");
    }

    let fx = new_fx();
    fx.shim.seed("item1", "API_KEY", VALUE, "PASSWORD");
    fx.shim.fail("delete", "[ERROR] session expired");
    let err = fx.backend.delete(&v, &k).unwrap_err();
    assert!(matches!(err, SecretsError::BackendLocked { .. }), "{err:?}");
}

/// Why: A4 — a missing `op` is a typed error naming the program and the fix.
/// Test: itself.
#[test]
fn onepassword_missing_cli_is_cli_not_installed() {
    let fx = fx_with(|s| {
        s.program = "/nonexistent/op-7519".into();
        s.leading_args.clear();
    });
    let err = fx.backend.get(&vault(), &key("A")).unwrap_err();
    match err {
        SecretsError::CliNotInstalled { program, hint } => {
            assert_eq!(program, "/nonexistent/op-7519");
            assert!(hint.contains("1Password CLI"), "{hint}");
        }
        other => panic!("expected CliNotInstalled, got {other:?}"),
    }
}

/// Why: A4/A10 — headless with no token, `op` fails and the backend reports
/// it locked: no prompt, no fallback. With a token, it reaches the child
/// only through the environment overlay, never argv, stdin, an error or
/// `Debug`.
/// Test: itself.
#[test]
fn onepassword_headless_without_a_token_fails_closed() {
    let fx = new_fx();
    fx.shim.headless(TOKEN);
    let err = fx.backend.get(&vault(), &key("A")).unwrap_err();
    assert!(matches!(err, SecretsError::BackendLocked { .. }), "{err:?}");
    assert!(!fx.shim.env_log().contains(TOKEN));

    let fx = fx_with(|s| s.token = Some(SecretValue::new(TOKEN)));
    fx.shim.headless(TOKEN);
    fx.backend.set(&vault(), &key("A"), &value()).unwrap();
    let read = fx.backend.get(&vault(), &key("A")).unwrap().unwrap();
    assert_eq!(read.expose(), VALUE);
    let overlay = format!("{SERVICE_ACCOUNT_TOKEN_ENV}={TOKEN}");
    assert!(fx.shim.env_log().contains(&overlay));
    assert!(!fx.shim.calls().contains(TOKEN) && !fx.shim.stdin_log().contains(TOKEN));
    assert!(!format!("{:?}", fx.backend).contains(TOKEN));
    fx.shim.fail("delete", "[ERROR] could not delete");
    let err = fx.backend.delete(&vault(), &key("A")).unwrap_err();
    assert!(!shown(&err).contains(TOKEN), "{}", shown(&err));
}

/// Why: A6 — capabilities are declared honestly, and an operation refused
/// before the backend spawns no process.
/// Test: itself.
#[test]
fn onepassword_capabilities_are_read_write_and_list_names_spawns_nothing() {
    let fx = new_fx();
    let caps = fx.backend.capabilities();
    assert_eq!(caps, Capabilities::READ | Capabilities::WRITE);
    assert_eq!(fx.backend.id(), BackendId::onepassword());
    let err = fx.backend.list_names(&vault()).unwrap_err();
    assert!(matches!(err, SecretsError::Unsupported { .. }), "{err:?}");

    let index = NamesIndex::at(fx.tmp.path().join("index"));
    let store = SecretStore::new(std::sync::Arc::new(fx.shim.backend(&fx.templates())), index);
    let err = store
        .set(&vault(), &key("A"), &SecretValue::new(""))
        .unwrap_err();
    assert!(matches!(err, SecretsError::InvalidValue { .. }), "{err:?}");
    assert!(!fx.shim.spawned(), "a refused operation spawned `op`");
}

/// Why: A7 — only validated names reach argv or a reference. The reference
/// builder refuses `/`, `..`, `op://`, a newline and a leading `-` in any
/// segment, and the same strings cannot become a key.
/// Test: itself.
#[test]
fn onepassword_item_path_builder_refuses_hostile_segments() {
    let long = "x".repeat(65);
    let hostile = [
        "",
        ".",
        "..",
        "/",
        "a/b",
        "../vault",
        "op://vault/item/password",
        "op:",
        "a\nb",
        "a\rb",
        "-rf",
        "--vault",
        "a b",
        "a\"b",
        long.as_str(),
    ];
    for raw in hostile {
        assert!(item::checked_id(raw).is_err(), "{raw:?}");
        assert!(item::op_reference(raw, "item1").is_err(), "vault {raw:?}");
        assert!(item::op_reference("vault1", raw).is_err(), "item {raw:?}");
    }
    assert_eq!(
        item::op_reference("vault7519", "abc-123_x.y").unwrap(),
        "op://vault7519/abc-123_x.y/password"
    );
    for raw in [
        "/etc",
        "a/b",
        "..",
        "op://v/i/f",
        "a\nb",
        "-rf",
        "--account",
    ] {
        assert!(SecretKey::new(raw).is_err(), "key {raw:?}");
    }
    // A key that is valid but odd never reaches argv; it is only the title.
    let fx = new_fx();
    fx.backend.set(&vault(), &key("a..b"), &value()).unwrap();
    assert!(!fx.shim.calls().contains("a..b"), "{}", fx.shim.calls());
}

/// Why: A7 — an id `op item list` returned is checked before it reaches
/// argv: a hostile one stops every operation after the listing.
/// Test: itself.
#[test]
fn onepassword_hostile_list_ids_never_reach_argv() {
    let fx = new_fx();
    fx.shim.seed("-rf", "API_KEY", VALUE, "PASSWORD");
    let (v, k) = (vault(), key("API_KEY"));
    assert!(fx.backend.get(&v, &k).is_err());
    assert!(fx.backend.set(&v, &k, &value()).is_err());
    assert!(fx.backend.delete(&v, &k).is_err());
    let calls = fx.shim.calls();
    assert!(
        calls.lines().all(|l| l.starts_with("item list ")),
        "{calls}"
    );
}

/// Why: a key must map to one Password item. Two items with its title are
/// refused for read and write, and a delete clears both; an item of
/// another category is never read, edited or deleted.
/// Test: itself.
#[test]
fn onepassword_duplicates_and_foreign_items_are_refused() {
    let (v, k) = (vault(), key("API_KEY"));
    let fx = new_fx();
    fx.shim.seed("a1", "API_KEY", "one", "PASSWORD");
    fx.shim.seed("a2", "API_KEY", "two", "PASSWORD");
    assert!(fx.backend.get(&v, &k).is_err());
    assert!(fx.backend.set(&v, &k, &value()).is_err());
    assert_eq!(fx.shim.items().len(), 2);
    assert!(fx.backend.delete(&v, &k).unwrap());
    assert!(fx.shim.items().is_empty());

    let fx = new_fx();
    fx.shim.seed("l1", "API_KEY", "login-password", "LOGIN");
    assert!(fx.backend.get(&v, &k).is_err());
    assert!(fx.backend.set(&v, &k, &value()).is_err());
    assert!(fx.backend.delete(&v, &k).is_err());
    assert_eq!(fx.shim.items()[0].2, "login-password");
    let calls = fx.shim.calls();
    assert!(
        calls.lines().all(|l| l.starts_with("item list ")),
        "{calls}"
    );
}

/// Why: owner ruling 2026-10-07 — `account` and `config_path` come only
/// from the machine config, and as argv flags, never the env overlay. A
/// value that is not one safe argv word is refused, without echoing it.
/// Test: itself.
#[test]
fn onepassword_machine_settings_reach_argv() {
    let shim = OpShim::new();
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("machine.yaml");
    let machine = |account: &str, config: &str| MachineSecretsConfig {
        onepassword: Some(CliSettings {
            account: Some(account.to_string()),
            config_path: Some(PathBuf::from(config)),
        }),
        ..MachineSecretsConfig::default()
    };
    let mut settings = OnePasswordSettings::from_machine(
        &machine("my.1password.com", "/opt/op-config"),
        &path,
        tmp.path().join("tmp"),
        Some(SecretValue::new("")),
    )
    .unwrap();
    assert!(settings.token.is_none(), "an empty token is no token");
    let shimmed = shim.settings(&tmp.path().join("tmp"));
    settings.program = shimmed.program;
    settings.leading_args = shimmed.leading_args;
    let backend = OnePasswordBackend::new(settings);
    assert!(backend.get(&vault(), &key("A")).unwrap().is_none());
    assert!(
        shim.calls()
            .starts_with("--account my.1password.com --config /opt/op-config item list "),
        "{}",
        shim.calls()
    );
    assert!(!shim.env_log().contains("my.1password.com"));

    for (account, config) in [
        ("--evil", "/opt/x"),
        ("a b", "/opt/x"),
        ("ok", "relative/dir"),
    ] {
        let err = OnePasswordSettings::from_machine(
            &machine(account, config),
            &path,
            tmp.path().join("tmp"),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, SecretsError::Config { .. }), "{err:?}");
        assert!(!err.to_string().contains("--evil"), "{err}");
    }
}

/// Why: #7519 P1 carry-over (a) — 1Password opens only when the untracked
/// machine config enables it, so every value this server writes there is
/// one the delete sweep reaches. Opening spawns nothing.
/// Test: itself.
#[test]
fn onepassword_open_requires_machine_enablement() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("machine.yaml");
    let templates = tmp.path().join("tmp");
    let open_with = |yaml: Option<&str>| {
        if let Some(yaml) = yaml {
            std::fs::write(&path, yaml).unwrap();
        }
        open(&path, &templates, None)
    };
    for yaml in [None, Some("secrets:\n  default_backend: keychain\n")] {
        let err = open_with(yaml).unwrap_err();
        assert!(
            matches!(&err, SecretsError::BackendNotEnabled { backend } if backend == "onepassword"),
            "{err:?}"
        );
    }
    for yaml in [
        "secrets:\n  onepassword: {}\n",
        "secrets:\n  default_backend: onepassword\n",
    ] {
        let backend = open_with(Some(yaml)).unwrap();
        assert_eq!(backend.id(), BackendId::onepassword());
    }
    let err = open_with(Some("secrets:\n  onepassword:\n    account: '-bad'\n")).unwrap_err();
    assert!(matches!(err, SecretsError::Config { .. }), "{err:?}");

    let enabled = MachineSecretsConfig {
        onepassword: Some(CliSettings::default()),
        ..MachineSecretsConfig::default()
    };
    assert!(enabled.enables(&BackendId::onepassword()));
    assert!(!enabled.enables(&BackendId::keychain()));
    assert!(!MachineSecretsConfig::default().enables(&BackendId::onepassword()));
}

/// Why: the server keeps its first spawner's environment; every `OP_*`
/// variable but an `op signin` session is removed, and the token is read
/// first so it can reach `op` through the overlay only.
/// Test: itself.
#[test]
fn onepassword_inherited_op_vars_keep_only_sessions() {
    let names = [
        "OP_SERVICE_ACCOUNT_TOKEN",
        "OP_CONNECT_HOST",
        "OP_CONNECT_TOKEN",
        "OP_ACCOUNT",
        "OP_CONFIG_DIR",
        "OP_SESSION_my",
        "PATH",
        "OPX",
        "op_lower",
    ];
    let removed: Vec<String> = inherited_op_vars(names.iter().map(|n| OsString::from(*n)))
        .into_iter()
        .map(|n| n.into_string().unwrap())
        .collect();
    assert_eq!(removed, &names[..5]);
    let token = token_from(|name| (name == SERVICE_ACCOUNT_TOKEN_ENV).then(|| TOKEN.to_string()));
    assert_eq!(token.unwrap().expose(), TOKEN);
    assert!(token_from(|_| Some(String::new())).is_none());
    assert!(token_from(|_| None).is_none());
}

/// Why: A3 — the marker table decides miss versus error. A phrase in both
/// lists, or a generic one, could read a failure as a miss.
/// Test: itself.
#[test]
fn onepassword_marker_table_is_narrow_and_disjoint() {
    for marker in markers::MISSING.iter().chain(markers::LOCKED) {
        assert!(marker.len() >= 10, "{marker:?} is too generic");
        assert_eq!(*marker, marker.to_ascii_lowercase());
        assert!(!marker.contains("not found"), "{marker:?}");
    }
    for missing in markers::MISSING {
        for locked in markers::LOCKED {
            assert!(!missing.contains(locked) && !locked.contains(missing));
        }
    }
}
