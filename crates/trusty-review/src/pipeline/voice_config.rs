//! Voice configuration resolution for the review pipeline.
//!
//! Why: extracted from `runner.rs` to keep it under the 500-line cap (#610).
//! Centralising voice-config resolution here keeps the prompt builder pure
//! (no I/O, no config reading) and lets tests exercise resolution in isolation.
//!
//! What: `build_voice_config` maps a `ReviewConfig` to a `VoiceConfig` by
//! loading the configured voice package (if any) via `VoiceLoader` and enabling
//! the principles layer per the config flag.
//!
//! Test: `build_voice_config_no_voice`, `build_voice_config_principles_on`,
//! `build_voice_config_principles_off`, `build_voice_config_duetto_bundled`,
//! `build_voice_config_unknown_voice_degrades`.

use crate::{
    config::ReviewConfig,
    voice::{VoiceConfig, VoiceLoader, principles::principles_addendum},
};

/// Build the resolved `VoiceConfig` from `ReviewConfig` for the prompt builder.
///
/// Why: the runner is the single place that knows both the config (which voice
/// package is selected, whether principles are on) and the loader (which
/// discovers and parses voice.toml files).  Centralising resolution here keeps
/// the prompt builder pure (no I/O, no config reading).
/// What: enables/disables the principles layer per `config.voice_principles`;
/// loads the named voice package (if any) via `VoiceLoader`, falling back to
/// the bundled fixture for `"duetto"` or degrading silently to no-voice for
/// unknown packages.  A missing voice package is not fatal — the review proceeds
/// with the stock + principles layers.
/// Test: `build_voice_config_no_voice`, `build_voice_config_duetto_bundled`.
pub fn build_voice_config(config: &ReviewConfig) -> VoiceConfig {
    let principles = if config.voice_principles {
        Some(principles_addendum().to_string())
    } else {
        None
    };

    let (voice_addendum, voice_name) = match config.voice_package.as_deref() {
        None | Some("") => (None, None),
        Some(name) => {
            let loader = VoiceLoader::new();
            match loader.load(name) {
                Ok(pkg) => {
                    let addendum = pkg.effective_addendum();
                    if addendum.is_empty() {
                        tracing::warn!(
                            voice = name,
                            "voice package loaded but effective_addendum is empty"
                        );
                        (None, Some(name.to_string()))
                    } else {
                        (Some(addendum), Some(name.to_string()))
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        voice = name,
                        error = %e,
                        "voice package not found; proceeding without voice layer"
                    );
                    (None, None)
                }
            }
        }
    };

    VoiceConfig {
        principles,
        voice_addendum,
        voice_name,
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: minimal ReviewConfig with all voice defaults (principles ON, no package).
    fn config_default_voice() -> ReviewConfig {
        crate::config::ReviewConfig::from_env_and_file(None, None)
    }

    /// build_voice_config with no voice package and principles ON.
    ///
    /// Why: the default config (no TRUSTY_REVIEW_VOICE_PACKAGE set) must produce
    /// a VoiceConfig with only principles enabled and no voice addendum.
    /// What: asserts voice_addendum is None and principles is Some.
    /// Test: no filesystem writes.
    #[test]
    fn build_voice_config_no_voice() {
        let mut config = config_default_voice();
        config.voice_package = None;
        config.voice_principles = true;
        let vc = build_voice_config(&config);
        assert!(
            vc.principles.is_some(),
            "principles must be enabled by default"
        );
        assert!(
            !vc.principles.as_deref().unwrap_or("").is_empty(),
            "principles must be non-empty"
        );
        assert!(
            vc.voice_addendum.is_none(),
            "no voice package → no voice addendum"
        );
        assert!(vc.voice_name.is_none(), "no voice package → no voice name");
    }

    /// build_voice_config with principles explicitly OFF.
    ///
    /// Why: operators must be able to disable the principles layer
    /// (`TRUSTY_REVIEW_PRINCIPLES=false`).
    /// What: asserts principles is None when `voice_principles = false`.
    /// Test: no filesystem writes.
    #[test]
    fn build_voice_config_principles_off() {
        let mut config = config_default_voice();
        config.voice_package = None;
        config.voice_principles = false;
        let vc = build_voice_config(&config);
        assert!(
            vc.principles.is_none(),
            "principles=false must produce None"
        );
        assert!(
            !vc.has_any_addendum(),
            "no layers → has_any_addendum must be false"
        );
    }

    /// build_voice_config loads the bundled duetto voice.
    ///
    /// Why: the bundled `duetto` fixture must be discoverable without external files.
    /// What: sets voice_package to "duetto"; asserts voice_addendum and voice_name
    /// are Some and contain expected content.
    /// Test: uses bundled fixture; no network.
    #[test]
    fn build_voice_config_duetto_bundled() {
        let mut config = config_default_voice();
        config.voice_package = Some("duetto".to_string());
        config.voice_principles = true;
        let vc = build_voice_config(&config);
        assert!(
            vc.voice_addendum.is_some(),
            "duetto voice must produce a non-None addendum"
        );
        assert!(
            !vc.voice_addendum.as_deref().unwrap_or("").is_empty(),
            "duetto addendum must be non-empty"
        );
        assert_eq!(
            vc.voice_name.as_deref(),
            Some("duetto"),
            "voice_name must be set to \"duetto\""
        );
        assert!(
            vc.has_any_addendum(),
            "duetto + principles must report has_any_addendum=true"
        );
    }

    /// build_voice_config degrades silently for an unknown package.
    ///
    /// Why: a typo in the voice name must not block reviews; the pipeline must
    /// degrade to stock + principles (no panic, no error propagation).
    /// What: sets voice_package to a non-existent name; asserts voice_addendum is
    /// None (graceful fallback) and principles remain active.
    /// Test: no filesystem writes.
    #[test]
    fn build_voice_config_unknown_voice_degrades() {
        let mut config = config_default_voice();
        config.voice_package = Some("nonexistent-voice-xyz".to_string());
        config.voice_principles = true;
        let vc = build_voice_config(&config);
        assert!(
            vc.voice_addendum.is_none(),
            "unknown voice must degrade to None (not panic)"
        );
        assert!(
            vc.voice_name.is_none(),
            "unknown voice must produce None voice_name"
        );
        // Principles still active — degraded, not silent.
        assert!(
            vc.principles.is_some(),
            "principles must remain active even when voice is missing"
        );
    }

    /// Full pipeline: principles + duetto produce a combined addendum with correct order.
    ///
    /// Why: the combined_addendum must have principles before the voice addendum;
    /// this mirrors the intended injection order in the system prompt.
    /// What: loads duetto; asserts principles text precedes duetto content.
    /// Test: uses bundled fixture.
    #[test]
    fn build_voice_config_combined_ordering() {
        let mut config = config_default_voice();
        config.voice_package = Some("duetto".to_string());
        config.voice_principles = true;
        let vc = build_voice_config(&config);
        let combined = vc.combined_addendum();
        // The principles layer mentions "Review principles" heading.
        let p_pos = combined.find("Review principles").unwrap_or(usize::MAX);
        // Duetto mentions correctness / data and control flow.
        let v_pos = combined
            .find("data and control flow")
            .or_else(|| combined.find("correctness first"))
            .unwrap_or(usize::MAX);
        assert!(
            p_pos != usize::MAX,
            "combined must contain principles heading"
        );
        assert!(v_pos != usize::MAX, "combined must contain duetto content");
        assert!(
            p_pos < v_pos,
            "principles must come before voice content in combined addendum"
        );
    }
}
