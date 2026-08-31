//! Credential status reporting for `system_status` — names and tiers only.
//!
//! Why: the owner's ask (and the security constraint that governs this whole
//! tool) is that `system_status` must NEVER report a secret VALUE — only
//! which providers are configured and via which tier, exactly like `tagent
//! config keys list` (`trusty_common::inference::config::ops::list`). Rather
//! than shell out to that text-writing function, this module reuses its exact
//! tier-classification logic (`classify_tier`) to build a structured
//! `Vec<CredentialStatus>` — the same decision, a machine-readable shape.
//! What: [`CredentialStatus`] + [`list_status`], parameterised over a
//! `&dyn KeyStore` AND a `&dyn EnvLocalSource` so tests can inject a sandboxed
//! `FileKeyStore` and a fixed `.env.local` table instead of touching the real
//! OS keychain / secure store or the real filesystem.
//! Test: `super::tests::credential_status_never_leaks_a_value`.

use serde::Serialize;
use trusty_common::credentials::{KeyStore, env_local_value};
use trusty_common::inference::config::ops::classify_tier;
use trusty_common::inference::registry::all;

/// One provider's configuration status — never a value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CredentialStatus {
    pub provider: String,
    pub status: String,
}

/// The `.env.local` tier signal, injectable exactly like `store: &dyn KeyStore`.
///
/// Why: [`list_status`] read that signal straight from
/// `trusty_common::credentials::env_local_value`, which searches upward from the
/// real `std::env::current_dir()` for a real `.env.local`. A developer's own
/// `.env.local` — the file this crate's CLAUDE.md tells everyone to create —
/// therefore decided what the tests observed, so
/// `credential_status_never_leaks_a_value` failed on any machine whose ancestor
/// `.env.local` bound `OPENROUTER_API_KEY` (#5678). The precedence
/// `classify_tier` applies is correct and stays untouched; only where the signal
/// comes from becomes injectable.
/// What: one method answering the same question `env_local_value` answers.
/// Production passes [`WorkspaceEnvLocal`]; tests pass a fixed table and read no
/// file at all.
/// Test: `super::tests::env_local_tier_comes_from_the_injected_source`.
pub trait EnvLocalSource {
    /// The value `.env.local` binds to `var`, or `None` when it binds nothing.
    fn value(&self, var: &str) -> Option<String>;
}

/// Production [`EnvLocalSource`] — the cwd-upward `.env.local` search.
pub struct WorkspaceEnvLocal;

impl EnvLocalSource for WorkspaceEnvLocal {
    fn value(&self, var: &str) -> Option<String> {
        env_local_value(var)
    }
}

/// List every known provider's key status (names/tiers only).
///
/// Why: mirrors `trusty_common::inference::config::ops::list` field-for-field
/// (same tier precedence, same "AWS credential chain" / "not configured"
/// wording) so `tagent config keys list` and `system_status`'s credentials
/// section never disagree about a provider's state.
/// What: for the keyless Bedrock-style chain, reports the AWS-credential-chain
/// note; for keyed providers, classifies the tier via [`classify_tier`] using
/// the same three signals `ops::list` reads (process env, `.env.local`, the
/// injected `store`).
/// Test: `super::tests::credential_status_never_leaks_a_value`,
/// `super::tests::unconfigured_provider_reports_not_configured`.
pub fn list_status(store: &dyn KeyStore) -> Vec<CredentialStatus> {
    // #5678: production reads the real `.env.local`; tests inject a table.
    list_status_from(store, &WorkspaceEnvLocal)
}

/// [`list_status`] with the `.env.local` tier signal supplied by the caller.
///
/// Why: the seam #5678 asked for — the same shape as the existing `store`
/// injection, so a test can state every one of `classify_tier`'s three signals
/// instead of inheriting one from whatever machine it runs on.
/// What: identical to [`list_status`], reading the `.env.local` signal from
/// `env_local` rather than the filesystem.
/// Test: `super::tests::credential_status_never_leaks_a_value`,
/// `super::tests::env_local_tier_comes_from_the_injected_source`.
pub fn list_status_from(
    store: &dyn KeyStore,
    env_local: &dyn EnvLocalSource,
) -> Vec<CredentialStatus> {
    all()
        .iter()
        .map(|caps| {
            let provider = caps.id.as_str().to_string();
            let status = match caps.credential_env {
                None => "AWS credential chain (no API key)".to_string(),
                Some(env_var) => {
                    let env = std::env::var(env_var)
                        .ok()
                        .filter(|v| !v.is_empty())
                        .is_some();
                    let has_env_local = env_local.value(env_var).is_some();
                    let stored = store.get(&provider).is_some();
                    match classify_tier(env, has_env_local, stored) {
                        Some(tier) => format!("configured via {}", tier.label()),
                        None => "not configured".to_string(),
                    }
                }
            };
            CredentialStatus { provider, status }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::credentials::FileKeyStore;

    /// A `.env.local` that binds nothing — the hermetic default (#5678).
    struct NoEnvLocal;

    impl EnvLocalSource for NoEnvLocal {
        fn value(&self, _var: &str) -> Option<String> {
            None
        }
    }

    /// A `.env.local` binding exactly one variable (#5678).
    struct OneVarEnvLocal(&'static str);

    impl EnvLocalSource for OneVarEnvLocal {
        fn value(&self, var: &str) -> Option<String> {
            (var == self.0).then(|| "value-that-must-never-be-reported".to_string())
        }
    }

    /// Why: the never-echo-a-value mandate is the single most security
    /// critical property of this whole tool — a seeded sentinel key must
    /// never appear anywhere in the structured status output, only the
    /// provider name and tier label.
    /// What: seeds a `FileKeyStore` under a tempdir with a fake sentinel
    /// value for `openrouter`, calls `list_status_from` with a `.env.local`
    /// that binds nothing, and asserts the sentinel string is absent from
    /// every field of every entry.
    ///
    /// #5678: the store-tier assertion below is what an ancestor `.env.local`
    /// binding `OPENROUTER_API_KEY` used to break — the injected source is
    /// what makes it hermetic.
    /// Test: itself.
    #[test]
    fn credential_status_never_leaks_a_value() {
        let _env_guard = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _home_guard = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let prev_openrouter = std::env::var_os("OPENROUTER_API_KEY");
        // SAFETY: ENV_LOCK held for the whole test body.
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
        }

        const SENTINEL: &str = "sk-or-FAKE-system-status-sentinel-value"; // pragma: allowlist secret
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileKeyStore::at(tmp.path());
        KeyStore::set(&store, "openrouter", SENTINEL).expect("seed store");

        let statuses = list_status_from(&store, &NoEnvLocal);

        // SAFETY: lock still held.
        unsafe {
            if let Some(v) = prev_openrouter {
                std::env::set_var("OPENROUTER_API_KEY", v);
            }
        }

        assert!(!statuses.is_empty(), "provider registry must not be empty");
        for s in &statuses {
            assert!(
                !s.provider.contains(SENTINEL),
                "provider field leaked the sentinel: {s:?}"
            );
            assert!(
                !s.status.contains(SENTINEL),
                "status field leaked the sentinel: {s:?}"
            );
        }
        let openrouter = statuses
            .iter()
            .find(|s| s.provider == "openrouter")
            .expect("openrouter is a known provider");
        assert!(
            openrouter.status.contains("secure store"),
            "openrouter should report the store tier, got: {}",
            openrouter.status
        );
    }

    /// Why: a provider with no key anywhere must report "not configured",
    /// not silently disappear from the list or panic.
    /// Test: itself.
    #[test]
    fn unconfigured_provider_reports_not_configured() {
        let _env_guard = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let prev = std::env::var_os("TOGETHER_API_KEY");
        // SAFETY: ENV_LOCK held for the whole test body.
        unsafe {
            std::env::remove_var("TOGETHER_API_KEY");
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileKeyStore::at(tmp.path());
        // #5678: same hermeticity fix — a real ancestor `.env.local` binding
        // TOGETHER_API_KEY would otherwise report a tier here.
        let statuses = list_status_from(&store, &NoEnvLocal);

        // SAFETY: lock still held.
        unsafe {
            if let Some(v) = prev {
                std::env::set_var("TOGETHER_API_KEY", v);
            }
        }

        let together = statuses
            .iter()
            .find(|s| s.provider == "together")
            .expect("together is a known provider");
        assert_eq!(together.status, "not configured");
    }

    /// Why: the seam is only worth having if the reported tier actually
    /// follows the injected source — a source that binds the variable must
    /// produce the `.env.local` tier even with a seeded store present, and one
    /// that binds nothing must fall through to the store (#5678).
    /// What: runs both directions over the same seeded `FileKeyStore`, with
    /// the process env var cleared so `classify_tier`'s highest tier is out of
    /// play.
    /// Test: itself.
    #[test]
    fn env_local_tier_comes_from_the_injected_source() {
        let _env_guard = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let prev = std::env::var_os("OPENROUTER_API_KEY");
        // SAFETY: ENV_LOCK held for the whole test body.
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileKeyStore::at(tmp.path());
        KeyStore::set(&store, "openrouter", "sk-or-FAKE-injected-source") // pragma: allowlist secret
            .expect("seed store");

        let with_env_local = list_status_from(&store, &OneVarEnvLocal("OPENROUTER_API_KEY"));
        let without_env_local = list_status_from(&store, &NoEnvLocal);

        // SAFETY: lock still held.
        unsafe {
            if let Some(v) = prev {
                std::env::set_var("OPENROUTER_API_KEY", v);
            }
        }

        let tier_of = |statuses: &[CredentialStatus]| {
            statuses
                .iter()
                .find(|s| s.provider == "openrouter")
                .expect("openrouter is a known provider")
                .status
                .clone()
        };
        assert!(
            tier_of(&with_env_local).contains(".env.local"),
            "a binding source must report the .env.local tier, got: {}",
            tier_of(&with_env_local)
        );
        assert!(
            tier_of(&without_env_local).contains("secure store"),
            "an empty source must fall through to the store, got: {}",
            tier_of(&without_env_local)
        );
    }
}
