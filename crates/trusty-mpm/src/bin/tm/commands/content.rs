//! `tm content install|update|status` (ADR-0064, #8378 PR-C).
//!
//! Why: the runtime-only content ruling (#8974) needs an operator surface for
//! the content cache; the logic lives in `trusty_mpm::content`, so this file
//! only resolves the cache directory, runs the verb and prints the result.
//! What: daemon-less; runs on a blocking thread because the update path uses
//! a blocking HTTP client and a blocking file lock. Every failure returns an
//! error, so `tm` exits non-zero; `status` also fails when nothing serves,
//! except "nothing installed" while built-in content still ships.
//! Test: `cli_parses_content_install_update_and_status`,
//! `status_report_is_info_and_exit_0_with_nothing_installed_while_builtin_content_ships`;
//! the verbs' behaviour is covered in `trusty_mpm::content::bundle_cache::tests`.

use anyhow::Context;
use trusty_mpm::content::BUILTIN_CONTENT_EMBEDDED;
use trusty_mpm::content::bundle_cache::{
    GithubReleases, INSTALL_HINT, UpdateAction, UpdateOutcome, install_from_file, update,
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
            let source = GithubReleases::new()
                .map_err(|e| anyhow::anyhow!("could not build an HTTP client: {}", e.reason))?;
            print_outcome(&update(&cache, &source, content_ref.as_deref())?);
        }
        ContentAction::Status => {
            let cwd = std::env::current_dir().ok();
            let status = content_status(&cache, cwd.as_deref());
            let (lines, verdict) = status_report(&status, BUILTIN_CONTENT_EMBEDDED);
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
/// Why: critic M2 on #8982 — the verb must honour `BUILTIN_CONTENT_EMBEDDED`
/// the way the doctor row does, so the decision lives here, not inline.
/// What: the status lines, plus the `info:` line while built-in content
/// serves; an error when neither a source nor built-in content serves. A
/// broken lock or bundle is always an error.
/// Test: `status_report_is_info_and_exit_0_with_nothing_installed_while_builtin_content_ships`.
fn status_report(
    status: &ContentStatus,
    builtin_embedded: bool,
) -> (Vec<String>, anyhow::Result<()>) {
    let mut lines = status.lines();
    // See ADR-0064: built-in content serves until PHASE_1 removes it.
    lines.extend(status.builtin_info(builtin_embedded).map(str::to_owned));
    let verdict = if status.exits_ok(builtin_embedded) {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "no verified content source; run `tm content update`, or offline `{INSTALL_HINT}`"
        ))
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

    /// Critic M2 on #8982: with nothing installed, `tm content status` prints
    /// the `info:` line and exits 0 while built-in content ships, and exits
    /// non-zero once ADR-0064 PHASE_1 removes it. A broken lock never exits 0.
    #[test]
    fn status_report_is_info_and_exit_0_with_nothing_installed_while_builtin_content_ships() {
        let cache = tempfile::tempdir().expect("tempdir");
        let status = content_status(cache.path(), None);

        let (lines, verdict) = status_report(&status, true);
        assert!(verdict.is_ok(), "{verdict:?}");
        assert!(lines.iter().any(|l| l.starts_with("info: ")), "{lines:?}");

        let (lines, verdict) = status_report(&status, false);
        let err = verdict.expect_err("no source once PHASE_1 lands");
        assert!(err.to_string().contains(INSTALL_HINT), "{err}");
        assert!(!lines.iter().any(|l| l.starts_with("info: ")), "{lines:?}");

        let lock = cache.path().join(trusty_common::content::LOCK_FILE_NAME);
        std::fs::write(&lock, "not = [valid").expect("write lock");
        let broken = content_status(cache.path(), None);
        let (lines, verdict) = status_report(&broken, true);
        assert!(verdict.is_err(), "a broken lock must not exit 0: {lines:?}");
        assert!(!lines.iter().any(|l| l.starts_with("info: ")), "{lines:?}");
    }
}
