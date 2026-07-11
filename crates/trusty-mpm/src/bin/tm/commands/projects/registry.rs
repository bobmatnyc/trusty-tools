//! `tm projects` registry verbs: list / register / show / status (#2115).
//!
//! Why: the registry-B half of the project control plane — enumerate, upsert,
//! inspect (config + read-only nested sessions), and roll up a project's status.
//! Each verb is a thin client over a `DaemonClient` method (`projects.rs`),
//! rendering either a human view or, with `--json`, the raw wire JSON for
//! scripting (matching the `--json`-everywhere convention of `tm sessions`).
//! What: [`list`], [`register`], [`show`], [`status`] plus the pure human-render
//! helpers they delegate to. `show`'s nested sessions come from the fleet endpoint
//! (read-only per §3.1); the render explicitly points mutation at `tm sessions`.
//! Test: `render_status_line_*`, `render_project_line_*` (pure render) in the
//! `tests` submodule; live HTTP via the daemon route tests.

use trusty_mpm::client::DaemonClient;
use trusty_mpm::client::http_client::projects::{ProjectStatusWire, RegisterProjectArgs};
use trusty_mpm::project::Project;

/// Owned inputs for [`register`], mirroring the `Register` clap variant.
///
/// Why: passing the seven register fields as one struct keeps the dispatcher and
/// this handler's signature readable (clippy `too_many_arguments`).
/// What: the register flags; `tags` is empty (not `None`) when unspecified.
/// Test: covered via `register`'s live-HTTP path.
pub(crate) struct RegisterInput {
    pub name: String,
    pub repo_url: String,
    pub default_branch: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub stack_hint: Option<String>,
    pub gh_user: Option<String>,
}

/// Build a `DaemonClient` from the CLI's shared `(reqwest::Client, url)` pair.
fn daemon(client: &reqwest::Client, url: &str) -> DaemonClient {
    DaemonClient::with_client(client.clone(), url.to_string())
}

/// `tm projects list [--json] [--tag <t>]` — enumerate registered projects.
pub(crate) async fn list(
    client: &reqwest::Client,
    url: &str,
    json: bool,
    tag: Option<String>,
) -> anyhow::Result<()> {
    let projects = daemon(client, url)
        .registry_list_projects(tag.as_deref())
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&projects)?);
        return Ok(());
    }
    if projects.is_empty() {
        println!("no projects registered");
        return Ok(());
    }
    for p in &projects {
        println!("{}", render_project_line(p));
    }
    Ok(())
}

/// `tm projects register <name> --repo-url ...` — idempotent upsert.
pub(crate) async fn register(
    client: &reqwest::Client,
    url: &str,
    input: RegisterInput,
) -> anyhow::Result<()> {
    let args = RegisterProjectArgs {
        name: input.name,
        repo_url: input.repo_url,
        default_branch: input.default_branch,
        description: input.description,
        tags: if input.tags.is_empty() {
            None
        } else {
            Some(input.tags)
        },
        stack_hint: input.stack_hint,
        gh_user: input.gh_user,
    };
    let project = daemon(client, url).registry_register_project(&args).await?;
    println!(
        "registered project '{}' ({} @ {})",
        project.name, project.repo_url, project.default_branch
    );
    Ok(())
}

/// `tm projects show <name> [--json]` — config + read-only nested sessions.
pub(crate) async fn show(
    client: &reqwest::Client,
    url: &str,
    name: &str,
    json: bool,
) -> anyhow::Result<()> {
    let daemon = daemon(client, url);
    let project = daemon.registry_get_project(name).await?;

    // Read-only nested sessions via the fleet endpoint (§3.1). A fleet read
    // failure must not sink the whole `show` — the config is the primary payload
    // — so degrade to an empty session list on error.
    let sessions = daemon
        .fleet_managed_sessions()
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|g| g.project_name == project.name)
        .map(|g| g.sessions)
        .unwrap_or_default();

    if json {
        let out = serde_json::json!({
            "project": project,
            "sessions": sessions.iter().map(|s| serde_json::json!({
                "id": s.id, "name": s.name, "state": s.state,
                "branch": s.branch, "task": s.task,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("{}", render_project_line(&project));
    if let Some(desc) = &project.description {
        println!("  description: {desc}");
    }
    if !project.tags.is_empty() {
        println!("  tags: {}", project.tags.join(", "));
    }
    if let Some(gh) = &project.gh_user {
        println!("  gh_user: {gh}");
    }
    println!(
        "  sessions ({}) [read-only — mutate via `tm sessions <verb>`]:",
        sessions.len()
    );
    if sessions.is_empty() {
        println!("    (none)");
    } else {
        for s in &sessions {
            let branch = s.branch.as_deref().unwrap_or("-");
            println!("    {} {} [{}] {}", s.id, s.name, s.state, branch);
        }
    }
    Ok(())
}

/// `tm projects status <name> [--json]` — deterministic status rollup.
pub(crate) async fn status(
    client: &reqwest::Client,
    url: &str,
    name: &str,
    json: bool,
) -> anyhow::Result<()> {
    let status = daemon(client, url).project_status(name).await?;
    if json {
        // Re-serialize the parsed rollup. Additive #2382 fields the client does
        // not model are dropped here; `--json` consumers that need them can hit
        // the endpoint directly. The human view below is the stable surface.
        let out = serde_json::json!({
            "project_name": status.project_name,
            "repo_url": status.repo_url,
            "sessions": {
                "provisioning": status.sessions.provisioning,
                "active": status.sessions.active,
                "stopped": status.sessions.stopped,
                "errored": status.sessions.errored,
                "decommissioned": status.sessions.decommissioned,
                "total": status.sessions.total,
            },
            "last_activity_at": status.last_activity_at,
            "config": {
                "gh_user_set": status.config.gh_user_set,
                "github_binding_set": status.config.github_binding_set,
            },
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    for line in render_status(&status) {
        println!("{line}");
    }
    Ok(())
}

/// Render one project as a compact `name  repo_url  (branch)` line.
///
/// Why: shared between `list` and `show`; a pure helper keeps it testable.
/// What: name, repo URL, and default branch on one line.
/// Test: `render_project_line_basic`.
fn render_project_line(p: &Project) -> String {
    format!("{}\t{}\t({})", p.name, p.repo_url, p.default_branch)
}

/// Render the status rollup as human lines.
///
/// Why: `tm projects status` prints a small multi-line rollup; a pure function of
/// the parsed [`ProjectStatusWire`] keeps it deterministic and testable.
/// What: the identity line, the session histogram, last activity, and the config
/// flags.
/// Test: `render_status_line_counts`, `render_status_no_activity`.
fn render_status(s: &ProjectStatusWire) -> Vec<String> {
    let c = &s.sessions;
    let activity = s
        .last_activity_at
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| "never".to_string());
    vec![
        format!("{} ({})", s.project_name, s.repo_url),
        format!(
            "  sessions: {} total  ({} active, {} provisioning, {} stopped, {} errored, {} decommissioned)",
            c.total, c.active, c.provisioning, c.stopped, c.errored, c.decommissioned
        ),
        format!("  last activity: {activity}"),
        format!(
            "  config: gh_user {}, gh-binding {}",
            if s.config.gh_user_set { "set" } else { "unset" },
            if s.config.github_binding_set {
                "set"
            } else {
                "unset"
            }
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::client::http_client::projects::{
        ProjectConfigFlagsWire, SessionStateCountsWire,
    };

    fn project() -> Project {
        Project {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: "main".into(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            github: None,
            commit_name: None,
            commit_email: None,
        }
    }

    #[test]
    fn render_project_line_basic() {
        let line = render_project_line(&project());
        assert!(line.contains("widget"));
        assert!(line.contains("https://github.com/acme/widget"));
        assert!(line.contains("main"));
    }

    #[test]
    fn render_status_line_counts() {
        let s = ProjectStatusWire {
            project_name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            sessions: SessionStateCountsWire {
                provisioning: 1,
                active: 2,
                stopped: 0,
                errored: 1,
                decommissioned: 0,
                total: 4,
            },
            last_activity_at: Some(
                chrono::DateTime::parse_from_rfc3339("2026-07-10T12:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
            config: ProjectConfigFlagsWire {
                gh_user_set: true,
                github_binding_set: false,
            },
        };
        let lines = render_status(&s);
        assert!(lines[0].contains("widget"));
        assert!(lines[1].contains("4 total"));
        assert!(lines[1].contains("2 active"));
        assert!(lines[2].contains("2026-07-10"));
        assert!(lines[3].contains("gh_user set"));
    }

    #[test]
    fn render_status_no_activity() {
        let s = ProjectStatusWire {
            project_name: "widget".into(),
            repo_url: "u".into(),
            sessions: SessionStateCountsWire::default(),
            last_activity_at: None,
            config: ProjectConfigFlagsWire::default(),
        };
        let lines = render_status(&s);
        assert!(lines[2].contains("never"));
        assert!(lines[3].contains("gh_user unset"));
    }
}
