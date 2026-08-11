//! Operator-supplied agentic markers, read from a file at runtime (#5414).
//!
//! Why: the marker set that decides `commits.agentic_mode` shipped as a fixed
//! in-crate list, so a target org whose commits carry a house footer nobody
//! anticipated reports a low agentic share and reads as a repository with no
//! AI in it. Adding that footer meant editing `BUILTIN` and cutting a release.
//! Carrying the patterns as a field on [`crate::core::config::Config`] would
//! have worked and was rejected: `Config` has twenty public fields, no private
//! field and derives `Default`, so it is exhaustively constructible by struct
//! literal from outside the crate and any change to its field set is a major
//! break (`constructible_struct_adds_field`). A new public type is additive.
//! What: [`MarkerConfig`] deserializes a small YAML document — a list of
//! `tool` / `mode` / `scope` / `pattern` entries, at most [`MAX_MARKERS`] of
//! them — from the path [`marker_file_path`] resolves.
//! [`crate::collect::ai_markers`] appends the result to `BUILTIN` and compiles
//! it once per process, and consults it only when the shipped markers return
//! no verdict; nothing here touches `Config`, the CLI, or any existing
//! signature.
//! Test: [`tests`] below, plus `tests/ai_markers_operator_file.rs` and
//! `tests/ai_markers_bad_file.rs`, which drive the whole path through
//! `ai_markers::detect` in their own processes.
//!
//! ```yaml
//! # ~/.config/tga/ai-markers.yaml
//! markers:
//!   - tool: acme-bot
//!     mode: full_agentic       # or ide_assisted
//!     scope: message           # or trailer, email
//!     pattern: '(?i)Generated\s+with\s+Acme\s+Bot'
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::collect::ai_attribution::AgenticMode;
use crate::core::config::expand_path;

/// Environment variable naming the operator marker file.
///
/// Overrides [`DEFAULT_MARKER_FILE`]; a `~` prefix is expanded.
pub const ENV_AI_MARKERS: &str = "TGA_AI_MARKERS";

/// Marker-file path used when [`ENV_AI_MARKERS`] is unset.
pub const DEFAULT_MARKER_FILE: &str = "~/.config/tga/ai-markers.yaml";

/// Most markers one file may declare.
///
/// `detect` runs once per commit and scans the operator markers linearly, so
/// an unbounded file slows a whole-history walk in proportion to its size.
/// 256 is far above any plausible house-marker list and far below where the
/// linear scan is noticeable; a file over it is rejected like any other bad
/// file rather than silently truncated.
pub const MAX_MARKERS: usize = 256;

/// Which text a marker's pattern is applied to.
///
/// Why: the three marker families — trailers, body footers, and
/// agent-identifying emails — need different haystacks. Matching a trailer
/// pattern against the whole message would let a quoted mention in a commit
/// body count as a co-author.
/// What: `Trailer` runs against each `Co-Authored-By:` value in isolation,
/// `Message` against the raw commit message (use `(?m)^` to anchor a trailer
/// with a different key, e.g. `X-AI-Model:`), and `Email` against the author
/// and committer addresses.
/// Test: `crate::collect::ai_markers::tests::trailer_scope_does_not_match_body_prose`,
/// and [`tests::yaml_scopes_deserialize`] for the YAML spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MarkerScope {
    /// Each `Co-Authored-By:` value, in isolation.
    Trailer,
    /// The whole commit message, subject and body.
    Message,
    /// The author and committer email addresses.
    Email,
}

/// The classification an operator marker asserts when it matches.
///
/// Why: [`AgenticMode`] carries a `None` variant that means "nothing matched",
/// which a marker can never assert — a marker that classifies a commit as
/// unmarked is a contradiction. This enum is the two-valued subset a config
/// file may name, so an impossible entry cannot be written in the first place.
/// What: deserializes from `full_agentic` / `ide_assisted`, the same strings
/// [`AgenticMode::as_str`] persists.
/// Test: [`tests::marker_mode_maps_to_agentic_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MarkerMode {
    /// Autonomous CLI agent.
    FullAgentic,
    /// Inline AI completion from an IDE plugin.
    IdeAssisted,
}

impl MarkerMode {
    /// The [`AgenticMode`] this marker asserts.
    pub fn as_agentic_mode(self) -> AgenticMode {
        match self {
            MarkerMode::FullAgentic => AgenticMode::FullAgentic,
            MarkerMode::IdeAssisted => AgenticMode::IdeAssisted,
        }
    }
}

/// One operator-supplied marker.
///
/// Why: this is the unit an operator adds to catch a marker the shipped set
/// does not know, and it mirrors the fields of a `BUILTIN` entry exactly so
/// the two sets stay one concept with one matcher.
/// What: `tool` is the label written to `commits.ai_tool`; `pattern` is a
/// `regex` crate expression applied to whichever text `scope` names. Patterns
/// are matched unanchored — write `(?i)` for case-insensitivity and `\b` or an
/// address fragment to keep a human co-author from tripping the marker.
/// Test: [`tests::spec_round_trips_from_yaml`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct MarkerSpec {
    /// Label written to `commits.ai_tool` when this marker wins.
    pub tool: String,
    /// Classification this marker asserts.
    pub mode: MarkerMode,
    /// Which text `pattern` is applied to.
    pub scope: MarkerScope,
    /// `regex` crate expression.
    pub pattern: String,
}

impl MarkerSpec {
    /// Build a spec in code, for tests and for callers embedding tga.
    pub fn new(
        tool: impl Into<String>,
        mode: MarkerMode,
        scope: MarkerScope,
        pattern: impl Into<String>,
    ) -> Self {
        Self {
            tool: tool.into(),
            mode,
            scope,
            pattern: pattern.into(),
        }
    }
}

/// The operator marker file, parsed.
///
/// Why: #5414's requirement is that a new marker is addable "without a code
/// change and without a release", so the marker set has to be data on disk.
/// This type is the whole of that data — deliberately a NEW public type rather
/// than a field on [`crate::core::config::Config`], which is the shape that
/// keeps tga on 2.x (see the module note).
/// What: a single `markers:` list. Unknown keys are rejected rather than
/// ignored, at either level: a typo'd `patern:` that silently produced a
/// marker-less config would be indistinguishable from an operator who
/// configured nothing, which is the failure this file exists to prevent.
/// Test: [`tests::unknown_key_is_rejected`], [`tests::empty_document_is_empty`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct MarkerConfig {
    /// Operator markers, in the order the file lists them.
    #[serde(default)]
    pub markers: Vec<MarkerSpec>,
}

/// Why a marker file could not be used.
///
/// Every variant is fail-open at the call site: [`crate::collect::ai_markers`]
/// logs the error, records it in `detection_disclosure()`, and runs the
/// builtin set. A collect run never aborts because a marker file is bad.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MarkerConfigError {
    /// The file exists but could not be read.
    #[error("cannot read the marker file: {0}")]
    Read(#[from] std::io::Error),
    /// The file is not a valid marker document.
    #[error("not a valid marker document: {0}")]
    Parse(#[from] serde_yaml::Error),
    /// A `pattern:` is not a valid `regex` crate expression.
    #[error("marker {index} (tool `{tool}`) has an invalid pattern `{pattern}`: {source}")]
    Pattern {
        /// Zero-based position in the `markers:` list.
        index: usize,
        /// The offending entry's `tool:` label.
        tool: String,
        /// The offending expression.
        pattern: String,
        /// The `regex` crate's own diagnosis.
        #[source]
        source: regex::Error,
    },
    /// A `tool:` label is blank, so the marker could never be attributed.
    #[error("marker {index} has an empty `tool` label")]
    EmptyTool {
        /// Zero-based position in the `markers:` list.
        index: usize,
    },
    /// The file declares more markers than [`MAX_MARKERS`].
    #[error("{count} markers exceeds the {max}-marker cap")]
    TooMany {
        /// Markers the file declared.
        count: usize,
        /// The cap, [`MAX_MARKERS`].
        max: usize,
    },
}

impl MarkerConfig {
    /// Parse a marker document from YAML text.
    ///
    /// Why: separating parse from read is what lets the error arm be tested
    /// without a filesystem, and lets an embedding caller supply markers from
    /// somewhere other than a file.
    /// What: strict deserialization — an unknown key or an unknown `scope` /
    /// `mode` spelling is an error, not a skipped entry. Pattern validity is
    /// NOT checked here; it is checked once at compile time in
    /// `ai_markers`, so no expression is compiled twice.
    /// Test: [`tests::spec_round_trips_from_yaml`], [`tests::unknown_key_is_rejected`].
    ///
    /// # Errors
    ///
    /// [`MarkerConfigError::Parse`] if the document does not deserialize,
    /// [`MarkerConfigError::TooMany`] if it declares more than
    /// [`MAX_MARKERS`] entries.
    pub fn from_yaml_str(yaml: &str) -> Result<Self, MarkerConfigError> {
        let cfg: Self = serde_yaml::from_str(yaml)?;
        // #5414: `detect` runs once per commit and scans the operator slice
        // linearly, so an unbounded file is a self-inflicted slowdown over a
        // whole history. The `regex` crate is linear-time, so this is a size
        // bound, not a ReDoS guard.
        if cfg.markers.len() > MAX_MARKERS {
            return Err(MarkerConfigError::TooMany {
                count: cfg.markers.len(),
                max: MAX_MARKERS,
            });
        }
        Ok(cfg)
    }

    /// Read and parse a marker file.
    ///
    /// # Errors
    ///
    /// [`MarkerConfigError::Read`] if the file cannot be read,
    /// [`MarkerConfigError::Parse`] if it does not deserialize.
    ///
    /// Test: [`tests::load_from_reports_a_missing_file`].
    pub fn load_from(path: &Path) -> Result<Self, MarkerConfigError> {
        let text = std::fs::read_to_string(path)?;
        Self::from_yaml_str(&text)
    }

    /// Number of markers in the document.
    pub fn len(&self) -> usize {
        self.markers.len()
    }

    /// Whether the document contributes no markers.
    pub fn is_empty(&self) -> bool {
        self.markers.is_empty()
    }
}

/// Where the operator marker file is looked for.
///
/// Why: the whole point of #5414 is that a marker is addable without a code
/// change, so the location has to be discoverable without one — an env var for
/// a per-run or per-repo file, and a fixed default for a machine-wide one.
/// What: [`ENV_AI_MARKERS`] when set and non-empty, else
/// [`DEFAULT_MARKER_FILE`]; either way a leading `~` is expanded. The path is
/// resolved, never required — an absent file is the normal case.
/// Test: [`tests::env_var_overrides_default_path`].
pub fn marker_file_path() -> PathBuf {
    let raw = match std::env::var(ENV_AI_MARKERS) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => DEFAULT_MARKER_FILE.to_string(),
    };
    expand_path(Path::new(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_round_trips_from_yaml() {
        let cfg = MarkerConfig::from_yaml_str(
            "markers:\n  - tool: acme-bot\n    mode: full_agentic\n    scope: message\n    pattern: 'Acme Bot'\n",
        )
        .expect("parses");
        assert_eq!(cfg.len(), 1);
        assert_eq!(
            cfg.markers[0],
            MarkerSpec::new(
                "acme-bot",
                MarkerMode::FullAgentic,
                MarkerScope::Message,
                "Acme Bot"
            )
        );
    }

    #[test]
    fn yaml_scopes_deserialize() {
        let cfg = MarkerConfig::from_yaml_str(
            "markers:\n\
             \x20 - { tool: a, mode: full_agentic, scope: trailer, pattern: x }\n\
             \x20 - { tool: b, mode: ide_assisted, scope: message, pattern: y }\n\
             \x20 - { tool: c, mode: full_agentic, scope: email, pattern: z }\n",
        )
        .expect("parses");
        let scopes: Vec<MarkerScope> = cfg.markers.iter().map(|m| m.scope).collect();
        assert_eq!(
            scopes,
            vec![
                MarkerScope::Trailer,
                MarkerScope::Message,
                MarkerScope::Email
            ]
        );
        assert_eq!(cfg.markers[1].mode, MarkerMode::IdeAssisted);
    }

    #[test]
    fn marker_mode_maps_to_agentic_mode() {
        assert_eq!(
            MarkerMode::FullAgentic.as_agentic_mode(),
            AgenticMode::FullAgentic
        );
        assert_eq!(
            MarkerMode::IdeAssisted.as_agentic_mode(),
            AgenticMode::IdeAssisted
        );
    }

    /// Why: a silently ignored typo produces a marker-less config that looks
    /// exactly like "the operator configured nothing".
    #[test]
    fn unknown_key_is_rejected() {
        let err = MarkerConfig::from_yaml_str(
            "markers:\n  - tool: a\n    mode: full_agentic\n    scope: message\n    patern: oops\n",
        )
        .expect_err("unknown key rejected");
        assert!(matches!(err, MarkerConfigError::Parse(_)), "{err}");

        let err = MarkerConfig::from_yaml_str("marker:\n  - {}\n").expect_err("unknown top key");
        assert!(matches!(err, MarkerConfigError::Parse(_)), "{err}");
    }

    /// Why: an unrecognised `scope:` must fail loudly rather than default.
    #[test]
    fn unknown_scope_is_rejected() {
        let err = MarkerConfig::from_yaml_str(
            "markers:\n  - { tool: a, mode: full_agentic, scope: subject, pattern: x }\n",
        )
        .expect_err("unknown scope rejected");
        assert!(matches!(err, MarkerConfigError::Parse(_)), "{err}");
    }

    /// Why: `detect` scans the operator slice once per commit, so file size
    /// is a cost paid across a whole history walk.
    #[test]
    fn a_file_over_the_cap_is_rejected() {
        let mut yaml = String::from("markers:\n");
        for i in 0..=MAX_MARKERS {
            yaml.push_str(&format!(
                "  - {{ tool: t{i}, mode: full_agentic, scope: message, pattern: x }}\n"
            ));
        }
        let err = MarkerConfig::from_yaml_str(&yaml).expect_err("over cap");
        assert!(
            matches!(err, MarkerConfigError::TooMany { count, max }
                     if count == MAX_MARKERS + 1 && max == MAX_MARKERS),
            "{err}"
        );

        // Exactly at the cap is fine — the bound is a ceiling, not a fence.
        let at_cap: String = yaml
            .lines()
            .take(MAX_MARKERS + 1)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            MarkerConfig::from_yaml_str(&at_cap)
                .expect("at cap parses")
                .len(),
            MAX_MARKERS
        );
    }

    #[test]
    fn empty_document_is_empty() {
        let cfg = MarkerConfig::from_yaml_str("markers: []\n").expect("parses");
        assert!(cfg.is_empty());
        assert_eq!(cfg.len(), 0);
    }

    #[test]
    fn load_from_reports_a_missing_file() {
        let err = MarkerConfig::load_from(Path::new("/definitely/not/here/ai-markers.yaml"))
            .expect_err("missing file is an error at this level");
        assert!(matches!(err, MarkerConfigError::Read(_)), "{err}");
    }

    /// Why: the env var is the "without a code change" half of #5414; if it
    /// were ignored, only the fixed default path would be configurable.
    ///
    /// Serialised against the other env-touching test in this module by a
    /// module-local mutex, since env vars are process-global.
    #[test]
    fn env_var_overrides_default_path() {
        let _guard = env_lock();
        let prev = std::env::var(ENV_AI_MARKERS).ok();

        std::env::set_var(ENV_AI_MARKERS, "/tmp/tga-markers-5414.yaml");
        assert_eq!(
            marker_file_path(),
            PathBuf::from("/tmp/tga-markers-5414.yaml")
        );

        // Blank is treated as unset, so an exported-but-empty var does not
        // point detection at "".
        std::env::set_var(ENV_AI_MARKERS, "   ");
        assert!(marker_file_path().ends_with(".config/tga/ai-markers.yaml"));

        std::env::remove_var(ENV_AI_MARKERS);
        assert!(marker_file_path().ends_with(".config/tga/ai-markers.yaml"));

        match prev {
            Some(v) => std::env::set_var(ENV_AI_MARKERS, v),
            None => std::env::remove_var(ENV_AI_MARKERS),
        }
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
}
