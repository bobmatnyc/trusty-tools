//! Offline integration suite for the Bedrock Converse adapter (issue #2407).
//!
//! Why: the `bedrock/*` routing → factory → adapter wiring must be provable
//! without any AWS call, so CI (which never holds AWS credentials) can gate it.
//! The adapter's Converse conversion is unit-tested in-crate; this suite proves
//! the *configurator seam*: `register_bedrock_factory` makes a `Configurator`
//! resolve a `bedrock/*` slug (no key — AWS credential chain) into a live,
//! lazily-constructed `BedrockAdapter`, and that an unregistered configurator
//! alarms instead of silently falling back.
//! What: `bedrock_factory_registers_and_builds`,
//! `unregistered_bedrock_provider_errors`.

use trusty_common::inference::credentials::MemoryKeyStore;
use trusty_common::inference::{
    Configurator, InferenceError, ProviderId, register_bedrock_factory,
};

/// Registering the Bedrock factory makes the configurator build a real Bedrock
/// adapter for a `bedrock/*` slug — offline, with an empty credential store
/// (Bedrock resolves with no key via the AWS chain) and no AWS call (the client
/// is constructed lazily on first `chat`).
///
/// Why: this is the epic's construction seam for the Anthropic-dialect provider;
/// a broken registration would make `bedrock/*` slugs unbuildable.
/// What: register the factory, build a representative inference-profile slug,
/// assert the adapter's name + capability posture (native tools, no OpenRouter
/// detailed-usage directive, Anthropic-Messages tool dialect).
/// Test: this test.
#[test]
fn bedrock_factory_registers_and_builds() {
    // Bedrock authenticates via the AWS credential chain, so an EMPTY key store
    // still resolves — no `OPENROUTER_API_KEY` fallback is consulted for an
    // explicit `bedrock/` prefix.
    let store = MemoryKeyStore::new();
    let mut cfg = Configurator::new();
    register_bedrock_factory(&mut cfg);

    let adapter = cfg
        .build("bedrock/us.anthropic.claude-sonnet-4-6", &store)
        .expect("bedrock adapter builds");

    assert_eq!(adapter.name(), "bedrock");
    assert_eq!(adapter.capabilities().id, ProviderId::Bedrock);
    assert!(
        adapter.supports_native_tools(),
        "Bedrock-hosted models support native tool use"
    );
    assert!(
        !adapter.wants_detailed_usage(),
        "the OpenRouter usage directive is never sent to Bedrock"
    );
}

/// A `bedrock/*` slug against a configurator that never registered the Bedrock
/// factory alarms with `NoAdapterRegistered`, not a silent fallback.
///
/// Why: pins that Bedrock is opt-in — it is deliberately absent from
/// `register_default_factories`, so a consumer that forgets `register_bedrock_factory`
/// gets an explicit, actionable error rather than a mystery OpenRouter route.
/// What: build a `bedrock/*` slug on an empty configurator; assert the error is
/// `NoAdapterRegistered { provider: Bedrock }` and classifies as an alarm.
/// Test: this test.
#[test]
fn unregistered_bedrock_provider_errors() {
    let store = MemoryKeyStore::new();
    let cfg = Configurator::new(); // nothing registered

    let Err(err) = cfg.build("bedrock/us.anthropic.claude-sonnet-4-6", &store) else {
        panic!("expected NoAdapterRegistered for an unregistered Bedrock provider");
    };
    assert!(err.is_alarm());
    assert!(matches!(
        err,
        InferenceError::NoAdapterRegistered {
            provider: ProviderId::Bedrock
        }
    ));
}
