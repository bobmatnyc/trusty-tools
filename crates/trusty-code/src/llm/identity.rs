//! Boilerplate for the identity methods of [`trusty_common::inference::InferenceAdapter`]
//! (#4425).
//!
//! Why: the shared trait requires `name()` and `capabilities()` from every
//! implementor, because a real adapter serves exactly one provider and its
//! capability-derived defaults (`supports_native_tools`,
//! `supports_prompt_caching`, `wants_detailed_usage`, `context_window`) all
//! read from them. trusty-code has ~20 implementors that are NOT real
//! providers: scripted offline mocks and transparent decorators. Hand-writing
//! near-identical methods (plus their Why/What/Test docs) twenty times
//! would bury the behaviour that actually differs between those doubles in
//! boilerplate. These two macros express the two honest answers once.
//! What: [`mock_adapter_identity`] for a test double — a caller-supplied label
//! plus the OpenRouter capability profile, which is what trusty-code's default
//! model slug (`provider::routing::DEFAULT_MODEL`, an OpenRouter slug) resolves
//! to, so a mock behaves like the backend it stands in for.
//! [`delegating_adapter_identity`] for a decorator wrapping another adapter —
//! it forwards to the inner adapter, because a decorator must never
//! misrepresent the backend it is recording.
//! Test: exercised by every implementor that uses them; the capability answer
//! is asserted in `crate::llm::dispatch::tests::capabilities_report_openrouter_default`.

/// Implement `name`/`capabilities` for a non-provider test double.
///
/// Why: a scripted mock has no real backend, but must still answer the trait.
/// Naming it explicitly (rather than claiming to be `"openrouter"`) keeps a log
/// line honest about the fact that no network call happened.
/// What: expands to the two trait methods — `name()` returns `$label`,
/// `capabilities()` returns the OpenRouter profile (see the module doc).
/// Test: the mocks' own behavioural tests exercise these implementors.
macro_rules! mock_adapter_identity {
    ($label:expr) => {
        fn name(&self) -> &str {
            $label
        }

        fn capabilities(&self) -> &::trusty_common::inference::ProviderCapabilities {
            ::trusty_common::inference::capabilities(
                ::trusty_common::inference::ProviderId::OpenRouter,
            )
        }
    };
}

/// Implement `name`/`capabilities` by forwarding to a wrapped adapter.
///
/// Why: a decorator (transcript recorder, wire-dump capture) is transparent by
/// construction — reporting its own identity would make every telemetry line
/// attribute the turn to the decorator instead of the real backend, and would
/// silently change the capability-derived defaults the caller reads.
/// What: expands to the three trait methods, each delegating to `self.$inner`
/// — including the model-aware `capabilities_for` (#4425), without which a
/// decorator wrapping a per-request ROUTING adapter would fall back to the
/// trait default (`capabilities()`) and report the routing default's profile
/// for every slug, silently undoing the routing the wrapped adapter performs.
/// Test: `crate::run_task::recorder` and `crate::llm::debug_capture` tests;
/// `crate::llm::debug_capture::tests::decorator_forwards_capabilities_for`.
macro_rules! delegating_adapter_identity {
    ($inner:ident) => {
        fn name(&self) -> &str {
            self.$inner.name()
        }

        fn capabilities(&self) -> &::trusty_common::inference::ProviderCapabilities {
            self.$inner.capabilities()
        }

        fn capabilities_for(
            &self,
            model: &str,
        ) -> &::trusty_common::inference::ProviderCapabilities {
            self.$inner.capabilities_for(model)
        }
    };
}

pub(crate) use {delegating_adapter_identity, mock_adapter_identity};
