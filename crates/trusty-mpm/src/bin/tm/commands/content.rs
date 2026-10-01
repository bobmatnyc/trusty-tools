//! `tm content install|update|status` (ADR-0064, #8378 PR-C).
//!
//! Why: the runtime-only content ruling (#8974) needs an operator surface for
//! the content cache; the logic lives in `trusty_mpm::content`, so this file
//! only resolves the cache directory, runs the verb and prints the result.
//! What: daemon-less; runs on a blocking thread because the update path uses
//! a blocking HTTP client and a blocking file lock. Every failure returns an
//! error, so `tm` exits non-zero; `status` also fails when nothing serves.
//! Test: `cli_parses_content_install_update_and_status`; the verbs' behaviour
//! is covered in `trusty_mpm::content::bundle_cache::tests`.

use anyhow::Context;
use trusty_mpm::content::BUILTIN_CONTENT_EMBEDDED;
use trusty_mpm::content::bundle_cache::{
    GithubReleases, INSTALL_HINT, UpdateAction, UpdateOutcome, install_from_file, update,
};
use trusty_mpm::content::status::content_status;

use crate::cli::ContentAction;

/// Runs one `tm content` verb.
pub(crate) async fn run(action: ContentAction) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || run_blocking(action))
        .await
        .context("the tm content task did not complete")?
}

fn run_blocking(action: ContentAction) -> anyhow::Result<()> {
    let cache = trusty_common::content::default_cache_dir()
        .context("no home directory resolves, so the content cache cannot be located")?;
    match action {
        ContentAction::Install { from } => {
            print_outcome(&install_from_file(&cache, &from)?);
        }
        ContentAction::Update { content_ref } => {
            let source = GithubReleases::new()
                .map_err(|e| anyhow::anyhow!("could not build an HTTP client: {}", e.reason))?;
            print_outcome(&update(&cache, &source, content_ref.as_deref())?);
        }
        ContentAction::Status => {
            let cwd = std::env::current_dir().ok();
            let status = content_status(&cache, cwd.as_deref());
            for line in status.lines() {
                println!("{line}");
            }
            // See ADR-0064: built-in content serves until PHASE_1 removes it.
            if let Some(info) = status.builtin_info(BUILTIN_CONTENT_EMBEDDED) {
                println!("{info}");
            }
            if !status.exits_ok(BUILTIN_CONTENT_EMBEDDED) {
                anyhow::bail!(
                    "no verified content source; run `tm content update`, or offline `{INSTALL_HINT}`"
                );
            }
        }
    }
    Ok(())
}

fn print_outcome(outcome: &UpdateOutcome) {
    let verb = match outcome.action {
        UpdateAction::Installed => "pinned",
        UpdateAction::Repaired => "restored",
        UpdateAction::AlreadyCurrent => "already pinned and verified:",
    };
    println!("{verb} {} (sha256 {})", outcome.tag, outcome.sha256);
}
