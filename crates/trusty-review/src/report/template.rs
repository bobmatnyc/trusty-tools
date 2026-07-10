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
//! [`parse_section_instructions`] (#2357 layered instructions) reads a
//! template's `<!-- instruct:<section_id> ... -->` blocks into an override map
//! consumed by the synthesis prompt builder — the template-override tier of the
//! generic → template → analyst instruction layering (see
//! `section_instructions.rs`).
//! Test: `template.rs` tests cover bundled fallback for both templates, an XDG
//! override via an injected extra dir, the not-found error, and instruct-block
//! parsing (single-line, multi-line, unknown-id, and non-instruct comments).

use std::collections::BTreeMap;
use std::path::PathBuf;

use tracing::warn;

use super::error::ReportError;
use super::polish::balanced_comment_len;
use super::section_instructions::is_valid_section_id;

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

/// Parse `<!-- instruct:<section_id> ... -->` override blocks from raw template
/// source (#2357 layered instructions).
///
/// Why: a template may want its own methodology's voice for a synthesized
/// section (e.g. CAST's health-factor language in its executive summary)
/// without forking the whole prompt-building code; embedding the override AS
/// A TEMPLATE COMMENT keeps it co-located with the template it belongs to and,
/// since it is an ordinary HTML comment, it is automatically stripped from
/// rendered output by [`super::polish::strip_template_comments`] (verified by
/// `template_tests.rs::instruct_blocks_never_render`) with no special-casing
/// needed there — this parser simply reads the override BEFORE that stripping
/// discards it, from the raw template text returned by [`TemplateLoader::load`].
/// What: scans for HTML comments whose inner content starts with `instruct:`;
/// the token immediately after (up to the next whitespace) is the section id,
/// and everything from there to the comment's balanced close is the
/// instruction text (trimmed).  An id not recognised by
/// [`super::section_instructions::is_valid_section_id`] is logged via
/// `tracing::warn!` and ignored — a template typo must not abort the run.
/// Nested `<!--`/`-->` pairs inside the instruction body are balanced (reusing
/// [`balanced_comment_len`]) so an embedded documentation example does not
/// truncate the override early.
/// Test: `template.rs::{parses_instruct_override, multiline_instruct_override,
/// unknown_section_id_warns_and_ignores, non_instruct_comments_ignored}`.
pub fn parse_section_instructions(template: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut i = 0usize;
    while let Some(rel_start) = template[i..].find("<!--") {
        let abs_start = i + rel_start;
        let rest = &template[abs_start..];
        let Some(len) = balanced_comment_len(rest) else {
            break; // unterminated comment — nothing further to parse
        };
        let inner = &rest[4..len - 3];
        let trimmed = inner.trim_start();
        if let Some(after) = trimmed.strip_prefix("instruct:") {
            let after = after.trim_start();
            let (id, text) = match after.find(char::is_whitespace) {
                Some(sp) => (&after[..sp], after[sp..].trim()),
                None => (after.trim_end(), ""),
            };
            if is_valid_section_id(id) {
                out.insert(id.to_string(), text.to_string());
            } else {
                warn!(
                    section_id = %id,
                    "template `instruct:` override names an unknown section id; ignoring"
                );
            }
        }
        i = abs_start + len;
    }
    out
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

    // ── Section-instruction override parsing (#2357 layered instructions) ────

    /// Why: a single-line override is the simplest authoring form.
    /// What: `<!-- instruct:executive_summary Some text -->` yields one entry.
    /// Test: this test itself.
    #[test]
    fn parses_instruct_override() {
        let tpl = "# Title\n\n<!-- instruct:executive_summary Focus on health-factor language. -->\n\nBody";
        let overrides = parse_section_instructions(tpl);
        assert_eq!(
            overrides.get("executive_summary").map(String::as_str),
            Some("Focus on health-factor language.")
        );
    }

    /// Why: authors will want multi-line, paragraph-style overrides too.
    /// What: a multi-line body between the id line and the closing `-->` parses
    /// with internal newlines preserved (only outer whitespace trimmed).
    /// Test: this test itself.
    #[test]
    fn multiline_instruct_override() {
        let tpl = "<!-- instruct:top_risks\nList at most 3 rows.\nBe terse.\n-->";
        let overrides = parse_section_instructions(tpl);
        assert_eq!(
            overrides.get("top_risks").map(String::as_str),
            Some("List at most 3 rows.\nBe terse.")
        );
    }

    /// Why: a mistyped section id must warn and be ignored, never abort the run.
    /// What: `instruct:executiv_summary` (typo) is absent from the result.
    /// Test: this test itself.
    #[test]
    fn unknown_section_id_warns_and_ignores() {
        let tpl = "<!-- instruct:executiv_summary Typo. -->";
        let overrides = parse_section_instructions(tpl);
        assert!(overrides.is_empty(), "typo'd id must not produce an entry");
    }

    /// Why: the parser must not mistake an ordinary instructional or dataset
    /// comment for an override.
    /// What: unrelated comments (including a `dataset:` marker) yield no entries.
    /// Test: this test itself.
    #[test]
    fn non_instruct_comments_ignored() {
        let tpl = "<!-- just some authoring guidance --><!-- dataset: x | chart: bar -->";
        assert!(parse_section_instructions(tpl).is_empty());
    }

    /// Why: both bundled templates ship valid instruct syntax (or none) — a
    /// regression here would mean a shipped template's own override silently
    /// stopped parsing.
    /// What: parses both bundled templates without panicking; the CAST template
    /// carries its documented `executive_summary` demonstration override.
    /// Test: this test itself.
    #[test]
    fn bundled_cast_template_carries_demo_override() {
        let cast = TemplateLoader::bundled_only()
            .load("report-technical-dd-cast")
            .expect("bundled cast");
        let overrides = parse_section_instructions(&cast);
        assert!(
            overrides.contains_key("executive_summary"),
            "CAST template must ship its documented executive_summary override"
        );
        assert!(
            overrides["executive_summary"]
                .to_lowercase()
                .contains("health-factor"),
            "CAST override should reference health-factor language: {:?}",
            overrides["executive_summary"]
        );
    }
}
