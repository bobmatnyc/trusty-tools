//! Two-stage provider resolution (issue #2402; the `provider_for` resolver).
//!
//! Why: a model slug must map to a provider AND a resolved credential in one
//! authoritative place, or every consumer re-invents the "which backend, whose
//! key" decision. The epic's rule is precise: an explicit `<prefix>/…` slug
//! selects a family, but that family is only used if its credential actually
//! resolves — otherwise (and for bare slugs) resolution defaults to OpenRouter.
//! What: [`provider_for`] implements that two-stage ladder against the #2401
//! credential resolver (env > `.env.local` > secure store), returning a
//! [`ResolvedProvider`] carrying the provider id, the model slug, and the
//! resolved key (wrapped in a redacting [`SecretString`]). Bedrock (AWS
//! credential chain) and Local (unauthenticated `localhost` endpoint, #3247)
//! both resolve with no key — [`ProviderId::credential_name`] returning `None`
//! is what routes either one through this no-key branch.
//! Test: inline `tests` — `explicit_prefix_with_key_wins`,
//! `explicit_prefix_missing_key_falls_back_to_openrouter`, `bare_slug_uses_openrouter`,
//! `bedrock_resolves_without_key`, `local_resolves_without_key`,
//! `no_credential_anywhere_errors`.

use crate::inference::credentials::{KeyStore, resolve_key_with};
use crate::inference::error::InferenceError;
use crate::inference::registry::{ProviderCapabilities, ProviderId, capabilities};
use crate::inference::types::SecretString;

/// The outcome of resolving a model slug: which provider, which model, and the
/// credential to build an adapter with.
///
/// Why: the configurator needs all three to construct an adapter; bundling them
/// keeps the resolver's contract explicit and lets the adapter factory stay a
/// pure function of this value.
/// What: `provider` is the resolved [`ProviderId`]; `model` is the (possibly
/// re-homed) slug to send; `key` is the resolved credential or `None` for
/// Bedrock. `Debug` is safe to log — `key`'s [`SecretString`] redacts itself.
/// Test: every resolver test asserts the resolved fields.
#[derive(Debug, Clone)]
pub struct ResolvedProvider {
    provider: ProviderId,
    model: String,
    key: Option<SecretString>,
}

impl ResolvedProvider {
    /// Construct a resolution outcome.
    ///
    /// Why: the resolver builds this in a few branches; one constructor keeps
    /// them uniform.
    /// What: stores the three fields verbatim.
    /// Test: exercised by every resolver test.
    pub(crate) fn new(provider: ProviderId, model: String, key: Option<SecretString>) -> Self {
        Self {
            provider,
            model,
            key,
        }
    }

    /// The resolved provider.
    ///
    /// Why: the configurator selects an adapter factory by this id.
    /// What: returns the [`ProviderId`].
    /// Test: `explicit_prefix_with_key_wins`.
    pub fn provider(&self) -> ProviderId {
        self.provider
    }

    /// The model slug to send to the provider.
    ///
    /// Why: an adapter needs the concrete slug for its request body.
    /// What: returns the stored slug.
    /// Test: `bare_slug_uses_openrouter`.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The resolved API key, if any.
    ///
    /// Why: adapter construction (#2403) authenticates with it; Bedrock returns
    /// `None` (AWS credential chain). Exposed as the redacting wrapper so callers
    /// must opt into the raw value via [`SecretString::expose`].
    /// What: returns the stored [`SecretString`] or `None`.
    /// Test: `bedrock_resolves_without_key`, `explicit_prefix_with_key_wins`.
    pub fn key(&self) -> Option<&SecretString> {
        self.key.as_ref()
    }

    /// The capability descriptor for the resolved provider.
    ///
    /// Why: an adapter factory reads capabilities to build the adapter; this
    /// saves it a separate registry lookup.
    /// What: delegates to [`capabilities`].
    /// Test: `explicit_prefix_with_key_wins`.
    pub fn capabilities(&self) -> &'static ProviderCapabilities {
        capabilities(self.provider)
    }
}

/// Resolve a model slug to a provider + credential via the two-stage ladder.
///
/// Why: the single authoritative "which backend, whose key" decision every
/// consumer shares.
/// What: **Stage 1** — if `slug` carries an explicit family prefix
/// ([`ProviderId::from_slug_prefix`]): for Bedrock, resolve immediately with no
/// key; for a keyed family, use it ONLY when its credential resolves via
/// [`resolve_key_with`] (env > `.env.local` > `store`), otherwise fall through.
/// **Stage 2 (default)** — resolve OpenRouter with its credential. If even
/// OpenRouter has no credential, return [`InferenceError::MissingCredential`].
/// The `store` is injected (rather than using the process default) so callers —
/// and tests — control the secure-store tier explicitly.
/// Test: `explicit_prefix_with_key_wins`,
/// `explicit_prefix_missing_key_falls_back_to_openrouter`, `bare_slug_uses_openrouter`,
/// `bedrock_resolves_without_key`, `no_credential_anywhere_errors`.
pub fn provider_for(slug: &str, store: &dyn KeyStore) -> Result<ResolvedProvider, InferenceError> {
    // ── Stage 1: explicit family prefix ──
    if let Some(family) = ProviderId::from_slug_prefix(slug) {
        match family.credential_name() {
            // Bedrock authenticates via the AWS chain — no key to resolve.
            None => return Ok(ResolvedProvider::new(family, slug.to_string(), None)),
            Some(cred) => {
                if let Some(key) = resolve_key_with(cred, store) {
                    return Ok(ResolvedProvider::new(
                        family,
                        slug.to_string(),
                        Some(SecretString::new(key)),
                    ));
                }
                // Family key missing → fall through to the OpenRouter default.
            }
        }
    }

    // ── Stage 2: OpenRouter default (gated by its own credential) ──
    let or_cred = ProviderId::OpenRouter
        .credential_name()
        .unwrap_or("openrouter");
    match resolve_key_with(or_cred, store) {
        Some(key) => Ok(ResolvedProvider::new(
            ProviderId::OpenRouter,
            slug.to_string(),
            Some(SecretString::new(key)),
        )),
        None => Err(InferenceError::MissingCredential {
            provider: ProviderId::OpenRouter,
        }),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::credentials::MemoryKeyStore;
    use serial_test::serial;

    /// Build a store with the given provider→key entries, and clear any process
    /// env vars that would otherwise shadow the store tier.
    fn store_with(entries: &[(&str, &str)]) -> MemoryKeyStore {
        for var in [
            "OPENROUTER_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "FIREWORKS_API_KEY",
        ] {
            // SAFETY: guarded by `#[serial(dotenv_credential_env)]` on each test.
            unsafe { std::env::remove_var(var) };
        }
        let store = MemoryKeyStore::new();
        for (p, k) in entries {
            store.set(p, k).expect("seed store");
        }
        store
    }

    /// Why: an explicit family whose key resolves must be used as-is.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn explicit_prefix_with_key_wins() {
        let store = store_with(&[("anthropic", "sk-ant-xyz")]); // pragma: allowlist secret
        let r = provider_for("anthropic/claude-sonnet-4-5", &store).expect("resolves");
        assert_eq!(r.provider(), ProviderId::Anthropic);
        assert_eq!(r.model(), "anthropic/claude-sonnet-4-5");
        assert_eq!(r.key().map(|k| k.expose()), Some("sk-ant-xyz"));
        assert_eq!(r.capabilities().id, ProviderId::Anthropic);
    }

    /// Why: an explicit family with NO key must fall back to OpenRouter (when
    /// OpenRouter's key is present) — the epic's precedence rule.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn explicit_prefix_missing_key_falls_back_to_openrouter() {
        let store = store_with(&[("openrouter", "sk-or-abc")]); // pragma: allowlist secret
        let r = provider_for("anthropic/claude-sonnet-4-5", &store).expect("resolves");
        assert_eq!(r.provider(), ProviderId::OpenRouter);
        // The original slug is preserved for OpenRouter to route.
        assert_eq!(r.model(), "anthropic/claude-sonnet-4-5");
        assert_eq!(r.key().map(|k| k.expose()), Some("sk-or-abc"));
    }

    /// Why: a bare (prefix-less) slug resolves to OpenRouter by default.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn bare_slug_uses_openrouter() {
        let store = store_with(&[("openrouter", "sk-or-abc")]); // pragma: allowlist secret
        let r = provider_for("some-vendor/some-model", &store).expect("resolves");
        assert_eq!(r.provider(), ProviderId::OpenRouter);
    }

    /// Why: Bedrock resolves with no key (AWS credential chain) regardless of
    /// store contents.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn bedrock_resolves_without_key() {
        let store = store_with(&[]);
        let r = provider_for("bedrock/us.anthropic.claude-sonnet-4-5", &store).expect("resolves");
        assert_eq!(r.provider(), ProviderId::Bedrock);
        assert!(r.key().is_none());
    }

    /// Why: Local (#3247) resolves with no key (unauthenticated `localhost`
    /// endpoint) regardless of store contents, exactly like Bedrock.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn local_resolves_without_key() {
        let store = store_with(&[]);
        let r = provider_for("local/llama3.1", &store).expect("resolves");
        assert_eq!(r.provider(), ProviderId::Local);
        assert!(r.key().is_none());
        // The `ollama/` alias resolves identically.
        let r2 = provider_for("ollama/qwen3:30b", &store).expect("resolves");
        assert_eq!(r2.provider(), ProviderId::Local);
        assert!(r2.key().is_none());
    }

    /// Why: with no credential anywhere (and no Bedrock prefix), resolution must
    /// alarm via `MissingCredential`, not panic or synthesise a key.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn no_credential_anywhere_errors() {
        let store = store_with(&[]);
        let err = provider_for("openai/gpt-4o-mini", &store).expect_err("must error");
        assert!(err.is_alarm());
        assert!(matches!(
            err,
            InferenceError::MissingCredential {
                provider: ProviderId::OpenRouter
            }
        ));
    }
}
