//! Handlers for `trusty-analyze setup` — one-command integration wiring.
//!
//! Why: getting trusty-analyze usable from Claude Code, Cursor, and claude-mpm
//! involves writing the right MCP / agent / skill config files in the right
//! places. `setup` automates that so users don't have to hand-edit JSON or
//! remember plist paths.
//! What: each `SetupTarget` variant writes (or merges) one configuration
//! artifact via the shared `trusty_common::claude_config` helpers. `All` runs
//! every target in sequence.
//! Test: `mod tests` covers the markdown writers and the path-selection
//! plumbing; the JSON merge logic itself lives in (and is tested by)
//! `trusty_common::claude_config`.

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Subcommand;
use colored::Colorize;

use trusty_analyze::service::DEFAULT_PORT;
use trusty_common::claude_config::{
    default_settings_max_depth, discover_claude_settings, mcp_server_entry, patch_mcp_server,
};

/// Server key used in the `mcpServers` object of every host's config.
const MCP_SERVER_KEY: &str = "trusty-analyzer";

/// CLI command name (matches `[[bin]] name`).
const MCP_SERVER_COMMAND: &str = "trusty-analyze";

/// Args the host should pass to the binary to launch the MCP stdio server.
const MCP_SERVER_ARGS: &[&str] = &["serve", "--mcp"];

/// Targets for `trusty-analyze setup`.
///
/// Why: each integration (Claude Code, Cursor, claude-mpm, the daemon itself)
/// needs a different artifact written to a different place; modelling them as
/// subcommands keeps the wiring explicit and discoverable via `--help`.
/// What: see the per-variant docs. `All` chains every target.
/// Test: exercised by `mod tests` and by the binary integration paths.
#[derive(Subcommand, Debug)]
pub enum SetupTarget {
    /// Configure all targets at once
    All,
    /// Register as MCP server in Claude Code
    ClaudeCode {
        /// Patch every discovered Claude settings file under $HOME instead of
        /// the project's `.mcp.json`.
        #[arg(long)]
        global: bool,
        /// Project directory (default: current dir)
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Register as MCP server in Cursor
    Cursor {
        /// Write to global ~/.cursor/mcp.json instead of project .cursor/mcp.json
        #[arg(long)]
        global: bool,
        /// Project directory (default: current dir)
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Register as claude-mpm agent + skill
    ClaudeMpm,
    /// Install and start the background daemon
    Daemon,
}

/// Dispatch a `trusty-analyze setup <target>` invocation.
///
/// Why: a single entry point keeps `main.rs` thin — it just maps the parsed
/// subcommand here.
/// What: routes each `SetupTarget` to its handler; `All` runs daemon →
/// claude-code → cursor → claude-mpm in order.
/// Test: covered transitively; the merge/writer helpers are unit-tested.
pub async fn run_setup(target: SetupTarget) -> Result<()> {
    match target {
        SetupTarget::All => setup_all().await,
        SetupTarget::ClaudeCode { global, project } => setup_claude_code(global, project),
        SetupTarget::Cursor { global, project } => setup_cursor(global, project),
        SetupTarget::ClaudeMpm => setup_claude_mpm(),
        SetupTarget::Daemon => setup_daemon().await,
    }
}

/// Run every setup target in sequence.
///
/// Why: most users want "wire up everything"; chaining the targets is the
/// one-command path.
/// What: daemon → claude-code (project) → cursor (project) → claude-mpm.
/// Test: covered transitively by the per-target tests.
async fn setup_all() -> Result<()> {
    println!("{}", "Setting up all integrations…".bold());
    setup_daemon().await?;
    setup_claude_code(false, None)?;
    setup_cursor(false, None)?;
    setup_claude_mpm()?;
    println!("{} all integrations configured", "✓".green());
    Ok(())
}

/// Resolve the project directory: the `--project` override or the current dir.
fn project_dir(project: Option<PathBuf>) -> Result<PathBuf> {
    match project {
        Some(p) => Ok(p),
        None => std::env::current_dir().context("resolve current directory"),
    }
}

/// Register trusty-analyze as an MCP server in Claude Code.
///
/// Why: Claude Code auto-discovers `.mcp.json` in the project root and reads
/// `mcpServers` from every `~/.claude/settings.json` / `settings.local.json`
/// under the user's home directory. Patching them with the shared
/// `patch_mcp_server` helper makes the analyzer's MCP tools available.
/// What: in `--global` mode, walks `$HOME` via
/// [`discover_claude_settings`] and patches every discovered settings file. In
/// project mode (default), patches the project's `.mcp.json`.
/// Test: `setup_claude_code_writes_project_mcp_json` covers the project path;
/// the underlying merge logic is tested in `trusty-common`.
fn setup_claude_code(global: bool, project: Option<PathBuf>) -> Result<()> {
    let entry = mcp_server_entry(MCP_SERVER_COMMAND, MCP_SERVER_ARGS);

    if global {
        let home = dirs::home_dir().context("resolve $HOME")?;
        let settings_files = discover_claude_settings(&home, default_settings_max_depth());
        if settings_files.is_empty() {
            // Fall back to creating the canonical global settings file so
            // first-time users still get wired up.
            let fallback = home.join(".claude").join("settings.json");
            let changed = patch_mcp_server(&fallback, MCP_SERVER_KEY, &entry)?;
            report_config_write("Claude Code", &fallback, !changed);
            return Ok(());
        }
        for path in settings_files {
            let changed = patch_mcp_server(&path, MCP_SERVER_KEY, &entry)?;
            report_config_write("Claude Code", &path, !changed);
        }
        Ok(())
    } else {
        let path = project_dir(project)?.join(".mcp.json");
        let changed = patch_mcp_server(&path, MCP_SERVER_KEY, &entry)?;
        report_config_write("Claude Code", &path, !changed);
        Ok(())
    }
}

/// Register trusty-analyze as an MCP server in Cursor.
///
/// Why: Cursor reads `.cursor/mcp.json` (project) and `~/.cursor/mcp.json`
/// (global) for MCP server definitions.
/// What: patches the chosen config file via [`patch_mcp_server`].
/// Test: the underlying merge logic is tested in `trusty-common`.
fn setup_cursor(global: bool, project: Option<PathBuf>) -> Result<()> {
    let path = if global {
        let home = dirs::home_dir().context("resolve $HOME")?;
        home.join(".cursor").join("mcp.json")
    } else {
        project_dir(project)?.join(".cursor").join("mcp.json")
    };
    let entry = mcp_server_entry(MCP_SERVER_COMMAND, MCP_SERVER_ARGS);
    let changed = patch_mcp_server(&path, MCP_SERVER_KEY, &entry)?;
    report_config_write("Cursor", &path, !changed);
    Ok(())
}

/// Print a uniform "configured" / "already configured" line.
fn report_config_write(host: &str, path: &Path, already: bool) {
    if already {
        println!(
            "{} {host}: already configured ({})",
            "✓".green(),
            path.display()
        );
    } else {
        println!(
            "{} {host}: wrote MCP config to {}",
            "✓".green(),
            path.display()
        );
    }
}

/// claude-mpm agent definition (frontmatter-only markdown).
const CLAUDE_MPM_AGENT: &str = r#"---
name: trusty-analyzer
description: >
  Code analysis agent backed by trusty-analyze. Runs complexity analysis,
  smell detection, quality grading, multi-tool linting, and PR review
  against trusty-search indexed projects. Use for: code quality checks,
  pre-merge review, complexity hotspot investigation, fact extraction.
tools:
  - name: analyzer_health
    endpoint: GET http://127.0.0.1:7879/health
  - name: complexity_hotspots
    endpoint: GET http://127.0.0.1:7879/indexes/{index_id}/complexity_hotspots
  - name: find_smells
    endpoint: GET http://127.0.0.1:7879/indexes/{index_id}/smells
  - name: analyze_quality
    endpoint: GET http://127.0.0.1:7879/indexes/{index_id}/quality
  - name: run_diagnostics
    endpoint: GET http://127.0.0.1:7879/indexes/{index_id}/diagnostics
  - name: review_diff
    endpoint: POST http://127.0.0.1:7879/review
  - name: review_github_pr
    endpoint: POST http://127.0.0.1:7879/review/github-pr
---
"#;

/// claude-mpm skill body written to `.claude/skills/trusty-analyzer.md`.
const CLAUDE_MPM_SKILL: &str = r#"# trusty-analyzer skill

Use the trusty-analyze HTTP API (port 7879) for code analysis:

- `GET /health` — check daemon status
- `GET /indexes/:id/complexity_hotspots` — top complex functions
- `GET /indexes/:id/smells` — code smell detection
- `GET /indexes/:id/quality` — overall grade A–F
- `GET /indexes/:id/diagnostics` — multi-tool linter output
- `POST /review` — analyze a git diff (body: unified diff text)
- `POST /review/github-pr` — analyze a GitHub PR by number

Ensure trusty-search is running on port 7878 and trusty-analyze on 7879.
Run `trusty-analyze status` to confirm.
"#;

/// Register trusty-analyze as a claude-mpm agent and skill.
///
/// Why: claude-mpm discovers agents under `~/.claude-mpm/agents/` and skills
/// under `.claude/skills/`; dropping the two files there wires the analyzer
/// into an mpm workflow.
/// What: writes `~/.claude-mpm/agents/trusty-analyzer.md` and
/// `<cwd>/.claude/skills/trusty-analyzer.md`, creating parent dirs.
/// Test: `claude_mpm_writes_both_files` writes into a temp HOME/CWD and checks
/// both files exist with the expected markers.
fn setup_claude_mpm() -> Result<()> {
    let home = dirs::home_dir().context("resolve $HOME")?;
    let agent_path = home
        .join(".claude-mpm")
        .join("agents")
        .join("trusty-analyzer.md");
    write_file_with_parents(&agent_path, CLAUDE_MPM_AGENT)?;
    println!(
        "{} claude-mpm: wrote agent to {}",
        "✓".green(),
        agent_path.display()
    );

    let skill_path = std::env::current_dir()
        .context("resolve current directory")?
        .join(".claude")
        .join("skills")
        .join("trusty-analyzer.md");
    write_file_with_parents(&skill_path, CLAUDE_MPM_SKILL)?;
    println!(
        "{} claude-mpm: wrote skill to {}",
        "✓".green(),
        skill_path.display()
    );
    Ok(())
}

/// Write `content` to `path`, creating any missing parent directories.
fn write_file_with_parents(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create directory {}", parent.display()))?;
        }
    }
    std::fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// TCP-probe whether the analyzer daemon's port is accepting connections.
fn port_reachable(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// Install and start the background daemon (idempotent).
///
/// Why: gives users a one-command "make the daemon run forever" path.
/// What: if the port already answers, returns early. Otherwise installs the
/// launchd service (when not already installed) and polls `/health` for up to
/// 10 s.
/// Test: on a machine with the daemon already up, prints "already running" and
/// exits 0; the launchd path is macOS-only and verified manually.
async fn setup_daemon() -> Result<()> {
    if port_reachable(DEFAULT_PORT) {
        println!(
            "{} daemon already running on port {DEFAULT_PORT}",
            "✓".green()
        );
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        use crate::commands::service::{launchd_plist_path, service_install};

        let plist = launchd_plist_path()?;
        if !plist.exists() {
            service_install()?;
        } else {
            // Plist exists but the port is down — (re)load it.
            let status = std::process::Command::new("launchctl")
                .arg("load")
                .arg(&plist)
                .status()
                .context("launchctl load")?;
            if !status.success() {
                tracing::warn!("launchctl load exited with {status}");
            }
        }

        // Poll /health for up to 10 s.
        // #4392: loopback target — an exported HTTP_PROXY would otherwise make
        // this poll time out against a daemon that started correctly.
        let client = trusty_common::http_client::loopback_client_builder()
            .build()
            .context("build health-poll client")?;
        let url = format!("http://127.0.0.1:{DEFAULT_PORT}/health");
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if let Ok(resp) = client
                .get(&url)
                .timeout(Duration::from_secs(2))
                .send()
                .await
            {
                if resp.status().is_success() {
                    println!("{} daemon installed and healthy", "✓".green());
                    return Ok(());
                }
            }
        }
        println!(
            "{} daemon installed but did not report healthy within 10 s — \
             check `trusty-analyze service logs`",
            "⚠".yellow()
        );
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        println!(
            "{} `setup daemon` installs a macOS launchd service; on this \
             platform start the daemon with `trusty-analyze start` or your \
             distro's service manager.",
            "·".dimmed()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn mcp_server_entry_is_stable() {
        let e = mcp_server_entry(MCP_SERVER_COMMAND, MCP_SERVER_ARGS);
        assert_eq!(e["command"], "trusty-analyze");
        assert_eq!(e["args"][0], "serve");
        assert_eq!(e["args"][1], "--mcp");
        // trusty_common's entry intentionally omits an empty `env` block;
        // assert it stays that way so downstream Claude Code configs don't
        // gain a stray key on every re-run.
        assert!(e.get("env").is_none(), "env must not be set in entry");
    }

    #[test]
    fn setup_claude_code_writes_project_mcp_json() {
        let dir = tempdir().unwrap();
        let result = setup_claude_code(false, Some(dir.path().to_path_buf()));
        assert!(result.is_ok(), "setup failed: {result:?}");
        let written = dir.path().join(".mcp.json");
        assert!(written.exists(), ".mcp.json should be created");
        let text = std::fs::read_to_string(&written).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed["mcpServers"]["trusty-analyzer"]["command"],
            "trusty-analyze"
        );
        assert_eq!(parsed["mcpServers"]["trusty-analyzer"]["args"][1], "--mcp");
    }

    #[test]
    fn setup_claude_code_is_idempotent() {
        let dir = tempdir().unwrap();
        setup_claude_code(false, Some(dir.path().to_path_buf())).unwrap();
        // Second run must succeed and leave the file in a valid state.
        setup_claude_code(false, Some(dir.path().to_path_buf())).unwrap();
        let text = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed["mcpServers"]["trusty-analyzer"]["command"],
            "trusty-analyze"
        );
    }

    #[test]
    fn setup_cursor_writes_project_mcp_json() {
        let dir = tempdir().unwrap();
        let result = setup_cursor(false, Some(dir.path().to_path_buf()));
        assert!(result.is_ok(), "setup_cursor failed: {result:?}");
        let written = dir.path().join(".cursor").join("mcp.json");
        assert!(written.exists(), ".cursor/mcp.json should be created");
    }

    #[test]
    fn write_file_with_parents_creates_dirs() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("c.md");
        write_file_with_parents(&path, "hello").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn claude_mpm_constants_are_well_formed() {
        // Agent is frontmatter; skill is a markdown doc — sanity-check markers.
        assert!(CLAUDE_MPM_AGENT.starts_with("---\n"));
        assert!(CLAUDE_MPM_AGENT.contains("name: trusty-analyzer"));
        assert!(CLAUDE_MPM_AGENT.contains("review_github_pr"));
        assert!(CLAUDE_MPM_SKILL.contains("# trusty-analyzer skill"));
        assert!(CLAUDE_MPM_SKILL.contains("POST /review/github-pr"));
    }
}
