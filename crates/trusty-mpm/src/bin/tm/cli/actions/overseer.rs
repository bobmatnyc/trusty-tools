//! `tm overseer` inspection command group.
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`OverseerAction`] — `status`.
//! Test: exercised via `cli::Command::Overseer` parse coverage in `tests.rs`.

use clap::Subcommand;

/// Actions for the `overseer` subcommand.
#[derive(Debug, Subcommand)]
pub(crate) enum OverseerAction {
    /// Show the overseer's enabled status and handler type.
    Status,
}
