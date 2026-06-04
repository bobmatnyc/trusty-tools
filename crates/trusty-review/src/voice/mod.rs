//! Voice package system for trusty-review (#754).
//!
//! Why: review personalisation via named, versioned, shareable voice packages
//! synthesised from standout human reviewers.  Each package captures HOW a team
//! reviews; the universal principles layer (#756) captures WHAT the field
//! considers effective code review.  Together they compose the 3-layer pipeline:
//!   stock base prompt → principles → voice addendum.
//!
//! What: re-exports `VoicePackage` and related types (from `types`), the
//! universal best-practices layer (from `principles`), and the voice loader
//! (`loader`).  The `EffectiveVoice` struct is the resolved, ready-to-inject
//! form used by `pipeline/prompt.rs`.
//!
//! Test: unit tests live in `voice/tests.rs` covering types, principles,
//! loader, and layering.

pub mod loader;
pub mod principles;
pub mod types;

pub use loader::{VoiceLoader, VoiceLoaderError};
pub use principles::principles_addendum;
pub use types::{VoiceKnobs, VoiceMeta, VoicePackage, VoiceProvenance, VoiceSection};

/// Configuration for the voice layer passed into the prompt builder.
///
/// Why: the prompt builder needs to know (a) which principles/voice layers are
/// enabled and (b) the resolved addendum strings, without carrying filesystem
/// I/O dependencies into the prompt module.  This struct is cheaply cloneable
/// and holds only the resolved text.
/// What: holds optional principles and voice addendum strings; when absent the
/// corresponding layer is omitted and the prompt is stock-only.
/// Test: `voice_config_none_gives_stock_only` in voice/tests.rs.
#[derive(Debug, Default, Clone)]
pub struct VoiceConfig {
    /// Resolved universal best-practices addendum (from `principles`).
    /// `None` = principles layer disabled (default-on in production via `VoiceConfig::default_production`).
    pub principles: Option<String>,
    /// Resolved voice package addendum (base + custom overlay merged).
    /// `None` = no voice package selected (opt-in).
    pub voice_addendum: Option<String>,
    /// Name of the loaded voice package for diagnostics / health reporting.
    pub voice_name: Option<String>,
}

impl VoiceConfig {
    /// Construct a `VoiceConfig` with principles ON by default and no voice.
    ///
    /// Why: issue #756 specifies the principles layer is default-on (universal /
    /// safe); voice is always opt-in.  This constructor reflects that default.
    /// What: enables the principles addendum; leaves `voice_addendum` as `None`.
    /// Test: `voice_config_production_default_enables_principles`.
    pub fn default_production() -> Self {
        Self {
            principles: Some(principles_addendum().to_string()),
            voice_addendum: None,
            voice_name: None,
        }
    }

    /// Construct a stock-only `VoiceConfig` with no layers (testing / legacy).
    ///
    /// Why: tests that only need the stock base prompt can obtain a zero-layer
    /// config without importing the principles text.
    /// What: returns `VoiceConfig::default()` (all `None`).
    /// Test: `voice_config_none_gives_stock_only`.
    pub fn stock_only() -> Self {
        Self::default()
    }

    /// True when at least one addendum layer is active.
    ///
    /// Why: the prompt builder uses this to decide whether to append a separator
    /// before the addendum section.
    /// What: returns `true` if either `principles` or `voice_addendum` is `Some`
    /// and non-empty.
    /// Test: `voice_config_has_addendum_helpers`.
    pub fn has_any_addendum(&self) -> bool {
        self.principles
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
            || self
                .voice_addendum
                .as_deref()
                .map(|s| !s.is_empty())
                .unwrap_or(false)
    }

    /// Render the combined addendum text in layer order: principles → voice.
    ///
    /// Why: the prompt builder calls this once to get the final injectable text.
    /// What: concatenates non-empty layers separated by a double newline; returns
    /// an empty string when no layers are active.
    /// Test: `voice_config_combined_addendum_ordering`.
    pub fn combined_addendum(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if let Some(p) = self.principles.as_deref()
            && !p.is_empty()
        {
            parts.push(p);
        }
        if let Some(v) = self.voice_addendum.as_deref()
            && !v.is_empty()
        {
            parts.push(v);
        }
        parts.join("\n\n")
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests_voice_config.rs"]
mod tests_voice_config;

#[cfg(test)]
#[path = "tests_integration.rs"]
mod tests_integration;
