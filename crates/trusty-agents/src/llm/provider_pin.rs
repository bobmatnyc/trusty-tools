//! Per-agent inference-provider pinning (#3765).
//!
//! Why: `[agent].provider_id` has been accepted and persisted by
//! `PATCH /api/agents/:name` since #3243, but nothing in the dispatch path
//! ever read it — the loader ignored the key and
//! `llm::credentials::pick_credentials` re-probed the ambient environment on
//! every turn. Setting a provider therefore looked like it worked and silently
//! did nothing, and an agent meant for one provider quietly ran on whatever
//! credential happened to exist. This module turns that inert key into a real,
//! enforced routing decision.
//! What: [`resolve`] validates a declared pin (known provider → dispatchable
//! provider → credential present) and FAILS CLOSED on each step;
//! [`pinned_model_slug`] rewrites the agent's model slug so it carries the
//! pinned provider's routing marker, which is the mechanism
//! [`crate::llm::adapter::adapter_for_model`] already uses to select an
//! adapter. Encoding the pin in the slug (rather than threading a second
//! argument through a dozen call sites) means every place that re-derives an
//! adapter from `cfg.agent.model` agrees with the pin by construction.
//! Test: `tests` below — one case per normalisation rule and one per
//! fail-closed branch.

use trusty_common::inference::registry::{self, ProviderId};

/// Why a declared `[agent].provider_id` could not be honoured.
///
/// Why: #3765's requirement is that a pin whose provider is unusable produces
/// "a clear, actionable error naming the provider and the env var it needs —
/// NOT a silent fallback to whatever ambient credential happens to exist".
/// Each variant names the offending value and what would fix it.
/// What: three fail-closed branches — unknown name, known-but-undispatchable
/// provider, and known-and-dispatchable but no credential. Never a warning:
/// every variant aborts the agent load.
/// Test: `unknown_provider_is_rejected`, `unpinnable_provider_is_rejected`,
/// `missing_credential_fails_closed_and_names_the_env_var`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderPinError {
    /// The string is not any provider in `trusty_common::inference::registry`.
    #[error(
        "agent '{agent}': [agent].provider_id = '{value}' is not a known provider \
         (known: {known})"
    )]
    Unknown {
        agent: String,
        value: String,
        known: String,
    },

    /// A real provider, but this build's chat dispatch cannot reach it — see
    /// [`is_pinnable`].
    #[error(
        "agent '{agent}': [agent].provider_id = '{provider}' is a known provider but \
         trusty-agents cannot dispatch to it yet, so pinning it would fail at the \
         first turn (pinnable: {pinnable})"
    )]
    Unpinnable {
        agent: String,
        provider: &'static str,
        pinnable: String,
    },

    /// Dispatchable, but no credential resolves through any tier.
    #[error(
        "agent '{agent}': [agent].provider_id = '{provider}' is pinned but no \
         credential for it resolves. Set {env_var} in the environment or \
         .env.local, or run `tagent config keys set {provider}`. The pin is \
         honoured strictly — no other provider's credential is substituted."
    )]
    MissingCredential {
        agent: String,
        provider: &'static str,
        env_var: &'static str,
    },
}

/// Providers a pin may name, in the order [`is_pinnable`] documents them.
///
/// Why: pinning an agent to a provider the chat loop cannot reach is a
/// guaranteed runtime failure dressed up as configuration. Rejecting it at
/// load — with the reachable set spelled out — is the fail-closed reading of
/// #3765, and it keeps this list and
/// `crate::api::server::models::reachable_today` (what the GUI offers) from
/// drifting apart: both describe the same property, "chat dispatch resolves a
/// working adapter for this provider".
/// What: the six providers `crate::llm::adapter::adapter_for_model` resolves
/// to a provider-native endpoint. [`ProviderId::OpenAI`] and
/// [`ProviderId::Together`] are deliberately absent — they still fall through
/// to the OpenRouter endpoint carrying a bare, unroutable model id (#2410).
const PINNABLE: [ProviderId; 6] = [
    ProviderId::OpenRouter,
    ProviderId::Anthropic,
    ProviderId::Bedrock,
    ProviderId::Fireworks,
    ProviderId::AtlasCloud,
    ProviderId::Local,
];

/// Whether `id` may be pinned — i.e. whether chat dispatch reaches it today.
///
/// Why: the single predicate both the loader and the `PATCH /api/agents/:name`
/// write gate consult, so the API cannot persist a pin the loader will reject.
/// What: membership in [`PINNABLE`].
/// Test: `pinnable_set_matches_reachable_today`.
pub fn is_pinnable(id: ProviderId) -> bool {
    PINNABLE.contains(&id)
}

/// The `<marker>/` prefix `crate::llm::adapter::adapter_for_model` matches to
/// select `id`'s adapter, or `None` when `id` has no marker.
///
/// Why: [`pinned_model_slug`] needs to know what to prepend, and the answer is
/// not always `id.as_str()` — the local provider's marker is the historical
/// `ollama/` (the only spelling `adapter_for_model` matches), and OpenRouter
/// has no marker at all because it is an AGGREGATOR whose wire model id IS the
/// full `vendor/model` slug (the same exemption
/// [`ProviderId::wire_model_id`] documents).
/// What: `None` for OpenRouter and for every non-pinnable provider; the
/// literal prefix otherwise.
/// Test: `routing_marker_covers_every_pinnable_provider`.
fn routing_marker(id: ProviderId) -> Option<&'static str> {
    match id {
        ProviderId::Anthropic => Some("anthropic"),
        ProviderId::Bedrock => Some("bedrock"),
        ProviderId::Fireworks => Some("fireworks"),
        ProviderId::AtlasCloud => Some("atlascloud"),
        ProviderId::Local => Some("ollama"),
        // Aggregator (routes by the full slug) or not pinnable at all.
        ProviderId::OpenRouter | ProviderId::OpenAI | ProviderId::Together => None,
    }
}

/// Whether a slug marker naming `id` would steer `adapter_for_model` AWAY from
/// the OpenRouter endpoint.
///
/// Why: when a pin replaces a slug's existing marker, only a marker that
/// actually changes routing may be stripped. `anthropic/…` and `openai/…` are
/// ALSO legitimate OpenRouter vendor namespaces (`anthropic/claude-sonnet-4-6`
/// is the correct OpenRouter id), so stripping them when pinning to OpenRouter
/// would corrupt a working slug. `bedrock/…`, `fireworks/…`, `ollama/…` and
/// `atlascloud/…` have no such double meaning — each is a hard routing branch.
/// What: `true` for exactly the four providers with a prefix branch in
/// `adapter_for_model`.
/// Test: `openrouter_pin_strips_a_hard_routing_marker`,
/// `openrouter_pin_keeps_a_vendor_namespace`.
fn has_routing_branch(id: ProviderId) -> bool {
    matches!(
        id,
        ProviderId::Bedrock | ProviderId::Fireworks | ProviderId::AtlasCloud | ProviderId::Local
    )
}

/// Resolve `provider_id` as declared on `agent`, failing closed at every step.
///
/// Why: this is the whole point of #3765. A pin is an operator's explicit
/// statement about where an agent's turns go; honouring it partially — or
/// falling back to an ambient credential when it cannot be honoured — is the
/// defect being fixed.
/// What: `None`/blank yields `Ok(None)` and the caller keeps today's ambient
/// behaviour untouched. Otherwise the value must (1) name a registry provider,
/// (2) be [`is_pinnable`], and (3) have a credential resolving through the
/// shared 3-tier resolver (process env > `.env.local` > secure store). Only the
/// PRESENCE of the credential is probed; the value is dropped immediately and
/// never logged. [`ProviderId::Bedrock`] and [`ProviderId::Local`] have no
/// resolver-managed key (`credential_name()` is `None` — AWS chain and an
/// unauthenticated localhost endpoint respectively), so step 3 is skipped for
/// them; a broken AWS chain still surfaces at the first turn, not here.
/// Test: `absent_pin_is_none`, `blank_pin_is_none`, `unknown_provider_is_rejected`,
/// `unpinnable_provider_is_rejected`,
/// `missing_credential_fails_closed_and_names_the_env_var`,
/// `keyless_providers_resolve_without_a_credential`.
pub fn resolve(
    agent: &str,
    provider_id: Option<&str>,
) -> Result<Option<ProviderId>, ProviderPinError> {
    let Some(raw) = provider_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };

    let Some(caps) = registry::capabilities_for(raw) else {
        return Err(ProviderPinError::Unknown {
            agent: agent.to_string(),
            value: raw.to_string(),
            known: registry::all()
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        });
    };

    if !is_pinnable(caps.id) {
        return Err(ProviderPinError::Unpinnable {
            agent: agent.to_string(),
            provider: caps.id.as_str(),
            pinnable: PINNABLE
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        });
    }

    if let Some(name) = caps.id.credential_name()
        && trusty_common::credentials::resolve_key(name).is_none()
    {
        return Err(ProviderPinError::MissingCredential {
            agent: agent.to_string(),
            provider: caps.id.as_str(),
            env_var: caps.credential_env.unwrap_or("its API key"),
        });
    }

    Ok(Some(caps.id))
}

/// Rewrite `model` so it carries `pin`'s routing marker — the slug form
/// `adapter_for_model` selects an adapter from.
///
/// Why: the pin must WIN over slug inference. Today the model slug implicitly
/// carries provider information (`bedrock/…` beats everything, then substring
/// heuristics on `claude`/`gpt`), and a dozen call sites re-derive the adapter
/// from `cfg.agent.model`. Normalising the slug once, at load, makes the pin
/// the thing every one of those sites reads — instead of leaving the pin and
/// the slug as two competing sources of truth.
/// What: the precedence rule, in order:
///
/// | `model` | `pin` | result |
/// |---|---|---|
/// | `claude-sonnet-4-6` | anthropic | `anthropic/claude-sonnet-4-6` |
/// | `openai/gpt-5.6-sol` | atlascloud | `atlascloud/openai/gpt-5.6-sol` |
/// | `atlascloud/openai/gpt-5.6-sol` | atlascloud | unchanged |
/// | `local/qwen3:8b` | local | `ollama/qwen3:8b` (the marker `adapter_for_model` matches) |
/// | `bedrock/anthropic.claude-3-5` | openrouter | `anthropic.claude-3-5` (hard marker stripped) |
/// | `anthropic/claude-sonnet-4-6` | openrouter | unchanged (vendor namespace, not a routing marker) |
///
/// A marker naming a DIFFERENT provider is removed only when it is a hard
/// routing branch ([`has_routing_branch`]); a vendor namespace that OpenRouter
/// itself routes by survives. The result is idempotent: applying it twice
/// changes nothing.
/// Test: `pin_prefixes_a_bare_slug`, `pin_beats_a_conflicting_slug_prefix`,
/// `pin_is_idempotent`, `local_pin_normalises_to_the_ollama_marker`,
/// `openrouter_pin_strips_a_hard_routing_marker`,
/// `openrouter_pin_keeps_a_vendor_namespace`.
pub fn pinned_model_slug(pin: ProviderId, model: &str) -> String {
    let marker = routing_marker(pin);

    // Already carries THIS provider's exact marker → nothing to do.
    if let Some(m) = marker
        && let Some((head, _)) = model.split_once('/')
        && head.eq_ignore_ascii_case(m)
    {
        return model.to_string();
    }

    // Drop the existing marker only when it would hard-route. The
    // exact-marker case already returned above, so anything reaching here is
    // either a different provider or a different SPELLING of this one
    // (`local/` vs the `ollama/` marker `adapter_for_model` matches) — both
    // must be replaced.
    let rest = match ProviderId::from_slug_prefix(model) {
        Some(other) if has_routing_branch(other) => {
            model.split_once('/').map_or(model, |(_, rest)| rest)
        }
        _ => model,
    };

    match marker {
        Some(m) => format!("{m}/{rest}"),
        None => rest.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Why: an agent with no pin must take the exact path it takes today —
    /// #3765 is explicit that unpinned agents must not regress.
    /// Test: itself.
    #[test]
    fn absent_pin_is_none() {
        assert_eq!(resolve("a", None), Ok(None));
    }

    /// Why: `provider_id = ""` is what a GUI clear-the-field write leaves
    /// behind; it must read as "unpinned", not as an unknown provider.
    /// Test: itself.
    #[test]
    fn blank_pin_is_none() {
        assert_eq!(resolve("a", Some("   ")), Ok(None));
    }

    /// Why: a typo'd provider must be a loud error, never a silent fallback.
    /// Test: itself.
    #[test]
    fn unknown_provider_is_rejected() {
        let err = resolve("izzie", Some("atlas-cloud")).expect_err("must reject");
        assert!(matches!(err, ProviderPinError::Unknown { .. }));
        let msg = err.to_string();
        assert!(
            msg.contains("izzie") && msg.contains("atlas-cloud"),
            "{msg}"
        );
        assert!(msg.contains("atlascloud"), "must list the known set: {msg}");
    }

    /// Why: `openai`/`together` are real registry providers whose dispatch
    /// still falls through to OpenRouter with an unroutable bare slug. Pinning
    /// one would fail at the first turn, so it is refused at load with the
    /// usable set named.
    /// Test: itself.
    #[test]
    fn unpinnable_provider_is_rejected() {
        for provider in ["openai", "together"] {
            let err = resolve("a", Some(provider)).expect_err("must reject");
            assert!(
                matches!(err, ProviderPinError::Unpinnable { .. }),
                "{provider}: {err:?}"
            );
            assert!(err.to_string().contains("atlascloud"), "{err}");
        }
    }

    /// Why: THE core fail-closed guarantee. A pinned provider with no
    /// resolvable credential must name the provider AND the env var that would
    /// fix it, and must never silently borrow another provider's key.
    /// What: sandboxes `$HOME` (so the secure store is a tempdir, never the
    /// real one) and clears every credential env var, mirroring
    /// `llm::credentials`' own store-sandboxing tests.
    /// Test: itself.
    #[test]
    #[serial]
    fn missing_credential_fails_closed_and_names_the_env_var() {
        let _env = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _home = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::test_env::force_env_local_loaded();
        crate::test_env::clear_all_credential_env_vars();

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let prev_home = std::env::var_os("HOME");
        // SAFETY: HOME_LOCK held for the entire test body.
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let err = resolve("izzie", Some("atlascloud")).expect_err("must fail closed");
        let msg = err.to_string();

        // SAFETY: HOME_LOCK still held.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }

        assert!(
            matches!(err, ProviderPinError::MissingCredential { .. }),
            "{err:?}"
        );
        assert!(msg.contains("atlascloud"), "must name the provider: {msg}");
        assert!(
            msg.contains("ATLASCLOUD_API_KEY"),
            "must name the env var: {msg}"
        );
        assert!(
            msg.contains("tagent config keys set"),
            "must name the store tier: {msg}"
        );
    }

    /// Why: Bedrock authenticates via the AWS chain and Local is an
    /// unauthenticated localhost endpoint — neither has a resolver-managed key,
    /// so demanding one would make them unpinnable in practice.
    /// Test: itself.
    #[test]
    #[serial]
    fn keyless_providers_resolve_without_a_credential() {
        let _env = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::test_env::force_env_local_loaded();
        crate::test_env::clear_all_credential_env_vars();
        assert_eq!(resolve("a", Some("bedrock")), Ok(Some(ProviderId::Bedrock)));
        assert_eq!(resolve("a", Some("local")), Ok(Some(ProviderId::Local)));
    }

    /// Why: every pinnable provider must have a defined marker policy —
    /// a `None` that is an oversight rather than the documented aggregator
    /// exemption would silently drop the pin.
    /// Test: itself.
    #[test]
    fn routing_marker_covers_every_pinnable_provider() {
        for id in PINNABLE {
            match id {
                ProviderId::OpenRouter => assert_eq!(routing_marker(id), None),
                other => assert!(routing_marker(other).is_some(), "{other} has no marker"),
            }
        }
    }

    /// Why: the pinnable set and `models::reachable_today` describe the same
    /// property; if they drift, the GUI offers a provider the loader refuses.
    /// Test: itself.
    #[test]
    fn pinnable_set_matches_reachable_today() {
        for caps in registry::all() {
            assert_eq!(
                is_pinnable(caps.id),
                crate::api::server::reachable_today(caps.id),
                "{} disagrees between PINNABLE and reachable_today",
                caps.id
            );
        }
    }

    #[test]
    fn pin_prefixes_a_bare_slug() {
        assert_eq!(
            pinned_model_slug(ProviderId::AtlasCloud, "openai/gpt-5.6-sol"),
            "atlascloud/openai/gpt-5.6-sol"
        );
        assert_eq!(
            pinned_model_slug(ProviderId::Anthropic, "claude-sonnet-4-6"),
            "anthropic/claude-sonnet-4-6"
        );
        assert_eq!(
            pinned_model_slug(ProviderId::Fireworks, "accounts/fireworks/models/x"),
            "fireworks/accounts/fireworks/models/x"
        );
    }

    /// Why: the pin must WIN over what the slug implies — that precedence is
    /// the behavioural contract #3765 asks for.
    /// Test: itself.
    #[test]
    fn pin_beats_a_conflicting_slug_prefix() {
        assert_eq!(
            pinned_model_slug(ProviderId::AtlasCloud, "bedrock/anthropic.claude-3-5"),
            "atlascloud/anthropic.claude-3-5"
        );
        assert_eq!(
            pinned_model_slug(ProviderId::Fireworks, "ollama/qwen3:8b"),
            "fireworks/qwen3:8b"
        );
    }

    #[test]
    fn pin_is_idempotent() {
        for (pin, slug) in [
            (ProviderId::AtlasCloud, "openai/gpt-5.6-sol"),
            (ProviderId::Anthropic, "claude-sonnet-4-6"),
            (ProviderId::OpenRouter, "anthropic/claude-sonnet-4-6"),
            (ProviderId::Local, "qwen3:8b"),
        ] {
            let once = pinned_model_slug(pin, slug);
            assert_eq!(pinned_model_slug(pin, &once), once, "{pin} / {slug}");
        }
    }

    /// Why: `ProviderId::from_slug_prefix` accepts `local/` AND `ollama/`, but
    /// `adapter_for_model` matches only `ollama/` — a `local/` slug would fall
    /// through to `GenericAdapter` (OpenRouter) and silently ignore the pin.
    /// Test: itself.
    #[test]
    fn local_pin_normalises_to_the_ollama_marker() {
        assert_eq!(
            pinned_model_slug(ProviderId::Local, "local/qwen3:8b"),
            "ollama/qwen3:8b"
        );
        assert_eq!(
            pinned_model_slug(ProviderId::Local, "ollama/qwen3:8b"),
            "ollama/qwen3:8b"
        );
    }

    /// Why: pinning to the aggregator must actually pull a hard-routed slug
    /// back to OpenRouter rather than leaving it pointing at Bedrock.
    /// Test: itself.
    #[test]
    fn openrouter_pin_strips_a_hard_routing_marker() {
        assert_eq!(
            pinned_model_slug(ProviderId::OpenRouter, "bedrock/anthropic.claude-3-5"),
            "anthropic.claude-3-5"
        );
        assert_eq!(
            pinned_model_slug(
                ProviderId::OpenRouter,
                "fireworks/accounts/fireworks/models/x"
            ),
            "accounts/fireworks/models/x"
        );
    }

    /// Why: `anthropic/claude-sonnet-4-6` IS OpenRouter's own model id —
    /// stripping the vendor segment would corrupt a working slug.
    /// Test: itself.
    #[test]
    fn openrouter_pin_keeps_a_vendor_namespace() {
        assert_eq!(
            pinned_model_slug(ProviderId::OpenRouter, "anthropic/claude-sonnet-4-6"),
            "anthropic/claude-sonnet-4-6"
        );
        assert_eq!(
            pinned_model_slug(ProviderId::OpenRouter, "openai/gpt-4o-mini"),
            "openai/gpt-4o-mini"
        );
    }
}
