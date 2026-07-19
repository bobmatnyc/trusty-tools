//! Local / OpenAI-compatible provider adapter — Ollama by default (#3247).
//!
//! Why: the MVP's "use a local model" goal (epic #3052) must work with zero
//! external credentials and zero configuration for the common case (a bare
//! `ollama serve` on the same machine), while still letting a caller point at
//! a different local/self-hosted OpenAI-compatible server (LM Studio, vLLM, a
//! remote Ollama host) without a code change. Ollama already speaks the exact
//! same `/v1/chat/completions` schema every other [`super::openai_compat`]
//! provider does, so this is a thin config over the shared core — the only
//! deltas are the base URL and the (normally absent) bearer credential.
//! What: [`LocalConfig`] (base URL + optional auth override, sourced from
//! [`LocalConfig::from_env`]), [`build`] (constructs an [`OpenAiCompatAdapter`]
//! from a [`LocalConfig`]; never requires a resolved credential — unlike the
//! keyed providers, a missing key is not an error), and [`factory`] (the
//! production entry point [`super::register_default_factories`] registers
//! under [`ProviderId::Local`]).
//!
//! Override mechanism: set [`LOCAL_HOST_ENV`] (`OLLAMA_HOST`, matching the
//! env var `trusty-agents`' legacy `OllamaAdapter` already reads — a bare
//! host, e.g. `http://192.168.1.50:11434`, NOT including `/v1`) to point at a
//! non-default local endpoint, and [`LOCAL_API_KEY_ENV`]
//! (`TRUSTY_LOCAL_API_KEY`) to supply a bearer credential for the rare local
//! server that enforces auth. Both are optional; Ollama needs neither.
//! Test: inline `tests` — `factory_builds_named_adapter_with_defaults`,
//! `host_env_override_appends_v1_suffix`, `api_key_env_override_is_used`,
//! `placeholder_key_used_when_no_override`.

use super::openai_compat::{OpenAiCompatAdapter, OpenAiCompatConfig};
use crate::inference::adapter::InferenceAdapter;
use crate::inference::configurator::ResolvedProvider;
use crate::inference::error::InferenceError;
use crate::inference::registry::ProviderId;
use crate::inference::types::SecretString;

/// Default local OpenAI-compatible base URL — Ollama's native `/v1` shim.
pub const LOCAL_BASE_URL: &str = "http://localhost:11434/v1";

/// Env var overriding the local host. Takes a BARE host (no `/v1` suffix,
/// e.g. `http://192.168.1.50:11434`); `/v1` is appended automatically, same as
/// [`LOCAL_BASE_URL`]. Named `OLLAMA_HOST` (rather than a new `TRUSTY_*` name)
/// so it consolidates with the identical override `trusty-agents`' legacy
/// `llm::adapter::impls::OllamaAdapter::api_endpoint` already reads — setting
/// it once affects both the legacy dispatch and this registry-based path.
pub const LOCAL_HOST_ENV: &str = "OLLAMA_HOST";

/// Env var providing an optional bearer credential for local OpenAI-compatible
/// servers that DO enforce auth (e.g. LM Studio / vLLM configured with an API
/// key). Absent for a bare Ollama install — Ollama ignores the header.
pub const LOCAL_API_KEY_ENV: &str = "TRUSTY_LOCAL_API_KEY";

/// Placeholder bearer value sent when no [`LOCAL_API_KEY_ENV`] override is
/// set. [`OpenAiCompatConfig::api_key`] is a required field even though most
/// local servers ignore the `Authorization` header entirely — a non-empty
/// placeholder satisfies clients (and any strict proxy in front of the local
/// server) that assume a bearer value is always present, without implying a
/// real credential exists.
pub const LOCAL_PLACEHOLDER_KEY: &str = "not-needed";

/// Per-connection configuration for the local provider.
///
/// Why: the base URL and the (usually absent) auth credential are the only
/// two things that vary between a bare Ollama install, a remote Ollama host,
/// and a local OpenAI-compatible server that enforces auth. Bundling them in
/// one struct (rather than two loose parameters) keeps [`build`]'s signature
/// stable as more override knobs are added later.
/// What: `base_url` is the API root `/chat/completions` is appended to
/// (already including any `/v1` suffix); `auth` is the optional bearer
/// override — `None` means "use [`LOCAL_PLACEHOLDER_KEY`]".
/// Test: `host_env_override_appends_v1_suffix`, `api_key_env_override_is_used`.
pub struct LocalConfig {
    /// The API root; `/chat/completions` is appended by [`OpenAiCompatAdapter`].
    pub base_url: String,
    /// Optional bearer credential override; `None` uses the placeholder.
    pub auth: Option<SecretString>,
}

impl LocalConfig {
    /// Build the production config from [`LOCAL_HOST_ENV`] / [`LOCAL_API_KEY_ENV`],
    /// falling back to [`LOCAL_BASE_URL`] / no auth override.
    ///
    /// Why: the single place that implements the documented override
    /// mechanism, so [`factory`] and any future CLI/config surface share
    /// identical precedence.
    /// What: reads [`LOCAL_HOST_ENV`]; when set and non-blank, trims any
    /// trailing slash and appends `/v1` (mirroring the legacy `OllamaAdapter`
    /// exactly). Reads [`LOCAL_API_KEY_ENV`]; when set and non-blank, wraps it
    /// in a [`SecretString`]. Both env vars are optional.
    /// Test: `host_env_override_appends_v1_suffix`, `api_key_env_override_is_used`,
    /// `from_env_defaults_when_unset`.
    pub fn from_env() -> Self {
        let base_url = std::env::var(LOCAL_HOST_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(|host| format!("{}/v1", host.trim_end_matches('/')))
            .unwrap_or_else(|| LOCAL_BASE_URL.to_string());
        let auth = std::env::var(LOCAL_API_KEY_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(SecretString::new);
        Self { base_url, auth }
    }
}

/// Build a Local adapter from `config`.
///
/// Why: unlike every keyed provider ([`super::together::build`] etc.), Local
/// never requires a resolved credential — [`ProviderId::Local`] resolves with
/// `key: None` unconditionally (see [`crate::inference::registry::ProviderId::credential_name`]),
/// so a missing key is the expected common case, not an alarm.
/// What: uses `config.auth` when present, otherwise [`LOCAL_PLACEHOLDER_KEY`];
/// constructs an [`OpenAiCompatAdapter`] against `config.base_url` with no
/// attribution headers and [`ProviderId::Local`]'s registry capabilities.
/// Test: `factory_builds_named_adapter_with_defaults`.
pub fn build(
    _resolved: &ResolvedProvider,
    config: LocalConfig,
) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    let api_key = config
        .auth
        .unwrap_or_else(|| SecretString::new(LOCAL_PLACEHOLDER_KEY));
    let cfg = OpenAiCompatConfig {
        name: ProviderId::Local.as_str().to_string(),
        base_url: config.base_url,
        api_key,
        extra_headers: Vec::new(),
        capabilities: *crate::inference::registry::capabilities(ProviderId::Local),
    };
    Ok(Box::new(OpenAiCompatAdapter::new(cfg)?))
}

/// Production factory: build a Local adapter from the live env overrides.
///
/// Why: this is what [`super::register_default_factories`] registers into the
/// [`crate::inference::Configurator`] so a `local/*` or `ollama/*` slug yields
/// a live adapter with zero required configuration.
/// What: delegates to [`build`] with [`LocalConfig::from_env`].
/// Test: `crates/trusty-common/tests/inference_adapters.rs` (via the default
/// factory registration test).
pub fn factory(resolved: &ResolvedProvider) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    build(resolved, LocalConfig::from_env())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn resolved() -> ResolvedProvider {
        ResolvedProvider::new(ProviderId::Local, "local/llama3.1".to_string(), None)
    }

    /// Why: with no env overrides, the factory must build a named Local
    /// adapter pointed at the default base URL and the placeholder key.
    /// Test: itself.
    #[test]
    #[serial(local_provider_env)]
    fn factory_builds_named_adapter_with_defaults() {
        // SAFETY: guarded by `#[serial(local_provider_env)]`.
        unsafe {
            std::env::remove_var(LOCAL_HOST_ENV);
            std::env::remove_var(LOCAL_API_KEY_ENV);
        }
        let adapter = build(&resolved(), LocalConfig::from_env()).expect("built");
        assert_eq!(adapter.name(), "local");
        assert!(adapter.supports_native_tools());
        assert_eq!(adapter.capabilities().id, ProviderId::Local);
    }

    /// Why: [`LocalConfig::from_env`] must default to [`LOCAL_BASE_URL`] /
    /// no auth override when neither env var is set.
    /// Test: itself.
    #[test]
    #[serial(local_provider_env)]
    fn from_env_defaults_when_unset() {
        // SAFETY: guarded by `#[serial(local_provider_env)]`.
        unsafe {
            std::env::remove_var(LOCAL_HOST_ENV);
            std::env::remove_var(LOCAL_API_KEY_ENV);
        }
        let config = LocalConfig::from_env();
        assert_eq!(config.base_url, LOCAL_BASE_URL);
        assert!(config.auth.is_none());
    }

    /// Why: `OLLAMA_HOST` (a bare host) must be normalised into a `/v1`
    /// endpoint the same way the legacy `OllamaAdapter` does, so setting it
    /// once affects both dispatch paths identically.
    /// Test: itself.
    #[test]
    #[serial(local_provider_env)]
    fn host_env_override_appends_v1_suffix() {
        // SAFETY: guarded by `#[serial(local_provider_env)]`.
        unsafe {
            std::env::set_var(LOCAL_HOST_ENV, "http://192.168.1.50:11434/");
            std::env::remove_var(LOCAL_API_KEY_ENV);
        }
        let config = LocalConfig::from_env();
        assert_eq!(config.base_url, "http://192.168.1.50:11434/v1");
        let adapter = build(&resolved(), config).expect("built");
        assert_eq!(adapter.capabilities().id.credential_name(), None);
        // SAFETY: guarded by `#[serial(local_provider_env)]`.
        unsafe {
            std::env::remove_var(LOCAL_HOST_ENV);
        }
    }

    /// Why: an explicit `TRUSTY_LOCAL_API_KEY` must be used verbatim instead
    /// of the placeholder, for the rare local server that enforces auth.
    /// Test: itself.
    #[test]
    #[serial(local_provider_env)]
    fn api_key_env_override_is_used() {
        // SAFETY: guarded by `#[serial(local_provider_env)]`.
        unsafe {
            std::env::remove_var(LOCAL_HOST_ENV);
            std::env::set_var(LOCAL_API_KEY_ENV, "sk-local-test"); // pragma: allowlist secret
        }
        let config = LocalConfig::from_env();
        assert_eq!(
            config.auth.as_ref().map(SecretString::expose),
            Some("sk-local-test")
        );
        // SAFETY: guarded by `#[serial(local_provider_env)]`.
        unsafe {
            std::env::remove_var(LOCAL_API_KEY_ENV);
        }
    }

    /// Why: when no auth override is configured, [`build`] must still
    /// construct a working adapter using [`LOCAL_PLACEHOLDER_KEY`] rather than
    /// erroring — Local never requires a credential.
    /// Test: itself.
    #[test]
    fn placeholder_key_used_when_no_override() {
        let config = LocalConfig {
            base_url: LOCAL_BASE_URL.to_string(),
            auth: None,
        };
        // `build` succeeding at all (no MissingCredential) is the assertion —
        // Local's `ResolvedProvider` always carries `key: None`.
        let adapter = build(&resolved(), config).expect("built without any credential");
        assert_eq!(adapter.name(), "local");
    }
}
