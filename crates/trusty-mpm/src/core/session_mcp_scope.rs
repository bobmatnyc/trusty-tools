//! The MCP servers a tm-launched Claude Code session adds, and how (#7892).
//!
//! Why: tm points `CLAUDE_CONFIG_DIR` at one protected directory (ADR-0042)
//! whose `.claude.json` holds the operator's own USER-SCOPE `mcpServers` map.
//! Owner ruling 2026-09-14 (#7892, superseding the 2026-09-11 default-deny
//! ruling of #7422 and PR #7468): that map has STANDARD Claude Code user-scope
//! semantics — every server in it loads in every tm session, with no per-project
//! grant. The per-project model it replaces cost an operator eight working
//! servers in one session and four commands to get them back.
//!
//! What: tm's own trusty-* builtins are added ON TOP, through the one file this
//! module composes and hands to `--mcp-config`. That flag is ADDITIVE in Claude
//! Code: it merges its servers with the user-scope map rather than replacing it.
//! `--strict-mcp-config`, which would have suppressed the user scope, is gone
//! (#7892) — [`mcp_config_flag_string`] and [`mcp_config_argv`] render the
//! `--mcp-config <file>` pair alone.
//!
//! WHAT THIS MODULE NO LONGER DOES. It does not read the operator's user-scope
//! map to filter it, does not copy it into the composed file, and does not
//! consult `tm project trust`, `tm mcp share`, or `[session] mcp_servers` for
//! the server half. A project's own `<cwd>/.mcp.json` is Claude Code's to
//! approve, through its native `enableAllProjectMcpServers` /
//! `enabledMcpjsonServers` settings or its own prompt — tm neither pre-approves
//! nor suppresses it. [`crate::core::project_mcp_approval`] only REPORTS that
//! native state for `tm doctor` and `tm mcp list`.
//!
//! THE PLUGIN HALF OF `[session]` IS UNCHANGED and still gated on
//! [`crate::core::project_trust::is_project_trusted`], through
//! [`granted_plugins`]. Claude Code has no per-project plugin approval —
//! `enabledPlugins` is settings-only — so there is no native standard for
//! #7892 to defer to, and a clone that names an operator-installed plugin would
//! otherwise turn its skills, commands and hooks on inside a
//! `--dangerously-skip-permissions` pane (#7422, issue #3033).
//!
//! THE COMPOSED FILE STILL NEVER LIVES IN THE REPOSITORY. It is written to
//! `~/.trusty-tools/trusty-mpm/session-mcp/<hash of cwd>.json` at mode `0600`,
//! keyed on the cwd so two worktrees of one repository never share a file, and
//! rewritten on every launch.
//!
//! FAIL-OPEN (#7892), where #7422 failed closed: a `.claude.json` tm cannot
//! read or parse no longer stops anything, because nothing in the composition
//! depends on it. The launch proceeds with the builtins, and
//! [`McpScope::degraded`] carries a one-line warning naming the file, which
//! [`provision_at`] prints. An unwritable state directory is still the one
//! hard failure ([`ScopeError`]) — without the composed file the session would
//! silently lose tm's own builtins.
//! Test: `session_mcp_scope_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::core::mcp_config::{BUILTIN_MANAGED_MCP_SERVERS, builtin_server_entry};

/// Directory under the tm-managed state root holding every composed file.
///
/// Why: the file is tm-owned launch state, so it lives beside the rest of tm's
/// user-scope state and never inside a working tree.
/// What: `session-mcp`, under `~/.trusty-tools/trusty-mpm/`.
/// Test: `session_mcp_path_is_outside_the_repository`.
pub const SESSION_MCP_DIR: &str = "session-mcp";

/// Owner-only permissions for the composed file and its directory.
///
/// Why: the file is a launch-state record under the operator's own `$HOME`, so
/// it is never group- or world-readable, the same bar `ProjectTrustStore::save`
/// holds its own store to.
/// What: `0o600` for the file, `0o700` for the directory.
#[cfg(unix)]
const OWNER_ONLY_FILE: u32 = 0o600;
/// Owner-only directory permissions — see [`OWNER_ONLY_FILE`].
#[cfg(unix)]
const OWNER_ONLY_DIR: u32 = 0o700;

/// Basename of the protected, tm-managed Claude Code config.
const CLAUDE_JSON: &str = ".claude.json";

/// Why a session-scoped MCP config could not be produced.
///
/// Why: without the composed file a session loses tm's own builtins, so this
/// stays a typed error the spawn paths propagate rather than a logged warning.
/// What: `Write` for a state directory or file tm cannot write; `Encode` for a
/// composed map that will not serialise.
/// Test: `provision_errors_when_the_state_dir_is_unwritable`.
#[derive(Debug, thiserror::Error)]
pub enum ScopeError {
    /// The composed file could not be written.
    #[error(
        "could not write the session MCP config at {}: {source}; \
         refusing to launch without tm's own builtin servers (#7892)",
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
    #[error("could not serialise the session MCP config: {0}")]
    Encode(String),
}

/// What a session loads: tm's builtins, plus the user scope Claude Code adds.
///
/// Why: `tm doctor`, `tm mcp list` and `tm session instructions` all have to
/// tell an operator which servers a session in this project actually gets.
/// Since #7892 that is an additive answer with no exclusions in it, so the type
/// carries no `excluded` list — there is nothing tm scopes out.
/// What: `servers` is the `mcpServers` map tm writes and passes to
/// `--mcp-config`; `included` names it, sorted; `user_scope` names the
/// operator's own entries in `$CLAUDE_CONFIG_DIR/.claude.json`, which Claude
/// Code loads itself and tm only reports; `degraded` is set only when that file
/// exists and could not be read or parsed.
/// Test: `resolve_scope_is_the_builtins_plus_the_reported_user_scope`,
/// `resolve_scope_never_filters_the_user_scope`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpScope {
    /// The composed `mcpServers` map tm writes — the builtins, and only those.
    pub servers: Map<String, Value>,
    /// Every server name tm's own composed file carries, sorted.
    pub included: Vec<String>,
    /// The operator's user-scope server names, reported not composed, sorted.
    pub user_scope: Vec<String>,
    /// Set when the protected `.claude.json` exists but could not be read.
    pub degraded: Option<String>,
}

/// The opt-in plugin names this project's committed config declares.
///
/// Why: a plugin's skills cost every session in the project its context whether
/// or not that project uses them, and Claude Code has no per-project approval
/// for one.
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
/// Why (#7422, retained by #7892): `[session] plugins` is an in-repo
/// declaration, so it cannot be the permission for itself. The server half of
/// that table stopped needing a grant under #7892 because Claude Code has its
/// own user-scope and `.mcp.json` approval semantics to defer to; plugins have
/// none, so this gate stays.
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
/// Why: the hermetic seam — the trust bit lives under the operator's `$HOME`,
/// so a test of the gate itself would otherwise have to redirect it.
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
    // The same `~/.trusty-tools/trusty-mpm` root the project-trust store and the
    // managed Claude config dir already nest under.
    trusty_common::crate_config::crate_config_dir(crate::core::trusty_tools_config::CRATE_NAME)
        .map(|root| session_mcp_path_at(&root, cwd))
}

/// The composed file's path, but only for a spawn that relocates its config dir.
///
/// Why: a spawn that leaves `CLAUDE_CONFIG_DIR` alone reads the operator's own
/// `~/.claude.json`, which already declares whatever they want everywhere; tm
/// adds nothing to it.
/// What: [`session_mcp_path`] when `config_dir` is `Some`; `None` otherwise.
/// Test: `scoped_for_declines_a_non_relocated_spawn`.
pub fn scoped_for(cwd: &Path, config_dir: Option<&Path>) -> Option<PathBuf> {
    config_dir?;
    session_mcp_path(cwd)
}

/// Render the `--mcp-config` flag pair for a shell command line.
///
/// Why: the flag must appear wherever
/// [`crate::core::model_inject::setting_sources_flag`] appears, so both are
/// rendered by a pure function of the same spawn posture.
/// What: `" --mcp-config '<path>'"` (leading space, single-quoted path) when
/// `mcp_config` is `Some`; the empty string otherwise. // #7892: no
/// `--strict-mcp-config` — that flag suppressed the operator's user scope.
/// Test: `mcp_config_flag_string_quotes_the_path`,
/// `mcp_config_flag_string_is_empty_without_a_file`,
/// `mcp_config_flag_string_never_renders_strict`.
pub fn mcp_config_flag_string(mcp_config: Option<&Path>) -> String {
    match mcp_config {
        None => String::new(),
        Some(path) => format!(
            " --mcp-config {}",
            crate::core::spawn_disclaim::pane::shell_single_quote(&path.display().to_string())
        ),
    }
}

/// Render the `--mcp-config` flag pair as argv tokens.
///
/// Why: the in-place relaunch path `exec`s directly with no shell in between,
/// so its path token must NOT be quoted — quoting would embed literal quote
/// characters in the filename Claude Code then fails to open.
/// What: `["--mcp-config", "<path>"]` when `mcp_config` is `Some`; an empty
/// vector otherwise. // #7892: no `--strict-mcp-config`.
/// Test: `mcp_config_argv_is_two_unquoted_tokens`,
/// `mcp_config_argv_never_carries_strict_mcp_config`.
pub fn mcp_config_argv(mcp_config: Option<&Path>) -> Vec<String> {
    match mcp_config {
        None => Vec::new(),
        Some(path) => vec!["--mcp-config".to_owned(), path.display().to_string()],
    }
}

/// The operator's user-scope server names, read fail-open (#7892).
///
/// Why: these load in every session whatever tm does, so this read exists ONLY
/// to report them. [`crate::core::mcp_config::list_servers`] renames a malformed
/// file to `.claude.json.corrupt`, which is right for an operator-invoked
/// `tm mcp` command and wrong on every launch against a file that also holds
/// OAuth state.
/// What: `Ok(names)` — sorted — for an absent, server-less or parsed file;
/// `Err(one-line reason naming the file)` when it exists and cannot be read or
/// parsed. An absent file is the empty, non-degraded case: an operator who has
/// registered nothing has lost nothing.
/// Test: `user_scope_servers_names_every_entry`,
/// `user_scope_servers_reports_a_malformed_config`,
/// `user_scope_servers_is_empty_without_a_config`.
pub fn user_scope_servers(config_dir: &Path) -> Result<Vec<String>, String> {
    let path = config_dir.join(CLAUDE_JSON);
    let text = match std::fs::read_to_string(&path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(format!("{} could not be read: {err}", path.display())),
        Ok(text) => text,
    };
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|err| format!("{} is not valid JSON: {err}", path.display()))?;
    let mut names: Vec<String> = parsed
        .get("mcpServers")
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    Ok(names)
}

/// Compose what a session in this config dir will load.
///
/// Why: see the module doc — one decision, read by the launch, `tm doctor`,
/// `tm mcp list` and `tm session instructions` alike. Since #7892 it takes no
/// project directory, because nothing about it varies by project: the composed
/// set is tm's builtins, and the user scope loads regardless.
/// What: `servers`/`included` are [`BUILTIN_MANAGED_MCP_SERVERS`], built by
/// [`builtin_server_entry`] rather than copied from the shared map, so a
/// session keeps its trusty-* servers even when `.claude.json` is unreadable.
/// `user_scope` reports [`user_scope_servers`]; its failure becomes `degraded`
/// and nothing else.
/// Test: `resolve_scope_is_the_builtins_plus_the_reported_user_scope`,
/// `resolve_scope_never_filters_the_user_scope`,
/// `resolve_scope_degrades_but_still_composes_the_builtins`.
pub fn resolve_scope(config_dir: &Path) -> McpScope {
    let mut servers: Map<String, Value> = Map::new();
    // #7892: the builtins are the WHOLE composed set — additive on top of the
    // user scope Claude Code loads for itself, never a replacement for it.
    for name in BUILTIN_MANAGED_MCP_SERVERS {
        if let Some(entry) = builtin_server_entry(name) {
            servers.insert((*name).to_string(), entry);
        }
    }
    let (user_scope, degraded) = match user_scope_servers(config_dir) {
        Ok(names) => (names, None),
        Err(reason) => (
            Vec::new(),
            Some(format!(
                "{reason}; your user-scope MCP servers cannot be listed, and this \
                 session starts with tm's builtins only"
            )),
        ),
    };
    let mut included: Vec<String> = servers.keys().cloned().collect();
    included.sort();
    McpScope {
        servers,
        included,
        user_scope,
        degraded,
    }
}

/// The exact bytes a launch writes into the composed session-MCP file.
///
/// Why: `tm doctor --fix` has to answer "is the file on disk already what a
/// launch would write?" before deciding to rewrite it, and the only honest way
/// to answer that is to compose the same bytes the writer composes (#7678).
/// What: pretty-printed `{"mcpServers": …}` for [`resolve_scope`]'s decision.
/// Silent: the degraded warning belongs to the launch, not to a comparison.
///
/// # Errors
///
/// [`ScopeError::Encode`] when the composed map will not serialise.
/// Test: `composed_body_matches_the_provisioned_file`.
pub fn composed_body(config_dir: &Path) -> Result<String, ScopeError> {
    encode(resolve_scope(config_dir).servers)
}

/// Serialise a composed `mcpServers` map.
///
/// Why: [`composed_body`] and [`provision_at`] must produce identical bytes, and
/// `provision_at` needs the [`McpScope`] itself to print its warning.
///
/// # Errors
///
/// [`ScopeError::Encode`] when the map will not serialise.
/// Test: `composed_body_matches_the_provisioned_file`.
fn encode(servers: Map<String, Value>) -> Result<String, ScopeError> {
    serde_json::to_string_pretty(&serde_json::json!({ "mcpServers": Value::Object(servers) }))
        .map_err(|err| ScopeError::Encode(err.to_string()))
}

/// Compose and write a session's MCP config, returning the file to point at.
///
/// Why: the write is the step that can fail, so it is separated from the pure
/// [`resolve_scope`] decision and from the pure flag rendering. Callers run it
/// BEFORE building a launch command and propagate its error.
/// What: resolves the managed state root, creates `<root>/session-mcp/` at
/// `0700`, and overwrites the file at `0600`. Rewritten on every launch, and
/// never inside the repository — see the module doc.
///
/// # Errors
///
/// [`ScopeError::Write`] when the state root cannot be resolved, or the
/// directory or file cannot be written; [`ScopeError::Encode`] when the
/// composed map will not serialise.
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
    provision_at(&path, config_dir)
}

/// [`provision`] against an already-resolved output path.
///
/// Why: the hermetic core, so tests write into a temp dir without redirecting
/// `$HOME`, and so the one home-resolution failure has exactly one call site.
/// What: see [`provision`]; `path` is the file to write. // #7892: a protected
/// `.claude.json` tm cannot read prints one line on stderr and the launch
/// continues with the builtins — it used to be composed FROM that file, and now
/// is not, so there is nothing left for the failure to take away.
///
/// # Errors
///
/// See [`provision`].
/// Test: `provision_writes_the_composed_map`,
/// `provision_writes_an_owner_only_file`,
/// `provision_warns_and_still_writes_when_the_shared_config_is_malformed`.
pub fn provision_at(path: &Path, config_dir: &Path) -> Result<PathBuf, ScopeError> {
    let scope = resolve_scope(config_dir);
    if let Some(reason) = &scope.degraded {
        eprintln!("tm: warning: {reason}");
        tracing::warn!("session MCP config degraded: {reason} (#7892)");
    }
    let body = encode(scope.servers)?;
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
    // Create the file owner-only BEFORE any byte reaches it — a write-then-chmod
    // leaves a readable window.
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
/// What: `Ok(None)` when `config_dir` is `None`; otherwise [`provision`]'s
/// result wrapped in `Some`.
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

/// [`provision_for_spawn`] against an explicit `~/.trusty-tools/trusty-mpm`
/// root (#8233).
///
/// Why: [`provision_for_spawn`] resolves that root from the process home, so a
/// test driving the real managed-launch path wrote a session-mcp file into the
/// operator's own `~/.trusty-tools/`. The daemon already holds the layout this
/// launch belongs to; naming the root here is what lets it reach the write.
/// What: `Ok(None)` when `config_dir` is `None`; otherwise [`provision_at`]
/// against [`session_mcp_path_at`]`(root, cwd)`, wrapped in `Some`.
///
/// # Errors
///
/// Propagates [`provision_at`]'s error unchanged.
/// Test: `provision_for_spawn_at_writes_under_the_named_root`,
/// `provision_for_spawn_at_declines_a_non_relocated_spawn`.
pub fn provision_for_spawn_at(
    root: &Path,
    cwd: &Path,
    config_dir: Option<&Path>,
) -> Result<Option<PathBuf>, ScopeError> {
    match config_dir {
        None => Ok(None),
        Some(dir) => provision_at(&session_mcp_path_at(root, cwd), dir).map(Some),
    }
}

#[cfg(test)]
#[path = "session_mcp_scope_tests.rs"]
mod tests;
