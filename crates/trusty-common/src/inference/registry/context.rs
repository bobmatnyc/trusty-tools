//! Per-model context-window resolution (issue #2402; closes #2330).
//!
//! Why: compaction/budget code must scale with the model actually in use. A flat
//! default is model-blind — the #2330 bug was `claude-haiku-*` slugs falling
//! through to the 128K default instead of Haiku's real 200K window, so Haiku
//! runs compacted far too aggressively. This resolver turns a slug into its real
//! window, checking model-family substrings first and falling back to the
//! provider's declared max.
//! What: [`context_window`] maps a slug → tokens; [`DEFAULT_CONTEXT_WINDOW`] is
//! the conservative floor for a slug no substring rule and no provider default
//! covers.
//! Test: inline `tests` — `haiku_resolves_to_200k` (#2330 regression guard),
//! `sonnet_and_opus_200k`, `gpt4o_128k`, `falls_back_to_provider_default`.

use super::ProviderCapabilities;

/// Conservative fallback context window (tokens) when neither a model-family
/// substring rule nor a provider default applies.
///
/// Why: [`context_window`] must always return a usable number even for a typo'd
/// or brand-new slug with no capabilities context; 128K is a widely-supported
/// floor.
/// What: the final fallback tier of [`context_window`].
/// Test: `falls_back_to_provider_default` (via the `None` caps path).
pub const DEFAULT_CONTEXT_WINDOW: usize = 128_000;

/// Resolve the real context-window size (in tokens) for a model slug.
///
/// Why: a slug's window is a property of the MODEL, not the provider default —
/// e.g. Bedrock's default is 200K but a Haiku model served through it is still
/// 200K while a hypothetical smaller model would differ. Substring rules capture
/// the model families the ecosystem actually uses; `caps` supplies the
/// provider's max as the next tier so an unlisted model on a known provider
/// still gets that provider's window rather than the global floor.
/// What: checked in order — (1) `claude-haiku*` / `haiku-3` / `haiku-4` → 200_000
/// (the #2330 fix); (2) `claude-sonnet*` / `sonnet-4` / `claude-opus*` /
/// `opus-4` → 200_000; (3) `gpt-4o` → 128_000; (4) `caps.max_context_window`
/// when `caps` is `Some`; else (5) [`DEFAULT_CONTEXT_WINDOW`]. Matching is
/// case-insensitive and covers both bare Anthropic slugs and Bedrock-routed
/// `bedrock/us.anthropic.claude-*` inference-profile forms.
/// Test: `haiku_resolves_to_200k`, `sonnet_and_opus_200k`, `gpt4o_128k`,
/// `falls_back_to_provider_default`.
pub fn context_window(model: &str, caps: Option<&ProviderCapabilities>) -> usize {
    let m = model.to_ascii_lowercase();

    // (1) #2330: Haiku's real window is 200K, not the 128K default.
    if m.contains("claude-haiku") || m.contains("haiku-3") || m.contains("haiku-4") {
        return 200_000;
    }
    // (2) Sonnet / Opus — 200K (bare + Bedrock-routed forms).
    if m.contains("claude-sonnet")
        || m.contains("sonnet-4")
        || m.contains("claude-opus")
        || m.contains("opus-4")
    {
        return 200_000;
    }
    // (3) GPT-4o family — 128K.
    if m.contains("gpt-4o") {
        return 128_000;
    }
    // (4) Provider default, when known; (5) else the global floor.
    caps.map(|c| c.max_context_window)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::{ProviderId, capabilities};
    use super::*;

    /// Why: #2330 regression guard — Haiku slugs must resolve to 200K, not the
    /// 128K default. Covers the bare, versioned, and Bedrock-routed forms.
    /// Test: itself.
    #[test]
    fn haiku_resolves_to_200k() {
        assert_eq!(context_window("anthropic/claude-haiku-4-5", None), 200_000);
        assert_eq!(context_window("claude-haiku-3-5", None), 200_000);
        assert_eq!(
            context_window("bedrock/us.anthropic.claude-haiku-4-5", None),
            200_000
        );
        // The exact regression: WITHOUT the #2330 rule this returned 128_000.
        assert_ne!(
            context_window("claude-haiku-4-5", None),
            DEFAULT_CONTEXT_WINDOW
        );
    }

    /// Why: Sonnet and Opus share Haiku's 200K window.
    /// Test: itself.
    #[test]
    fn sonnet_and_opus_200k() {
        assert_eq!(
            context_window("bedrock/us.anthropic.claude-sonnet-4-6", None),
            200_000
        );
        assert_eq!(context_window("anthropic/claude-opus-4-1", None), 200_000);
    }

    /// Why: the GPT-4o family is a 128K window.
    /// Test: itself.
    #[test]
    fn gpt4o_128k() {
        assert_eq!(context_window("openai/gpt-4o-mini", None), 128_000);
    }

    /// Why: an unlisted model on a known provider gets that provider's max;
    /// with no caps it gets the global floor.
    /// Test: itself.
    #[test]
    fn falls_back_to_provider_default() {
        // Fireworks default is 128K in the seed table.
        let fw = capabilities(ProviderId::Fireworks);
        assert_eq!(
            context_window("fireworks/some-new-model", Some(fw)),
            128_000
        );
        // No caps → global floor.
        assert_eq!(
            context_window("some/unheard-of-model", None),
            DEFAULT_CONTEXT_WINDOW
        );
    }
}
