//! `tm projects milestones` subtree: list / add / show (#2381, §10.8).
//!
//! Why: the CLI over the Milestone CRUD API. Milestone status is a rollup, not a
//! user-driven state machine (§10.5), so there is no `set-status` verb here —
//! §10.8 lists only `list|add|show` for milestones, and this subtree matches it
//! exactly.
//! What: [`dispatch`] routes a [`MilestonesAction`]; the per-verb handlers are
//! thin `DaemonClient` calls with human/`--json` rendering. `add` parses the
//! `--target-date` RFC-3339 string into the `DateTime<Utc>` the API expects,
//! failing loudly on a malformed value.
//! Test: `render_milestone_line_has_name`, `parse_target_date_*` in the `tests`
//! submodule; live HTTP via the daemon's `deliverable_routes::tests`.

use anyhow::Context;
use chrono::{DateTime, Utc};

use trusty_mpm::client::DaemonClient;
use trusty_mpm::client::http_client::deliverables::CreateMilestoneArgs;
use trusty_mpm::deliverable::Milestone;

use crate::cli::MilestonesAction;

/// Build a `DaemonClient` from the CLI's shared `(reqwest::Client, url)` pair.
fn daemon(client: &reqwest::Client, url: &str) -> DaemonClient {
    DaemonClient::with_client(client.clone(), url.to_string())
}

/// Route a `tm projects milestones <action>` invocation.
pub(crate) async fn dispatch(
    client: &reqwest::Client,
    url: &str,
    action: MilestonesAction,
) -> anyhow::Result<()> {
    match action {
        MilestonesAction::List { project, json } => {
            let milestones = daemon(client, url).list_milestones(&project).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&milestones)?);
            } else if milestones.is_empty() {
                println!("no milestones for project '{project}'");
            } else {
                for m in &milestones {
                    println!("{}", render_milestone_line(m));
                }
            }
            Ok(())
        }
        MilestonesAction::Add {
            project,
            name,
            target_date,
            description,
        } => {
            let target_date = parse_target_date(&target_date)?;
            let args = CreateMilestoneArgs {
                name,
                description,
                target_date,
            };
            let created = daemon(client, url)
                .create_milestone(&project, &args)
                .await?;
            println!("created milestone {}", created.id);
            println!("{}", render_milestone_line(&created));
            Ok(())
        }
        MilestonesAction::Show { project, id, json } => {
            let m = daemon(client, url).get_milestone(&project, &id).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&m)?);
            } else {
                for line in render_milestone_detail(&m) {
                    println!("{line}");
                }
            }
            Ok(())
        }
    }
}

/// Parse an RFC-3339 `--target-date` into a UTC timestamp, failing loudly.
///
/// Why: the Milestone API requires a concrete `target_date`; a malformed value
/// must be a clear CLI error, not a silent default.
/// What: parses via `DateTime::parse_from_rfc3339` and normalizes to UTC.
/// Test: `parse_target_date_ok`, `parse_target_date_rejects_garbage`.
fn parse_target_date(s: &str) -> anyhow::Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)
        .with_context(|| {
            format!("invalid --target-date {s:?}; expected RFC 3339 (e.g. 2026-09-01T00:00:00Z)")
        })?
        .with_timezone(&Utc))
}

/// Render one Milestone as a compact `id  status  target  name` line.
fn render_milestone_line(m: &Milestone) -> String {
    format!(
        "{}\t{:?}\t{}\t{}",
        m.id,
        m.status,
        m.target_date.to_rfc3339(),
        m.name
    )
}

/// Render a Milestone's full detail as human lines.
fn render_milestone_detail(m: &Milestone) -> Vec<String> {
    let mut lines = vec![
        format!("{} — {}", m.id, m.name),
        format!("  project: {}", m.project_name),
        format!("  status: {:?}", m.status),
        format!("  target: {}", m.target_date.to_rfc3339()),
        format!("  deliverables: {}", m.deliverables.len()),
    ];
    if !m.description.is_empty() {
        lines.push(format!("  description: {}", m.description));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn milestone() -> Milestone {
        Milestone {
            id: trusty_mpm::deliverable::MilestoneId::new(),
            project_name: "widget".into(),
            name: "v1.0 Alpha".into(),
            description: String::new(),
            target_date: Utc::now(),
            status: trusty_mpm::deliverable::MilestoneStatus::Proposed,
            deliverables: vec![],
            created_at: Utc::now(),
        }
    }

    #[test]
    fn render_milestone_line_has_name() {
        let line = render_milestone_line(&milestone());
        assert!(line.contains("v1.0 Alpha"), "{line}");
        assert!(line.contains("Proposed"), "{line}");
    }

    #[test]
    fn parse_target_date_ok() {
        let dt = parse_target_date("2026-09-01T00:00:00Z").unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-09-01T00:00:00+00:00");
    }

    #[test]
    fn parse_target_date_rejects_garbage() {
        assert!(parse_target_date("not-a-date").is_err());
    }
}
