//! Default-deny MCP scoping for a tm-launched Claude Code session (#7422).
//!
//! Why: every tm-managed session points `CLAUDE_CONFIG_DIR` at one shared
//! directory whose `.claude.json` holds a single machine-wide `mcpServers` map
//! (ADR-0042). Claude Code connects every entry in that map with no approval,
//! so one `tm mcp add` for one project loaded its server — and its whole tool
//! catalog — into every session on the host. The measured cost on this machine
//! was eight unrelated servers in each session. The owner ruling (2026-09-11)
//! is DEFAULT-DENY: a session loads the trusty-* framework builtins and
//! nothing else unless the operator has trusted this project.
//!
//! IN-REPO DECLARATIONS NEED AN OUT-OF-REPO GRANT. `<cwd>/.mcp.json` and the
//! `[session] mcp_servers` list in `<cwd>/.trusty-mpm.toml` both ship WITH a
//! clone, so neither can be the permission for itself: a pane runs
//! `--dangerously-skip-permissions` against a `.claude.json` that
//! `standalone::trust_seed::preseed_managed_trust` has already marked
//! `hasTrustDialogAccepted`, so an
//! `.mcp.json` entry spelling `{"command": "sh", "args": ["-c", "curl … | sh"]}`
//! would execute on the first `tm run` against a hostile clone. Both surfaces
//! are therefore gated on [`crate::core::project_trust::is_project_trusted`] —
//! the durable USER-scope decision `tm project trust` records under
//! `~/.trusty-tools/trusty-mpm/`, which a repository cannot flip from inside
//! itself (issue #3033, ADR-0042, owner ruling 2026-07-18). An untrusted
//! project loads the builtins only.
//!
//! THE `[session] plugins` HALF IS GATED THE SAME WAY, through
//! [`granted_plugins`]. A plugin brings its own skills, commands and hooks into
//! every session in the project, so an ungated read would let the same hostile
//! clone turn an operator-installed plugin on in that same
//! `--dangerously-skip-permissions` pane. Untrusted grants nothing, which
//! writes every known plugin `false`.
//!
//! What: [`resolve_scope`] composes that set and also reports what it left out;
//! [`provision`] writes it under the tm-managed state root and hands back the
//! path; [`strict_mcp_flag_string`] / [`strict_mcp_argv`] render the
//! `--strict-mcp-config --mcp-config <file>` pair every relocated spawn appends
//! beside its `--setting-sources` flag.
//!
//! THE COMPOSED FILE NEVER LIVES IN THE REPOSITORY. It copies shared entries
//! verbatim — a stdio server's `env` and a remote server's `headers` carry
//! bearer tokens — so it is written to
//! `~/.trusty-tools/trusty-mpm/session-mcp/<hash of cwd>.json` at mode `0600`,
//! not into a working tree where a `git add` or a stray archive would publish
//! it. Keying on the cwd keeps two worktrees of one repository on separate
//! files, and the file is rewritten on every launch.
//!
//! FAIL-CLOSED, in two arms that are deliberately different:
//! - an unreadable or malformed `<cwd>/.mcp.json`, and an untrusted project,
//!   both DEGRADE — the affected servers are dropped, [`McpScope::degraded`]
//!   says which and why, and the launch proceeds with the builtins. The session
//!   loses servers; it never gains one it did not ask for.
//! - an unwritable state directory FAILS the launch ([`ScopeError`]). The only
//!   alternative is spawning with no `--mcp-config`, which is exactly the
//!   unscoped shared map this module exists to stop.
//!
//! Test: `session_mcp_scope_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::core::mcp_config::{BUILTIN_MANAGED_MCP_SERVERS, MCP_JSON, builtin_server_entry};

/// Directory under the tm-managed state root holding every composed file.
///
/// Why: the composed file carries the `env` and `headers` of every server it
/// names — credentials the operator gave tm, never the repository — so it lives
/// beside the rest of tm's user-scope state and never inside a working tree.
/// What: `session-mcp`, under `~/.trusty-tools/trusty-mpm/`.
/// Test: `session_mcp_path_is_outside_the_repository`.
pub const SESSION_MCP_DIR: &str = "session-mcp";

/// Owner-only permissions for the composed file and its directory.
///
/// Why: see [`SESSION_MCP_DIR`] — the file is a credential-bearing record under
/// the operator's own `$HOME`, so it is never group- or world-readable, the
/// same bar `ProjectTrustStore::save` holds its own store to.
/// What: `0o600` for the file, `0o700` for the directory.
#[cfg(unix)]
const OWNER_ONLY_FILE: u32 = 0o600;
/// Owner-only directory permissions — see [`OWNER_ONLY_FILE`].
#[cfg(unix)]
const OWNER_ONLY_DIR: u32 = 0o700;

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
///
/// THIS IS THE RAW, UNGATED READ of a file that ships with the clone. Nothing
/// outside this module should call it: the list only takes effect in a trusted
/// project, so every consumer wants [`granted_plugins`] instead.
/// Test: `opt_in_plugins_reads_the_project_config`.
pub fn opt_in_plugins(project_dir: &Path) -> Vec<String> {
    crate::core::project_config::load_or_report(project_dir)
        .and_then(|cfg| cfg.session)
        .and_then(|s| s.plugins)
        .unwrap_or_default()
}

/// The plugin opt-ins this project has actually been GRANTED.
///
/// Why (#7422): `[session] plugins` is an in-repo declaration, exactly like
/// `[session] mcp_servers` and `.mcp.json`, so it cannot be the permission for
/// itself. An untrusted clone that names a plugin would otherwise turn that
/// plugin's whole skill catalog on inside a pane running
/// `--dangerously-skip-permissions`, and an installed plugin can carry hooks
/// and commands. The server list is gated in
/// [`resolve_scope_with_trust`]; this is the same gate, on the same store, for
/// the other half of the same `[session]` table.
/// What: [`opt_in_plugins`] in a trusted project, an empty vector otherwise —
/// which maps every known plugin to `false`, the default-deny state.
/// Test: `granted_plugins_reads_the_real_trust_store_and_denies_by_default`,
/// `plugin_scope_denies_an_opt_in_from_an_untrusted_project`.
pub fn granted_plugins(project_dir: &Path) -> Vec<String> {
    granted_plugins_with_trust(
        project_dir,
        crate::core::project_trust::is_project_trusted(project_dir),
    )
}

/// [`granted_plugins`] against an explicit trust decision.
///
/// Why: the hermetic seam, mirroring [`resolve_scope_with_trust`] — the trust
/// bit lives under the operator's `$HOME`, so a test of the gate itself would
/// otherwise have to redirect it.
/// What: the declared list when `trusted`, an empty vector when not.
/// Test: `granted_plugins_are_empty_for_an_untrusted_project`,
/// `granted_plugins_pass_through_for_a_trusted_project`.
pub fn granted_plugins_with_trust(project_dir: &Path, trusted: bool) -> Vec<String> {
    if trusted {
        opt_in_plugins(project_dir)
    } else {
        Vec::new()
    }
}

/// Stable per-workspace filename component for `cwd`.
///
/// Why: one composed file per workspace, named without leaking the path into a
/// directory listing and without any character a filesystem could reject. Two
/// worktrees of one repository canonicalize differently, so they never collide.
/// What: the first 32 hex characters of the sha256 of the canonicalized `cwd`
/// (the path as given when it cannot be canonicalized, matching
/// `project_trust::normalize`'s fallback so the two agree on the same
/// directory).
/// Test: `session_mcp_path_is_per_workspace`.
fn workspace_key(cwd: &Path) -> String {
    use sha2::{Digest as _, Sha256};
    let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())[..32].to_owned()
}

/// Where a session's composed MCP config is written, under an explicit root.
///
/// Why: the hermetic core of [`session_mcp_path`]; tests point `root` at a
/// temp dir so they never touch the real `~/.trusty-tools`.
/// What: `<root>/session-mcp/<workspace key>.json`.
/// Test: `session_mcp_path_is_per_workspace`,
/// `session_mcp_path_is_outside_the_repository`.
pub fn session_mcp_path_at(root: &Path, cwd: &Path) -> PathBuf {
    root.join(SESSION_MCP_DIR)
        .join(format!("{}.json", workspace_key(cwd)))
}

/// Where a session's composed MCP config is written.
///
/// Why: one derivation, so the writer and the `--mcp-config` flag can never
/// point at different files.
/// What: `~/.trusty-tools/trusty-mpm/session-mcp/<workspace key>.json`.
/// `None` only when the home directory cannot be resolved — the spawn paths
/// call [`provision_for_spawn`] first, which turns that into a typed error
/// before any argv naming this path is built.
/// Test: `session_mcp_path_is_outside_the_repository`.
pub fn session_mcp_path(cwd: &Path) -> Option<PathBuf> {
    // #7422: the same `~/.trusty-tools/trusty-mpm` root the project-trust store
    // and the managed Claude config dir already nest under.
    trusty_common::crate_config::crate_config_dir(crate::core::trusty_tools_config::CRATE_NAME)
        .map(|root| session_mcp_path_at(&root, cwd))
}

/// The composed file's path, but only for a spawn that relocates its config dir.
///
/// Why: a spawn that leaves `CLAUDE_CONFIG_DIR` alone reads the operator's own
/// `~/.claude.json`, which tm neither owns nor seeds — scoping it would take
/// away servers the operator configured for themselves, which is not what this
/// issue is about.
/// What: [`session_mcp_path`] when `config_dir` is `Some`; `None` otherwise.
/// Test: `scoped_for_declines_a_non_relocated_spawn`.
pub fn scoped_for(cwd: &Path, config_dir: Option<&Path>) -> Option<PathBuf> {
    config_dir?;
    session_mcp_path(cwd)
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
/// What: resolves the project's trust bit from
/// [`crate::core::project_trust::is_project_trusted`] and delegates to
/// [`resolve_scope_with_trust`].
/// Test: `resolve_scope_includes_builtins_and_project_servers`,
/// `resolve_scope_excludes_a_shared_only_server`,
/// `resolve_scope_includes_an_opted_in_shared_server`,
/// `resolve_scope_degrades_on_a_malformed_project_mcp_json`.
pub fn resolve_scope(cwd: &Path, config_dir: &Path) -> McpScope {
    resolve_scope_with_trust(
        cwd,
        config_dir,
        crate::core::project_trust::is_project_trusted(cwd),
    )
}

/// [`resolve_scope`] against an explicit trust decision.
///
/// Why: the trust bit lives under the operator's `$HOME`, so every test of the
/// composition itself would otherwise have to redirect `$HOME`. Splitting the
/// lookup from the decision also makes the gate visible at the one call site
/// that performs it.
/// What: always unions the trusty-* framework builtins
/// ([`BUILTIN_MANAGED_MCP_SERVERS`], defined by [`builtin_server_entry`] rather
/// than copied from the shared map). When `trusted`, it adds the project's own
/// `.mcp.json` and every shared-map entry the project's `[session] mcp_servers`
/// names; every remaining shared-map name lands in `excluded`. A
/// `[session] mcp_servers` entry naming a server the shared map does not hold
/// is ignored — the allowlist grants access to a declaration, it does not
/// create one — and stays out of both lists.
///
/// When NOT `trusted`, both in-repo surfaces are dropped: every shared-map name
/// is `excluded`, and `degraded` names `tm project trust` — but only when the
/// project actually declared something, since a project that declared nothing
/// lost nothing.
/// Test: `resolve_scope_drops_project_mcp_json_for_an_untrusted_project`,
/// `resolve_scope_ignores_opt_ins_for_an_untrusted_project`,
/// `resolve_scope_is_silent_for_an_untrusted_project_that_declares_nothing`.
pub fn resolve_scope_with_trust(cwd: &Path, config_dir: &Path, trusted: bool) -> McpScope {
    let mut servers: Map<String, Value> = Map::new();

    // #7422: the framework builtins come from the canonical entry builder, not
    // from the shared map — a session must keep its trusty-* servers even when
    // the shared `.claude.json` is unreadable.
    for name in BUILTIN_MANAGED_MCP_SERVERS {
        if let Some(entry) = builtin_server_entry(name) {
            servers.insert((*name).to_string(), entry);
        }
    }

    let project = project_servers(cwd);
    let opt_in = opt_in_servers(cwd);
    let mut degraded = None;

    if trusted {
        match project {
            Ok(entries) => {
                for (name, entry) in entries {
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
    } else if project.as_ref().is_ok_and(|e| !e.is_empty())
        || project.is_err()
        || !opt_in.is_empty()
    {
        // #7422: an in-repo declaration is not its own permission — say what was
        // dropped and how to grant it, once, rather than per server.
        let reason = format!(
            "{} is not a trusted project, so its {} and its [session] mcp_servers \
             opt-ins were ignored; run `tm project trust {}` to load them",
            cwd.display(),
            MCP_JSON,
            cwd.display()
        );
        tracing::warn!("session-scoped MCP config degraded: {reason} (#7422)");
        degraded = Some(reason);
    }

    let shared = shared_servers(config_dir);
    let mut excluded: Vec<String> = Vec::new();
    for (name, entry) in &shared {
        if servers.contains_key(name) {
            continue;
        }
        if trusted && opt_in.iter().any(|n| n == name) {
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
/// What: resolves the managed state root, creates `<root>/session-mcp/` at
/// `0700`, serialises `{"mcpServers": …}` from [`resolve_scope`], and
/// overwrites the file at `0600`. Rewritten on every launch, so a stale set
/// from a previous launch can never be reused, and never written inside the
/// repository — see the module doc.
///
/// # Errors
///
/// [`ScopeError::Write`] when the state root cannot be resolved, or the
/// directory or file cannot be written; [`ScopeError::Encode`] when the
/// composed map will not serialise. Both abandon the launch.
/// Test: `provision_writes_the_composed_map`,
/// `provision_errors_when_the_state_dir_is_unwritable`,
/// `provision_writes_an_owner_only_file`, `provision_rewrites_on_every_call`.
pub fn provision(cwd: &Path, config_dir: &Path) -> Result<PathBuf, ScopeError> {
    let path = session_mcp_path(cwd).ok_or_else(|| ScopeError::Write {
        path: PathBuf::from(SESSION_MCP_DIR),
        source: std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve the home directory holding ~/.trusty-tools",
        ),
    })?;
    provision_at(&path, cwd, config_dir)
}

/// The exact bytes a launch writes into the composed session-MCP file (#7678).
///
/// Why: `tm doctor --fix` has to answer "is the file on disk already what a
/// launch would write?" before deciding to rewrite it, and the only honest way
/// to answer that is to compose the same bytes the writer composes. Extracting
/// the body from [`provision_at`] rather than re-deriving it is what keeps the
/// comparison and the write from drifting apart.
/// What: pretty-printed `{"mcpServers": …}` for [`resolve_scope`]'s decision.
///
/// # Errors
///
/// [`ScopeError::Encode`] when the composed map will not serialise.
/// Test: `composed_body_matches_the_provisioned_file`.
pub fn composed_body(cwd: &Path, config_dir: &Path) -> Result<String, ScopeError> {
    let scope = resolve_scope(cwd, config_dir);
    serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": Value::Object(scope.servers),
    }))
    .map_err(|err| ScopeError::Encode(err.to_string()))
}

/// [`provision`] against an already-resolved output path.
///
/// Why: the hermetic core, so tests write into a temp dir without redirecting
/// `$HOME`, and so the one home-resolution failure has exactly one call site.
/// What: see [`provision`]; `path` is the file to write.
///
/// # Errors
///
/// See [`provision`].
/// Test: `provision_writes_the_composed_map`,
/// `provision_writes_an_owner_only_file`.
pub fn provision_at(path: &Path, cwd: &Path, config_dir: &Path) -> Result<PathBuf, ScopeError> {
    // #7678: composed first, and by the one function `tm doctor --fix` compares
    // the on-disk file against — so "already current" and "what gets written"
    // are the same bytes by construction.
    let body = composed_body(cwd, config_dir)?;
    let dir = path.parent().unwrap_or(path).to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|source| ScopeError::Write {
        path: dir.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(OWNER_ONLY_DIR)).map_err(
            |source| ScopeError::Write {
                path: dir.clone(),
                source,
            },
        )?;
    }
    // #7422: create the file owner-only BEFORE any credential-bearing byte
    // reaches it — a write-then-chmod leaves a readable window.
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(OWNER_ONLY_FILE);
    }
    let mut file = options.open(path).map_err(|source| ScopeError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    std::io::Write::write_all(&mut file, body.as_bytes()).map_err(|source| ScopeError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        // An existing file keeps its old mode through `OpenOptions::mode`, which
        // only applies at creation — so restate it for the rewrite case.
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(OWNER_ONLY_FILE)).map_err(
            |source| ScopeError::Write {
                path: path.to_path_buf(),
                source,
            },
        )?;
    }
    Ok(path.to_path_buf())
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
