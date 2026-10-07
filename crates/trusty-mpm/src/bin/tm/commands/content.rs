//! `tm content install|update|status` (ADR-0064, #8378 PR-C).
//!
//! Why: the runtime-only content ruling (#8974) needs an operator surface for
//! the content cache; the logic lives in `trusty_mpm::content`, so this file
//! only resolves the cache directory, runs the verb and prints the result.
//! What: daemon-less; runs on a blocking thread because the update path uses
//! a blocking HTTP client and a blocking file lock. Every failure returns an
//! error, so `tm` exits non-zero; `status` also fails when nothing serves.
//! Test: `cli_parses_content_install_update_and_status`,
//! `status_report_exits_non_zero_with_nothing_installed`;
//! the verbs' behaviour is covered in `trusty_mpm::content::bundle_cache::tests`.

use anyhow::Context;
use trusty_agents_common::agent_content::REMEDY;
use trusty_mpm::content::bundle_cache::{
    UpdateAction, UpdateOutcome, github_source, install_from_file, update,
};
use trusty_mpm::content::status::{ContentStatus, content_status};

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
            // #9396: retried on a 5xx; authenticated through `gh` when no
            // token variable is set.
            let source = github_source()
                .map_err(|e| anyhow::anyhow!("could not build an HTTP client: {}", e.reason))?;
            print_outcome(&update(&cache, &source, content_ref.as_deref())?);
        }
        ContentAction::Status => {
            let cwd = std::env::current_dir().ok();
            let status = content_status(&cache, cwd.as_deref());
            let (lines, verdict) = status_report(&status);
            for line in lines {
                println!("{line}");
            }
            verdict?;
        }
    }
    Ok(())
}

/// What `tm content status` prints, and whether it exits 0.
///
/// Why: critic M2 on #8982 — the verb and the doctor row must agree on what
/// healthy means, so the decision lives here, not inline.
/// What: the status lines, and an error when no source serves (#9012: the
/// binary compiles in no instructional content). A broken lock or bundle is
/// always an error.
/// Test: `status_report_exits_non_zero_with_nothing_installed`.
fn status_report(status: &ContentStatus) -> (Vec<String>, anyhow::Result<()>) {
    let lines = status.lines();
    let verdict = if status.exits_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("no verified content source; {REMEDY}"))
    };
    (lines, verdict)
}

fn print_outcome(outcome: &UpdateOutcome) {
    let verb = match outcome.action {
        UpdateAction::Installed => "pinned",
        UpdateAction::Repaired => "restored",
        UpdateAction::AlreadyCurrent => "already pinned and verified:",
    };
    println!("{verb} {} (sha256 {})", outcome.tag, outcome.sha256);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Critic M2 on #8982, after #9012: with nothing installed, `tm content
    /// status` exits non-zero naming the fix. A broken lock never exits 0.
    #[test]
    fn status_report_exits_non_zero_with_nothing_installed() {
        let cache = tempfile::tempdir().expect("tempdir");
        let status = content_status(cache.path(), None);

        let (lines, verdict) = status_report(&status);
        let err = verdict.expect_err("no source serves");
        assert!(err.to_string().contains(REMEDY), "{err}");
        assert!(!lines.iter().any(|l| l.starts_with("info: ")), "{lines:?}");

        let lock = cache.path().join(trusty_common::content::LOCK_FILE_NAME);
        std::fs::write(&lock, "not = [valid").expect("write lock");
        let broken = content_status(cache.path(), None);
        let (lines, verdict) = status_report(&broken);
        assert!(verdict.is_err(), "a broken lock must not exit 0: {lines:?}");
        assert!(!lines.iter().any(|l| l.starts_with("info: ")), "{lines:?}");
    }

    /// Critic M1 on #8982: `update --help` claims only what `commit` checks —
    /// a re-fetch of the currently pinned tag — not every republished tag.
    #[test]
    fn update_help_limits_the_pin_check_to_the_current_pin() {
        use clap::CommandFactory as _;
        let mut root = crate::cli::Cli::command();
        let help = root
            .find_subcommand_mut("content")
            .and_then(|c| c.find_subcommand_mut("update"))
            .expect("content update")
            .render_long_help()
            .to_string();
        assert!(help.contains("currently pinned tag"), "{help}");
        assert!(help.contains("Only the current pin is checked"), "{help}");
        assert!(!help.contains("tag republished"), "{help}");
    }

    /// #9396: `update --help` documents the `TRUSTY_CONTENT_OFFLINE=1` switch
    /// that turns the first-use fetch off.
    #[test]
    fn update_help_names_the_offline_switch() {
        use clap::CommandFactory as _;
        let mut root = crate::cli::Cli::command();
        let help = root
            .find_subcommand_mut("content")
            .and_then(|c| c.find_subcommand_mut("update"))
            .expect("content update")
            .render_long_help()
            .to_string();
        assert!(help.contains("TRUSTY_CONTENT_OFFLINE=1"), "{help}");
        assert!(help.contains("first use"), "{help}");
    }
}
