//! Registry and vault parity against the Python-written `registry.sqlite3`
//! and `vault.json`, plus the opt-in Rust-to-Python reverse check.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use common::{fixture_key, fixture_service, fixtures, items, json, s, state_dir};
use trusty_browser::audit::Audit;
use trusty_browser::pyjson::Json;
use trusty_browser::registry::{PairingRow, Registry, RegistryError};
use trusty_browser::vault::{self, KeyStore, MemoryKeyStore, Vault, VaultError};

/// `(profile, email, key)` from vault.json, ordered by email like `pairings()`.
fn expected() -> Vec<(String, String, String)> {
    let mut out: Vec<_> = items(json("vault.json").get("pairings").unwrap_or(&Json::Null))
        .iter()
        .map(|p| {
            (
                s(p, "profile").to_owned(),
                s(p, "email").to_owned(),
                s(p, "key").to_owned(),
            )
        })
        .collect();
    out.sort_by(|a, b| a.1.as_bytes().cmp(b.1.as_bytes()));
    out
}

fn store_with(service: &str, state: &Path, key: &str) -> Box<MemoryKeyStore> {
    let store = MemoryKeyStore::new();
    let account = fs::canonicalize(state)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    store
        .set(service, &account, key)
        .unwrap_or_else(|e| panic!("{e}"));
    Box::new(store)
}

fn copy_fixture_registry(state: &Path) {
    let dest = state.join("registry.sqlite3");
    fs::copy(fixtures().join("registry.sqlite3"), &dest).unwrap_or_else(|e| panic!("{e}"));
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o600)).unwrap_or_default();
}

fn master(db: &rusqlite::Connection) -> Vec<(String, String, Option<String>)> {
    let mut stmt = db
        .prepare("SELECT type, name, sql FROM sqlite_master ORDER BY name")
        .unwrap_or_else(|e| panic!("{e}"));
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap_or_else(|e| panic!("{e}"));
    rows.map(|r| r.unwrap_or_else(|e| panic!("{e}"))).collect()
}

#[test]
fn fixture_registry_unseals_every_pairing() {
    let state = state_dir();
    copy_fixture_registry(state.path());
    let before = master(
        &rusqlite::Connection::open(fixtures().join("registry.sqlite3"))
            .unwrap_or_else(|e| panic!("{e}")),
    );
    let registry = Registry::open(state.path()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        master(registry.connection()),
        before,
        "open must not change the Python schema"
    );
    let vault = Vault::with_service(
        state.path(),
        &fixture_service(),
        store_with(&fixture_service(), state.path(), &fixture_key()),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let want = expected();
    let listed = registry.pairings().unwrap_or_default();
    assert_eq!(
        listed,
        want.iter()
            .map(|(p, e, _)| (p.clone(), e.clone()))
            .collect::<Vec<_>>()
    );
    for (profile, email, key) in &want {
        let row = registry
            .pairing(profile)
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("{profile}"));
        assert_eq!(&row.email, email);
        assert!(row.browser_key.starts_with(vault::PREFIX));
        let opened = vault.unseal(&format!("browser:{profile}"), &row.browser_key);
        assert_eq!(opened.ok().as_ref(), Some(key), "{email}");
        // The AAD binds the profile: another profile's purpose cannot open it.
        assert!(matches!(
            vault.unseal("browser:other", &row.browser_key),
            Err(VaultError::CannotOpen)
        ));
    }
}

#[test]
fn fresh_schema_matches_python_sqlite_master() {
    let state = state_dir();
    let registry = Registry::open(state.path()).unwrap_or_else(|e| panic!("{e}"));
    let python = rusqlite::Connection::open(fixtures().join("registry.sqlite3"))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(master(registry.connection()), master(&python));
    let secure: i64 = registry
        .connection()
        .query_row("PRAGMA secure_delete", [], |r| r.get(0))
        .unwrap_or(0);
    assert_eq!(secure, 1);
    let mode = fs::metadata(registry.path())
        .map(|m| m.permissions().mode() & 0o777)
        .ok();
    assert_eq!(mode, Some(0o600));
}

#[test]
fn registry_refuses_a_group_readable_state_dir() {
    let state = state_dir();
    fs::set_permissions(state.path(), fs::Permissions::from_mode(0o750)).unwrap_or_default();
    assert!(matches!(
        Registry::open(state.path()),
        Err(RegistryError::NotPrivate(_))
    ));
    fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap_or_default();
    let file = state.path().join("registry.sqlite3");
    fs::write(&file, b"").unwrap_or_default();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap_or_default();
    assert!(matches!(
        Registry::open(state.path()),
        Err(RegistryError::NotPrivate(_))
    ));
}

#[test]
fn rust_seal_round_trips_and_binds_aad() {
    let state = state_dir();
    let vault =
        Vault::new(state.path(), Box::new(MemoryKeyStore::new())).unwrap_or_else(|e| panic!("{e}"));
    assert!(matches!(
        vault.unseal("browser:p", "tb1.AAAA"),
        Err(VaultError::CannotOpen)
    ));
    let sealed = vault
        .seal("browser:p", "secret")
        .unwrap_or_else(|e| panic!("{e}"));
    // 4 + base64(12 + 6 + 16) = 4 + 48
    assert_eq!(sealed.len(), 52, "{sealed}");
    assert_eq!(
        vault.unseal("browser:p", &sealed).ok().as_deref(),
        Some("secret")
    );
    assert!(matches!(
        vault.unseal("browser:q", &sealed),
        Err(VaultError::CannotOpen)
    ));
    assert!(matches!(
        vault.unseal("browser:p", &sealed[1..]),
        Err(VaultError::Invalid)
    ));
    let mut tampered = sealed.clone().into_bytes();
    let last = tampered.len() - 2;
    tampered[last] = if tampered[last] == b'A' { b'B' } else { b'A' };
    let tampered = String::from_utf8(tampered).unwrap_or_default();
    assert!(matches!(
        vault.unseal("browser:p", &tampered),
        Err(VaultError::CannotOpen)
    ));
    // Python maps -_ to +/ then decodes the standard alphabet, so both spellings open.
    let standard = sealed.replace('-', "+").replace('_', "/");
    assert_eq!(
        vault.unseal("browser:p", &standard).ok().as_deref(),
        Some("secret")
    );

    let other = state_dir();
    let reader =
        Vault::new(other.path(), Box::new(MemoryKeyStore::new())).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        matches!(
            reader.unseal("browser:p", &sealed),
            Err(VaultError::Missing)
        ),
        "a read never creates a key"
    );
}

#[test]
fn fixture_key_reseals_to_python_bytes() {
    // A Rust seal with the nonce Python used reproduces the stored value exactly.
    let state = state_dir();
    copy_fixture_registry(state.path());
    let registry = Registry::open(state.path()).unwrap_or_else(|e| panic!("{e}"));
    let vault = Vault::new(
        state.path(),
        store_with(vault::SERVICE, state.path(), &fixture_key()),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    use base64::Engine as _;
    for (profile, _, key) in expected() {
        let row = registry
            .pairing(&profile)
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("{profile}"));
        let raw = base64::engine::general_purpose::URL_SAFE
            .decode(&row.browser_key[vault::PREFIX.len()..])
            .unwrap_or_default();
        let again = vault.seal_with_nonce(&format!("browser:{profile}"), &key, &raw[..12]);
        assert_eq!(again.ok().as_ref(), Some(&row.browser_key), "{profile}");
    }
}

/// Write a state dir with Rust: fixture pairings sealed under the production
/// service, plus audit lines; return it for the Python reader.
fn rust_written_state() -> tempfile::TempDir {
    let state = state_dir();
    let mut registry = Registry::open(state.path()).unwrap_or_else(|e| panic!("{e}"));
    let vault = Vault::new(
        state.path(),
        store_with(vault::SERVICE, state.path(), &fixture_key()),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    for (profile, email, key) in expected() {
        let browser_key = vault
            .seal(&format!("browser:{profile}"), &key)
            .unwrap_or_else(|e| panic!("{e}"));
        registry
            .insert_pairing(&PairingRow {
                profile,
                email,
                browser_key,
            })
            .unwrap_or_else(|e| panic!("{e}"));
    }
    let log = Audit::new(state.path());
    log.refuse(
        "pair",
        "enroll_conflict_key",
        Some("p"),
        Some("exämple.test"),
    );
    log.write(
        "operation",
        &[
            ("op", Some(Json::from("tabs"))),
            ("tab", Some(Json::Int(7))),
        ],
    )
    .unwrap_or_else(|e| panic!("{e}"));
    state
}

#[test]
fn rust_written_state_reads_back_in_rust() {
    let state = rust_written_state();
    let registry = Registry::open(state.path()).unwrap_or_else(|e| panic!("{e}"));
    let vault = Vault::new(
        state.path(),
        store_with(vault::SERVICE, state.path(), &fixture_key()),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    for (profile, _, key) in expected() {
        let row = registry
            .pairing(&profile)
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("{profile}"));
        assert_eq!(
            vault
                .unseal(&format!("browser:{profile}"), &row.browser_key)
                .ok(),
            Some(key)
        );
    }
}

/// Opt-in: set `TRUSTY_BROWSER_PY` to a Python with trusty-browser 0.7.0 installed.
#[test]
fn rust_written_state_reads_in_python_0_7_0() {
    let Ok(python) = std::env::var("TRUSTY_BROWSER_PY") else {
        eprintln!("skipped: TRUSTY_BROWSER_PY not set");
        return;
    };
    let state = rust_written_state();
    let home = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = Command::new(python)
        .arg("-I")
        .arg(fixtures().join("capture/reverse_check.py"))
        .arg(state.path())
        .arg(fixture_key())
        .env("HOME", home.path())
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    eprintln!("python stdout: {stdout}");
    assert!(
        out.status.success(),
        "python failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = Json::parse(stdout.trim().as_bytes()).unwrap_or(Json::Null);
    let got: Vec<_> = items(report.get("pairings").unwrap_or(&Json::Null))
        .iter()
        .map(|p| {
            (
                s(p, "profile").to_owned(),
                s(p, "email").to_owned(),
                s(p, "key").to_owned(),
            )
        })
        .collect();
    assert_eq!(got, expected());
    assert_eq!(report.get("audit_lines"), Some(&Json::Int(2)));
}
