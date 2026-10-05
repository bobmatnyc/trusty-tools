//! Unit tests for the Keychain backend's result mapping (macOS, where
//! `keyring` is linked) and its no-backend arm (every other target). None
//! touches the OS keychain; the real round trip is the ignored integration
//! test.
//!
//! Test: itself.

use super::*;

const FAKE_VALUE: &str = "sk-fake-keychain-7a6b5c";

fn names() -> (VaultName, SecretKey) {
    (
        VaultName::new("trusty/acme/web").unwrap(),
        SecretKey::new("API_KEY").unwrap(),
    )
}

/// Why: the Keychain cannot enumerate, so it must not claim `LIST_NAMES`;
/// listing goes through the index.
/// Test: itself.
#[test]
fn keychain_capabilities_are_read_write_only() {
    let caps = KeychainBackend::new().capabilities();
    assert!(caps.contains(Capabilities::READ | Capabilities::WRITE));
    assert!(!caps.contains(Capabilities::LIST_NAMES));
    assert!(!caps.contains(Capabilities::SYNC_TARGET));
    assert_eq!(KeychainBackend::new().id().as_str(), "keychain");
}

/// Why: fail closed — a locked or broken keychain must surface as an error,
/// never read as "no such secret". Downgrading the failure arm to `Ok(None)`
/// fails this test.
/// Test: itself.
#[test]
#[cfg(target_os = "macos")]
fn keychain_get_maps_no_entry_to_none_and_failures_to_errors() {
    let (vault, key) = names();
    let hit = map_get(&vault, &key, Ok(FAKE_VALUE.to_string())).unwrap();
    assert_eq!(hit.unwrap().expose(), FAKE_VALUE);

    assert!(
        map_get(&vault, &key, Err(keyring::Error::NoEntry))
            .unwrap()
            .is_none()
    );

    let locked = keyring::Error::NoStorageAccess("keychain is locked".into());
    let err = map_get(&vault, &key, Err(locked)).unwrap_err();
    match err {
        SecretsError::Backend {
            backend,
            vault,
            key,
            reason,
        } => {
            assert_eq!(backend, "keychain");
            assert_eq!(vault, "trusty/acme/web");
            assert_eq!(key, "API_KEY");
            assert!(reason.contains("keychain is locked"), "{reason}");
        }
        other => panic!("expected a Backend error, got {other:?}"),
    }
}

/// Why: same fail-closed rule for removal — a refused delete must not read
/// as "already gone".
/// Test: itself.
#[test]
#[cfg(target_os = "macos")]
fn keychain_delete_maps_no_entry_to_false_and_failures_to_errors() {
    let (vault, key) = names();
    assert!(map_delete(&vault, &key, Ok(())).unwrap());
    assert!(!map_delete(&vault, &key, Err(keyring::Error::NoEntry)).unwrap());
    let err = map_delete(
        &vault,
        &key,
        Err(keyring::Error::PlatformFailure("errSecIO".into())),
    )
    .unwrap_err();
    assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");
}

/// Why: `BadEncoding` carries the stored bytes — the secret. The mapped
/// error, in both `Display` and `Debug`, must not contain them.
/// Test: itself.
#[test]
#[cfg(target_os = "macos")]
fn keychain_error_text_never_carries_value_bytes() {
    let (vault, key) = names();
    let leaked = keyring::Error::BadEncoding(FAKE_VALUE.as_bytes().to_vec());
    let err = map_get(&vault, &key, Err(leaked)).unwrap_err();
    let shown = format!("{err} / {err:?}");
    assert!(!shown.contains(FAKE_VALUE), "{shown}");
    assert!(shown.contains("not valid UTF-8"), "{shown}");

    let invalid = keyring::Error::Invalid("user".into(), FAKE_VALUE.into());
    assert!(!keyring_reason(&invalid).contains(FAKE_VALUE));
}

/// Why: #9064 — the no-backend arm must be an error, never a silent success.
/// What: the error names the `keychain` backend as unavailable in this build.
/// Test: itself.
#[test]
fn keychain_without_os_backend_fails_closed() {
    match no_os_backend() {
        SecretsError::UnknownBackend { backend } => assert_eq!(backend, "keychain"),
        other => panic!("expected UnknownBackend, got {other:?}"),
    }
}

/// Why: #9064 — off macOS no `keyring` is linked. A build that fell back to
/// `keyring`'s in-memory mock would accept the write and return the value on
/// read; this test fails on either.
/// Test: itself.
#[test]
#[cfg(not(target_os = "macos"))]
fn keychain_backend_fails_closed_off_macos() {
    let (vault, key) = names();
    let backend = KeychainBackend::new();
    let value = SecretValue::new(FAKE_VALUE.to_string());
    let unavailable = |r: Result<(), SecretsError>| {
        assert!(
            matches!(r, Err(SecretsError::UnknownBackend { ref backend }) if backend == "keychain"),
            "{r:?}"
        );
    };
    unavailable(backend.set(&vault, &key, &value));
    unavailable(backend.get(&vault, &key).map(|_| ()));
    unavailable(backend.delete(&vault, &key).map(|_| ()));
}
