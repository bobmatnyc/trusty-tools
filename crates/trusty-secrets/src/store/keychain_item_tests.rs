//! Tests for the any-item Keychain read (#10998). None touches the OS.
//!
//! Test: itself.

use std::time::Duration;

use super::*;

fn label(text: &str) -> KeychainLabel {
    KeychainLabel::new(text).unwrap()
}

fn found(_: &KeychainLabel, _: &KeychainLabel) -> Result<Option<SecretValue>, SecretsError> {
    Ok(Some(SecretValue::new("item-value-10998")))
}

fn missing(_: &KeychainLabel, _: &KeychainLabel) -> Result<Option<SecretValue>, SecretsError> {
    Ok(None)
}

fn hangs(_: &KeychainLabel, _: &KeychainLabel) -> Result<Option<SecretValue>, SecretsError> {
    std::thread::sleep(Duration::from_secs(5));
    Ok(None)
}

fn panics(_: &KeychainLabel, _: &KeychainLabel) -> Result<Option<SecretValue>, SecretsError> {
    panic!("simulated keychain fault")
}

/// Why: #10998 — a read that blocks on an unanswered access prompt must end
/// at the limit as a timeout, never a miss; a found item, a miss and a
/// panicked read pass through as themselves.
/// Test: itself.
#[test]
fn keychain_item_read_times_out_and_passes_results_through() {
    let (service, account) = (label("gh:github.com"), label("octocat"));
    let limit = Duration::from_secs(2);
    let value = read_within(&service, &account, limit, found).unwrap();
    assert_eq!(
        value.map(|v| v.expose().to_owned()).as_deref(),
        Some("item-value-10998")
    );
    assert!(
        read_within(&service, &account, limit, missing)
            .unwrap()
            .is_none()
    );
    let short = Duration::from_millis(50);
    assert!(matches!(
        read_within(&service, &account, short, hangs),
        Err(SecretsError::Timeout { .. })
    ));
    assert!(matches!(
        read_within(&service, &account, limit, panics),
        Err(SecretsError::Backend { .. })
    ));
}

/// Why: #9064's rule for the any-item read — off macOS no `keyring` is
/// linked, so the read fails closed instead of reaching a mock store.
/// Test: itself.
#[cfg(not(target_os = "macos"))]
#[test]
fn keychain_item_read_fails_closed_off_macos() {
    let result = read_keychain_item(&label("gh:github.com"), &label("octocat"), os_read);
    assert!(matches!(result, Err(SecretsError::UnknownBackend { .. })));
}
