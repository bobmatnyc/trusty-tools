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
//! `&dyn KeyStore` so tests can inject a sandboxed `FileKeyStore` instead of
//! touching the real OS keychain / secure store.
//! Test: `super::tests::credential_status_never_leaks_a_value`.

use serde::Serialize;
use trusty_common::inference::config::ops::classify_tier;
use trusty_common::inference::credentials::{KeyStore, env_local_value};
use trusty_common::inference::registry::all;

/// One provider's configuration status — never a value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CredentialStatus {
    pub provider: String,
    pub status: String,
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
                    let env_local = env_local_value(env_var).is_some();
                    let stored = store.get(&provider).is_some();
                    match classify_tier(env, env_local, stored) {
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
    use trusty_common::inference::credentials::FileKeyStore;

    /// Why: the never-echo-a-value mandate is the single most security
    /// critical property of this whole tool — a seeded sentinel key must
    /// never appear anywhere in the structured status output, only the
    /// provider name and tier label.
    /// What: seeds a `FileKeyStore` under a tempdir with a fake sentinel
    /// value for `openrouter`, calls `list_status`, and asserts the
    /// sentinel string is absent from every field of every entry.
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

        let statuses = list_status(&store);

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
        let statuses = list_status(&store);

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
}
