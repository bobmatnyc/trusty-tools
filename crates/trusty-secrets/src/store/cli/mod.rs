//! The runner and template-file guard the CLI-backed backends share (#7519).
//!
//! Why: 1Password and Keeper are reached through their CLIs (DOC-74 §8.2).
//! Every way a value can leak from a child process — argv, the environment,
//! stderr copied into an error, a template file left on disk — is decided
//! here once, not per backend.
//! What: [`CliSpec`] (a backend's static facts), [`CliCommand`] (the
//! blocking runner), [`Verdict`] and [`CliRun`] (what a run meant), and
//! [`TemplateFile`] with [`sweep_stale_templates`] (a 0600 file for a CLI
//! that reads input only from a path). Behind the `cli-backends` feature,
//! Unix only. No backend-specific code lives here.
//! Test: `runner_tests.rs` and `template_tests.rs` beside this file, and
//! `tests/cli_backends_feature_closure.rs`.
//!
//! Deliberate duplication: this module re-implements the guarantees of
//! trusty-common's `credentials/external_cli.rs` (#9311) rather than
//! depending on it, under owner ruling 2026-10-07, "Secrets should have no
//! common dependencies." A fix to either copy should be checked against the
//! other.

mod classify;
mod runner;
mod spec;
mod template;

pub use classify::Verdict;
pub use runner::{CliCommand, CliRun, OUTPUT_CAP};
pub use spec::CliSpec;
pub use template::{TMP_SUBDIR, TemplateFile, default_tmp_root, sweep_stale_templates};

#[cfg(test)]
mod runner_tests;
#[cfg(test)]
mod template_tests;
