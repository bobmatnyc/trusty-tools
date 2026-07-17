//! `tm generate` command handler — dispatches to the dev-time codegen engine
//! (issue #2913).
//!
//! Why: keeps the CLI-facing handler thin — argument dispatch only — while
//! `crate::generate` owns the actual generation logic (clap-tree walking,
//! MCP catalog rendering, agent/skill roster rendering, doctor-check
//! rendering) so it stays independently unit-testable.
//! What: `generate` dispatches `GenerateAction::Capabilities` to
//! `crate::generate::run_capabilities`.
//! Test: `cli_parses_generate_capabilities`,
//! `cli_parses_generate_capabilities_check` in `cli/tests.rs` cover argument
//! parsing; `crate::generate`'s own test modules cover the generation logic.

use crate::cli::GenerateAction;

/// `tm generate <action>` — dispatch to the requested generator.
pub(crate) async fn generate(action: GenerateAction) -> anyhow::Result<()> {
    match action {
        GenerateAction::Capabilities { check } => crate::generate::run_capabilities(check),
    }
}
