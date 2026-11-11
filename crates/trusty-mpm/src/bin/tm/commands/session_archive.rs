//! `tm session archive <id> [--dry-run] [--force] [--json]` (#10991).
//!
//! Why: the CLI entry point for owner ruling D8b's archive verb; a thin client
//! over the daemon's one body, so the CLI cannot skip a guard.
//! What: [`session_archive`] POSTs the verb and prints the report; any daemon
//! refusal returns `Err`, so the process exits non-zero with its message.
//! Test: `cli_parses_session_archive` in `tests_behavior_e_tests.rs`;
//! `render_names_source_entry_and_stops` below.

use trusty_mpm::client::DaemonClient;
use trusty_mpm::session_manager::session_archive::{ArchiveOptions, SessionArchiveReport};

/// Run `tm session archive`.
pub(crate) async fn session_archive(
    daemon: &DaemonClient,
    id: &str,
    dry_run: bool,
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    let report = daemon
        .archive_managed_session(id, ArchiveOptions { dry_run, force })
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    for line in render(&report) {
        println!("{line}");
    }
    Ok(())
}

/// The human view: what moved (or would), where to, and who stopped.
fn render(report: &SessionArchiveReport) -> Vec<String> {
    let plan = &report.plan;
    let verb = if report.dry_run {
        "would archive"
    } else {
        "archived"
    };
    let mut lines = vec![
        format!("{verb} {}", plan.source.display()),
        format!("  to       {}", plan.destination.display()),
        format!("  manifest {}", plan.manifest_path.display()),
    ];
    if let Some(project) = &plan.project {
        lines.push(format!("  project  {project} (marked archived)"));
    }
    if !report.stopped.is_empty() {
        let label = if report.dry_run {
            "would stop"
        } else {
            "stopped"
        };
        lines.push(format!("  {label}: {}", report.stopped.join(", ")));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::session_manager::archive_move::ArchivePlan;

    #[test]
    fn render_names_source_entry_and_stops() {
        let report = SessionArchiveReport {
            dry_run: true,
            plan: ArchivePlan {
                source: "/w/widget".into(),
                root: "/a".into(),
                entry: "/a/widget-x".into(),
                destination: "/a/widget-x/content".into(),
                manifest_path: "/a/widget-x/manifest.json".into(),
                session_id: "s1".into(),
                project: Some("widget".into()),
            },
            stopped: vec!["s1".into()],
            manifest: None,
        };
        let lines = render(&report);
        assert_eq!(lines[0], "would archive /w/widget");
        assert!(lines.iter().any(|l| l.contains("/a/widget-x/content")));
        assert!(lines.iter().any(|l| l.contains("would stop: s1")));
    }
}
