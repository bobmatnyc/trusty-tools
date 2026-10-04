//! Compiled-in framework artifacts: the hook policy files and the output-style
//! registry.
//!
//! Why: `tm install` deploys the optimizer/overseer policy as part of a
//! working framework root, and that policy is code-adjacent config, not
//! instructional content. Everything instructional — the agent roster (#9011),
//! skills, PM instruction sections, output-style bodies, SM instructions and
//! the bundled docs (#9012) — is runtime content read through
//! [`crate::core::content_source`] (ADR-0064). (#3374 removed the former
//! `CLAUDE_STUB` artifact; #4286 split A removed the bundled
//! `instructions/INSTRUCTIONS.md` stub.)
//! What: the two hook policies as `pub const &str` via `include_str!`, the
//! [`BundledArtifact`] table naming their install paths, and the
//! [`OUTPUT_STYLES`] registry (id, file name, content path — the body is read
//! from content).
//! Test: `bundle_table_is_complete`, `every_bundled_style_reads_from_content`.

use crate::core::framework_content::FrameworkContent;

/// Default token-optimizer policy installed to `hooks/optimizer.toml`.
pub const OPTIMIZER_TOML: &str = include_str!("../assets/hooks/optimizer.toml");

/// Default session-overseer policy installed to `hooks/overseer.toml`.
///
/// Overseer oversight is opt-in: the shipped policy has `enabled = false`, so
/// installing it is inert until an operator flips the flag.
pub const OVERSEER_TOML: &str = include_str!("../assets/hooks/overseer.toml");

/// The default output-style id used when none is configured/selected.
///
/// Why: callers (config resolution, settings writer) need a single source of
/// truth for the professional default so they cannot drift.
/// What: the frontmatter `name:` of `instructions/output-styles/trusty-mpm.md`.
/// Test: `bundle_tests::output_style_registry_default_resolves`.
pub const DEFAULT_OUTPUT_STYLE_ID: &str = "trusty-mpm";

/// One entry in the bundled output-style registry.
///
/// Why: the multi-style launch path (HR-4) must map a configured/selected style
/// id to its content and the file name it deploys to; keeping the id, file name
/// and content path together keeps those three facts from drifting.
/// What: the style id (matching the file's frontmatter `name:`), the file name
/// written under `~/.claude/output-styles/`, and the content path the body is
/// read from at runtime (#9012).
/// Test: `bundle_tests::output_style_registry_ids_match_frontmatter`.
#[derive(Debug, Clone, Copy)]
pub struct BundledStyle {
    /// Style id — matches the frontmatter `name:` and the `outputStyle` settings
    /// key Claude Code resolves against `~/.claude/output-styles/<file_name>`.
    pub id: &'static str,
    /// File name written under `~/.claude/output-styles/`.
    pub file_name: &'static str,
    /// The style's file in the content bundle, relative to `instructions/`
    /// (#9012: read at runtime, no longer compiled in).
    pub content_path: &'static str,
}

impl BundledStyle {
    /// The style's Markdown, from loaded content (#9012).
    ///
    /// Every registry path is in
    /// [`crate::core::framework_content::REQUIRED_INSTRUCTIONS`], so a loaded
    /// [`FrameworkContent`] always carries it.
    /// Test: `every_bundled_style_reads_from_content`.
    pub fn content<'a>(&self, content: &'a FrameworkContent) -> &'a str {
        content.required(self.content_path)
    }
}

/// All bundled output styles, default first.
///
/// Why: `deploy_output_style` writes every entry, and the style resolver looks
/// up a configured/selected id against this table; a single ordered slice keeps
/// both behaviours consistent.
/// What: the professional (`trusty-mpm`), teaching (`trusty-mpm-teacher`), and
/// research (`trusty-mpm-research`) PM styles, then the fleet-supervisor style
/// (`trusty-mpm-supervisor`, #8453). Deploying and `tm doctor` cover all four;
/// only a supervisor session may select the last.
/// Test: `bundle_tests::output_style_registry_has_four_distinct_ids`.
pub const OUTPUT_STYLES: &[BundledStyle] = &[
    BundledStyle {
        id: DEFAULT_OUTPUT_STYLE_ID,
        file_name: "trusty-mpm.md",
        content_path: "output-styles/trusty-mpm.md",
    },
    BundledStyle {
        id: "trusty-mpm-teacher",
        file_name: "trusty-mpm-teacher.md",
        content_path: "output-styles/trusty-mpm-teacher.md",
    },
    BundledStyle {
        id: "trusty-mpm-research",
        file_name: "trusty-mpm-research.md",
        content_path: "output-styles/trusty-mpm-research.md",
    },
    BundledStyle {
        id: crate::core::session_profile::SUPERVISOR_OUTPUT_STYLE_ID,
        file_name: "trusty-mpm-supervisor.md",
        content_path: "output-styles/trusty-mpm-supervisor.md",
    },
];

/// The bundled PM output styles: [`OUTPUT_STYLES`] without the supervisor style.
///
/// Why (#8453): the PM invariants — the mandatory-delegation floor, the
/// identity protocol, the TodoWrite section — hold for every style a PM session
/// can select, and the supervisor style deliberately carries none of them.
/// Test: `the_pm_styles_are_every_style_but_the_supervisor_style`.
pub fn pm_output_styles() -> impl Iterator<Item = &'static BundledStyle> {
    OUTPUT_STYLES
        .iter()
        .filter(|s| s.id != crate::core::session_profile::SUPERVISOR_OUTPUT_STYLE_ID)
}

// BundledArtifact, InstallPolicy, and ALL are defined in bundle_all.rs.
// They are included here so they can access the constants above via `use super::*`.
#[path = "bundle_all.rs"]
mod all_inner;
pub use all_inner::{ALL, BundledArtifact, InstallPolicy};

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;
