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

use crate::cli::ProjectAction;

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
/// local `.trusty-mpm/`; `List` prints the persistent registry
/// (`GET /api/v1/projects`, see [`list_rows`] for why not `GET /projects`)
/// with an `[mcp-trusted]` marker per project; `Info` prints the current
/// directory's project via `GET /projects/current` plus its trust status;
/// `Trust` is handled entirely locally by [`trust_cmd`] (no daemon round-trip — see its
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
            // #5994: rows come from the persistent registry, not the daemon's
            // in-memory session-derived project map.
            for row in list_rows(client, url).await? {
                println!("{row}");
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

/// The lines `tm project list` prints, read from the PERSISTENT registry.
///
/// Why (#5994): this verb used to `GET /projects`, which is
/// `DaemonState::projects` — an in-memory `HashMap` rebuilt on every daemon
/// start and populated only from `config.yaml` and session history. On a host
/// whose `~/.trusty-mpm/project-registry/projects.json`, `project_list` MCP
/// tool, and `GET /api/v1/projects` route all listed five projects, that map
/// was empty and the command printed "no projects registered". `/api/v1/projects`
/// is the authoritative surface — it is served straight from the on-disk
/// registry, and it is the same store the MCP tool reads — so this reads there.
/// What: `GET /api/v1/projects` via [`trusty_mpm::client::DaemonClient::registry_list_projects`],
/// rendered by the SAME [`crate::commands::projects::registry::render_project_line`]
/// `tm projects list` uses, plus the `[mcp-trusted]` marker this verb has always
/// carried. Trust is keyed on a local PATH and the registry record holds none,
/// so the local alias store (`~/.trusty-mpm/project-paths.json`) supplies it;
/// a project with no local alias simply gets no marker. Returns the rows
/// instead of printing them so a test can assert both the endpoint and the text.
/// Test: `project_list_reads_the_persistent_registry`,
/// `project_list_row_marks_a_trusted_local_path`.
pub(crate) async fn list_rows(client: &reqwest::Client, url: &str) -> anyhow::Result<Vec<String>> {
    let projects = trusty_mpm::client::DaemonClient::with_client(client.clone(), url.to_string())
        .registry_list_projects(None)
        .await?;
    if projects.is_empty() {
        return Ok(vec!["no projects registered".to_string()]);
    }
    let aliases = local_project_paths();
    Ok(projects
        .iter()
        .map(|p| {
            let trusted = aliases
                .get(&p.name)
                .is_some_and(|path| trusty_mpm::core::project_trust::is_project_trusted(path));
            project_list_row(p, trusted)
        })
        .collect())
}

/// One `tm project list` row: the shared registry line plus the trust marker.
pub(crate) fn project_list_row(p: &trusty_mpm::project::Project, trusted: bool) -> String {
    let marker = if trusted { " [mcp-trusted]" } else { "" };
    format!(
        "{}{marker}",
        crate::commands::projects::registry::render_project_line(p)
    )
}

/// `alias → local path` from `~/.trusty-mpm/project-paths.json`, best-effort.
///
/// Why: an unreadable or absent alias store must degrade to "no marker", never
/// fail the listing — the registry rows are the answer the operator asked for.
fn local_project_paths() -> std::collections::HashMap<String, std::path::PathBuf> {
    let root = trusty_mpm::core::paths::FrameworkPaths::default().root;
    trusty_mpm::core::project_aliases::ProjectAliasStore::load(&root)
        .unwrap_or_default()
        .list()
        .iter()
        .map(|e| (e.alias.clone(), e.path.clone()))
        .collect()
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
    let Some(root) = trusty_mpm::core::project_trust::trust_store_root() else {
        anyhow::bail!("cannot resolve the home directory for the project-trust store");
    };
    trust_cmd_in(dir, revoke, &root)
}

/// [`trust_cmd`] against an explicit store root.
///
/// Why (#5544): the store root resolves from `$HOME`, so the only way to test
/// the grant/revoke cycle used to be repointing the process's `$HOME` — a
/// PROCESS-GLOBAL write every sibling test in the `tm` bin target could observe
/// mid-flight, which is the flake class that issue tracks. `$HOME` is read
/// transitively by `dirs::home_dir`, `FrameworkPaths`, and the three-tier agent
/// roster scan, so the exposed readers cannot be enumerated and `#[serial]`
/// cannot cover them. Taking the root as a parameter removes the write.
/// What: identical to [`trust_cmd`] with `root` supplied; production resolves
/// it from [`trusty_mpm::core::project_trust::trust_store_root`].
/// Test: `project_trust_grants_and_revokes`.
pub(crate) fn trust_cmd_in(
    dir: Option<String>,
    revoke: bool,
    root: &std::path::Path,
) -> anyhow::Result<()> {
    let path = resolve_dir(dir)?;
    let mut store = trusty_mpm::core::project_trust::ProjectTrustStore::load(root)?;
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
/// #4832: the directory is created under the project's HARNESS ROOT — the
/// checkout that owns the project — so running `tm project init` from a
/// worktree scaffolds the project's one `.trusty-mpm/`, not a per-branch copy.
/// It also seeds `framework/`, the project-stable config layer the manifest
/// override now lives in.
/// What: creates `.trusty-mpm/sessions/` and `.trusty-mpm/framework/`, and
/// writes `config.toml` (only when absent — never clobbering an edited
/// config); returns a per-path report.
/// Test: `project_init_scaffolds_dotdir`, `project_init_keeps_existing_config`.
pub(crate) fn scaffold_project_dir(project: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let mut report = Vec::new();
    let dotdir = trusty_mpm::core::harness_root::harness_dir(project);
    let sessions = dotdir.join("sessions");
    std::fs::create_dir_all(&sessions)?;
    report.push(format!("\u{2713} {}", sessions.display()));

    let framework = dotdir.join(trusty_mpm::core::harness_root::FRAMEWORK_DIR);
    std::fs::create_dir_all(&framework)?;
    report.push(format!("\u{2713} {}", framework.display()));

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
