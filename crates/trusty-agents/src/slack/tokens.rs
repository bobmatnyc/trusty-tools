//! Slack token resolution for the Socket Mode gateway (#10120).
//!
//! Why: `tm doctor --fix-launchd-secrets` moves `SLACK_APP_TOKEN` and
//! `SLACK_BOT_TOKEN` out of the LaunchAgent plist into the trusty-common
//! credential store. The gateway read the process environment only, so
//! applying that fix stopped the gateway.
//! What: [`resolve_slack_token`] checks the process environment, then
//! `.env.local`, then the credential store. The first non-empty value wins. A
//! miss in all three is a [`SecretResolveError`] naming the variable; no arm
//! formats a token value. The store tier goes through trusty-common's bounded,
//! single-flight read under the same provider key the doctor fix writes.
//! Test: `tests` below.

use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use trusty_common::credential_registry::provider_for_env_var;
use trusty_common::credentials::{
    KeyStore, STORE_READ_TIMEOUT, SecretResolveError, default_store,
    resolve_provider_bounded_store_first_with,
};

use crate::system_status::credentials::{EnvLocalSource, WorkspaceEnvLocal};

/// The Socket Mode app-level token (`xapp-…`).
pub(super) const SLACK_APP_TOKEN: &str = "SLACK_APP_TOKEN";
/// The bot token used by `chat.postMessage` (`xoxb-…`).
pub(super) const SLACK_BOT_TOKEN: &str = "SLACK_BOT_TOKEN";

/// Resolve one Slack token for the gateway: env, then `.env.local`, then the store.
///
/// Why: the gateway must keep starting after an operator moves its tokens into
/// the credential store (#10120).
/// What: runs [`resolve_slack_token_with`] against the real process
/// environment, the cwd-upward `.env.local`, and
/// [`default_store`] on a blocking thread, because the store read can wait up
/// to [`STORE_READ_TIMEOUT`]. A failure becomes an operator-facing error via
/// [`missing_token_error`].
/// Test: the hermetic core is covered by `tests`; this wiring reads real
/// sources and is exercised by a live gateway start (see the runbook
/// `docs/runbooks/slack-gateway-token-store.md`).
pub(super) async fn resolve_slack_token(var: &'static str) -> anyhow::Result<String> {
    let resolved = tokio::task::spawn_blocking(move || {
        resolve_slack_token_with(
            var,
            |name| std::env::var(name).ok(),
            &WorkspaceEnvLocal,
            Arc::from(default_store()),
            STORE_READ_TIMEOUT,
        )
    })
    .await
    // #10120: the JoinError is dropped, not formatted — a panic payload is text
    // this module does not control.
    .map_err(|_| anyhow!("resolving {var} for the Slack gateway did not complete"))?;
    resolved.map_err(missing_token_error)
}

/// Hermetic core of [`resolve_slack_token`]: every source is a parameter.
///
/// Why: tests prove the order with a fake environment, a temp `.env.local` and
/// an isolated store, without touching the real process env, a real
/// `.env.local`, or the Keychain.
/// What: maps `var` to its registered provider key, returns the first
/// non-empty value from `env(var)`, then `env_local.value(var)`, then the
/// bounded read of `store` under that provider key.
///
/// # Errors
///
/// [`SecretResolveError::Absent`] when no source holds a value;
/// `Unregistered` for a name outside the credential registry; `Timeout` or
/// `Store` when the store read failed. None carries a value.
///
/// Test: `process_env_wins_over_env_local_and_store`,
/// `env_local_wins_over_store_when_env_is_unset`,
/// `store_supplies_a_token_missing_from_env_and_env_local`,
/// `empty_values_fall_through_to_the_next_source`,
/// `missing_everywhere_is_a_named_error_without_any_value`.
pub(super) fn resolve_slack_token_with(
    var: &str,
    env: impl Fn(&str) -> Option<String>,
    env_local: &dyn EnvLocalSource,
    store: Arc<dyn KeyStore>,
    timeout: Duration,
) -> Result<String, SecretResolveError> {
    let Some(provider) = provider_for_env_var(var) else {
        return Err(SecretResolveError::Unregistered {
            var: var.to_string(),
        });
    };
    if let Some(value) = env(var).filter(|v| !v.is_empty()) {
        return Ok(value);
    }
    if let Some(value) = env_local.value(var).filter(|v| !v.is_empty()) {
        return Ok(value);
    }
    // #10120: both environment tiers already ran, so the store is the last
    // source; the fallback closure answers nothing and a miss is `Absent`.
    resolve_provider_bounded_store_first_with(provider, store, timeout, |_| None)
}

/// The operator-facing gateway startup error for an unresolved token.
///
/// Why: the bare resolver error says what is missing; the operator also needs
/// the three places a token can live (#10120).
/// What: the value-free [`SecretResolveError`] text plus the remediation.
/// Test: `missing_everywhere_is_a_named_error_without_any_value`.
pub(super) fn missing_token_error(err: SecretResolveError) -> anyhow::Error {
    let var = err.var().to_string();
    anyhow!(
        "{err}. Export {var}, add it to .env.local, or move it from the LaunchAgent plist into \
         the credential store with `tm doctor --fix-launchd-secrets --yes` before running --slack."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use trusty_common::credentials::{FileKeyStore, MemoryKeyStore, read_var_from_env_local};

    // Fixture values only, never real tokens. Each source holds a distinct one
    // so a test can tell which source answered.
    const ENV_VALUE: &str = "xapp-fixture-from-process-env";
    const ENV_LOCAL_VALUE: &str = "xapp-fixture-from-env-local";
    const STORE_VALUE: &str = "xapp-fixture-from-store";

    /// `.env.local` read through trusty-common's own parser from a temp file.
    struct TempEnvLocal(PathBuf);

    impl EnvLocalSource for TempEnvLocal {
        fn value(&self, var: &str) -> Option<String> {
            read_var_from_env_local(&self.0, var)
        }
    }

    /// A `.env.local` that binds nothing.
    struct NoEnvLocal;

    impl EnvLocalSource for NoEnvLocal {
        fn value(&self, _var: &str) -> Option<String> {
            None
        }
    }

    fn temp_env_local(dir: &tempfile::TempDir, body: &str) -> TempEnvLocal {
        let path = dir.path().join(".env.local");
        std::fs::write(&path, body).expect("write temp .env.local");
        TempEnvLocal(path)
    }

    fn fake_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    fn store_with(provider: &str, value: &str) -> Arc<dyn KeyStore> {
        let store = MemoryKeyStore::new();
        store.set(provider, value).expect("memory store set");
        Arc::new(store)
    }

    /// Assert the resolved token by name of the source, never printing a value.
    fn assert_source(actual: Result<String, SecretResolveError>, expected: &str, source: &str) {
        match actual {
            Ok(value) => assert!(value == expected, "expected the {source} value to win"),
            Err(err) => panic!(
                "expected the {source} value to win, got error kind {}",
                err.kind()
            ),
        }
    }

    #[test]
    #[serial(slack_token_store)]
    fn process_env_wins_over_env_local_and_store() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let env_local = temp_env_local(&dir, &format!("SLACK_APP_TOKEN={ENV_LOCAL_VALUE}\n"));
        let resolved = resolve_slack_token_with(
            SLACK_APP_TOKEN,
            fake_env(&[(SLACK_APP_TOKEN, ENV_VALUE)]),
            &env_local,
            store_with("slack-app", STORE_VALUE),
            STORE_READ_TIMEOUT,
        );
        assert_source(resolved, ENV_VALUE, "process env");
    }

    #[test]
    #[serial(slack_token_store)]
    fn env_local_wins_over_store_when_env_is_unset() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let env_local = temp_env_local(&dir, &format!("SLACK_APP_TOKEN={ENV_LOCAL_VALUE}\n"));
        let resolved = resolve_slack_token_with(
            SLACK_APP_TOKEN,
            fake_env(&[]),
            &env_local,
            store_with("slack-app", STORE_VALUE),
            STORE_READ_TIMEOUT,
        );
        assert_source(resolved, ENV_LOCAL_VALUE, ".env.local");
    }

    /// The regression: the doctor fix leaves the token only in the store.
    /// Uses the 0600 `FileKeyStore` the fix writes, rooted in a temp dir.
    #[test]
    #[serial(slack_token_store)]
    fn store_supplies_a_token_missing_from_env_and_env_local() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let file_store = FileKeyStore::at(dir.path());
        file_store
            .set("slack", "xoxb-fixture-from-store")
            .expect("file store set");
        let resolved = resolve_slack_token_with(
            SLACK_BOT_TOKEN,
            fake_env(&[]),
            &NoEnvLocal,
            Arc::new(file_store),
            STORE_READ_TIMEOUT,
        );
        assert_source(resolved, "xoxb-fixture-from-store", "credential store");
    }

    #[test]
    #[serial(slack_token_store)]
    fn empty_values_fall_through_to_the_next_source() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let env_local = temp_env_local(&dir, "SLACK_APP_TOKEN=\n");
        let resolved = resolve_slack_token_with(
            SLACK_APP_TOKEN,
            fake_env(&[(SLACK_APP_TOKEN, "")]),
            &env_local,
            store_with("slack-app", STORE_VALUE),
            STORE_READ_TIMEOUT,
        );
        assert_source(resolved, STORE_VALUE, "credential store");
    }

    /// Other sources hold values for OTHER names; none may leak into the error.
    #[test]
    #[serial(slack_token_store)]
    fn missing_everywhere_is_a_named_error_without_any_value() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let env_local = temp_env_local(&dir, &format!("SLACK_BOT_TOKEN={ENV_LOCAL_VALUE}\n"));
        let err = resolve_slack_token_with(
            SLACK_APP_TOKEN,
            fake_env(&[(SLACK_BOT_TOKEN, ENV_VALUE)]),
            &env_local,
            store_with("slack", STORE_VALUE),
            STORE_READ_TIMEOUT,
        )
        .expect_err("no source holds SLACK_APP_TOKEN");
        assert_eq!(err.kind(), "absent");
        assert_eq!(err.var(), SLACK_APP_TOKEN);

        let text = format!("{:#}", missing_token_error(err));
        assert!(text.contains(SLACK_APP_TOKEN), "error must name the token");
        for value in [ENV_VALUE, ENV_LOCAL_VALUE, STORE_VALUE, "xapp-", "xoxb-"] {
            assert!(
                !text.contains(value),
                "error text carries a token value or prefix"
            );
        }
    }
}
