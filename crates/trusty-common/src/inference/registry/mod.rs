//! Provider capability registry (issue #2402, epic #2400 Wave 1).
//!
//! Why: per-model/per-provider behaviour — native tool-calling vs. prompt
//! emulation, streaming, prompt caching, structured output, vision, the real
//! context window, and pricing — must be described in ONE queryable place so
//! adapters, the configurator, and consumers all agree instead of scattering
//! `slug.starts_with(...)` checks. This is the seam that drives later per-model
//! decisions (compaction thresholds, cost estimates, tool-format fallback).
//! What: [`ProviderId`] (the five epic-#2400 providers), [`ToolDialect`],
//! [`ProviderCapabilities`], the static seed table with
//! [`capabilities`]/[`capabilities_for`]/[`all`] queries, and — from the
//! `context` and `pricing` submodules — [`context_window`] (incl. the #2330
//! haiku fix) and [`pricing`]/[`Pricing`].
//! Test: inline `tests` + `context`/`pricing` submodule tests; registry queries
//! in `crates/trusty-common/tests/inference_foundation.rs`.

mod context;
mod pricing;

pub use context::{DEFAULT_CONTEXT_WINDOW, context_window};
pub use pricing::{Pricing, pricing};

use std::fmt;

/// One of the five inference providers epic #2400 targets.
///
/// Why: a closed enum (rather than a bare string) lets the configurator and the
/// registry share one exhaustively-matched identity, so adding a provider is a
/// compile error until every match arm is handled.
/// What: the five target providers. `Bedrock` authenticates via the AWS
/// credential chain (no API key), which is why [`Self::credential_name`] returns
/// `None` for it.
/// Test: `provider_id_round_trips`, `from_slug_prefix_matches`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderId {
    /// OpenRouter aggregator (the default when no explicit prefix resolves).
    OpenRouter,
    /// Fireworks AI.
    Fireworks,
    /// AWS Bedrock (Converse API; AWS credential chain, no API key).
    Bedrock,
    /// Anthropic first-party API.
    Anthropic,
    /// OpenAI first-party API.
    OpenAI,
}

impl ProviderId {
    /// Stable lowercase identifier used in logs, the credential resolver, and
    /// the `config` CLI grammar.
    ///
    /// Why: one canonical spelling per provider that never changes across
    /// releases.
    /// What: `"openrouter"`, `"fireworks"`, `"bedrock"`, `"anthropic"`, `"openai"`.
    /// Test: `provider_id_round_trips`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter",
            Self::Fireworks => "fireworks",
            Self::Bedrock => "bedrock",
            Self::Anthropic => "anthropic",
            Self::OpenAI => "openai",
        }
    }

    /// The provider name to hand the credential resolver, or `None` when the
    /// provider does not use an API key.
    ///
    /// Why: [`crate::inference::credentials::resolve_key_with`] keys off a
    /// provider name; Bedrock has no key (AWS chain), so its resolution is
    /// skipped entirely by the configurator.
    /// What: same string as [`Self::as_str`] for the four keyed providers;
    /// `None` for [`Self::Bedrock`].
    /// Test: `credential_name_none_only_for_bedrock`.
    pub fn credential_name(self) -> Option<&'static str> {
        match self {
            Self::Bedrock => None,
            other => Some(other.as_str()),
        }
    }

    /// Resolve a provider from an explicit `<prefix>/…` model slug.
    ///
    /// Why: stage 1 of the configurator's two-stage resolver keys off the slug
    /// prefix (`bedrock/`, `fireworks/`, `anthropic/`, `openai/`, `openrouter/`);
    /// this is the single mapping it uses.
    /// What: matches the segment before the first `/` case-insensitively;
    /// returns `None` for a bare slug or an unrecognised prefix (the caller then
    /// falls back to the OpenRouter default).
    /// Test: `from_slug_prefix_matches`, `from_slug_prefix_unknown_is_none`.
    pub fn from_slug_prefix(slug: &str) -> Option<Self> {
        let prefix = slug.split('/').next()?;
        match prefix.to_ascii_lowercase().as_str() {
            "openrouter" => Some(Self::OpenRouter),
            "fireworks" => Some(Self::Fireworks),
            "bedrock" => Some(Self::Bedrock),
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::OpenAI),
            _ => None,
        }
    }
}

impl fmt::Display for ProviderId {
    /// Render the canonical identifier (see [`Self::as_str`]).
    ///
    /// Why: error messages ([`crate::inference::InferenceError`]) embed the
    /// provider; a `Display` impl keeps those `#[error(...)]` templates terse.
    /// What: writes [`Self::as_str`].
    /// Test: `provider_id_round_trips`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a provider expects tool definitions and tool-choice on the wire.
///
/// Why: adapters translate the neutral tool surface into the provider's dialect;
/// naming the dialect in the registry (rather than per-adapter booleans) lets a
/// consumer reason about tool compatibility before an adapter is even built.
/// What: `OpenAiFunctions` (OpenRouter/Fireworks/OpenAI), `AnthropicMessages`
/// (Bedrock/Anthropic-direct), and `PromptEmulated` (models without native tool
/// support — the loop injects tool guidance into the prompt).
/// Test: exercised via the seed-table assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolDialect {
    /// OpenAI-style `tools` array + `tool_choice` string/object.
    OpenAiFunctions,
    /// Anthropic Messages-style `tools` + `tool_choice`.
    AnthropicMessages,
    /// No native tool support; tools are emulated via prompt injection.
    PromptEmulated,
}

/// The capability descriptor for one provider.
///
/// Why: a single struct consumers can query for every per-provider behaviour,
/// so a new capability is one field here rather than a new lookup scattered
/// across crates.
/// What: identity, native-tool + dialect, streaming, prompt caching, structured
/// output, vision, the OpenRouter detailed-usage opt-in, the provider's default
/// (max) context window, its default model slug, and its credential env var
/// name (`None` for Bedrock). Per-MODEL context windows come from
/// [`context_window`]; per-model pricing from [`pricing`].
/// Test: `all_five_providers_seeded`, and the `context`/`pricing` submodules.
#[derive(Debug, Clone, Copy)]
pub struct ProviderCapabilities {
    /// The provider this describes.
    pub id: ProviderId,
    /// Whether the provider supports native function-calling.
    pub native_tool_calling: bool,
    /// The wire dialect for tool definitions + tool-choice.
    pub tool_dialect: ToolDialect,
    /// Whether streaming responses are supported.
    pub streaming: bool,
    /// Whether Anthropic-style prompt caching is honoured.
    pub prompt_caching: bool,
    /// Whether structured-output / JSON-schema response formatting is supported
    /// (the `supports_structured_output` capability merged from trusty-review).
    pub structured_output: bool,
    /// Whether image/vision inputs are supported.
    pub vision: bool,
    /// Whether the provider should be asked for detailed usage accounting
    /// (OpenRouter's `usage: {"include": true}` directive).
    pub detailed_usage_accounting: bool,
    /// The provider's default/maximum context window in tokens — the fallback
    /// for a model [`context_window`] does not recognise by substring.
    pub max_context_window: usize,
    /// The provider's default model slug when the caller does not specify one.
    pub default_model: &'static str,
    /// The API-key env var name, or `None` when the provider uses a non-key
    /// credential chain (Bedrock → AWS).
    pub credential_env: Option<&'static str>,
}

/// Seed capability table for the five target providers.
///
/// Why: a single static source of truth. Values are a documented BEST-EFFORT
/// seed sufficient for the foundation; the concrete adapters in #2403 refine
/// any that drift from the provider's live capabilities. Sources: tcode's
/// `provider::openrouter`/`bedrock` (native-tool + caching posture) and
/// trusty-review's model notes (structured output).
/// What: one entry per [`ProviderId`], indexed by [`capabilities`].
/// Test: `all_five_providers_seeded`.
const SEED: [ProviderCapabilities; 5] = [
    ProviderCapabilities {
        id: ProviderId::OpenRouter,
        native_tool_calling: true,
        tool_dialect: ToolDialect::OpenAiFunctions,
        streaming: true,
        prompt_caching: true,
        structured_output: true,
        vision: true,
        detailed_usage_accounting: true,
        max_context_window: 200_000,
        default_model: "openai/gpt-4o-mini",
        credential_env: Some("OPENROUTER_API_KEY"),
    },
    ProviderCapabilities {
        id: ProviderId::Fireworks,
        native_tool_calling: true,
        tool_dialect: ToolDialect::OpenAiFunctions,
        streaming: true,
        prompt_caching: false,
        structured_output: true,
        vision: false,
        detailed_usage_accounting: false,
        max_context_window: 128_000,
        default_model: "accounts/fireworks/models/llama-v3p1-70b-instruct",
        credential_env: Some("FIREWORKS_API_KEY"),
    },
    ProviderCapabilities {
        id: ProviderId::Bedrock,
        native_tool_calling: true,
        tool_dialect: ToolDialect::AnthropicMessages,
        streaming: true,
        prompt_caching: false,
        structured_output: true,
        vision: true,
        detailed_usage_accounting: false,
        max_context_window: 200_000,
        default_model: "bedrock/us.anthropic.claude-sonnet-4-5",
        credential_env: None,
    },
    ProviderCapabilities {
        id: ProviderId::Anthropic,
        native_tool_calling: true,
        tool_dialect: ToolDialect::AnthropicMessages,
        streaming: true,
        prompt_caching: true,
        structured_output: true,
        vision: true,
        detailed_usage_accounting: false,
        max_context_window: 200_000,
        default_model: "claude-sonnet-4-5",
        credential_env: Some("ANTHROPIC_API_KEY"),
    },
    ProviderCapabilities {
        id: ProviderId::OpenAI,
        native_tool_calling: true,
        tool_dialect: ToolDialect::OpenAiFunctions,
        streaming: true,
        prompt_caching: true,
        structured_output: true,
        vision: true,
        detailed_usage_accounting: false,
        max_context_window: 128_000,
        default_model: "gpt-4o-mini",
        credential_env: Some("OPENAI_API_KEY"),
    },
];

/// Look up the capability descriptor for a provider.
///
/// Why: the primary registry query — adapters and the configurator resolve a
/// [`ProviderId`] to its capabilities here.
/// What: returns the static [`ProviderCapabilities`] for `id`; total (never
/// panics) because [`SEED`] has one entry per variant.
/// Test: `all_five_providers_seeded`.
pub fn capabilities(id: ProviderId) -> &'static ProviderCapabilities {
    // SEED is exhaustive over ProviderId, so the find always succeeds; the
    // `unwrap_or(&SEED[0])` is an unreachable, panic-free floor kept only to
    // avoid `expect` on a runtime-reachable path per the crate conventions.
    SEED.iter().find(|c| c.id == id).unwrap_or(&SEED[0])
}

/// Look up a provider's capabilities by its string name.
///
/// Why: the `config` CLI grammar and log lines carry a provider NAME, not a
/// typed id; this is the by-name query the acceptance criteria call for.
/// What: matches `name` case-insensitively against [`ProviderId::as_str`];
/// `None` for an unknown name.
/// Test: `capabilities_for_by_name`, `capabilities_for_unknown_is_none`.
pub fn capabilities_for(name: &str) -> Option<&'static ProviderCapabilities> {
    let lower = name.to_ascii_lowercase();
    SEED.iter().find(|c| c.id.as_str() == lower)
}

/// All seeded provider capabilities.
///
/// Why: the `config <feature> list` verb and diagnostics enumerate every known
/// provider.
/// What: the full static seed slice.
/// Test: `all_five_providers_seeded`.
pub fn all() -> &'static [ProviderCapabilities] {
    &SEED
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the canonical identifier and slug-prefix mapping must round-trip.
    /// Test: itself.
    #[test]
    fn provider_id_round_trips() {
        for id in [
            ProviderId::OpenRouter,
            ProviderId::Fireworks,
            ProviderId::Bedrock,
            ProviderId::Anthropic,
            ProviderId::OpenAI,
        ] {
            assert_eq!(id.to_string(), id.as_str());
            assert_eq!(ProviderId::from_slug_prefix(&format!("{id}/x")), Some(id));
        }
    }

    /// Why: stage 1 of the resolver depends on prefix matching being exact.
    /// Test: itself.
    #[test]
    fn from_slug_prefix_matches() {
        assert_eq!(
            ProviderId::from_slug_prefix("anthropic/claude-sonnet-4-5"),
            Some(ProviderId::Anthropic)
        );
        assert_eq!(
            ProviderId::from_slug_prefix("BEDROCK/us.anthropic.claude"),
            Some(ProviderId::Bedrock)
        );
    }

    /// Why: a bare or unknown-prefix slug must not resolve a family (it falls
    /// back to the OpenRouter default in the configurator).
    /// Test: itself.
    #[test]
    fn from_slug_prefix_unknown_is_none() {
        assert_eq!(ProviderId::from_slug_prefix("claude-sonnet-4-5"), None);
        assert_eq!(ProviderId::from_slug_prefix("cohere/command"), None);
    }

    /// Why: only Bedrock uses a non-key credential chain.
    /// Test: itself.
    #[test]
    fn credential_name_none_only_for_bedrock() {
        assert_eq!(ProviderId::Bedrock.credential_name(), None);
        assert_eq!(ProviderId::OpenRouter.credential_name(), Some("openrouter"));
        assert_eq!(ProviderId::Anthropic.credential_name(), Some("anthropic"));
    }

    /// Why: every provider must be seeded and queryable by id and by name.
    /// Test: itself.
    #[test]
    fn all_five_providers_seeded() {
        assert_eq!(all().len(), 5);
        for id in [
            ProviderId::OpenRouter,
            ProviderId::Fireworks,
            ProviderId::Bedrock,
            ProviderId::Anthropic,
            ProviderId::OpenAI,
        ] {
            let caps = capabilities(id);
            assert_eq!(caps.id, id);
            assert!(caps.max_context_window >= 128_000);
        }
    }

    /// Why: the by-name query must be case-insensitive and total for knowns.
    /// Test: itself.
    #[test]
    fn capabilities_for_by_name() {
        assert_eq!(
            capabilities_for("OpenRouter").map(|c| c.id),
            Some(ProviderId::OpenRouter)
        );
        assert_eq!(
            capabilities_for("bedrock").map(|c| c.id),
            Some(ProviderId::Bedrock)
        );
    }

    /// Why: an unknown provider name must be `None`, not a panic or a guess.
    /// Test: itself.
    #[test]
    fn capabilities_for_unknown_is_none() {
        assert!(capabilities_for("cohere").is_none());
    }
}
