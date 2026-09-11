//! Default-deny MCP scoping for a tm-launched Claude Code session (#7422).
//!
//! Why: every tm-managed session points `CLAUDE_CONFIG_DIR` at one shared
//! directory whose `.claude.json` holds a single machine-wide `mcpServers` map
//! (ADR-0042). Claude Code connects every entry in that map with no approval,
//! so one `tm mcp add` for one project loaded its server — and its whole tool
//! catalog — into every session on the host. The measured cost on this machine
//! was eight unrelated servers in each session. The owner ruling (2026-09-11)
//! is DEFAULT-DENY: a session loads the trusty-* framework builtins, the
//! project's own `.mcp.json`, and nothing else unless the project's committed
//! config names it.
//!
//! What: [`resolve_scope`] composes that set and also reports what it left out;
//! [`provision`] writes it to `<cwd>/.trusty-mpm/session-mcp.json` and hands
//! back the path; [`strict_mcp_flag_string`] / [`strict_mcp_argv`] render the
//! `--strict-mcp-config --mcp-config <file>` pair every relocated spawn appends
//! beside its `--setting-sources` flag. The file is rewritten on every launch
//! and lives in the session's own state directory, so two worktrees of one
//! repository never share one.
//!
//! FAIL-CLOSED, in two arms that are deliberately different:
//! - an unreadable or malformed `<cwd>/.mcp.json` DEGRADES — the project's own
//!   servers are dropped, a warning names the file, and the launch proceeds
//!   with the builtins plus the opt-ins. The session loses servers; it never
//!   gains one it did not ask for.
//! - an unwritable state directory FAILS the launch ([`ScopeError`]). The only
//!   alternative is spawning with no `--mcp-config`, which is exactly the
//!   unscoped shared map this module exists to stop.
//!
//! Test: `session_mcp_scope_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::core::mcp_config::{BUILTIN_MANAGED_MCP_SERVERS, MCP_JSON, builtin_server_entry};

/// Directory, relative to a session's cwd, holding its machine-local state.
///
/// Why: the composed file must not be shared between two worktrees of one
/// repository, so it is resolved from the cwd AS GIVEN rather than through
/// [`crate::core::harness_root::harness_dir`], which deliberately hoists a
/// worktree's state to the owning checkout.
/// What: `.trusty-mpm`.
/// Test: `session_mcp_path_is_per_workspace`.
pub const SESSION_STATE_DIR: &str = ".trusty-mpm";

/// Basename of the composed, session-scoped MCP config.
///
/// Why: one literal, because the writer, the spawn flag, and the scaffolded
/// `.gitignore` entry must agree — a file that is written but not ignored
/// lands in a commit.
/// What: `session-mcp.json`.
/// Test: `session_mcp_path_is_per_workspace`.
pub const SESSION_MCP_FILE: &str = "session-mcp.json";

/// Basename of the shared, tm-managed Claude Code config.
const CLAUDE_JSON: &str = ".claude.json";

/// Why a session-scoped MCP config could not be produced.
///
/// Why: this is the arm that must never degrade into an unscoped launch, so it
/// is a typed error the spawn paths propagate rather than a logged warning.
/// What: `Write` for a state directory or file tm cannot write; `Encode` for a
/// composed map that will not serialise.
/// Test: `provision_errors_when_the_state_dir_is_unwritable`.
#[derive(Debug, thiserror::Error)]
pub enum ScopeError {
    /// The composed file could not be written.
    #[error(
        "could not write the session-scoped MCP config at {}: {source}; \
         refusing to launch with the unscoped shared server map (#7422)",
        .path.display()
    )]
    Write {
        /// The file (or its parent directory) that could not be written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// The composed map could not be serialised.
    #[error("could not serialise the session-scoped MCP config: {0}")]
    Encode(String),
}

/// What a session will and will not load, plus why anything was dropped.
///
/// Why: `tm doctor` and `tm session instructions` both have to tell an operator
/// which servers this project's sessions stopped loading and where to opt them
/// back in. Composing that answer in the same pass that composes the file is
/// what stops the diagnostic and the launch disagreeing.
/// What: `servers` is the `mcpServers` map to write; `included` and `excluded`
/// are sorted name lists; `degraded` carries the one-line reason when the
/// project's own `.mcp.json` could not be read.
/// Test: `resolve_scope_includes_builtins_and_project_servers`,
/// `resolve_scope_excludes_a_shared_only_server`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpScope {
    /// The composed `mcpServers` map, ready to serialise.
    pub servers: Map<String, Value>,
    /// Every server name the session will load, sorted.
    pub included: Vec<String>,
    /// Shared-map names the session will NOT load, sorted.
    pub excluded: Vec<String>,
    /// Set when the project's own `.mcp.json` was unreadable and skipped.
    pub degraded: Option<String>,
}

/// The opt-in server names this project's committed config declares.
///
/// Why: the allowlist is a property of the repository, not the host, so it
/// lives in the committed [`crate::core::project_config::ProjectLevelConfig`]
/// under `[session] mcp_servers`. An absent key means deny-all beyond the
/// builtins and the project's own `.mcp.json`.
/// What: the `session.mcp_servers` list, or an empty vector when the file is
/// absent, unparseable, or declines the key.
/// Test: `opt_in_servers_reads_the_project_config`,
/// `opt_in_servers_is_empty_without_a_config`.
pub fn opt_in_servers(project_dir: &Path) -> Vec<String> {
    crate::core::project_config::load_or_report(project_dir)
        .and_then(|cfg| cfg.session)
        .and_then(|s| s.mcp_servers)
        .unwrap_or_default()
}

/// The opt-in plugin names this project's committed config declares.
///
/// Why: same surface, same reasoning as [`opt_in_servers`] — a plugin's skills
/// cost every session in the project its context whether or not that project
/// uses them.
/// What: the `session.plugins` list, or an empty vector.
/// Test: `opt_in_plugins_reads_the_project_config`.
pub fn opt_in_plugins(project_dir: &Path) -> Vec<String> {
    crate::core::project_config::load_or_report(project_dir)
        .and_then(|cfg| cfg.session)
        .and_then(|s| s.plugins)
        .unwrap_or_default()
}

/// Where a session's composed MCP config is written.
///
/// Why: one derivation, so the writer and the `--mcp-config` flag can never
/// point at different files.
/// What: `<cwd>/.trusty-mpm/session-mcp.json`.
/// Test: `session_mcp_path_is_per_workspace`.
pub fn session_mcp_path(cwd: &Path) -> PathBuf {
    cwd.join(SESSION_STATE_DIR).join(SESSION_MCP_FILE)
}

/// The composed file's path, but only for a spawn that relocates its config dir.
///
/// Why: a spawn that leaves `CLAUDE_CONFIG_DIR` alone reads the operator's own
/// `~/.claude.json`, which tm neither owns nor seeds — scoping it would take
/// away servers the operator configured for themselves, which is not what this
/// issue is about.
/// What: `Some(path)` when `config_dir` is `Some`; `None` otherwise.
/// Test: `scoped_for_declines_a_non_relocated_spawn`.
pub fn scoped_for(cwd: &Path, config_dir: Option<&Path>) -> Option<PathBuf> {
    config_dir.map(|_| session_mcp_path(cwd))
}

/// Render the strict-MCP flag pair for a shell command line.
///
/// Why: the flags must appear wherever
/// [`crate::core::model_inject::setting_sources_flag`] appears, so both are
/// rendered by a pure function of the same spawn posture.
/// What: `" --strict-mcp-config --mcp-config '<path>'"` (leading space,
/// single-quoted path) when `mcp_config` is `Some`; the empty string otherwise.
/// Test: `strict_mcp_flag_string_quotes_the_path`,
/// `strict_mcp_flag_string_is_empty_without_a_file`.
pub fn strict_mcp_flag_string(mcp_config: Option<&Path>) -> String {
    match mcp_config {
        None => String::new(),
        Some(path) => format!(
            " --strict-mcp-config --mcp-config {}",
            crate::core::spawn_disclaim::pane::shell_single_quote(&path.display().to_string())
        ),
    }
}

/// Render the strict-MCP flag pair as argv tokens.
///
/// Why: the in-place relaunch path `exec`s directly with no shell in between,
/// so its path token must NOT be quoted — quoting would embed literal quote
/// characters in the filename Claude Code then fails to open.
/// What: `["--strict-mcp-config", "--mcp-config", "<path>"]` when `mcp_config`
/// is `Some`; an empty vector otherwise.
/// Test: `strict_mcp_argv_is_three_unquoted_tokens`.
pub fn strict_mcp_argv(mcp_config: Option<&Path>) -> Vec<String> {
    match mcp_config {
        None => Vec::new(),
        Some(path) => vec![
            "--strict-mcp-config".to_owned(),
            "--mcp-config".to_owned(),
            path.display().to_string(),
        ],
    }
}

/// Read the shared, tm-managed `mcpServers` map without quarantining it.
///
/// Why: [`crate::core::mcp_config::list_servers`] renames a malformed file to
/// `.claude.json.corrupt`. That is right for an operator-invoked `tm mcp`
/// command and wrong here — this runs on every launch against a file that also
/// holds OAuth state, so a per-launch quarantine would be a per-launch
/// data-loss event.
/// What: the top-level `mcpServers` object, or an empty map for an absent,
/// unreadable, malformed, or server-less file.
/// Test: `shared_servers_tolerates_a_malformed_config`.
fn shared_servers(config_dir: &Path) -> Map<String, Value> {
    let path = config_dir.join(CLAUDE_JSON);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Map::new();
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .as_ref()
        .and_then(|v| v.get("mcpServers"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// Read the project's own `.mcp.json` servers, distinguishing absent from broken.
///
/// Why: an absent file is the common case and carries no information; a file
/// that exists but will not parse is a fact the operator has to be told,
/// because the servers they declared are about to not load.
/// What: `Ok(map)` for an absent file (empty) or a parsed one; `Err(reason)`
/// for a file that exists and cannot be read or parsed.
/// Test: `resolve_scope_degrades_on_a_malformed_project_mcp_json`.
fn project_servers(cwd: &Path) -> Result<Map<String, Value>, String> {
    let path = cwd.join(MCP_JSON);
    let text = match std::fs::read_to_string(&path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(err) => return Err(format!("{} could not be read: {err}", path.display())),
        Ok(text) => text,
    };
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|err| format!("{} is not valid JSON: {err}", path.display()))?;
    Ok(parsed
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

/// Compose the set of MCP servers a session in `cwd` may load.
///
/// Why: see the module doc — this is the default-deny decision itself, in one
/// place, so the launch, `tm doctor`, and `tm session instructions` all read
/// the same answer.
/// What: unions the trusty-* framework builtins
/// ([`BUILTIN_MANAGED_MCP_SERVERS`], defined by
/// [`builtin_server_entry`] rather than copied from the shared map), the
/// project's own `.mcp.json`, and every shared-map entry the project's
/// `[session] mcp_servers` names. Reports every remaining shared-map name in
/// `excluded`. A `[session] mcp_servers` entry naming a server the shared map
/// does not hold is ignored — there is nothing to include — and stays out of
/// both lists.
/// Test: `resolve_scope_includes_builtins_and_project_servers`,
/// `resolve_scope_excludes_a_shared_only_server`,
/// `resolve_scope_includes_an_opted_in_shared_server`,
/// `resolve_scope_degrades_on_a_malformed_project_mcp_json`.
pub fn resolve_scope(cwd: &Path, config_dir: &Path) -> McpScope {
    let mut servers: Map<String, Value> = Map::new();

    // #7422: the framework builtins come from the canonical entry builder, not
    // from the shared map — a session must keep its trusty-* servers even when
    // the shared `.claude.json` is unreadable.
    for name in BUILTIN_MANAGED_MCP_SERVERS {
        if let Some(entry) = builtin_server_entry(name) {
            servers.insert((*name).to_string(), entry);
        }
    }

    let mut degraded = None;
    match project_servers(cwd) {
        Ok(project) => {
            for (name, entry) in project {
                servers.insert(name, entry);
            }
        }
        Err(reason) => {
            tracing::warn!(
                "session-scoped MCP config degraded: {reason}; \
                 this project's own servers will not load (#7422)"
            );
            degraded = Some(reason);
        }
    }

    let shared = shared_servers(config_dir);
    let opt_in = opt_in_servers(cwd);
    let mut excluded: Vec<String> = Vec::new();
    for (name, entry) in &shared {
        if servers.contains_key(name) {
            continue;
        }
        if opt_in.iter().any(|n| n == name) {
            servers.insert(name.clone(), entry.clone());
        } else {
            excluded.push(name.clone());
        }
    }

    let mut included: Vec<String> = servers.keys().cloned().collect();
    included.sort();
    excluded.sort();
    McpScope {
        servers,
        included,
        excluded,
        degraded,
    }
}

/// Compose and write a session's MCP config, returning the file to point at.
///
/// Why: the write is the step that can fail closed, so it is separated from the
/// pure [`resolve_scope`] decision and from the pure flag rendering. Callers
/// run it BEFORE building a launch command and propagate its error, which is
/// what makes "never silently fall back to the unscoped shared map" mechanical
/// rather than a convention.
/// What: creates `<cwd>/.trusty-mpm/`, serialises `{"mcpServers": …}` from
/// [`resolve_scope`], and overwrites the file. Rewritten on every launch, so a
/// stale set from a previous launch can never be reused.
///
/// # Errors
///
/// [`ScopeError::Write`] when the state directory or the file cannot be
/// written; [`ScopeError::Encode`] when the composed map will not serialise.
/// Both abandon the launch — see the module doc.
/// Test: `provision_writes_the_composed_map`,
/// `provision_errors_when_the_state_dir_is_unwritable`,
/// `provision_rewrites_on_every_call`.
pub fn provision(cwd: &Path, config_dir: &Path) -> Result<PathBuf, ScopeError> {
    let scope = resolve_scope(cwd, config_dir);
    let path = session_mcp_path(cwd);
    let dir = path.parent().unwrap_or(cwd).to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|source| ScopeError::Write {
        path: dir.clone(),
        source,
    })?;
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": Value::Object(scope.servers),
    }))
    .map_err(|err| ScopeError::Encode(err.to_string()))?;
    std::fs::write(&path, body).map_err(|source| ScopeError::Write {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// Provision a session's MCP config when the spawn relocates its config dir.
///
/// Why: every spawn path repeats the same two lines — skip when `config_dir` is
/// `None`, otherwise provision and propagate. One helper keeps the skip
/// condition identical across them.
/// What: `Ok(None)` when `config_dir` is `None`; otherwise
/// [`provision`]'s result wrapped in `Some`.
///
/// # Errors
///
/// Propagates [`provision`]'s error unchanged.
/// Test: `provision_for_spawn_declines_a_non_relocated_spawn`.
pub fn provision_for_spawn(
    cwd: &Path,
    config_dir: Option<&Path>,
) -> Result<Option<PathBuf>, ScopeError> {
    match config_dir {
        None => Ok(None),
        Some(dir) => provision(cwd, dir).map(Some),
    }
}

#[cfg(test)]
#[path = "session_mcp_scope_tests.rs"]
mod tests;
