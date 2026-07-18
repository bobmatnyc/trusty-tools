//! Named review-template loader for the PR-review pipeline (issue #2995).
//!
//! Why: `voice::VoiceConfig` already composes stock → principles → voice, but
//! that addendum is either opt-in-per-package (`[voice] package`) or absent —
//! there was no way to select a markdown addendum by name the way
//! `report::template::TemplateLoader` already does for the due-diligence
//! report pipeline. This module gives the PR-review pipeline the same
//! bundled-defaults + user-override UX, scoped to its own `templates/review/`
//! subdirectory so review-template names never collide with report-template
//! names in the shared XDG config tree.
//!
//! What: [`ReviewTemplateLoader`] resolves a template name to its markdown
//! source, trying extra dirs (tests) → the XDG user-config dir → bundled
//! defaults. One template ships bundled: `strict-security`. A review template
//! is APPENDED as a structured addendum section to the layered system prompt
//! (after voice/principles) — see
//! `pipeline::voice_config::build_voice_config` for the full layering order.
//! Wholesale rubric replacement is intentionally out of scope: there is no
//! mechanism here to remove or override the stock base prompt, only to append
//! to it.
//!
//! Test: `load_bundled_default`, `load_from_extra_dir`, `load_missing_errors`,
//! `bundled_only_ignores_xdg`.

use std::path::PathBuf;

use thiserror::Error;

/// The bundled `strict-security` review template, embedded at compile time.
const BUNDLED_STRICT_SECURITY: &str = include_str!("../../templates/review/strict-security.md");

/// Errors produced by the review-template loader.
///
/// Why: mirrors `report::error::ReportError`'s `TemplateNotFound` shape so a
/// missing name surfaces the same class of diagnostic across both template
/// systems, without this always-compiled module depending on the
/// feature-gated `report` module.
/// What: `NotFound` for an unknown template name; `Io` for a filesystem
/// failure while reading a discovered override file.
/// Test: `load_missing_errors`.
#[derive(Debug, Error)]
pub enum ReviewTemplateError {
    /// No review template with the given name exists in any search directory.
    #[error("review template '{name}' not found in any search directory")]
    NotFound { name: String },
    /// A filesystem I/O error occurred while reading a discovered override file.
    #[error("I/O error reading review template at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Resolves review-template names to markdown addendum source.
///
/// Why: single entry point for review-template loading so
/// `pipeline::voice_config::build_voice_config` need not reason about
/// search-directory precedence.
/// What: `load(name)` tries `extra_dirs` (test/local overrides), then the XDG
/// user-config dir (`~/.config/trusty-review/templates/review/<name>.md` on
/// Linux; the platform-equivalent `dirs::config_dir()` elsewhere), then
/// bundled defaults; returns the first hit. `skip_xdg` suppresses the
/// user-config lookup so tests can assert on the bundled default exclusively.
/// Test: `load_bundled_default`, `load_from_extra_dir`, `load_missing_errors`,
/// `bundled_only_ignores_xdg`.
#[derive(Debug, Default)]
pub struct ReviewTemplateLoader {
    extra_dirs: Vec<PathBuf>,
    skip_xdg: bool,
}

impl ReviewTemplateLoader {
    /// Construct a loader searching the XDG config dir and bundled defaults.
    ///
    /// Why: the production call site (`build_voice_config`) needs no custom
    /// directories.
    /// What: empty `extra_dirs`, XDG search enabled.
    /// Test: `load_bundled_default`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a loader with additional highest-priority search directories.
    ///
    /// Why: tests inject a temp directory holding a hand-written template to
    /// exercise the override path without writing to `~/.config`.
    /// What: `dirs` are searched before the XDG user-config dir.
    /// Test: `load_from_extra_dir`.
    pub fn with_extra_dirs(dirs: Vec<PathBuf>) -> Self {
        Self {
            extra_dirs: dirs,
            skip_xdg: false,
        }
    }

    /// Construct a loader that skips the XDG user-config path entirely.
    ///
    /// Why: tests asserting on the BUNDLED default must not accidentally load
    /// a developer's `~/.config/trusty-review/templates/review/<name>.md`.
    /// What: empty extra dirs, XDG search suppressed.
    /// Test: `bundled_only_ignores_xdg`.
    pub fn bundled_only() -> Self {
        Self {
            extra_dirs: vec![],
            skip_xdg: true,
        }
    }

    /// Load a review template's markdown source by name.
    ///
    /// Why: `build_voice_config` resolves the configured template name to its
    /// addendum text before it is appended to the layered system prompt.
    /// What: tries each extra dir, then the XDG dir, for
    /// `templates/review/<name>.md`; then the bundled defaults. Returns
    /// [`ReviewTemplateError::NotFound`] when nothing matches.
    /// Test: `load_bundled_default`, `load_from_extra_dir`,
    /// `load_missing_errors`.
    pub fn load(&self, name: &str) -> Result<String, ReviewTemplateError> {
        // 1. Extra dirs (highest priority — test injection + local overrides).
        for base in &self.extra_dirs {
            let candidate = base.join(format!("{name}.md"));
            if candidate.exists() {
                return std::fs::read_to_string(&candidate).map_err(|source| {
                    ReviewTemplateError::Io {
                        path: candidate,
                        source,
                    }
                });
            }
        }

        // 2. XDG user-config path: <config_dir>/trusty-review/templates/review/<name>.md
        if !self.skip_xdg
            && let Some(config_dir) = dirs::config_dir()
        {
            let candidate = config_dir
                .join("trusty-review")
                .join("templates")
                .join("review")
                .join(format!("{name}.md"));
            if candidate.exists() {
                return std::fs::read_to_string(&candidate).map_err(|source| {
                    ReviewTemplateError::Io {
                        path: candidate,
                        source,
                    }
                });
            }
        }

        // 3. Bundled fallback.
        match name {
            "strict-security" => Ok(BUNDLED_STRICT_SECURITY.to_string()),
            _ => Err(ReviewTemplateError::NotFound {
                name: name.to_string(),
            }),
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the bundled default must load with zero external setup.
    /// What: loads `strict-security` and asserts a known heading is present.
    /// Test: this test itself.
    #[test]
    fn load_bundled_default() {
        let loader = ReviewTemplateLoader::bundled_only();
        let text = loader.load("strict-security").expect("bundled template");
        assert!(text.contains("strict-security"));
        assert!(text.to_lowercase().contains("injection"));
    }

    /// Why: an operator/project override must win over the bundled default.
    /// What: writes a template into a temp extra dir and asserts it is
    /// returned instead of the bundled text.
    /// Test: this test itself.
    #[test]
    fn load_from_extra_dir() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("strict-security.md");
        std::fs::write(&path, "# OVERRIDE MARKER").expect("write");
        let loader = ReviewTemplateLoader::with_extra_dirs(vec![tmp.path().to_path_buf()]);
        let out = loader.load("strict-security").expect("override");
        assert!(out.contains("OVERRIDE MARKER"));
    }

    /// Why: an unknown template name must fail loudly, not silently.
    /// What: asserts a `NotFound` error for an unknown name.
    /// Test: this test itself.
    #[test]
    fn load_missing_errors() {
        let loader = ReviewTemplateLoader::bundled_only();
        let err = loader.load("does-not-exist").expect_err("must error");
        assert!(matches!(err, ReviewTemplateError::NotFound { .. }));
    }

    /// Why: bundled-only mode must ignore any external XDG template.
    /// What: asserts the bundled default loads and unknown names still error.
    /// Test: this test itself.
    #[test]
    fn bundled_only_ignores_xdg() {
        let loader = ReviewTemplateLoader::bundled_only();
        assert!(loader.load("strict-security").is_ok());
        assert!(loader.load("unknown").is_err());
    }
}
