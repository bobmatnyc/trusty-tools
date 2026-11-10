//! `tm projects decommission <name> [--json]` (#10988).
//!
//! Why: the CLI entry point for project decommission; a thin client over the
//! daemon's one body, like every other `tm projects` verb.
//! What: [`decommission`] POSTs the verb and prints the report; a daemon
//! refusal (unknown project, Architect session, failed stop) returns `Err`, so
//! the process exits non-zero with the daemon's message.
//! Test: `cli_parses_projects_decommission` in `tests_projects.rs`;
//! `render_report_names_stopped_and_kept_sessions` below.

use trusty_mpm::client::DaemonClient;
use trusty_mpm::session_manager::project_decommission::ProjectDecommissionReport;

/// Run `tm projects decommission`.
pub(crate) async fn decommission(
    client: &reqwest::Client,
    url: &str,
    name: &str,
    json: bool,
) -> anyhow::Result<()> {
    let daemon = DaemonClient::with_client(client.clone(), url.to_string());
    let report = daemon.registry_decommission_project(name).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    for line in render_report(&report) {
        println!("{line}");
    }
    Ok(())
}

/// The human view: one headline, then each session id under its outcome.
fn render_report(report: &ProjectDecommissionReport) -> Vec<String> {
    let mut lines = vec![format!(
        "project '{}' decommissioned — workspaces and search index kept",
        report.project.name
    )];
    lines.push(format!("  stopped ({}):", report.stopped.len()));
    lines.extend(report.stopped.iter().map(|id| format!("    {id}")));
    lines.push(format!(
        "  already not running ({}):",
        report.left_as_is.len()
    ));
    lines.extend(report.left_as_is.iter().map(|id| format!("    {id}")));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::project::Project;

    #[test]
    fn render_report_names_stopped_and_kept_sessions() {
        let report = ProjectDecommissionReport {
            project: Project {
                name: "widget".into(),
                repo_url: "https://github.com/acme/widget".into(),
                default_branch: "main".into(),
                stack_hint: None,
                tags: vec![],
                description: None,
                gh_user: None,
                gh_account: None,
                github: None,
                commit_name: None,
                commit_email: None,
                worktree: None,
                project_key: None,
                decommissioned_at: Some(chrono::Utc::now()),
            },
            stopped: vec!["s1".into()],
            left_as_is: vec!["s2".into()],
        };
        let lines = render_report(&report);
        assert!(lines[0].contains("widget"), "{lines:?}");
        assert!(lines.contains(&"    s1".to_string()), "{lines:?}");
        assert!(lines.contains(&"    s2".to_string()), "{lines:?}");
    }
}
