//! Tests for SM provider/model resolution (DOC-14 §5.2–§5.4).
//!
//! Why: resolution is the core of SM-2 — these tests pin every routing,
//! tier, alias-fallback, precedence, validation, and degraded-mode rule the
//! spec requires, with no real API calls.
//! What: prefix routing (all three + bare), tier defaults (Sonnet/Haiku),
//! deprecated `model` alias fallback, compaction override + fallback, unknown
//! provider rejection, `auto` precedence ordering, and degraded mode.
//! Test: included as `#[cfg(test)] mod tests` via `#[path]` from `resolve.rs`.

use super::*;
use crate::core::sm::config::SmInferenceConfig;

/// A minimal inference config with all tier fields empty, for targeted tests.
///
/// Why: most tests want to set exactly one or two fields without inheriting the
/// §10 defaults (which would mask the behavior under test).
/// What: returns an `SmInferenceConfig` with empty tier strings and `auto`.
/// Test: helper for the tests below.
fn empty_cfg() -> SmInferenceConfig {
    SmInferenceConfig {
        provider: "auto".to_string(),
        sm_model: String::new(),
        summary_model: String::new(),
        model: String::new(),
        fallback: Vec::new(),
        temperature: 0.3,
        context_token_budget: 24_000,
        compaction_model: String::new(),
        compressed_context_max_tokens: 4_000,
    }
}

// ── Prefix routing (D5.3) ──────────────────────────────────────────────────

/// Route, treating an error as a test failure.
fn routed(model: &str, default_provider: ProviderKind) -> (ProviderKind, String) {
    resolve_provider_and_model(model, default_provider)
        .unwrap_or_else(|e| panic!("{model:?} must route: {e}"))
}

#[test]
fn route_anthropic_prefix() {
    let (kind, model) = routed("anthropic/claude-sonnet-4-6", ProviderKind::OpenRouter);
    assert_eq!(kind, ProviderKind::Anthropic);
    assert_eq!(model, "claude-sonnet-4-6");
}

#[test]
fn route_bedrock_prefix() {
    let (kind, model) = routed(
        "bedrock/us.anthropic.claude-sonnet-4-6",
        ProviderKind::Anthropic,
    );
    assert_eq!(kind, ProviderKind::Bedrock);
    assert_eq!(model, "us.anthropic.claude-sonnet-4-6");
}

#[test]
fn route_openrouter_prefix() {
    let (kind, model) = routed("openrouter/anthropic/claude-haiku", ProviderKind::Bedrock);
    assert_eq!(kind, ProviderKind::OpenRouter);
    assert_eq!(model, "anthropic/claude-haiku");
}

#[test]
fn route_bare_uses_default() {
    // A bare id with no unambiguous shape keeps the supplied default provider.
    let (kind, model) = routed("claude-sonnet-4-6", ProviderKind::OpenRouter);
    assert_eq!(kind, ProviderKind::OpenRouter);
    assert_eq!(model, "claude-sonnet-4-6");

    // Same, with Auto default: stays Auto (precedence applied later).
    let (kind, _) = routed("claude-haiku", ProviderKind::Auto);
    assert_eq!(kind, ProviderKind::Auto);
}

// ── Model-shape inference (#6114) ──────────────────────────────────────────

/// #6114: an unprefixed OpenRouter slug names OpenRouter, whatever the default.
///
/// Why: the slug shape belongs to no other provider the SM builds, so handing it
/// to the configured default sends an id upstream that the default has never
/// heard of — or, under `auto`, to whichever provider happened to have a
/// credential.
/// What: asserts the routed kind is OpenRouter for a Bedrock default and for the
/// `auto` chain, with the id passed through unchanged. The slugs here
/// deliberately do NOT open with one of the SM's own routing prefixes: `anthropic/`
/// is a prefix in this crate (it pins the first-party API), so shape inference
/// never sees such an id — it only ever judges what is left after stripping.
/// Test: this test itself.
#[test]
fn route_bare_openrouter_slug_beats_the_default() {
    let (kind, model) = routed("google/gemini-3-pro", ProviderKind::Bedrock);
    assert_eq!(kind, ProviderKind::OpenRouter);
    assert_eq!(model, "google/gemini-3-pro");

    let (kind, _) = routed("openai/gpt-5.4-mini-20260317", ProviderKind::Auto);
    assert_eq!(
        kind,
        ProviderKind::OpenRouter,
        "an unambiguous shape outranks the credential precedence chain (#6114)"
    );
}

/// #6114: the mirror case — a Bedrock inference profile names Bedrock.
#[test]
fn route_bare_bedrock_profile_beats_the_default() {
    let (kind, model) = routed("us.anthropic.claude-sonnet-4-6", ProviderKind::OpenRouter);
    assert_eq!(kind, ProviderKind::Bedrock);
    assert_eq!(model, "us.anthropic.claude-sonnet-4-6");
}

/// #6114: a prefix that contradicts the id's shape is an error naming both,
/// never a silent call to the prefixed provider.
#[test]
fn route_prefix_contradicting_the_shape_is_an_error() {
    let err = resolve_provider_and_model(
        "bedrock/anthropic/claude-opus-4.8",
        ProviderKind::OpenRouter,
    )
    .expect_err("a bedrock/ prefix on an OpenRouter slug must not route");
    let msg = err.to_string();
    assert!(
        msg.contains("anthropic/claude-opus-4.8"),
        "the error must name the requested id: {msg}"
    );
    assert!(
        msg.contains("bedrock") && msg.contains("openrouter"),
        "the error must name both providers: {msg}"
    );

    // The Anthropic first-party provider cannot run a Bedrock profile id either.
    resolve_provider_and_model(
        "anthropic/us.anthropic.claude-sonnet-4-6",
        ProviderKind::Auto,
    )
    .expect_err("a bedrock profile id must not be sent to api.anthropic.com");
}

/// #6114 (code-critic MEDIUM 2): an explicit routing prefix survives a
/// merely-probable Bedrock shape.
///
/// Why: `openrouter/anthropic.claude-x` states the provider for this call. The
/// dotted-vendor rule is a guess from a vendor-name list, and a guess must not
/// turn a working configuration into a hard error. The catalogue-fact shapes
/// still veto — `route_prefix_contradicting_the_shape_is_an_error` proves that
/// half is intact.
/// What: pins the dotted remainder through the public resolver under each of
/// the three prefixes, and asserts the prefix wins every time.
/// Test: this test itself.
#[test]
fn route_explicit_prefix_survives_a_probable_bedrock_shape() {
    let cases = [
        ("openrouter/anthropic.claude-x", ProviderKind::OpenRouter),
        ("anthropic/anthropic.claude-x", ProviderKind::Anthropic),
        ("bedrock/anthropic.claude-x", ProviderKind::Bedrock),
    ];
    for (requested, expected) in cases {
        let (kind, model) = routed(requested, ProviderKind::Bedrock);
        assert_eq!(kind, expected, "the prefix on {requested:?} must win");
        assert_eq!(
            model, "anthropic.claude-x",
            "only the routing prefix is stripped"
        );
    }

    // With NO prefix the same id still reads as Bedrock — a config default does
    // not outrank the shape, only an explicit per-call prefix does.
    let (kind, _) = routed("anthropic.claude-x", ProviderKind::OpenRouter);
    assert_eq!(kind, ProviderKind::Bedrock);
}

/// #6114 (code-critic MEDIUM 1): the remedy never names a `provider` value the
/// config parser rejects.
///
/// Why: telling an operator to set `provider = "fireworks"` when
/// [`ProviderKind::parse`] rejects it sends them in a circle.
/// What: takes both shape errors, isolates the REMEDY half of each (the quoted
/// model id in the diagnosis half is not a suggestion), and asserts every
/// double-quoted token the remedy offers is either a parseable `provider` value
/// or a routing prefix the resolver strips.
/// Test: this test itself.
#[test]
fn shape_error_only_suggests_parseable_provider_values() {
    /// The remedy starts after the refusal sentence; everything before it is
    /// diagnosis, including the operator's own (unparseable) model id.
    const REFUSAL: &str = "than the one requested. ";

    let unbuildable = resolve_provider_and_model(
        "accounts/fireworks/models/llama-v3p1-70b-instruct",
        ProviderKind::OpenRouter,
    )
    .expect_err("a fireworks-native id must not run on OpenRouter")
    .to_string();
    let buildable = resolve_provider_and_model(
        "bedrock/anthropic/claude-opus-4.8",
        ProviderKind::OpenRouter,
    )
    .expect_err("an OpenRouter slug must not run on Bedrock")
    .to_string();

    for msg in [&unbuildable, &buildable] {
        let (_, remedy) = msg
            .split_once(REFUSAL)
            .unwrap_or_else(|| panic!("the message must carry a remedy: {msg}"));
        for token in remedy.split('"').skip(1).step_by(2) {
            let is_prefix = ProviderKind::ALL
                .iter()
                .filter_map(|k| k.model_prefix())
                .any(|p| token.starts_with(p));
            assert!(
                is_prefix || ProviderKind::parse(token).is_ok(),
                "the remedy offers {token:?}, which is neither a parseable provider \
                 value nor a routing prefix: {msg}"
            );
        }
    }

    // The unbuildable case must say so and name the set, rather than suggesting
    // a `provider` value that parse() rejects (the #6114 MEDIUM-1 defect).
    assert!(
        unbuildable.contains("has no client for fireworks")
            && unbuildable.contains(&ProviderKind::accepted_values()),
        "the unbuildable remedy must name the accepted set: {unbuildable}"
    );
    assert!(
        !unbuildable.contains("provider to \"fireworks\""),
        "must never suggest a provider value parse() rejects: {unbuildable}"
    );
}

/// #6114: a shape naming a provider the SM has no client for stops the call.
///
/// Why: the SM builds Anthropic, Bedrock, and OpenRouter. A Fireworks-native id
/// belongs to none of them, and running it on whichever one the config named is
/// the substitution this ticket removes.
/// What: asserts the error names the id and the fireworks provider.
/// Test: this test itself.
#[test]
fn route_shape_the_sm_cannot_build_is_an_error() {
    let err = resolve_provider_and_model(
        "accounts/fireworks/models/llama-v3p1-70b-instruct",
        ProviderKind::OpenRouter,
    )
    .expect_err("a fireworks-native id must not run on OpenRouter");
    let msg = err.to_string();
    assert!(
        msg.contains("accounts/fireworks/models/llama-v3p1-70b-instruct")
            && msg.contains("fireworks"),
        "the error must name the id and the provider it belongs to: {msg}"
    );
}

// ── Tier selection + alias fallback (§5.4) ─────────────────────────────────

#[test]
fn tier_orchestration_uses_sm_model() {
    let mut cfg = empty_cfg();
    cfg.sm_model = "anthropic/claude-sonnet-4-6".to_string();
    let m = resolve_tier_model(&cfg, SmModelTier::Orchestration).unwrap();
    assert_eq!(m, "anthropic/claude-sonnet-4-6");
}

/// Why: SM-1 review carry-forward — empty `sm_model` must fall back to the
/// deprecated `model` alias for the orchestration tier.
/// What: leaves `sm_model` empty, sets `model`, asserts the alias is used.
/// Test: this is the test.
#[test]
fn tier_orchestration_alias_fallback() {
    let mut cfg = empty_cfg();
    cfg.sm_model = String::new();
    cfg.model = "legacy/sonnet".to_string();
    let m = resolve_tier_model(&cfg, SmModelTier::Orchestration).unwrap();
    assert_eq!(m, "legacy/sonnet");
}

/// Why: the §10 defaults must keep working through resolution — `sm_model`
/// defaults to a Sonnet tier and `summary_model` to a Haiku tier.
/// What: uses `SmInferenceConfig::default()` and asserts the tier strings carry
/// the documented Sonnet/Haiku ids.
/// Test: this is the test.
#[test]
fn tier_defaults_are_sonnet_and_haiku() {
    let cfg = SmInferenceConfig::default();
    let orch = resolve_tier_model(&cfg, SmModelTier::Orchestration).unwrap();
    let summ = resolve_tier_model(&cfg, SmModelTier::Summary).unwrap();
    assert!(
        orch.contains("sonnet"),
        "orchestration default is Sonnet: {orch}"
    );
    assert!(summ.contains("haiku"), "summary default is Haiku: {summ}");
}

#[test]
fn tier_summary_default() {
    let mut cfg = empty_cfg();
    cfg.summary_model = "anthropic/claude-haiku".to_string();
    let m = resolve_tier_model(&cfg, SmModelTier::Summary).unwrap();
    assert_eq!(m, "anthropic/claude-haiku");
}

/// Why: a non-empty `compaction_model` must override `summary_model` for the
/// compaction tier only (§7.3 / §5.4).
/// What: sets distinct summary + compaction models, asserts compaction wins.
/// Test: this is the test.
#[test]
fn tier_compaction_override() {
    let mut cfg = empty_cfg();
    cfg.summary_model = "anthropic/claude-haiku".to_string();
    cfg.compaction_model = "bedrock/us.anthropic.claude-haiku".to_string();
    let m = resolve_tier_model(&cfg, SmModelTier::Compaction).unwrap();
    assert_eq!(m, "bedrock/us.anthropic.claude-haiku");
}

/// Why: an empty `compaction_model` must fall back to `summary_model`.
/// What: leaves compaction empty, sets summary, asserts the fallback.
/// Test: this is the test.
#[test]
fn tier_compaction_falls_back_to_summary() {
    let mut cfg = empty_cfg();
    cfg.summary_model = "anthropic/claude-haiku".to_string();
    cfg.compaction_model = String::new();
    let m = resolve_tier_model(&cfg, SmModelTier::Compaction).unwrap();
    assert_eq!(m, "anthropic/claude-haiku");
}

/// Why: a tier with no configured model (and no alias) is a config error, not a
/// silent empty request.
/// What: leaves every tier field empty and asserts a `Validation` error.
/// Test: this is the test.
#[test]
fn tier_empty_is_validation_error() {
    let cfg = empty_cfg();
    let err = resolve_tier_model(&cfg, SmModelTier::Orchestration).unwrap_err();
    assert!(matches!(err, SmLlmError::Validation(_)));
}

// ── Provider validation ────────────────────────────────────────────────────

/// Why: an unknown `provider` string must be rejected as a config error during
/// `build`, not silently ignored (SM-1 review carry-forward).
/// What: sets a bogus provider, asserts `build` returns a `Validation` alarm.
/// Test: this is the test.
#[tokio::test]
async fn registry_rejects_unknown_provider() {
    let cfg = SmInferenceConfig {
        provider: "totally-bogus".to_string(),
        ..SmInferenceConfig::default()
    };
    let registry = ProviderRegistry {
        anthropic_api_key: Some("sk-ant".to_string()),
        aws_credentials_available: false,
        openrouter_api_key: None,
    };
    let err = registry
        .build(&cfg, SmModelTier::Orchestration)
        .await
        .expect_err("unknown provider rejected");
    assert!(matches!(err, SmLlmError::Validation(_)));
    assert!(err.is_alarm());
}

// ── Registry: explicit prefix routing ──────────────────────────────────────

/// Why: an `anthropic/` prefix must build the Anthropic provider regardless of
/// the config `provider` and even when an OpenRouter key is also present.
/// What: routes an explicit `anthropic/` orchestration model and asserts the
/// resolved kind + bare model + provider name.
/// Test: this is the test (provider construction only; no API call).
#[tokio::test]
async fn registry_routes_explicit_prefix() {
    let mut cfg = empty_cfg();
    cfg.provider = "openrouter".to_string();
    cfg.sm_model = "anthropic/claude-sonnet-4-6".to_string();
    let registry = ProviderRegistry {
        anthropic_api_key: Some("sk-ant".to_string()),
        aws_credentials_available: false,
        openrouter_api_key: Some("sk-or".to_string()),
    };
    let resolved = registry
        .build(&cfg, SmModelTier::Orchestration)
        .await
        .expect("anthropic prefix builds");
    assert_eq!(resolved.kind, ProviderKind::Anthropic);
    assert_eq!(resolved.model, "claude-sonnet-4-6");
    assert_eq!(resolved.provider.name(), "anthropic");
}

// ── Registry: auto precedence + degraded ───────────────────────────────────

/// Why: `provider = "auto"` must prefer Anthropic → (Bedrock) → OpenRouter.
/// What: with only an OpenRouter key present, a bare-model auto config must
/// resolve to OpenRouter (Anthropic/Bedrock unavailable).
/// Test: this is the test.
#[tokio::test]
async fn registry_auto_precedence() {
    let mut cfg = empty_cfg();
    cfg.provider = "auto".to_string();
    cfg.sm_model = "claude-sonnet-4-6".to_string(); // bare → routes via precedence

    // Only OpenRouter available → OpenRouter selected.
    let or_only = ProviderRegistry {
        anthropic_api_key: None,
        aws_credentials_available: false,
        openrouter_api_key: Some("sk-or".to_string()),
    };
    let resolved = or_only
        .build(&cfg, SmModelTier::Orchestration)
        .await
        .expect("openrouter selected");
    assert_eq!(resolved.kind, ProviderKind::OpenRouter);
    assert_eq!(resolved.provider.name(), "openrouter");

    // Anthropic present → Anthropic wins over OpenRouter.
    let both = ProviderRegistry {
        anthropic_api_key: Some("sk-ant".to_string()),
        aws_credentials_available: false,
        openrouter_api_key: Some("sk-or".to_string()),
    };
    let resolved = both
        .build(&cfg, SmModelTier::Orchestration)
        .await
        .expect("anthropic wins");
    assert_eq!(resolved.kind, ProviderKind::Anthropic);
}

/// Why: with NO provider credentials, `auto` must degrade gracefully — a
/// reportable `Degraded` error, never a panic (§5.3 / G6).
/// What: builds against an empty registry and asserts `is_degraded()`.
/// Test: this is the test.
#[tokio::test]
async fn registry_degraded_without_creds() {
    let mut cfg = empty_cfg();
    cfg.provider = "auto".to_string();
    cfg.sm_model = "claude-sonnet-4-6".to_string();
    let registry = ProviderRegistry::default(); // no creds
    let err = registry
        .build(&cfg, SmModelTier::Orchestration)
        .await
        .expect_err("no creds → degraded");
    assert!(err.is_degraded(), "expected degraded, got: {err}");
    assert!(!err.is_alarm());
    assert!(!err.is_retryable());
}

/// Why: explicitly selecting `anthropic` without the key must degrade (not
/// silently fall through to another provider).
/// What: sets `provider = "anthropic"` with no key; asserts `Degraded`.
/// Test: this is the test.
#[tokio::test]
async fn registry_explicit_anthropic_without_key_degrades() {
    let mut cfg = empty_cfg();
    cfg.provider = "anthropic".to_string();
    cfg.sm_model = "claude-sonnet-4-6".to_string();
    let registry = ProviderRegistry {
        anthropic_api_key: None,
        aws_credentials_available: false,
        openrouter_api_key: Some("sk-or".to_string()),
    };
    let err = registry
        .build(&cfg, SmModelTier::Orchestration)
        .await
        .expect_err("anthropic without key degrades");
    assert!(err.is_degraded(), "expected degraded, got: {err}");
}

// ── Bedrock feature gating ─────────────────────────────────────────────────

/// Why: when built WITHOUT the `bedrock` feature, pinning a `bedrock/` model
/// must produce a clear, actionable validation error (rebuild with the
/// feature), not a confusing degraded/transport failure.
/// What: routes a `bedrock/` model and asserts a `Validation` error mentioning
/// the feature flag.
/// Test: only compiled in the default (no-bedrock) build.
#[cfg(not(feature = "bedrock"))]
#[tokio::test]
async fn registry_bedrock_prefix_without_feature() {
    let mut cfg = empty_cfg();
    cfg.provider = "auto".to_string();
    cfg.sm_model = "bedrock/us.anthropic.claude-sonnet-4-6".to_string();
    let registry = ProviderRegistry {
        anthropic_api_key: None,
        aws_credentials_available: true,
        openrouter_api_key: None,
    };
    let err = registry
        .build(&cfg, SmModelTier::Orchestration)
        .await
        .expect_err("bedrock without feature errors");
    match err {
        SmLlmError::Validation(msg) => {
            assert!(msg.contains("bedrock"), "msg should mention bedrock: {msg}");
            assert!(msg.contains("feature"), "msg should mention feature: {msg}");
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

/// Why: when built WITH the `bedrock` feature, `auto` precedence must prefer
/// Bedrock over OpenRouter when AWS creds are available and Anthropic is not.
/// What: routes a bare-model auto config with only AWS creds + an OpenRouter
/// key and asserts the resolved *kind* + bare model are Bedrock. Uses
/// `resolve_kind_and_model` (the pure decision half of `build`) so the test
/// makes ZERO network calls — it never reaches `construct`, which would load
/// the real AWS SDK config chain (potentially probing IMDS/network in CI).
/// Test: only compiled in the `--features bedrock` build.
#[cfg(feature = "bedrock")]
#[test]
fn registry_auto_prefers_bedrock_when_available() {
    let mut cfg = empty_cfg();
    cfg.provider = "auto".to_string();
    cfg.sm_model = "bedrock/us.anthropic.claude-sonnet-4-6".to_string();
    let registry = ProviderRegistry {
        anthropic_api_key: None,
        aws_credentials_available: true,
        openrouter_api_key: Some("sk-or".to_string()),
    };
    let (kind, model) = registry
        .resolve_kind_and_model(&cfg, SmModelTier::Orchestration)
        .expect("auto precedence resolves to bedrock");
    assert_eq!(kind, ProviderKind::Bedrock);
    assert_eq!(model, "us.anthropic.claude-sonnet-4-6");
}
