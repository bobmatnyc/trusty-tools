//! Voice package type definitions.
//!
//! Why: a voice package is the primary unit of review personalisation — a
//! named, versioned, shareable bundle that layers reviewer tone and emphasis on
//! top of the stock base prompt and the universal principles layer.
//! What: defines `VoicePackage` (the on-disk TOML shape) and `EffectiveVoice`
//! (the resolved addendum string after merging base + custom overlay).
//! Test: `voice_package_roundtrip_serde` in voice/tests.rs.

use serde::{Deserialize, Serialize};

// ─── On-disk TOML shape ───────────────────────────────────────────────────────

/// Provenance metadata for a generated voice package.
///
/// Why: auditable — reviewers and operators can inspect which corpus and model
/// produced the package without opening the synthesis log.
/// What: all fields are optional so hand-crafted packages need not fill them.
/// Test: covered by serde round-trip in voice/tests.rs.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct VoiceProvenance {
    /// Human-readable list of reviewer names whose corpus contributed.
    #[serde(default)]
    pub source_reviewers: Vec<String>,
    /// Number of reviewers in the corpus (may differ from `source_reviewers.len()`
    /// if some are anonymised after synthesis).
    #[serde(default)]
    pub reviewers_count: u32,
    /// Total comment records analysed.
    #[serde(default)]
    pub total_comments_analyzed: u32,
    /// Free-form notes (corpus caveats, weighting rationale).
    #[serde(default)]
    pub notes: String,
}

/// Top-level metadata block (`[meta]` in voice.toml).
///
/// Why: captures name, version, generation provenance, and the corpus cutoff so
/// the package can be re-synthesised from a later corpus without confusion.
/// What: required field is `name`; all others are optional with defaults.
/// Test: covered by serde round-trip in voice/tests.rs.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct VoiceMeta {
    /// Package name (e.g. `"duetto"`).
    pub name: String,
    /// SemVer-ish version string (e.g. `"0.1.0"`).
    #[serde(default)]
    pub version: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// ISO-8601 date the package was generated (e.g. `"2026-06-04"`).
    #[serde(default)]
    pub generated_at: String,
    /// trusty-review voice pipeline version that produced this package.
    #[serde(default)]
    pub pipeline_version: String,
    /// ISO-8601 date before which reviews were included in the corpus.
    #[serde(default)]
    pub corpus_cutoff: String,
    /// Model used for the synthesis run.
    #[serde(default)]
    pub llm_model: String,
    /// Corpus provenance details.
    #[serde(default)]
    pub provenance: VoiceProvenance,
}

/// Tuning knobs for the voice package (`[knobs]` in voice.toml).
///
/// Why: lets package authors express high-level calibration intent that overlay
/// tooling or future pipeline stages can interpret, without baking it into the
/// free-form `system_addendum`.
/// What: all fields are optional strings; unrecognised values are ignored
/// (forward-compatible).
/// Test: covered by serde round-trip in voice/tests.rs.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct VoiceKnobs {
    /// Reviewer tone style.  Values: `"collaborative-direct"`, `"formal"`, etc.
    #[serde(default)]
    pub tone: String,
    /// Block bias.  Values: `"conservative"` (default), `"moderate"`, `"strict"`.
    #[serde(default)]
    pub block_bias: String,
    /// Preferred comment length.  Values: `"short"`, `"medium"`, `"verbose"`.
    #[serde(default)]
    pub comment_length: String,
}

/// The generated voice section (`[voice]` in voice.toml).
///
/// Why: the `system_addendum` is the primary injectable prompt component; it is
/// appended verbatim to the stock system prompt after the principles layer.
/// What: a single `system_addendum` string.  An optional `extra_addendum` from
/// the `[custom]` overlay extends it without overwriting.
/// Test: covered by layering tests in voice/tests.rs.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct VoiceSection {
    /// The main injectable prompt component synthesised from the corpus.
    #[serde(default)]
    pub system_addendum: String,
}

/// The user-customisation overlay (`[custom]` in voice.toml).
///
/// Why: user hand-edits must survive re-generation of the base `[voice]`/`[knobs]`
/// sections; the `[custom]` section is never touched by the synthesis step.
/// What: optional `voice` sub-table that extends the base addendum.
/// Test: covered by layering tests in voice/tests.rs.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct CustomOverlay {
    /// Extra addendum appended after `[voice].system_addendum`.  Used to augment
    /// the generated voice without a full re-synthesis.
    #[serde(default)]
    pub extra_addendum: String,
    /// Optional full replacement voice sub-table (`[custom.voice]`).
    #[serde(default)]
    pub voice: Option<CustomVoiceOverride>,
}

/// Inner `[custom.voice]` section for advanced customisation.
///
/// Why: lets users replace just the voice addendum without touching meta/knobs.
/// What: mirrors `VoiceSection` but all fields optional.
/// Test: covered by layering tests in voice/tests.rs.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct CustomVoiceOverride {
    /// If set, replaces `[voice].system_addendum` entirely.
    #[serde(default)]
    pub system_addendum: String,
}

/// The full on-disk voice package as parsed from `voice.toml`.
///
/// Why: the top-level type that `toml::from_str` deserialises; it mirrors the
/// exact TOML shape so users can hand-author packages without a synthesis run.
/// What: four TOML tables: `[meta]`, `[voice]`, `[knobs]`, `[custom]`.  All
/// are optional (with defaults) except that `[meta].name` is recommended.
/// Test: `voice_package_roundtrip_serde` in voice/tests.rs.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct VoicePackage {
    /// Identity and provenance metadata.
    #[serde(default)]
    pub meta: VoiceMeta,
    /// The generated reviewer voice component.
    #[serde(default)]
    pub voice: VoiceSection,
    /// High-level calibration knobs.
    #[serde(default)]
    pub knobs: VoiceKnobs,
    /// User-managed customisation overlay.
    #[serde(default)]
    pub custom: CustomOverlay,
}

impl VoicePackage {
    /// Compute the effective system addendum after merging base + custom overlay.
    ///
    /// Why: the `[custom]` section is user-owned and must be merged at read-time
    /// so the caller always sees a single, coherent addendum string.
    /// What: if `[custom.voice].system_addendum` is non-empty it REPLACES the
    /// generated `[voice].system_addendum`; if `[custom].extra_addendum` is
    /// non-empty it is APPENDED after the effective base.
    /// Test: `effective_addendum_base_only`, `effective_addendum_custom_append`,
    /// `effective_addendum_custom_replace` in voice/tests.rs.
    pub fn effective_addendum(&self) -> String {
        // Step 1: resolve the base — custom.voice.system_addendum overrides if set.
        let base = if let Some(ref cv) = self.custom.voice {
            if !cv.system_addendum.is_empty() {
                cv.system_addendum.clone()
            } else {
                self.voice.system_addendum.clone()
            }
        } else {
            self.voice.system_addendum.clone()
        };

        // Step 2: append extra_addendum from the custom overlay if present.
        if !self.custom.extra_addendum.is_empty() {
            format!("{}\n\n{}", base, self.custom.extra_addendum)
        } else {
            base
        }
    }
}
