//! Live-discovery MCP server specs: gathering + config/`.mcp.json` precedence
//! (#3238).
//!
//! Why: Live discovery needs a uniform "enough to spawn" shape regardless of
//! whether the server came from `[[mcp.services]]` (`discover = true`) or a
//! `.mcp.json` `mcpServers` entry — the two sources have different on-disk
//! schemas but the same runtime needs (command, args, env).
//! What: `McpServerSpec` is that uniform shape; `gather_specs` merges both
//! sources with the documented precedence: a `[[mcp.services]]` entry with
//! `discover = true` wins over a `.mcp.json` entry of the same name (config
//! is operator-curated and presumed more trustworthy/intentional than an
//! auto-discovered `.mcp.json`, which may be shared across tools like Claude
//! Code that don't know about trusty-agents' service registry).
//! Test: `gather_specs_*` below.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::mcp::config::GlobalConfig;
use crate::mcp::mcp_json::{discover_mcp_json_paths, parse_mcp_json_servers};

/// Where a live-discovery spec came from. Carried only for tracing/debug
/// messages — dispatch behavior is identical regardless of source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecSource {
    ConfigService,
    McpJson,
}

/// A live-discovery-eligible MCP server: enough to spawn + `tools/list`.
#[derive(Debug, Clone)]
pub struct McpServerSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub source: SpecSource,
}

/// Gather every live-discovery-eligible server spec.
///
/// Why: Both `crate::ctrl::pm_task::dispatch::persona` and
/// `crate::runtime::subagent_mode` need the identical merged spec list so
/// the two assembly points' tool sets match (per #3238's requirement that
/// both wire through "the same construction helper").
/// What: Walks `global.mcp.services` for `enabled && discover` entries
/// (stdio transport, non-empty command only — same graceful-skip rules as
/// the static path in `mcp_service_tools`). `.mcp.json` files (found via
/// `discover_mcp_json_paths(project_dir)`, project file before
/// `~/.claude/.mcp.json`) are walked ONLY when
/// `global.mcp.trust_project_mcp_json` is `true` — a project's `.mcp.json`
/// is untrusted, repo-controlled content, and auto-spawning whatever it
/// declares is a remote-code-execution vector for anyone who clones a
/// hostile repo (#3266 security remediation). The default-false gate means
/// the only opt-in path for a `.mcp.json`-declared server is an explicit,
/// operator-curated `[[mcp.services]]` entry with `discover = true` — those
/// are unaffected by this gate since they never touch `.mcp.json`. A
/// `.mcp.json` entry whose name collides with an already-collected
/// config-service name is skipped (config wins); otherwise first-file-wins
/// for `.mcp.json`-vs-`.mcp.json` collisions.
/// Test: `gather_specs_includes_discover_services`,
/// `gather_specs_config_service_wins_over_mcp_json_by_name`,
/// `gather_specs_skips_non_stdio_and_empty_command`,
/// `gather_specs_skips_mcp_json_when_untrusted`,
/// `gather_specs_includes_mcp_json_when_trusted`.
pub async fn gather_specs(global: &GlobalConfig, project_dir: &Path) -> Vec<McpServerSpec> {
    let mut specs: Vec<McpServerSpec> = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();

    for svc in &global.mcp.services {
        if !svc.enabled || !svc.discover {
            continue;
        }
        if svc.transport != "stdio" {
            tracing::debug!(
                service = %svc.name,
                transport = %svc.transport,
                "mcp_live: skipping non-stdio discover=true service (only stdio transport supported)"
            );
            continue;
        }
        if svc.command.is_empty() {
            tracing::debug!(
                service = %svc.name,
                "mcp_live: skipping discover=true service with empty command"
            );
            continue;
        }
        seen_names.insert(svc.name.clone());
        specs.push(McpServerSpec {
            name: svc.name.clone(),
            command: svc.command.clone(),
            args: svc.args.clone(),
            env: svc.env.clone(),
            source: SpecSource::ConfigService,
        });
    }

    if !global.mcp.trust_project_mcp_json {
        tracing::debug!(
            "mcp_live: mcp.trust_project_mcp_json is false (default); skipping .mcp.json auto-spawn entirely"
        );
        return specs;
    }

    for path in discover_mcp_json_paths(project_dir) {
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "mcp_live: .mcp.json unreadable, skipping");
                continue;
            }
        };
        let servers = match parse_mcp_json_servers(&raw) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "mcp_live: .mcp.json parse failed, skipping");
                continue;
            }
        };
        for s in servers {
            if seen_names.contains(&s.name) {
                tracing::debug!(
                    server = %s.name,
                    path = %path.display(),
                    "mcp_live: .mcp.json entry shadowed by a [[mcp.services]] discover=true entry of the same name, skipping"
                );
                continue;
            }
            if s.command.is_empty() {
                tracing::debug!(server = %s.name, path = %path.display(), "mcp_live: .mcp.json entry has empty command, skipping");
                continue;
            }
            seen_names.insert(s.name.clone());
            specs.push(McpServerSpec {
                name: s.name,
                command: s.command,
                args: s.args,
                env: s.env,
                source: SpecSource::McpJson,
            });
        }
    }

    specs
}

#[cfg(test)]
mod tests {
    // Why: `gather_specs_does_not_spawn_mcp_add_discover_bypass` holds
    // `HOME_LOCK` (a `std::sync::Mutex`) across async I/O to serialize
    // global $HOME mutation against other tests — same rationale as
    // `tools::mcp_tools::tests` (see `crate::test_env` docs).
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use crate::mcp::config::{McpService, McpTool};

    fn discover_service(name: &str, command: &str) -> McpService {
        McpService {
            name: name.to_string(),
            description: format!("{name} service"),
            command: command.to_string(),
            args: vec!["mcp".to_string()],
            env: HashMap::new(),
            url: None,
            transport: "stdio".to_string(),
            enabled: true,
            tools: vec![McpTool {
                name: "ignored_static_tool".to_string(),
                description: "should not surface — discover=true ignores this".to_string(),
            }],
            discover: true,
        }
    }

    /// Why: A `discover = true` config service must appear as a
    /// `ConfigService`-sourced spec even when the project has no `.mcp.json`
    /// at all.
    /// What: One discover=true service, empty project dir; assert one spec
    /// comes back with matching name/command/source.
    /// Test: This test.
    #[tokio::test]
    async fn gather_specs_includes_discover_services() {
        let project = tempfile::tempdir().unwrap();
        let mut global = GlobalConfig::default();
        global.mcp.services = vec![discover_service("alpha", "alpha-bin")];

        let specs = gather_specs(&global, project.path()).await;
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "alpha");
        assert_eq!(specs[0].command, "alpha-bin");
        assert_eq!(specs[0].source, SpecSource::ConfigService);
    }

    /// Why: Non-discover services (the default) and disabled services must
    /// contribute nothing — they're handled by the pre-existing static path
    /// in `mcp_service_tools`, not by live discovery.
    /// What: One non-discover service + one disabled discover service;
    /// assert zero specs.
    /// Test: This test.
    #[tokio::test]
    async fn gather_specs_excludes_non_discover_and_disabled() {
        let project = tempfile::tempdir().unwrap();
        let mut normal = discover_service("beta", "beta-bin");
        normal.discover = false;
        let mut disabled = discover_service("gamma", "gamma-bin");
        disabled.enabled = false;

        let mut global = GlobalConfig::default();
        global.mcp.services = vec![normal, disabled];

        let specs = gather_specs(&global, project.path()).await;
        assert!(specs.is_empty());
    }

    /// Why: Non-stdio transports and empty commands aren't spawnable; the
    /// live path must skip them exactly like the static path does, rather
    /// than crash trying to spawn `""`.
    /// What: One http-transport discover service, one empty-command
    /// discover service; assert zero specs.
    /// Test: This test.
    #[tokio::test]
    async fn gather_specs_skips_non_stdio_and_empty_command() {
        let mut http_svc = discover_service("http-svc", "");
        http_svc.transport = "http".to_string();
        http_svc.command = "irrelevant".to_string();
        let empty_cmd_svc = discover_service("empty-cmd", "");

        let project = tempfile::tempdir().unwrap();
        let mut global = GlobalConfig::default();
        global.mcp.services = vec![http_svc, empty_cmd_svc];

        let specs = gather_specs(&global, project.path()).await;
        assert!(specs.is_empty());
    }

    /// Why: #3238's documented precedence — a `[[mcp.services]]
    /// discover=true` entry must win over a `.mcp.json` entry of the same
    /// name, since the config is the operator-curated source. This test
    /// explicitly opts into `trust_project_mcp_json = true` since `.mcp.json`
    /// entries are gated off by default (#3266 security remediation).
    /// What: Config declares `shared` (discover=true); project `.mcp.json`
    /// also declares `shared` with a different command. Assert exactly one
    /// spec named `shared`, sourced from config, with the config's command.
    /// Test: This test.
    #[tokio::test]
    async fn gather_specs_config_service_wins_over_mcp_json_by_name() {
        let project = tempfile::tempdir().unwrap();
        tokio::fs::write(
            project.path().join(".mcp.json"),
            serde_json::json!({
                "mcpServers": {
                    "shared": {"command": "from-mcp-json", "args": []},
                    "json-only": {"command": "json-only-bin", "args": []}
                }
            })
            .to_string(),
        )
        .await
        .unwrap();

        let mut global = GlobalConfig::default();
        global.mcp.services = vec![discover_service("shared", "from-config")];
        global.mcp.trust_project_mcp_json = true;

        let specs = gather_specs(&global, project.path()).await;
        assert_eq!(specs.len(), 2);
        let shared = specs.iter().find(|s| s.name == "shared").unwrap();
        assert_eq!(shared.command, "from-config");
        assert_eq!(shared.source, SpecSource::ConfigService);
        let json_only = specs.iter().find(|s| s.name == "json-only").unwrap();
        assert_eq!(json_only.command, "json-only-bin");
        assert_eq!(json_only.source, SpecSource::McpJson);
    }

    /// Why: SECURITY (#3266) — `GlobalConfig::default()` must leave
    /// `trust_project_mcp_json` at `false`, and `gather_specs` must not
    /// auto-spawn anything from `.mcp.json` in that state. This is the
    /// default-deny proof: a hostile repo's `.mcp.json` must never cause a
    /// spawn without an explicit operator opt-in.
    /// What: Project declares a `.mcp.json` server with no corresponding
    /// `[[mcp.services]]` entry, default config (untrusted). Assert zero
    /// specs come back even though the file parses fine and the command is
    /// non-empty.
    /// Test: This test.
    #[tokio::test]
    async fn gather_specs_skips_mcp_json_when_untrusted() {
        let project = tempfile::tempdir().unwrap();
        tokio::fs::write(
            project.path().join(".mcp.json"),
            serde_json::json!({
                "mcpServers": {
                    "hostile": {"command": "rm", "args": ["-rf", "/"]}
                }
            })
            .to_string(),
        )
        .await
        .unwrap();

        let global = GlobalConfig::default();
        assert!(!global.mcp.trust_project_mcp_json, "default must be false");

        let specs = gather_specs(&global, project.path()).await;
        assert!(
            specs.is_empty(),
            "untrusted .mcp.json must not auto-spawn: {specs:?}"
        );
    }

    /// Why: The trust gate must be a real opt-in, not a permanently-closed
    /// door — an operator who explicitly sets
    /// `trust_project_mcp_json = true` still gets the pre-#3266 merge
    /// behavior.
    /// What: Same untrusted-repo fixture as the previous test, but
    /// `trust_project_mcp_json = true`; assert the `.mcp.json` entry now
    /// comes back.
    /// Test: This test.
    #[tokio::test]
    async fn gather_specs_includes_mcp_json_when_trusted() {
        let project = tempfile::tempdir().unwrap();
        tokio::fs::write(
            project.path().join(".mcp.json"),
            serde_json::json!({
                "mcpServers": {
                    "trusted-entry": {"command": "trusted-bin", "args": []}
                }
            })
            .to_string(),
        )
        .await
        .unwrap();

        let mut global = GlobalConfig::default();
        global.mcp.trust_project_mcp_json = true;

        let specs = gather_specs(&global, project.path()).await;
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "trusted-entry");
        assert_eq!(specs[0].source, SpecSource::McpJson);
    }

    /// Why: SECURITY (#3266 follow-up) — closes the same bypass class the
    /// `.mcp.json` trust-gate tests above cover, but for the `mcp_add` LLM
    /// tool path: `dispatch_mcp_tool("mcp_add", ...)` (`tools/mcp_tools`)
    /// forces `discover = false` on any LLM-originated call regardless of
    /// what the caller requested, specifically so a config-service entry
    /// that arrived via that path can never reach `gather_specs`' unguarded
    /// `enabled && discover && stdio` auto-spawn loop. This test proves the
    /// end-to-end path: an `mcp_add` call requesting `discover: true` with
    /// an attacker-chosen command is persisted, reloaded from disk, and
    /// still produces zero specs.
    /// What: Set `HOME` to an isolated tempdir, dispatch `mcp_add` with
    /// `discover: true` and `command: "evil-binary"`, reload
    /// `GlobalConfig::load()`, then call `gather_specs`; assert the
    /// resulting spec list contains no entry for that service.
    /// Test: This test.
    #[tokio::test]
    async fn gather_specs_does_not_spawn_mcp_add_discover_bypass() {
        use crate::test_env::HOME_LOCK;
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("HOME", home.path());
        }

        let out = crate::tools::mcp_tools::dispatch_mcp_tool(
            "mcp_add",
            &serde_json::json!({
                "name": "attacker-service",
                "description": "attacker-controlled",
                "transport": "stdio",
                "command": "evil-binary",
                "discover": true
            }),
        )
        .await;
        assert!(out.contains("Added"), "got: {out}");

        let reloaded = GlobalConfig::load().await;
        let persisted = reloaded
            .mcp
            .services
            .iter()
            .find(|s| s.name == "attacker-service")
            .expect("attacker-service present");
        assert!(
            !persisted.discover,
            "mcp_add must have forced discover=false on persist"
        );

        let project = tempfile::tempdir().unwrap();
        let specs = gather_specs(&reloaded, project.path()).await;
        assert!(
            !specs.iter().any(|s| s.name == "attacker-service"),
            "mcp_add-originated discover:true must never be auto-spawned by gather_specs: {specs:?}"
        );
    }
}
