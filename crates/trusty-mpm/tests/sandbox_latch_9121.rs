//! The sandbox latch, set for real, in a process of its own (#9121).
//!
//! Why: `secret_source::enter_sandbox` is one-way and process-wide. Set inside
//! the shared `integration` or lib test binaries, it would change what every
//! sibling test's credential read returns. This target is its own `[[test]]`
//! binary, so the latch cannot leak.
//! What: with FAKE credentials in the environment — a Telegram bot token, an
//! OpenRouter key, a bug-report PAT — every test latches, then proves the
//! daemon's credential reads answer nothing: `resolve_secret`, the latched
//! `credential_store`, the `/activity` key-presence probe, the manager and
//! classifier inference stores, and the bug-report token. Every test is
//! `#[serial]` because they mutate the environment; no assertion prints a
//! resolved value.
//! Test: this file IS the test module.

// #8545: `common` arms the home-write fence before `main`.
mod common;

use std::ffi::OsString;
use std::sync::{Arc, Mutex, PoisonError};

use serial_test::serial;
use trusty_common::credential_registry::registered_providers;
use trusty_mpm::secret_source::{
    credential_store, credential_store_or, enter_sandbox, resolve_bounded_gated, resolve_secret,
};

/// A fake bot token. Never a real credential.
const FAKE_BOT_TOKEN: &str = "9121000000:fake-telegram-token-never-real";

/// A fake provider key. Never a real credential.
const FAKE_API_KEY: &str = "sk-or-v1-fake-9121-never-real";

/// The log line `secret_source` writes when the latched store is handed out.
const EMPTY_STORE_LINE: &str = "inference uses an empty credential store";

/// Sets or removes one variable until it drops, then restores it. Callers are
/// `#[serial]`.
struct EnvVarGuard {
    /// The variable this guard owns.
    key: String,
    /// Its value before the guard took it.
    prev: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &str, value: &str) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: every test in this binary is `#[serial]`.
        unsafe { std::env::set_var(key, value) };
        Self {
            key: key.to_string(),
            prev,
        }
    }

    fn unset(key: &str) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: as `set`.
        unsafe { std::env::remove_var(key) };
        Self {
            key: key.to_string(),
            prev,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: as `set`.
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var(&self.key, v),
                None => std::env::remove_var(&self.key),
            }
        }
    }
}

/// Set `key` to a fake value and prove the process environment holds it, so a
/// `None` below is the latch's doing and not an absent variable.
fn fake_env(key: &str, value: &str) -> EnvVarGuard {
    let guard = EnvVarGuard::set(key, value);
    assert!(
        std::env::var(key).is_ok_and(|v| v == value),
        "{key} did not take the fake value"
    );
    guard
}

/// A log writer into a shared buffer.
#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Buf {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run `f` under an INFO subscriber; return its result and the log text.
fn logged<T>(f: impl FnOnce() -> T) -> (T, String) {
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let text =
        String::from_utf8_lossy(&buf.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned();
    (out, text)
}

/// Why (#9121): the incident token sat in the environment. Latched, the
/// daemon's one credential read answers `None` with the fake token present —
/// the env tier is not consulted, nor `.env.local`, nor the store.
/// Test: this test.
#[test]
#[serial]
fn resolve_secret_answers_none_with_a_bot_token_in_the_env() {
    let _token = fake_env("TELEGRAM_BOT_TOKEN", FAKE_BOT_TOKEN);
    enter_sandbox();

    assert!(
        resolve_secret("TELEGRAM_BOT_TOKEN").is_none(),
        "a sandboxed resolve_secret returned a bot token"
    );
}

/// Why (#9121): `default_store()` probes the per-user Keychain. Latched, the
/// store a daemon builds inference over is empty and the real one is never
/// constructed — the injected constructor panics if it is.
/// Test: this test.
#[test]
#[serial]
fn the_credential_store_is_empty_and_never_built() {
    enter_sandbox();

    let gated = credential_store_or(|| panic!("the real store was built under the latch"));
    let store = credential_store();
    for (provider, _) in registered_providers() {
        assert!(gated.get(provider).is_none(), "{provider} resolved");
        assert!(store.get(provider).is_none(), "{provider} resolved");
    }
    assert!(store.list().is_empty(), "the latched store is not empty");
}

/// Why (#9121): the `/activity` key-presence probe walked `.env.local` and
/// the Keychain directly. Latched, it says absent even with a key in the env.
/// Test: this test.
#[test]
#[serial]
fn the_activity_key_probe_is_absent_with_a_key_in_the_env() {
    let _key = fake_env("OPENROUTER_API_KEY", FAKE_API_KEY);
    enter_sandbox();

    assert!(
        resolve_bounded_gated("OPENROUTER_API_KEY").is_err(),
        "the gated bounded resolver returned a key"
    );
    assert!(
        !trusty_mpm::daemon::managed_routes::activity::classifier_key_present(),
        "the activity probe reported a key under the latch"
    );
}

/// Why (#9121): `ManagerInference::provision` runs inside
/// `DaemonState::shared()` at startup and built its store with
/// `default_store()`. Latched, the manager and the activity classifier take
/// the empty store — proved by the line `secret_source` logs when it hands
/// that store out — and the manager resolves no provider.
/// Test: this test.
#[test]
#[serial]
fn inference_is_provisioned_over_the_empty_store() {
    let _cleared: Vec<EnvVarGuard> = registered_providers()
        .iter()
        .map(|(_, var)| EnvVarGuard::unset(var))
        .collect();
    let _manager_model = EnvVarGuard::set("TRUSTY_MANAGER_MODEL", "openai/gpt-4o-mini");
    let _llm_model = EnvVarGuard::set("TRUSTY_LLM_MODEL", "openai/gpt-4o-mini");
    enter_sandbox();
    // #9121: a broken gate panics here, before provision() can reach the Keychain.
    let _gate = credential_store_or(|| panic!("real store built under the latch"));

    let (resolved, manager_log) = logged(|| {
        trusty_mpm::daemon::manager::inference::ManagerInference::provision()
            .resolve()
            .is_ok()
    });
    let (_classifier, classifier_log) = logged(trusty_mpm::activity::OpenRouterClassifier::new);

    assert!(!resolved, "the sandboxed manager resolved a provider");
    assert!(
        manager_log.contains(EMPTY_STORE_LINE),
        "ManagerInference::provision did not take the latched store"
    );
    assert!(
        classifier_log.contains(EMPTY_STORE_LINE),
        "OpenRouterClassifier::new did not take the latched store"
    );
}

/// Why (#9121): the bug-report token is a credential the daemon reads for
/// `report_bug`. Latched, a PAT in the env is not read.
/// Test: this test.
#[test]
#[serial]
fn the_bug_report_token_is_none_with_a_pat_in_the_env() {
    let _pat = fake_env(
        trusty_mpm::daemon::bug_report::token::TOKEN_ENV_VAR,
        "ghp_fake9121neverreal",
    );
    enter_sandbox();

    assert!(
        trusty_mpm::daemon::bug_report::token::resolve_token().is_none(),
        "a sandboxed daemon read the bug-report token"
    );
}
