//! Unit tests for [`super::ManagerInference`] (WI-3/WI-4, #2580/#2581).
//!
//! Why: the inference seam's degrade behaviour and adapter injection must be
//! provable without a live provider key or network — the hermetic bar (#2584).
//! These drive the resolution paths directly with an empty `MemoryKeyStore` and a
//! `ScriptedAdapter`, plus the env-precedence of the documented model config key.
//! What: covers the no-provider degrade, the injected/fixed adapter, and the
//! [`super::MANAGER_MODEL_ENV`] > [`super::FALLBACK_MODEL_ENV`] > default ladder.
//! Test: this file IS the test module.

use std::sync::{Arc, Mutex};

use serial_test::serial;
use trusty_common::credentials::MemoryKeyStore;
use trusty_common::credentials::test_sandbox::CredentialSandbox;
use trusty_common::inference::InferenceAdapter;
use trusty_common::inference::registry::{ProviderId, capabilities};
use trusty_common::inference::test_support::ScriptedAdapter;

use super::{
    DEFAULT_MANAGER_MODEL, FALLBACK_MODEL_ENV, InferenceUnavailable, Inner, MANAGER_MODEL_ENV,
    ManagerInference, Source, resolve_manager_model,
};

/// Enter the credential sandbox with both manager model env keys cleared too,
/// so the resolver sees only the injected store (#9123). Callers are the
/// unkeyed `#[serial]` the sandbox requires.
fn credential_sandbox() -> CredentialSandbox {
    let mut sandbox = CredentialSandbox::enter();
    sandbox.remove(MANAGER_MODEL_ENV);
    sandbox.remove(FALLBACK_MODEL_ENV);
    sandbox
}

/// Build a credentialed seam over an explicit (empty) store, bypassing
/// `default_store` so the test never reads the operator's real credentials.
fn credentialed_over(store: MemoryKeyStore, model: &str) -> ManagerInference {
    let mut configurator = trusty_common::inference::Configurator::new();
    trusty_common::inference::register_default_factories(&mut configurator);
    ManagerInference {
        inner: Mutex::new(Inner {
            model: model.to_string(),
            source: Source::Credentialed {
                configurator,
                store: Box::new(store),
            },
        }),
    }
}

/// Why: an unconfigured provider must degrade to a typed [`InferenceUnavailable`]
/// (never a panic), so the digest handler can fall back deterministically.
/// Test: itself.
#[test]
#[serial]
fn resolve_reports_no_provider_when_unconfigured() {
    let _sandbox = credential_sandbox();
    let seam = credentialed_over(MemoryKeyStore::new(), "openai/gpt-4o-mini");
    // The Ok type carries `Arc<dyn InferenceAdapter>` (not `Debug`), so match the
    // error out rather than using `expect_err`.
    let Err(err) = seam.resolve() else {
        panic!("empty store must not resolve a provider");
    };
    assert!(matches!(err, InferenceUnavailable::NoProvider));
}

/// Why: an injected fixed adapter must be handed back verbatim with its model —
/// the seam the hermetic suite relies on.
/// Test: itself.
#[test]
fn resolve_returns_injected_adapter() {
    let caps = capabilities(ProviderId::OpenRouter);
    let scripted: Arc<dyn InferenceAdapter> = Arc::new(ScriptedAdapter::echo("scripted", caps));
    let seam = ManagerInference::with_adapter(scripted, "test/model");
    let (model, adapter) = seam.resolve().expect("fixed adapter resolves");
    assert_eq!(model, "test/model");
    assert_eq!(adapter.name(), "scripted");
}

/// Why: `set_adapter` must hot-swap a credentialed seam to a fixed one through a
/// shared `&self` (the HTTP-test override path).
/// Test: itself.
#[test]
#[serial]
fn set_adapter_overrides_credentialed_source() {
    let _sandbox = credential_sandbox();
    let seam = credentialed_over(MemoryKeyStore::new(), "openai/gpt-4o-mini");
    assert!(seam.resolve().is_err(), "starts unconfigured");
    let caps = capabilities(ProviderId::OpenRouter);
    let scripted: Arc<dyn InferenceAdapter> = Arc::new(ScriptedAdapter::echo("scripted", caps));
    seam.set_adapter(scripted, "swapped/model");
    let (model, adapter) = seam.resolve().expect("resolves after swap");
    assert_eq!(model, "swapped/model");
    assert_eq!(adapter.name(), "scripted");
}

/// Why: the documented model config key must win over the fleet fallback, which
/// in turn wins over the built-in default.
/// Test: itself.
#[test]
#[serial]
fn manager_model_env_precedence() {
    let mut sandbox = credential_sandbox();
    // Neither set → default.
    assert_eq!(resolve_manager_model(), DEFAULT_MANAGER_MODEL);

    // Fallback only.
    sandbox.set(FALLBACK_MODEL_ENV, "fleet/model");
    assert_eq!(resolve_manager_model(), "fleet/model");

    // Primary overrides fallback.
    sandbox.set(MANAGER_MODEL_ENV, "manager/model");
    assert_eq!(resolve_manager_model(), "manager/model");
}
