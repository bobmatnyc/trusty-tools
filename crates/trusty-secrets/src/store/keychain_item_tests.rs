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
    assert!(matches!(
        read_within(&service, &account, limit, panics),
        Err(SecretsError::Backend { .. })
    ));
    // A hung read gets its own item, so its abandoned thread blocks no
    // other assertion here.
    let short = Duration::from_millis(50);
    assert!(matches!(
        read_within(&service, &label("hung-octocat"), short, hangs),
        Err(SecretsError::Timeout { .. })
    ));
}

fn hangs_briefly(
    _: &KeychainLabel,
    _: &KeychainLabel,
) -> Result<Option<SecretValue>, SecretsError> {
    std::thread::sleep(Duration::from_millis(400));
    Ok(None)
}

/// Why: #10998 fix round 1 (code-critic MEDIUM) — while a timed-out read of
/// an item still runs, a retry of that item fails at once with a timeout
/// and starts no thread (no second prompt); once the hung read ends, the
/// item reads again. Red when the abandoned set is not consulted.
/// Test: itself.
#[test]
fn keychain_item_read_of_a_hung_item_fails_fast_until_it_ends() {
    let (service, account) = (label("gh:github.com"), label("prompt-open-10998"));
    let short = Duration::from_millis(50);
    assert!(matches!(
        read_within(&service, &account, short, hangs_briefly),
        Err(SecretsError::Timeout { .. })
    ));
    let retry = read_within(&service, &account, Duration::from_secs(2), found);
    assert!(
        matches!(retry, Err(SecretsError::Timeout { waited, .. }) if waited.is_zero()),
        "a retry of a hung item must fail at once"
    );
    let other = read_within(
        &service,
        &label("other-10998"),
        Duration::from_secs(2),
        found,
    );
    assert!(matches!(other, Ok(Some(_))), "only the hung item is held");
    let started = std::time::Instant::now();
    loop {
        match read_within(&service, &account, Duration::from_secs(2), found) {
            Ok(Some(_)) => break,
            Err(SecretsError::Timeout { .. }) if started.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            other => panic!("the item never cleared: {:?}", other.map(|v| v.is_some())),
        }
    }
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
