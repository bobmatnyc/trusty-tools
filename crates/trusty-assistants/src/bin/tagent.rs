//! `tagent`: the pre-rename name of `ta`, kept for one release (#11632).
//!
//! Why: scripts, launchd plists and shell history still call `tagent`. MB1516
//! §11 keeps the old name working for one release so those callers get a
//! warning instead of `command not found`.
//! What: prints one deprecation line on stderr, then runs exactly what `ta`
//! runs, with the same argv, stdout and exit status.
//! Test: `tests/tagent_alias.rs`.

#![deny(rustdoc::broken_intra_doc_links)]

use anyhow::Result;

fn main() -> Result<()> {
    eprintln!(
        "{}: deprecated; this name is now `{}` and the alias is removed in the next release",
        env!("CARGO_BIN_NAME"),
        trusty_assistants::cli::BIN_NAME
    );
    trusty_assistants::cli::run_to_completion()
}
