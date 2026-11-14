//! `tm session delete <id> --project-dir [--dry-run] [--yes]` (#10992).
//!
//! Why: owner ruling D8b — "delete = dry run + typed confirm + Touch ID". The
//! presence prompt can only reach the user from their own terminal, the way
//! `tm presence local` and `tm fleet bind` prompt today, so the delete runs in
//! this process; the daemon supplies the session list, and no HTTP route can
//! remove a directory.
//! What: [`run`] reads the session and the list from the daemon, the managed
//! workspace root and the recorded Architect directories from config, and
//! hands them to `project_dir_delete::run_dir_delete` on a blocking thread.
//! Every run shows the plan first; a real run then reads the typed directory
//! name (unless `--yes`) and asks `LocalAuthenticator` for presence. After the
//! directory is gone the record is marked deleted through the existing
//! `tm session delete` path.
//! Test: the guards in `session_manager/project_dir_delete_tests.rs`; the
//! flags by `cli_parses_session_delete_project_dir`; the view by
//! `render_names_the_directory_and_root` below.

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;

use anyhow::Context as _;
use trusty_mpm::client::DaemonClient;
use trusty_mpm::core::architect_store::{DEFAULT_ARCHITECT_DIR, recorded_architects};
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::presence_local::LocalAuthenticator;
use trusty_mpm::core::trusty_tools_config::{TrustyToolsConfig, workspace_root};
use trusty_mpm::session_manager::project_dir_delete::{
    Confirmation, DirDeleteInput, DirDeleteOptions, DirDeletePlan, DirSession, OsProbes,
    run_dir_delete,
};

/// Run `tm session delete <id> --project-dir`.
pub(crate) async fn run(
    daemon: &DaemonClient,
    id: String,
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<()> {
    let target = daemon
        .get_managed_session(&id)
        .await
        .with_context(|| format!("session {id}"))?;
    let mut sessions: Vec<DirSession> = daemon
        .list_managed_sessions()
        .await
        .context("listing sessions")?
        .iter()
        .map(DirSession::from_summary)
        .collect();
    if !sessions.iter().any(|s| s.id == target.id) {
        sessions.push(DirSession::from_summary(&target));
    }
    let root = workspace_root(&TrustyToolsConfig::load());
    let mut architect_dirs = recorded_architects(&MpmConfig::load_default());
    if let Some(home) = dirs::home_dir() {
        architect_dirs.push(home.join(DEFAULT_ARCHITECT_DIR));
    }
    let session_id = target.id.clone();
    let report = tokio::task::spawn_blocking(move || {
        delete_blocking(&session_id, &sessions, root, &architect_dirs, dry_run, yes)
    })
    .await
    .context("delete task")??;
    if report.dry_run {
        for line in render(&report.plan, true) {
            println!("{line}");
        }
        println!("dry run: nothing was deleted");
        return Ok(());
    }
    println!("deleted {}", report.plan.dir.display());
    // #10992: the directory is gone; mark the record as `tm session delete` does.
    crate::commands::delete::session_delete(daemon, target.id.clone(), false)
        .await
        .with_context(|| {
            format!(
                "{} was deleted, but session {} was not marked deleted",
                report.plan.dir.display(),
                target.id
            )
        })
}

/// The blocking body: guards, prompt, presence, removal.
fn delete_blocking(
    session_id: &str,
    sessions: &[DirSession],
    root: PathBuf,
    architect_dirs: &[PathBuf],
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<trusty_mpm::session_manager::project_dir_delete::DirDeleteReport> {
    let input = DirDeleteInput {
        session_id,
        sessions,
        root: &root,
        architect_dirs,
    };
    let mut confirm = |plan: &DirDeletePlan| {
        for line in render(plan, false) {
            println!("{line}");
        }
        if yes {
            return Confirmation::Yes;
        }
        print!(
            "Type the directory name '{}' to delete it: ",
            plan.confirm_name
        );
        let _ = std::io::stdout().flush();
        let mut typed = String::new();
        // #10992: an unreadable stdin types nothing, which never matches.
        let _ = std::io::stdin().lock().read_line(&mut typed);
        Confirmation::Typed(typed)
    };
    Ok(run_dir_delete(
        input,
        DirDeleteOptions { dry_run },
        &mut confirm,
        &LocalAuthenticator,
        &OsProbes,
    )?)
}

/// The plan, as the dry run and the real run both show it.
fn render(plan: &DirDeletePlan, dry_run: bool) -> Vec<String> {
    let verb = if dry_run {
        "would delete"
    } else {
        "will delete"
    };
    vec![
        format!("{verb} {}", plan.dir.display()),
        format!("  session {}", plan.session_id),
        format!("  inside  {}", plan.root.display()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command, SessionAction};
    use clap::Parser as _;

    #[test]
    fn cli_parses_session_delete_project_dir() {
        // #10992: `--project-dir` opts in; `--dry-run`/`--yes` need it; `--force`
        // (the record-only running bypass) cannot be combined with it.
        let cli = Cli::try_parse_from([
            "trusty-mpm",
            "session",
            "delete",
            "abc",
            "--project-dir",
            "--dry-run",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Command::Session {
                action:
                    SessionAction::Delete {
                        project_dir,
                        dry_run,
                        yes,
                        force,
                        ..
                    },
            } => {
                assert!(project_dir && dry_run && !yes && !force);
            }
            other => panic!("expected session delete, got {other:?}"),
        }
        let plain = Cli::try_parse_from(["trusty-mpm", "session", "delete", "abc"]).unwrap();
        match plain.command.unwrap() {
            Command::Session {
                action: SessionAction::Delete { project_dir, .. },
            } => assert!(!project_dir, "the directory is never removed by default"),
            other => panic!("expected session delete, got {other:?}"),
        }
        for args in [
            &["trusty-mpm", "session", "delete", "abc", "--dry-run"][..],
            &["trusty-mpm", "session", "delete", "abc", "--yes"][..],
            &[
                "trusty-mpm",
                "session",
                "delete",
                "abc",
                "--project-dir",
                "--force",
            ][..],
        ] {
            assert!(
                Cli::try_parse_from(args).is_err(),
                "{args:?} must not parse"
            );
        }
    }

    #[test]
    fn render_names_the_directory_and_root() {
        let plan = DirDeletePlan {
            session_id: "s1".into(),
            dir: "/w/acme/widget".into(),
            root: "/w".into(),
            confirm_name: "widget".into(),
        };
        let lines = render(&plan, true);
        assert_eq!(lines[0], "would delete /w/acme/widget");
        assert!(lines.iter().any(|l| l.contains("session s1")));
        assert_eq!(render(&plan, false)[0], "will delete /w/acme/widget");
    }
}
