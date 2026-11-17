//! `trusty-review content check`: does the installed content serve this
//! binary? (#10588)
//!
//! Why: a fresh install pinned `content-v2.3.0`, which predates
//! `prompts/principles_addendum.md`, reported success, and the first review
//! exited 1. `tctl install` / `tctl upgrade` run this verb after `tm content
//! update` so an install whose content is older than its binaries fails then,
//! naming the missing file.
//! What: [`run_check`] resolves the content this process would read (the
//! checkout `TRUSTY_REVIEW_CONTENT_CHECKOUT` names, else the pinned bundle)
//! and reads every `runtime_content::REQUIRED_FILES` entry. Each problem goes
//! to stderr on its own line; the returned error, which `main` prints last,
//! names every missing file and the remedy. Never fetches.
//! Test: `content_check_names_the_file_older_content_lacks`,
//! `content_check_passes_on_content_that_serves_this_binary`,
//! `content_check_without_content_names_the_remedy` (tests/content_check_cli.rs).

use anyhow::{Result, anyhow, bail};
use trusty_review::runtime_content::{
    CHECKOUT_ENV, PromptContentError, REQUIRED_FILES, check_required, describe_source,
    resolve_for_process,
};

/// Subcommands of `trusty-review content`.
#[derive(Debug, clap::Subcommand)]
pub enum ContentCommand {
    /// Check that the installed content has every file this binary reads;
    /// exit 1 naming each missing one (#10588).
    Check,
}

/// Runs `cmd`. See the module doc.
///
/// # Errors
/// The content cannot be resolved, or lacks or empties a required file.
pub fn run(cmd: &ContentCommand) -> Result<()> {
    match cmd {
        ContentCommand::Check => run_check(),
    }
}

/// Entry point for `trusty-review content check`. See the module doc.
fn run_check() -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    // Flattened so the cause is on the one line `tctl` reads back.
    let content = resolve_for_process().map_err(|e| anyhow!("{e}"))?;
    let origin = describe_source(content.source());
    let problems = check_required(&content);
    if problems.is_empty() {
        println!(
            "content {origin} has all {} files trusty-review {version} reads",
            REQUIRED_FILES.len()
        );
        return Ok(());
    }
    for problem in &problems {
        eprintln!("trusty-review: {problem}");
    }
    let missing: Vec<&str> = problems
        .iter()
        .map(|p| match p {
            PromptContentError::Missing { path, .. } | PromptContentError::Invalid { path, .. } => {
                path.as_str()
            }
            _ => "(unreadable content)",
        })
        .collect();
    // #10588: `tm content update` alone cannot help when the newest release is
    // older than this binary, so the remedy names the other two ways out.
    bail!(
        "content {origin} does not serve trusty-review {version}: missing or unusable {}; run \
         `tm content update`, and if the newest content release still lacks them, this binary \
         is newer than every published content release: install a released trusty-review, or \
         set {CHECKOUT_ENV} to a trusty-tools checkout",
        missing.join(", ")
    )
}
