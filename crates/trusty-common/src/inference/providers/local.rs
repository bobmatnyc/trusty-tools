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
//! [`LocalConfig::from_env`]), [`LocalAdapter`] (the liveness-probing wrapper
//! around an [`OpenAiCompatAdapter`], #4490), [`build`] (constructs one from a
//! [`LocalConfig`]; never requires a resolved credential — unlike the keyed
//! providers, a missing key is not an error), and [`factory`] (the production
//! entry point [`super::register_default_factories`] registers under
//! [`ProviderId::Local`]).
//!
//! Liveness (#4490): a local server is a process that is usually NOT running,
//! unlike a cloud endpoint that is usually up. Every request therefore probes
//! `{base_url}/models` first, through the shared
//! [`crate::local_probe`] entry point `chat::auto_detect_local_provider` also
//! uses, and fails inside [`crate::local_probe::LOCAL_PROBE_TIMEOUT`] with an
//! [`InferenceError::Transport`] naming the endpoint. Without it this adapter
//! POSTed unconditionally against a client with no timeout at all, so a
//! consumer migrating off `chat::ChatProvider` (#4427) lost local
//! auto-detection silently.
//!
//! Override mechanism: set [`LOCAL_HOST_ENV`] (`OLLAMA_HOST`, matching the
//! env var `trusty-agents`' legacy `OllamaAdapter` already reads — a bare
//! host, e.g. `http://192.168.1.50:11434`, NOT including `/v1`) to point at a
//! non-default local endpoint, and [`LOCAL_API_KEY_ENV`]
//! (`TRUSTY_LOCAL_API_KEY`) to supply a bearer credential for the rare local
//! server that enforces auth. Both are optional; Ollama needs neither.
//! Test: inline `tests` — `factory_builds_named_adapter_with_defaults`,
//! `host_env_override_appends_v1_suffix`, `api_key_env_override_is_used`,
//! `placeholder_key_used_when_no_override`,
//! `probe_url_is_derived_from_the_base_url`,
//! `chat_fails_inside_the_probe_budget_when_the_server_never_answers`;
//! the live-endpoint half is
//! `local_probe_passes_and_the_request_proceeds`
//! (`crates/trusty-common/tests/inference_adapters.rs`).

use async_trait::async_trait;
use serde_json::Value;

use super::openai_compat::{OpenAiCompatAdapter, OpenAiCompatConfig};
use crate::inference::adapter::InferenceAdapter;
use crate::inference::configurator::ResolvedProvider;
use crate::inference::error::InferenceError;
use crate::inference::registry::{ProviderCapabilities, ProviderId};
use crate::inference::streaming::ChatStream;
use crate::inference::types::{ChatRequest, ChatResponse, SecretString, ToolChoice};
use crate::local_probe::{self, LocalProbeError};

/// Default local OpenAI-compatible base URL — Ollama's native `/v1` shim.
pub const LOCAL_BASE_URL: &str = "http://localhost:11434/v1";

/// Env var overriding the local host. Takes a BARE host (no `/v1` suffix,
/// e.g. `http://192.168.1.50:11434`); `/v1` is appended automatically, same as
/// [`LOCAL_BASE_URL`]. Named `OLLAMA_HOST` (rather than a new `TRUSTY_*` name)
/// so it consolidates with the identical override `trusty-agents`' legacy
/// `llm::adapter::impls::OllamaAdapter::api_endpoint` already reads — setting
/// it once affects both the legacy dispatch and this registry-based path.
///
/// Re-exported from [`crate::local_probe`] (#4490), which is where the probe and
/// every `trusty-agents` call site read it from too.
pub use crate::local_probe::LOCAL_HOST_ENV;

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
    /// What: resolves the bare host through
    /// [`crate::local_probe::local_host`] — the one reader of [`LOCAL_HOST_ENV`]
    /// (#4490) — and appends `/v1`. Reads [`LOCAL_API_KEY_ENV`]; when set and
    /// non-blank, wraps it in a [`SecretString`]. Both env vars are optional.
    /// Test: `host_env_override_appends_v1_suffix`, `api_key_env_override_is_used`,
    /// `from_env_defaults_when_unset`.
    pub fn from_env() -> Self {
        // #4490: one resolver, so the probe and the request dial the same host.
        let base_url = format!("{}/v1", local_probe::local_host());
        let auth = std::env::var(LOCAL_API_KEY_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(SecretString::new);
        Self { base_url, auth }
    }
}

/// Translate a probe failure into the adapter's error surface.
///
/// Why: a dead local server is a transport condition, not a configuration one —
/// [`InferenceError::Transport`] is the variant
/// [`InferenceError::is_retryable`] already reports as worth retrying, which is
/// right for a server the operator may be about to start. The probe's own
/// `Display` already names the endpoint, so nothing is added here.
/// What: wraps the [`LocalProbeError`] text in [`InferenceError::Transport`].
/// Test: `chat_fails_inside_the_probe_budget_when_the_server_never_answers`.
fn probe_failure(err: LocalProbeError) -> InferenceError {
    InferenceError::Transport(err.to_string())
}

/// An [`OpenAiCompatAdapter`] that confirms the local server is live before it
/// sends anything (#4490).
///
/// Why: a cloud endpoint is up unless something is wrong; a local model server
/// is a process on the operator's machine that is usually NOT running. The
/// underlying [`OpenAiCompatAdapter`] builds its `reqwest` client with no
/// timeout at all, so against a host that accepts the connection and never
/// answers — a wedged Ollama, a port claimed by something else — a caller waits
/// indefinitely instead of falling back to a cloud provider. Probing first turns
/// that into a bounded, typed failure the caller can route around, which is the
/// capability `chat::auto_detect_local_provider` had and `inference::` did not.
/// What: delegates every [`InferenceAdapter`] method to the wrapped adapter, and
/// runs [`Self::probe`] ahead of [`InferenceAdapter::chat`] and
/// [`InferenceAdapter::chat_stream`]. The probe is
/// [`crate::local_probe::probe_models_endpoint`] against a URL derived once at
/// construction.
/// Test: `chat_fails_inside_the_probe_budget_when_the_server_never_answers`,
/// `probe_url_is_derived_from_the_base_url`.
pub struct LocalAdapter {
    inner: OpenAiCompatAdapter,
    probe_url: String,
}

impl LocalAdapter {
    /// Wrap `inner`, probing the models endpoint derived from `base_url`.
    ///
    /// Why: the base URL is consumed by [`OpenAiCompatConfig`], so the probe URL
    /// is derived once here rather than recomputed per request.
    /// What: stores `inner` and [`crate::local_probe::models_url`]'s output.
    /// Test: `probe_url_is_derived_from_the_base_url`.
    pub fn new(inner: OpenAiCompatAdapter, base_url: &str) -> Self {
        Self {
            inner,
            probe_url: local_probe::models_url(base_url),
        }
    }

    /// The `/v1/models` URL this adapter probes before each request.
    ///
    /// Why: exposed for diagnostics and so a caller can report what was dialled.
    /// What: the URL derived at construction.
    /// Test: `probe_url_is_derived_from_the_base_url`.
    pub fn probe_url(&self) -> &str {
        &self.probe_url
    }

    /// Confirm the local server is reachable right now.
    ///
    /// Why: the public form of the ported probe — a caller deciding between a
    /// local provider and a cloud one can ask without issuing a chat request.
    /// What: [`crate::local_probe::probe_models_endpoint`] against
    /// [`Self::probe_url`], with the failure mapped to
    /// [`InferenceError::Transport`].
    /// Test: `chat_fails_inside_the_probe_budget_when_the_server_never_answers`.
    pub async fn probe(&self) -> Result<(), InferenceError> {
        local_probe::probe_models_endpoint(&self.probe_url)
            .await
            .map_err(probe_failure)
    }
}

#[async_trait]
impl InferenceAdapter for LocalAdapter {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn capabilities(&self) -> &ProviderCapabilities {
        self.inner.capabilities()
    }

    fn capabilities_for(&self, model: &str) -> &ProviderCapabilities {
        self.inner.capabilities_for(model)
    }

    /// Guard the capability (#5588), probe (#4490), then delegate.
    ///
    /// The guard runs BEFORE the probe: a schema this provider cannot honour
    /// fails no matter what the probe finds, so spending a round-trip to learn
    /// the server is alive tells the caller nothing.
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        self.ensure_structured_output_supported(request)?;
        self.probe().await?;
        self.inner.chat(request).await
    }

    /// Guard, probe, then delegate — see [`Self::chat`].
    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChatStream, InferenceError> {
        self.ensure_structured_output_supported(request)?;
        self.probe().await?;
        self.inner.chat_stream(request).await
    }

    fn map_tool_choice(&self, choice: ToolChoice) -> Value {
        self.inner.map_tool_choice(choice)
    }

    fn supports_native_tools(&self) -> bool {
        self.inner.supports_native_tools()
    }

    fn supports_prompt_caching(&self) -> bool {
        self.inner.supports_prompt_caching()
    }

    fn supports_structured_output(&self) -> bool {
        self.inner.supports_structured_output()
    }

    fn wants_detailed_usage(&self) -> bool {
        self.inner.wants_detailed_usage()
    }

    fn context_window(&self, model: &str) -> usize {
        self.inner.context_window(model)
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
/// attribution headers and [`ProviderId::Local`]'s registry capabilities, then
/// wraps it in a [`LocalAdapter`] so every request is gated on the liveness
/// probe (#4490).
/// Test: `factory_builds_named_adapter_with_defaults`,
/// `chat_fails_inside_the_probe_budget_when_the_server_never_answers`.
pub fn build(
    _resolved: &ResolvedProvider,
    config: LocalConfig,
) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    let api_key = config
        .auth
        .unwrap_or_else(|| SecretString::new(LOCAL_PLACEHOLDER_KEY));
    let base_url = config.base_url;
    let cfg = OpenAiCompatConfig {
        name: ProviderId::Local.as_str().to_string(),
        base_url: base_url.clone(),
        api_key,
        extra_headers: Vec::new(),
        capabilities: *crate::inference::registry::capabilities(ProviderId::Local),
    };
    let inner = OpenAiCompatAdapter::new(cfg)?;
    Ok(Box::new(LocalAdapter::new(inner, &base_url)))
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

    /// Why (#4490): the probe URL is derived once at construction from a base
    /// URL that already carries `/v1`; getting that wrong dials `/v1/v1/models`
    /// and reports every live server as dead.
    /// Test: this test.
    #[test]
    fn probe_url_is_derived_from_the_base_url() {
        let inner = OpenAiCompatAdapter::new(OpenAiCompatConfig {
            name: ProviderId::Local.as_str().to_string(),
            base_url: LOCAL_BASE_URL.to_string(),
            api_key: SecretString::new(LOCAL_PLACEHOLDER_KEY),
            extra_headers: Vec::new(),
            capabilities: *crate::inference::registry::capabilities(ProviderId::Local),
        })
        .expect("inner adapter builds");
        let adapter = LocalAdapter::new(inner, LOCAL_BASE_URL);
        assert_eq!(adapter.probe_url(), "http://localhost:11434/v1/models");
    }

    /// A request against a local endpoint that accepts the connection and never
    /// answers fails inside the probe budget, naming the endpoint (#4490).
    ///
    /// Why: this is the regression the ticket exists to prevent. Without the
    /// probe, `chat` POSTs through [`OpenAiCompatAdapter`]'s client — which is
    /// built with NO timeout — so a wedged local server hangs the caller
    /// indefinitely instead of failing fast enough to fall back to a cloud
    /// provider. The black-hole listener is what separates the two behaviours: a
    /// merely CLOSED port refuses instantly either way and would prove nothing.
    /// What: binds a listener that accepts and holds connections without
    /// writing a byte, points a Local adapter at it, and asserts `chat` returns
    /// a retryable [`InferenceError::Transport`] naming the address, well inside
    /// the outer 5s guard. The guard is what fails (rather than hangs) if the
    /// probe is removed.
    /// Test: this test.
    #[tokio::test]
    async fn chat_fails_inside_the_probe_budget_when_the_server_never_answers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind black-hole listener");
        let addr = listener.local_addr().expect("listener addr");
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });

        let config = LocalConfig {
            base_url: format!("http://{addr}/v1"),
            auth: None,
        };
        let adapter = build(&resolved(), config).expect("built");
        let request = ChatRequest::new(
            "local/llama3.1",
            vec![crate::inference::types::ChatMessage::user("ping")],
        );

        let started = std::time::Instant::now();
        let outcome =
            tokio::time::timeout(std::time::Duration::from_secs(5), adapter.chat(&request))
                .await
                .expect(
                    "chat must return inside the probe budget, not hang on a dead local server",
                );
        let elapsed = started.elapsed();

        let err = outcome.expect_err("a server that never answers must not yield a response");
        assert!(
            matches!(err, InferenceError::Transport(_)),
            "expected Transport, got {err:?}"
        );
        assert!(
            err.to_string().contains(&addr.to_string()),
            "the error must name the endpoint that was dialled: {err}"
        );
        assert!(err.is_retryable(), "a dead local server is worth retrying");
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "probe budget is {:?}; took {elapsed:?}",
            crate::local_probe::LOCAL_PROBE_TIMEOUT
        );
    }
}
