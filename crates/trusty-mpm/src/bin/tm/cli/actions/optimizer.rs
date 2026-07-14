//! `tm optimizer` token-use optimizer command group.
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`OptimizerAction`] — `status`/`set`, plus its
//! [`CliCompressionLevel`] value enum.
//! Test: exercised via `cli::Command::Optimizer` parse coverage in `tests.rs`.

use clap::Subcommand;

/// Actions for the `optimizer` subcommand.
#[derive(Debug, Subcommand)]
pub(crate) enum OptimizerAction {
    /// Show current optimizer configuration.
    Status,
    /// Set the default compression level (rewrites the framework policy file).
    Set {
        /// Compression level: off, trim, summarise, caveman.
        #[arg(value_enum)]
        level: CliCompressionLevel,
    },
}

/// CLI-friendly compression level (mirrors `CompressionLevel`).
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum CliCompressionLevel {
    /// No compression.
    Off,
    /// Trim large outputs.
    Trim,
    /// Trim + strip ANSI + collapse blanks.
    Summarise,
    /// Drop all content, keep a one-line summary.
    Caveman,
}
