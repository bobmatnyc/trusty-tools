//! # tga — trusty-git-analytics
//!
//! Developer productivity analytics from git history. This crate exposes a
//! three-stage pipeline (collect → classify → report) as a library, plus
//! the `tga` binary that drives it.
//!
//! ## Modules
//!
//! - [`core`] — shared types, config, database, error definitions
//! - [`collect`] — Stage 1: git extraction and external-system fetches
//! - [`classify`] — Stage 2: four-tier commit classification cascade
//! - [`report`] — Stage 3: CSV / JSON / Markdown report generation
//! - [`commands`] — the subcommand handlers `main.rs` dispatches into
//! - [`audit`] — the one-shot AUDIT sweep that drives those handlers end to end
//! - [`profile`] — longitudinal per-contributor quality profiling (#5463)

#![warn(missing_docs)]
// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

// #5217: `audit::run_full_sweep` reuses the per-command `run` functions rather
// than reimplementing eight pipelines, so `commands` had to stop being
// bin-private and become a library module. The command modules address the
// library by its crate name (`tga::core::…`), which does not resolve from
// inside the crate; this alias makes those ~139 existing paths keep resolving
// unchanged instead of being rewritten to `crate::`.
extern crate self as tga;

pub mod audit;
pub mod classify;
pub mod collect;
pub mod commands;
pub mod core;
pub mod profile;
pub mod report;
