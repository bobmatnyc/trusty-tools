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
use trusty_mpm::project_config::{self, ConfigEdit};

use crate::cli::ConfigAction;

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
    pub gh_account: Option<String>,
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
        gh_account: input.gh_account,
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
    // — so degrade to an empty session list on error, but warn so the operator
    // knows the session list may be incomplete rather than genuinely empty.
    let sessions = match daemon.fleet_managed_sessions().await {
        Ok(groups) => groups
            .into_iter()
            .find(|g| g.project_name == project.name)
            .map(|g| g.sessions)
            .unwrap_or_default(),
        Err(e) => {
            eprintln!("warning: could not read sessions: {e}");
            Vec::new()
        }
    };

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
    if let Some(gh) = &project.gh_account {
        println!("  gh_account: {gh}");
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
                "gh_account_set": status.config.gh_account_set,
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

/// `tm projects config <name> [set|unset|tags]` — view or edit config fields
/// (DOC-35 §3.1/§6, #2120).
///
/// Why: the deterministic CLI half of the configurator — a thin client over
/// `PATCH /api/v1/projects/{name}` via the shared
/// [`trusty_mpm::project_config`] edit model, so this front end and the TUI
/// form can never drift on what a given edit means on the wire (§6 RESOLVED).
/// What: `action = None` is a read-only view (GET, human or `--json`); each
/// `Some(ConfigAction)` variant maps to exactly one [`ConfigEdit`] and PATCHes
/// it. `Tags` with both `--add`/`--remove` empty is rejected client-side
/// (disclosed judgment call — a friendlier `bail!` here rather than sending a
/// genuine no-op PATCH the server would just idempotently accept).
/// Test: `cli_parses_projects_config_*` (parse) in `tests_projects.rs`;
/// `projects_config_shared_cases_match_cli_arg_building` exercises this
/// module's own edit-to-`ConfigEdit` mapping against the shared case table;
/// live HTTP via the daemon route tests.
pub(crate) async fn config(
    client: &reqwest::Client,
    url: &str,
    name: &str,
    json: bool,
    action: Option<ConfigAction>,
) -> anyhow::Result<()> {
    match action {
        None => config_view(client, url, name, json).await,
        Some(ConfigAction::Set { field, value }) => {
            config_apply(client, url, name, ConfigEdit::Set(field.into(), value)).await
        }
        Some(ConfigAction::Unset { field }) => {
            config_apply(client, url, name, ConfigEdit::Unset(field.into())).await
        }
        Some(ConfigAction::Tags { add, remove }) => {
            if add.is_empty() && remove.is_empty() {
                anyhow::bail!("tags: provide --add and/or --remove");
            }
            config_apply(client, url, name, ConfigEdit::Tags { add, remove }).await
        }
    }
}

/// `tm projects config <name>` (no subcommand) — read-only config view.
async fn config_view(
    client: &reqwest::Client,
    url: &str,
    name: &str,
    json: bool,
) -> anyhow::Result<()> {
    let project = daemon(client, url).registry_get_project(name).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&project)?);
        return Ok(());
    }
    println!("{}", render_config_view(&project));
    Ok(())
}

/// Apply one [`ConfigEdit`] via `PATCH /api/v1/projects/{name}` and print the
/// updated config.
///
/// Why: the single call site every `ConfigAction` variant in [`config`]
/// routes through — one PATCH, one render, matching [`register`]'s
/// print-after-write convention.
/// What: builds the wire args via [`project_config::build_patch_args`] (the
/// SAME builder the TUI form's submit uses), PATCHes, and renders the
/// server's returned (authoritative, post-write) record. A 400/404 surfaces
/// as an `anyhow::Error` via `DaemonClient::registry_patch_project`, whose
/// message now carries the daemon's actual rejection reason (#2485) —
/// propagated unchanged, printed by `main`'s default error handler.
async fn config_apply(
    client: &reqwest::Client,
    url: &str,
    name: &str,
    edit: ConfigEdit,
) -> anyhow::Result<()> {
    let args = project_config::build_patch_args(&edit);
    let project = daemon(client, url)
        .registry_patch_project(name, &args)
        .await?;
    println!("updated project '{}'", project.name);
    println!("{}", render_config_view(&project));
    Ok(())
}

/// Render a project's configurator-scoped fields (DOC-35 §6's field table):
/// `default_branch`, `description`, `tags`, `stack_hint`, `gh_user` — no
/// nested sessions (that is `show`'s job, not `config`'s).
fn render_config_view(p: &Project) -> String {
    let lines = [
        format!("{} ({})", p.name, p.repo_url),
        format!("  default_branch: {}", p.default_branch),
        format!(
            "  description: {}",
            p.description.as_deref().unwrap_or("(unset)")
        ),
        format!(
            "  tags: {}",
            if p.tags.is_empty() {
                "(none)".to_string()
            } else {
                p.tags.join(", ")
            }
        ),
        format!(
            "  stack_hint: {}",
            p.stack_hint.as_deref().unwrap_or("(unset)")
        ),
        format!("  gh_user: {}", p.gh_user.as_deref().unwrap_or("(unset)")),
        format!(
            "  gh_account: {}",
            p.gh_account.as_deref().unwrap_or("(unset)")
        ),
    ];
    lines.join("\n")
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
            "  config: gh_user {}, gh_account {}, gh-binding {}",
            if s.config.gh_user_set { "set" } else { "unset" },
            if s.config.gh_account_set {
                "set"
            } else {
                "unset"
            },
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
            gh_account: None,
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
                gh_account_set: true,
            },
        };
        let lines = render_status(&s);
        assert!(lines[0].contains("widget"));
        assert!(lines[1].contains("4 total"));
        assert!(lines[1].contains("2 active"));
        assert!(lines[2].contains("2026-07-10"));
        assert!(lines[3].contains("gh_user set"));
        assert!(lines[3].contains("gh_account set"));
    }

    #[test]
    fn render_config_view_shows_every_field() {
        let mut p = project();
        p.description = Some("the widget".to_string());
        p.tags = vec!["backend".to_string()];
        p.stack_hint = Some("rust".to_string());
        p.gh_user = Some("acme-bot".to_string());
        p.gh_account = Some("acme-bot".to_string());
        let rendered = render_config_view(&p);
        assert!(rendered.contains("default_branch: main"));
        assert!(rendered.contains("description: the widget"));
        assert!(rendered.contains("tags: backend"));
        assert!(rendered.contains("stack_hint: rust"));
        assert!(rendered.contains("gh_user: acme-bot"));
        assert!(rendered.contains("gh_account: acme-bot"));
    }

    #[test]
    fn render_config_view_shows_unset_placeholders() {
        let rendered = render_config_view(&project());
        assert!(rendered.contains("description: (unset)"));
        assert!(rendered.contains("tags: (none)"));
        assert!(rendered.contains("stack_hint: (unset)"));
        assert!(rendered.contains("gh_user: (unset)"));
        assert!(rendered.contains("gh_account: (unset)"));
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
