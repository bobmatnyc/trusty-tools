//! [`Configurator`] — the provider-name → adapter construction seam.
//!
//! Why: the two-stage [`provider_for`] resolver decides WHICH provider and
//! credential to use, but something must turn that decision into a live
//! `Box<dyn InferenceAdapter>`. Concrete adapters do not exist in this
//! foundation ticket (they land in #2403/#2407), so the construction step is a
//! registration seam: real adapters register a factory per provider, and the
//! configurator wires resolution → factory → adapter. This ticket exercises the
//! seam end-to-end against the test-only `ScriptedAdapter`.
//! What: [`AdapterFactory`] (a per-provider builder, with a blanket impl for
//! plain closures), [`Configurator`] (a registry of factories plus
//! [`Configurator::build`], which resolves a slug then invokes the matching
//! factory), and the re-exported [`provider_for`]/[`ResolvedProvider`].
//! Test: inline `tests` + the full resolve→build→chat cycle in
//! `crates/trusty-common/tests/inference_foundation.rs`.

mod resolver;

pub use resolver::{ResolvedProvider, provider_for};

use std::collections::HashMap;

use crate::inference::adapter::InferenceAdapter;
use crate::inference::credentials::KeyStore;
use crate::inference::error::InferenceError;
use crate::inference::registry::ProviderId;

/// Builds a concrete [`InferenceAdapter`] from a [`ResolvedProvider`].
///
/// Why: decouples the configurator from any concrete adapter — #2403 registers
/// real HTTP factories, tests register a scripted-adapter factory, and neither
/// changes the configurator. `Send + Sync` because a `Configurator` may be
/// shared across `tokio` tasks.
/// What: one method mapping a resolution outcome to a boxed adapter (or an
/// [`InferenceError`] if construction fails, e.g. an unsupported model).
/// Test: the blanket closure impl is used by the configurator tests.
pub trait AdapterFactory: Send + Sync {
    /// Construct an adapter for the resolved provider/credential.
    ///
    /// Why: the single construction hook the configurator calls.
    /// What: returns a boxed [`InferenceAdapter`] or an [`InferenceError`].
    /// Test: `build_uses_registered_factory`.
    fn build(
        &self,
        resolved: &ResolvedProvider,
    ) -> Result<Box<dyn InferenceAdapter>, InferenceError>;
}

impl<F> AdapterFactory for F
where
    F: Fn(&ResolvedProvider) -> Result<Box<dyn InferenceAdapter>, InferenceError> + Send + Sync,
{
    /// Blanket impl so a plain closure is a factory.
    ///
    /// Why: registering an adapter should be one closure, not a bespoke unit
    /// struct + impl per provider.
    /// What: forwards to the closure.
    /// Test: `build_uses_registered_factory` registers a closure.
    fn build(
        &self,
        resolved: &ResolvedProvider,
    ) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
        self(resolved)
    }
}

/// Registry of per-provider adapter factories.
///
/// Why: the construction seam that lets adapters be wired in independently of
/// resolution. Empty by default — a build attempt against an unregistered
/// provider is an explicit [`InferenceError::NoAdapterRegistered`], never a
/// silent fallback.
/// What: a `ProviderId → AdapterFactory` map with [`Configurator::register`] and
/// [`Configurator::build`] (resolve via [`provider_for`], then dispatch to the
/// factory).
/// Test: `build_uses_registered_factory`, `build_unregistered_provider_errors`.
#[derive(Default)]
pub struct Configurator {
    factories: HashMap<ProviderId, Box<dyn AdapterFactory>>,
}

impl Configurator {
    /// Create an empty configurator with no factories registered.
    ///
    /// Why: consumers start empty and register exactly the providers they wire
    /// in — no implicit adapters.
    /// What: an empty factory map.
    /// Test: `build_unregistered_provider_errors`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a factory for a provider (replacing any existing one).
    ///
    /// Why: #2403's real adapters and tests both register through this one door.
    /// What: inserts `factory` under `id`; a later registration for the same id
    /// replaces the earlier one.
    /// Test: `build_uses_registered_factory`.
    pub fn register(&mut self, id: ProviderId, factory: Box<dyn AdapterFactory>) {
        self.factories.insert(id, factory);
    }

    /// Resolve `slug` and build the matching adapter.
    ///
    /// Why: the end-to-end entry point — one call turns a model slug + credential
    /// store into a live adapter.
    /// What: runs [`provider_for`] (two-stage resolution against `store`), looks
    /// up the resolved provider's factory, and invokes it. Returns
    /// [`InferenceError::MissingCredential`] when resolution fails, or
    /// [`InferenceError::NoAdapterRegistered`] when no factory is registered for
    /// the resolved provider.
    /// Test: `build_uses_registered_factory`, `build_unregistered_provider_errors`.
    pub fn build(
        &self,
        slug: &str,
        store: &dyn KeyStore,
    ) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
        let resolved = provider_for(slug, store)?;
        let factory = self.factories.get(&resolved.provider()).ok_or(
            InferenceError::NoAdapterRegistered {
                provider: resolved.provider(),
            },
        )?;
        factory.build(&resolved)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::credentials::MemoryKeyStore;
    use crate::inference::registry::capabilities;
    use crate::inference::test_support::ScriptedAdapter;
    use serial_test::serial;

    fn clear_env() {
        for var in ["OPENROUTER_API_KEY", "ANTHROPIC_API_KEY"] {
            // SAFETY: guarded by `#[serial(dotenv_credential_env)]`.
            unsafe { std::env::remove_var(var) };
        }
    }

    /// Why: a registered factory must be invoked with the resolution outcome and
    /// produce a working adapter.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn build_uses_registered_factory() {
        clear_env();
        let store = MemoryKeyStore::new();
        store.set("openrouter", "sk-or-abc").unwrap(); // pragma: allowlist secret

        let mut cfg = Configurator::new();
        cfg.register(
            ProviderId::OpenRouter,
            Box::new(|resolved: &ResolvedProvider| {
                let caps = capabilities(resolved.provider());
                Ok(Box::new(ScriptedAdapter::echo("openrouter", caps))
                    as Box<dyn InferenceAdapter>)
            }),
        );

        let adapter = cfg.build("some/model", &store).expect("built");
        assert_eq!(adapter.name(), "openrouter");
    }

    /// Why: resolving to a provider with no registered factory must be an
    /// explicit alarm error, never a silent fallback.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn build_unregistered_provider_errors() {
        clear_env();
        let store = MemoryKeyStore::new();
        store.set("openrouter", "sk-or-abc").unwrap(); // pragma: allowlist secret

        let cfg = Configurator::new(); // nothing registered
        // `Box<dyn InferenceAdapter>` is not `Debug`, so `expect_err` won't
        // compile; match the error out instead.
        let Err(err) = cfg.build("some/model", &store) else {
            panic!("expected NoAdapterRegistered");
        };
        assert!(err.is_alarm());
        assert!(matches!(
            err,
            InferenceError::NoAdapterRegistered {
                provider: ProviderId::OpenRouter
            }
        ));
    }
}
