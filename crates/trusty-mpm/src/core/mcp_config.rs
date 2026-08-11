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

/// Basename of the PROJECT-scope MCP config Claude Code reads from its cwd.
///
/// Why (#4181): four places joined this literal onto a directory — the
/// reader below, the shared injector write path, and the `.git/info/exclude`
/// call that keeps the written file out of the index. A writer that spells it
/// differently from the excluder silently reintroduces the tracked-file defect,
/// so there is one literal.
/// What: `.mcp.json`.
/// Test: `mcp_server_names_reads_workspace_mcp_json`.
pub const MCP_JSON: &str = ".mcp.json";

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
/// names have no manifest escape hatch (unlike the conditional pair, they
/// cannot be individually disabled), but the FORCE-OVERWRITE ITSELF can still
/// fail (disk full, permission error, transient I/O fault — issue #3950, the
/// fifth instance of this vulnerability class). [`managed_mcp_server_names`]
/// therefore takes each name's actual per-run pin result as an explicit
/// caller-supplied bool (`trusty_mpm_pinned` / `trusty_review_pinned`)
/// rather than assuming success unconditionally.
/// Test: `builtin_mcp_server_split_unions_to_full_set`.
pub const UNCONDITIONAL_BUILTIN_MCP_SERVERS: &[&str] = &[
    UNCONDITIONAL_BUILTIN_TRUSTY_MPM,
    UNCONDITIONAL_BUILTIN_TRUSTY_REVIEW,
];

/// The `trusty-mpm` builtin name — see [`UNCONDITIONAL_BUILTIN_MCP_SERVERS`].
pub const UNCONDITIONAL_BUILTIN_TRUSTY_MPM: &str = "trusty-mpm";

/// The `trusty-review` builtin name — see [`UNCONDITIONAL_BUILTIN_MCP_SERVERS`].
pub const UNCONDITIONAL_BUILTIN_TRUSTY_REVIEW: &str = "trusty-review";

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

/// Seed the framework builtin MCP servers into the user-scope `mcpServers`
/// map of `<config_dir>/.claude.json`, **inserting only the names that are
/// absent**.
///
/// Why (#4181, [ADR-0042]): a server declared in this map connects with no
/// approval prompt, while the same server declared in a workspace `.mcp.json`
/// comes up `⏸ Pending approval`. ADR-0042 moves the framework builtins' one
/// declaration here so it is written once and persists, instead of being
/// re-injected into every ephemeral worktree. That declaration must be seeded
/// on a path that runs before every spawn, which is
/// [`crate::core::standalone::global_config::ensure_global_config_dir`].
///
/// What: reads `<config_dir>/.claude.json`, and for each name in
/// [`BUILTIN_MANAGED_MCP_SERVERS`] that the top-level `mcpServers` map does
/// NOT already contain, inserts [`builtin_server_entry`]'s canonical entry.
/// Returns the names actually inserted; an empty vector means nothing was
/// written. Holds [`crate::core::claude_json_guard::lock`] across the whole
/// read-mutate-write cycle, because the daemon runs other `.claude.json`
/// seeders concurrently (#4072).
///
/// **Insert-if-absent is the contract, not an optimisation.** An entry already
/// under a builtin name is the operator's — `tm mcp add` is the supported edit
/// path (ADR-0042 decision item 3) and must win. This function never
/// overwrites, never reorders, and never removes.
///
/// **It also never quarantines.** Unlike [`read_config`], a `.claude.json`
/// that is malformed, or whose `mcpServers` is not an object, makes this
/// function warn and return `Ok(vec![])` with the file untouched — no rename,
/// no write. Seeding runs on every launch now, so a quarantine here would turn
/// a convenience into a per-launch data-loss event against a file that also
/// holds OAuth state. `standalone::trust_seed::preseed_managed_trust` still
/// owns that decision (and still logs it at `ERROR` with a timestamped
/// quarantine path, #4206); once it has replaced the unparseable file, the
/// next launch seeds normally.
///
/// [ADR-0042]: ../../../../docs/adr/0042-mcp-configuration-is-static-and-persistent.md
///
/// Test: `seed_builtin_servers_inserts_every_builtin_when_absent`,
/// `seed_builtin_servers_never_overwrites_an_existing_entry`,
/// `seed_builtin_servers_is_idempotent`,
/// `seed_builtin_servers_preserves_unrelated_keys`,
/// `seed_builtin_servers_declines_a_malformed_config_without_quarantining`,
/// `seed_builtin_servers_declines_a_non_object_mcp_servers_map`,
/// `seed_builtin_servers_creates_the_config_when_the_dir_is_absent`,
/// `seed_builtin_servers_errors_when_claude_json_is_unreadable`,
/// `seeding_and_workspace_injection_coexist_without_clobbering`.
pub fn seed_builtin_servers(config_dir: &Path) -> Result<Vec<String>> {
    // #4181: the guard spans read → mutate → write, not just the write.
    let _guard = crate::core::claude_json_guard::lock();

    let path = config_dir.join(CLAUDE_JSON);
    let Some(mut config) = read_config_for_seeding(&path)? else {
        return Ok(Vec::new());
    };

    let root = config
        .as_object_mut()
        .expect("read_config_for_seeding only yields objects");

    // #4181: a non-object `mcpServers` is operator state this function is not
    // entitled to destroy — `mcp_servers_mut` would replace it with `{}`.
    if let Some(existing) = root.get("mcpServers")
        && !existing.is_object()
    {
        tracing::warn!(
            "skipping builtin MCP seed: {}'s mcpServers is not an object — leaving it untouched",
            path.display()
        );
        return Ok(Vec::new());
    }

    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("mcpServers is an object — checked immediately above");

    let mut seeded: Vec<String> = Vec::new();
    for name in BUILTIN_MANAGED_MCP_SERVERS {
        if servers.contains_key(*name) {
            continue;
        }
        let Some(entry) = builtin_server_entry(name) else {
            continue;
        };
        servers.insert((*name).to_string(), entry);
        seeded.push((*name).to_string());
    }

    if seeded.is_empty() {
        return Ok(Vec::new());
    }
    write(&path, &config)?;
    Ok(seeded)
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

/// Enumerate every MCP server name a managed session can reach: the framework
/// builtins plus whatever the operator registered via `tm mcp add`.
///
/// **This is a DIAGNOSTIC enumeration, never a trust decision (#4181,
/// ADR-0042).** It used to be the primary derivation for
/// `enabledMcpjsonServers`, and its four `_pinned` parameters existed so a
/// builtin whose force-overwrite injector failed could be withheld from that
/// approval (#3934/#3950). Both the injectors and the approval are gone: MCP
/// servers are declared once in `<config_dir>/.claude.json`'s user-scope
/// `mcpServers` map, which Claude Code connects with no approval at all, and tm
/// pre-approves no project-scope name. Nothing here gates access any more, so
/// the flags — and the builtin subtraction they required — are gone with them.
/// What: takes an already-parsed `.claude.json` value and returns the union of
/// [`BUILTIN_MANAGED_MCP_SERVERS`] with the top-level `mcpServers` object keys,
/// sorted and deduped. Since [`seed_builtin_servers`] writes the builtins into
/// that same map, the two halves normally overlap completely; the constant is
/// still unioned in so a config dir seeded by an older tm, or one whose
/// `.claude.json` is unreadable, still enumerates the full set.
/// Test: `managed_mcp_server_names_unions_builtin_with_configured`,
/// `managed_mcp_server_names_defaults_to_builtin`.
pub fn managed_mcp_server_names(config: &Value) -> Vec<String> {
    let mut names: Vec<String> = BUILTIN_MANAGED_MCP_SERVERS
        .iter()
        .map(|n| (*n).to_string())
        .collect();
    if let Some(servers) = config.get("mcpServers").and_then(Value::as_object) {
        names.extend(servers.keys().cloned());
    }
    names.sort();
    names.dedup();
    names
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
    let mcp_path = workspace.join(MCP_JSON);
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
            //
            // Issue #4206: TIMESTAMPED via the shared
            // `trusty_common::claude_config::quarantine_path`, not the fixed
            // `.claude.json.corrupt` this used to use. This function and
            // `standalone::trust_seed::preseed_managed_trust` both quarantine
            // the SAME file, so with a fixed name whichever ran second silently
            // destroyed the first one's evidence. Sharing the helper keeps the
            // two writers from drifting apart again.
            let corrupt = trusty_common::claude_config::quarantine_path(&path);
            let reason = match &other {
                Ok(_) => "valid JSON but not an object".to_string(),
                Err(err) => format!("not valid JSON ({err})"),
            };
            // ERROR, not warn: this discards a `.claude.json` holding OAuth
            // state and every project's trust. `warn!` reaches no diagnostic
            // surface in this workspace — the error-capture layer filters on
            // `!= Level::ERROR` and is composed only for the Daemon and
            // Supervisor commands.
            match std::fs::rename(&path, &corrupt) {
                Ok(()) => tracing::error!(
                    "{} is {reason}; quarantined to {} — proceeding with fresh config",
                    path.display(),
                    corrupt.display()
                ),
                Err(rename_err) => tracing::error!(
                    "{} is {reason}; quarantine rename failed ({rename_err}) — \
                     proceeding with fresh config",
                    path.display()
                ),
            }
            Ok((path, Value::Object(Map::new())))
        }
    }
}

/// Read `<config_dir>/.claude.json` for [`seed_builtin_servers`], never
/// quarantining.
///
/// Why (#4181): [`read_config`] backs the operator-invoked `tm mcp` commands,
/// where a malformed file must not permanently wedge the command and the
/// operator is present to see the `ERROR` line. Seeding is neither — it runs
/// unattended on every launch, so the same rename would destroy OAuth state as
/// a side effect of a convenience. See [`seed_builtin_servers`]'s doc for why
/// declining is the safe half of that trade.
/// What: returns `Ok(Some(object))` for a missing file (an empty `{}`) or a
/// well-formed JSON object; `Ok(None)` — after a warning — for a file that is
/// malformed or is valid JSON but not an object; `Err` only for a real read
/// error (permissions, a directory in the file's place).
/// Test: `seed_builtin_servers_declines_a_malformed_config_without_quarantining`,
/// `seed_builtin_servers_creates_the_config_when_the_dir_is_absent`,
/// `seed_builtin_servers_errors_when_claude_json_is_unreadable`.
fn read_config_for_seeding(path: &Path) -> Result<Option<Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(Value::Object(Map::new())));
        }
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    match serde_json::from_str::<Value>(&text) {
        Ok(v) if v.is_object() => Ok(Some(v)),
        Ok(_) => {
            tracing::warn!(
                "skipping builtin MCP seed: {} is valid JSON but not an object — \
                 leaving it untouched",
                path.display()
            );
            Ok(None)
        }
        Err(err) => {
            tracing::warn!(
                "skipping builtin MCP seed: {} is not valid JSON ({err}) — leaving it untouched",
                path.display()
            );
            Ok(None)
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
