//! `tm generate` — dev-time regeneration of derived, committed artifacts
//! (issue #2913).
//!
//! Why: extracted from `cli.rs` (issue #2603 precedent) to keep the top-level
//! file under the 500-SLOC production cap.
//! What: [`GenerateAction`] — currently only `capabilities`, which
//! regenerates the `tm-capabilities` bundled skill from the live harness
//! surfaces (CLI tree, MCP tool catalog, agent roster, skill catalog, doctor
//! checks).
//! Test: `cli_parses_generate_capabilities`, `cli_parses_generate_capabilities_check`.

use clap::Subcommand;

/// Actions for the `generate` subcommand.
///
/// Why: `generate` is a namespace for dev-time codegen subcommands — today
/// only the `tm-capabilities` catalog, but scoped as its own action enum so a
/// future generator (e.g. shell completions) doesn't need a new top-level
/// `Command` variant.
/// What: `Capabilities` regenerates `crates/trusty-mpm/src/assets/skills/
/// tm-capabilities/` from the in-process CLI/MCP/agent/skill/doctor surfaces;
/// `--check` diffs the freshly generated output against the committed files
/// instead of writing, exiting non-zero on drift (the CI gate).
/// Test: `cli_parses_generate_capabilities`, `cli_parses_generate_capabilities_check`.
#[derive(Debug, Subcommand)]
pub(crate) enum GenerateAction {
    /// Regenerate the `tm-capabilities` bundled skill's generated files.
    ///
    /// Why: the CLI command tree, MCP tool catalog, agent roster, and skill
    /// catalog all drift from any hand-maintained doc the moment a command,
    /// tool, agent, or skill is added — this subcommand is the single source
    /// that re-derives the reference catalog from the harness's own in-memory
    /// data (never parses `--help` text or greps for literals).
    /// What: without `--check`, overwrites
    /// `crates/trusty-mpm/src/assets/skills/tm-capabilities.md` and its
    /// `references/{cli,mcp-tools,agents,skills,doctor}.md` siblings
    /// (`references/workflows.md` is hand-authored and never touched). With
    /// `--check`, generates to memory, diffs against the committed files, and
    /// exits non-zero with a summary of every mismatched/missing file instead
    /// of writing anything — the CI drift gate
    /// (`scripts/check_capabilities.sh`).
    /// Test: `cli_parses_generate_capabilities`, `cli_parses_generate_capabilities_check`.
    Capabilities {
        /// Diff against the committed output instead of writing; exit
        /// non-zero when the generated content would differ.
        #[arg(long)]
        check: bool,
    },
}
