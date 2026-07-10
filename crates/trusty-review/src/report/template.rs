//! Report template loader — XDG override + bundled fallback (M1, #2313).
//!
//! Why: report templates should be customisable per operator (drop a file at
//! `~/.config/trusty-review/templates/<name>.md`) but must also work out of the
//! box after `cargo install` with zero setup.  This mirrors the `VoiceLoader`
//! pattern: user-config directory wins, else a compile-time `include_str!`
//! bundled default.
//! What: [`TemplateLoader`] resolves a template name to its markdown source,
//! trying extra dirs → XDG config dir → bundled defaults.  Two templates ship
//! bundled: `report-technical-dd` and `report-technical-dd-cast`.
//! Test: `template.rs` tests cover bundled fallback for both templates, an XDG
//! override via an injected extra dir, and the not-found error.

use std::path::PathBuf;

use super::error::ReportError;

/// The generic vendor-neutral technical-DD template, bundled at compile time.
const BUNDLED_TECHNICAL_DD: &str = include_str!("../../templates/report-technical-dd.md");
/// The CAST-specific technical-DD template, bundled at compile time.
const BUNDLED_TECHNICAL_DD_CAST: &str = include_str!("../../templates/report-technical-dd-cast.md");

/// The default template name used when none is specified.
pub const DEFAULT_TEMPLATE: &str = "report-technical-dd";

/// Resolves report template names to markdown source.
///
/// Why: a single entry point for template loading so the reporter and CLI need
/// not reason about search-directory precedence.
/// What: `load(name)` tries `extra_dirs` (test/local overrides), then the XDG
/// user-config dir, then bundled defaults; returns the first hit.  `skip_xdg`
/// suppresses the user-config lookup so tests can assert on the bundled default
/// exclusively.
/// Test: `template.rs::{load_bundled_default, load_from_extra_dir,
/// load_missing_errors, bundled_only_ignores_xdg}`.
#[derive(Debug, Default)]
pub struct TemplateLoader {
    extra_dirs: Vec<PathBuf>,
    skip_xdg: bool,
}

impl TemplateLoader {
    /// Construct a loader searching the XDG config dir and bundled defaults.
    ///
    /// Why: the production call site needs no custom directories.
    /// What: empty `extra_dirs`, XDG search enabled.
    /// Test: `template.rs::load_bundled_default`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a loader with additional highest-priority search directories.
    ///
    /// Why: tests inject a temp directory holding a hand-written template to
    /// exercise the override path without writing to `~/.config`.
    /// What: `dirs` are searched before the XDG user-config dir.
    /// Test: `template.rs::load_from_extra_dir`.
    pub fn with_extra_dirs(dirs: Vec<PathBuf>) -> Self {
        Self {
            extra_dirs: dirs,
            skip_xdg: false,
        }
    }

    /// Construct a loader that skips the XDG user-config path entirely.
    ///
    /// Why: tests asserting on the BUNDLED default must not accidentally load a
    /// developer's `~/.config/trusty-review/templates/<name>.md`.
    /// What: empty extra dirs, XDG search suppressed.
    /// Test: `template.rs::bundled_only_ignores_xdg`.
    pub fn bundled_only() -> Self {
        Self {
            extra_dirs: vec![],
            skip_xdg: true,
        }
    }

    /// Load a template's markdown source by name.
    ///
    /// Why: the reporter resolves the chosen template name to its source before
    /// filling; graceful fallback to bundled defaults keeps zero-setup installs
    /// working.
    /// What: tries each extra dir, then the XDG dir, for `<name>.md`; then the
    /// bundled defaults.  Returns [`ReportError::TemplateNotFound`] when nothing
    /// matches.
    /// Test: `template.rs::{load_bundled_default, load_from_extra_dir,
    /// load_missing_errors}`.
    pub fn load(&self, name: &str) -> std::result::Result<String, ReportError> {
        // 1. Extra dirs (highest priority — test injection + local overrides).
        for base in &self.extra_dirs {
            let candidate = base.join(format!("{name}.md"));
            if candidate.exists() {
                return std::fs::read_to_string(&candidate).map_err(|source| ReportError::Io {
                    path: candidate,
                    source,
                });
            }
        }

        // 2. XDG user-config path: ~/.config/trusty-review/templates/<name>.md
        if !self.skip_xdg
            && let Some(config_dir) = dirs::config_dir()
        {
            let candidate = config_dir
                .join("trusty-review")
                .join("templates")
                .join(format!("{name}.md"));
            if candidate.exists() {
                return std::fs::read_to_string(&candidate).map_err(|source| ReportError::Io {
                    path: candidate,
                    source,
                });
            }
        }

        // 3. Bundled fallback.
        match name {
            "report-technical-dd" => Ok(BUNDLED_TECHNICAL_DD.to_string()),
            "report-technical-dd-cast" => Ok(BUNDLED_TECHNICAL_DD_CAST.to_string()),
            _ => Err(ReportError::TemplateNotFound {
                name: name.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: bundled defaults must load with zero external setup.
    /// What: loads both shipped templates and asserts a known heading is present.
    /// Test: this test itself.
    #[test]
    fn load_bundled_default() {
        let loader = TemplateLoader::bundled_only();
        let dd = loader.load("report-technical-dd").expect("bundled generic");
        assert!(dd.contains("Technical Due-Diligence Analysis"));
        let cast = loader
            .load("report-technical-dd-cast")
            .expect("bundled cast");
        assert!(cast.contains("CAST Technical Due-Diligence Analysis"));
    }

    /// Why: an operator override must win over the bundled default.
    /// What: writes a template into a temp extra dir and asserts it is returned.
    /// Test: this test itself.
    #[test]
    fn load_from_extra_dir() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("report-technical-dd.md");
        std::fs::write(&path, "# OVERRIDE MARKER {{title}}").expect("write");
        let loader = TemplateLoader::with_extra_dirs(vec![tmp.path().to_path_buf()]);
        let out = loader.load("report-technical-dd").expect("override");
        assert!(out.contains("OVERRIDE MARKER"));
    }

    /// Why: an unknown template name must fail loudly, not silently.
    /// What: asserts a `TemplateNotFound` error for an unknown name.
    /// Test: this test itself.
    #[test]
    fn load_missing_errors() {
        let loader = TemplateLoader::bundled_only();
        let err = loader.load("does-not-exist").expect_err("must error");
        assert!(matches!(err, ReportError::TemplateNotFound { .. }));
    }

    /// Why: bundled-only mode must ignore any external XDG template.
    /// What: asserts the bundled default loads and unknown names still error.
    /// Test: this test itself.
    #[test]
    fn bundled_only_ignores_xdg() {
        let loader = TemplateLoader::bundled_only();
        assert!(loader.load("report-technical-dd").is_ok());
        assert!(loader.load("unknown").is_err());
    }
}
