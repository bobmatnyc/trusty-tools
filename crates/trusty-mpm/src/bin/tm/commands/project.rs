//! `project` command handlers.
//!
//! Why: project registration, listing, and inspection are a self-contained
//! group that benefits from a dedicated file.
//! What: `project` dispatcher, `scaffold_project_dir`, `resolve_dir`.
//! `Trust`/`Trust { revoke: true }` are handled entirely LOCALLY (no daemon
//! HTTP round-trip, unlike `Init`/`List`/`Info`) via
//! `trusty_mpm::core::project_trust::ProjectTrustStore` — issue #3033's
//! consent gate for project-scope custom MCP bridging.
//! Test: `cli_parses_project_*` in `tests.rs`, `project_init_scaffolds_dotdir`/
//! `project_init_keeps_existing_config` in `tests_behavior_a.rs`,
//! `project_trust_grants_and_revokes` in `tests_project_trust_tests.rs`.

use serde::Deserialize;

use crate::cli::ProjectAction;
use crate::types::ProjectRow;

/// Resolve a `--dir` option to an absolute path, defaulting to the cwd.
///
/// Why: `project` and `session` subcommands all accept an optional directory;
/// centralizing the "default to cwd" rule keeps the handlers uniform.
/// What: returns `dir` as a `PathBuf` when given, otherwise the process cwd.
/// Test: covered indirectly by the project/session handler integration tests.
pub(crate) fn resolve_dir(dir: Option<String>) -> anyhow::Result<std::path::PathBuf> {
    match dir {
        Some(d) => Ok(std::path::PathBuf::from(d)),
        None => Ok(std::env::current_dir()?),
    }
}

/// `project` subcommand — define and manage trusty-mpm projects.
///
/// Why: a project is a registered working directory; operators need shell
/// commands to register one, list all, and inspect the current one without
/// hand-crafting HTTP requests. `Trust` additionally lets them consent to
/// project-scope custom MCP bridging (issue #3033).
/// What: `Init` registers the directory (`POST /projects`) and scaffolds a
/// local `.trusty-mpm/`; `List` prints `GET /projects` with an
/// `[mcp-trusted]` marker per project; `Info` prints the current directory's
/// project via `GET /projects/current` plus its trust status; `Trust` is
/// handled entirely locally by [`trust_cmd`] (no daemon round-trip — see its
/// own doc). Trust status in `List`/`Info` is read from the SAME local
/// `core::project_trust` store `Trust` writes, never from the daemon.
/// Test: `cli_parses_project_init`, `cli_parses_project_list`,
/// `cli_parses_project_info`, `cli_parses_project_trust`,
/// `cli_parses_project_trust_revoke`, `project_init_scaffolds_dotdir`,
/// `project_trust_grants_and_revokes` (the last three CLI-parse tests and the
/// behavioral test live in `tests_project_trust_tests.rs`).
pub(crate) async fn project(
    client: &reqwest::Client,
    url: &str,
    action: ProjectAction,
) -> anyhow::Result<()> {
    match action {
        ProjectAction::Init { dir } => {
            let path = resolve_dir(dir)?;
            let body: serde_json::Value = client
                .post(format!("{url}/projects"))
                .json(&serde_json::json!({ "path": path }))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let report = scaffold_project_dir(&path)?;
            for line in &report {
                println!("  {line}");
            }
            let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            println!("registered project '{name}' at {}", path.display());
        }
        ProjectAction::List => {
            #[derive(Deserialize)]
            struct Body {
                projects: Vec<ProjectRow>,
            }
            let body: Body = client
                .get(format!("{url}/projects"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if body.projects.is_empty() {
                println!("no projects registered");
            }
            for p in &body.projects {
                let trust_marker = if trusty_mpm::core::project_trust::is_project_trusted(&p.path) {
                    " [mcp-trusted]"
                } else {
                    ""
                };
                println!("{} {}{trust_marker}", p.name, p.path.display());
            }
        }
        ProjectAction::Info { dir } => {
            let path = resolve_dir(dir)?;
            let resp = client
                .get(format!("{url}/projects/current"))
                .query(&[("path", path.to_string_lossy().as_ref())])
                .send()
                .await?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                println!("{} is not a registered project", path.display());
            } else {
                let body: serde_json::Value = resp.error_for_status()?.json().await?;
                println!("{}", serde_json::to_string_pretty(&body)?);
            }
            let trusted = trusty_mpm::core::project_trust::is_project_trusted(&path);
            println!(
                "project-scope custom MCP trust: {}",
                if trusted { "trusted" } else { "untrusted" }
            );
        }
        ProjectAction::Trust { dir, revoke } => trust_cmd(dir, revoke)?,
    }
    Ok(())
}

/// `project trust` / `project trust --revoke` handler (issue #3033).
///
/// Why: entirely LOCAL and synchronous — unlike the other `project` actions,
/// this must work without a running daemon (mirrors how
/// `session_launch::custom_mcp` itself reads/writes plain files, never HTTP).
/// What: resolves the target directory (defaults to cwd, same as every other
/// `project` action), loads the trust store from
/// [`trusty_mpm::core::project_trust::trust_store_root`], and calls `trust`/
/// `revoke`, saving only when the store actually changed.
/// Test: `project_trust_grants_and_revokes`.
pub(crate) fn trust_cmd(dir: Option<String>, revoke: bool) -> anyhow::Result<()> {
    let path = resolve_dir(dir)?;
    let Some(root) = trusty_mpm::core::project_trust::trust_store_root() else {
        anyhow::bail!("cannot resolve the home directory for the project-trust store");
    };
    let mut store = trusty_mpm::core::project_trust::ProjectTrustStore::load(&root)?;
    if revoke {
        if store.revoke(&path) {
            store.save()?;
            println!("revoked trust for {}", path.display());
        } else {
            println!("{} was not trusted (no change)", path.display());
        }
    } else if store.trust(&path) {
        store.save()?;
        println!(
            "trusted {} — its [mcp.custom] manifest entries will now be bridged into fleet \
             sessions",
            path.display()
        );
    } else {
        println!("{} is already trusted (no change)", path.display());
    }
    Ok(())
}

/// Scaffold `<project>/.trusty-mpm/` with a config skeleton and `sessions/`.
///
/// Why: `project init` must give the operator an editable, version-controllable
/// project config; doing it in a testable helper keeps it covered without a
/// live daemon.
/// What: creates `.trusty-mpm/sessions/` and writes `config.toml` (only when
/// absent — never clobbering an edited config); returns a per-path report.
/// Test: `project_init_scaffolds_dotdir`, `project_init_keeps_existing_config`.
pub(crate) fn scaffold_project_dir(project: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let mut report = Vec::new();
    let dotdir = project.join(".trusty-mpm");
    let sessions = dotdir.join("sessions");
    std::fs::create_dir_all(&sessions)?;
    report.push(format!("\u{2713} {}", sessions.display()));

    let config = dotdir.join("config.toml");
    if config.exists() {
        report.push(format!("- {} (exists, skipped)", config.display()));
    } else {
        let name = trusty_mpm::core::project::name_from_path(project);
        let contents = format!(
            "# trusty-mpm project configuration\n\
             # Generated by: trusty-mpm project init\n\n\
             [project]\nname = \"{name}\"\n\n\
             [agents]\n\
             # Additional agent sources for this project\n\
             # sources = [\"https://example.com/agents\"]\n\n\
             [skills]\n\
             # Additional skill sources for this project\n\
             # sources = []\n"
        );
        std::fs::write(&config, contents)?;
        report.push(format!("\u{2713} {}", config.display()));
    }
    Ok(report)
}
