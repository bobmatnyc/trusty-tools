//! CLI handlers for `tm mcp add|remove|list|get`.
//!
//! Why: register user-level MCP servers into tm's OWN tm-owned
//! `CLAUDE_CONFIG_DIR` — the top-level `mcpServers` map of that dir's
//! `.claude.json`, which stock `claude mcp add` cannot target. Every tm-managed
//! session then sees the server (user scope, "available in all your projects";
//! no approval dialog). The CRUD itself lives in
//! [`trusty_mpm::core::mcp_config`]; these handlers are the thin CLI shell that
//! validates transport-specific flags, resolves the config dir, and formats
//! output — shaped like `commands::standalone`'s `*_cmd(&ManagedPaths, …)`.
//! Test: `cli_parses_mcp_*` in `tests.rs`; parse helpers unit-tested below;
//! CRUD in `core::mcp_config`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use crate::cli::McpTransportArg;
use trusty_mpm::core::mcp_config::{
    self, McpTransport, build_remote_entry, build_stdio_entry, strip_arg_separator,
};
use trusty_mpm::core::mcp_share::{McpShareStore, share_store_root};
use trusty_mpm::core::mcp_test::{self, McpTestResult};

/// Resolve the `.claude.json`-bearing config dir for a `tm mcp` invocation.
///
/// Why: the default target is the DAEMON-managed dir
/// (`~/.trusty-tools/trusty-mpm/claude-config/`) so servers reach every managed
/// session; `--root` switches to a STANDALONE root via the same precedence chain
/// `tm register`/`tm ls` use, reusing that resolver verbatim.
/// What: with `root = None` returns
/// [`trusty_mpm::core::trusty_tools_config::managed_claude_config_dir`]; with
/// `root = Some(_)` returns `resolve_managed_paths(root)?.claude_config_dir`.
/// Test: exercised end-to-end via the `tm mcp` handlers.
fn resolve_config_dir(root: Option<&str>) -> Result<PathBuf> {
    match root {
        Some(_) => Ok(super::managed_root::resolve_managed_paths(root)?.claude_config_dir),
        None => trusty_mpm::core::trusty_tools_config::managed_claude_config_dir()
            .context("cannot resolve home directory for the managed claude config dir"),
    }
}

/// Parse `KEY=VALUE` pairs into a JSON object.
///
/// Why: `-e KEY=VALUE` (repeatable) mirrors `claude mcp add -e`.
/// What: splits each item on the FIRST `=`; errors on a missing `=`.
/// Test: `parse_kv_env_splits_on_first_equals`.
fn parse_env(pairs: &[String]) -> Result<Map<String, Value>> {
    let mut map = Map::new();
    for p in pairs {
        let (k, v) = p
            .split_once('=')
            .with_context(|| format!("invalid --env '{p}': expected KEY=VALUE"))?;
        map.insert(k.to_string(), Value::String(v.to_string()));
    }
    Ok(map)
}

/// Parse `Name: Value` headers into a JSON object.
///
/// Why: `-H "Name: Value"` (repeatable) mirrors `claude mcp add --header`.
/// What: splits each item on the FIRST `:`, trimming surrounding whitespace from
/// the value; errors on a missing `:`.
/// Test: `parse_headers_splits_on_first_colon`.
fn parse_headers(items: &[String]) -> Result<Map<String, Value>> {
    let mut map = Map::new();
    for h in items {
        let (k, v) = h
            .split_once(':')
            .with_context(|| format!("invalid --header '{h}': expected 'Name: Value'"))?;
        map.insert(k.trim().to_string(), Value::String(v.trim().to_string()));
    }
    Ok(map)
}

/// Handle `tm mcp add <name> …`.
///
/// Why: the primary verb — upsert a user-scope MCP server into the tm config dir.
/// What: validates transport-specific inputs (stdio takes `-e`/args + a command
/// and WARNS (never blocks) when the command is absent from `PATH`, matching
/// `claude mcp add`; http/sse take `-H` + a URL), builds the entry, and calls
/// [`mcp_config::add_server`]. Prints whether the server was added/updated or
/// already present. `command_and_args` is the raw trailing-var-arg slice from
/// clap: its first element is the command (stdio) or URL (http/sse); any
/// remaining elements are stdio subprocess args (rejected for http/sse).
/// Test: `cli_parses_mcp_add`, `cli_parses_mcp_add_http`; CRUD in `core::mcp_config`.
///
/// Dispatch one `tm mcp <verb>` invocation.
///
/// Why (#7672): the verb list grew past what `main`'s single match arm could
/// hold without pushing that file over the 500-SLOC production cap, and the
/// arm was pure routing with no `main`-level concern in it. Keeping the routing
/// beside the handlers it routes to also means a new verb touches one file.
/// What: maps each [`crate::cli::McpCmd`] variant onto its handler below.
///
/// # Errors
///
/// Propagates the selected handler's error unchanged.
/// Test: `cli_parses_mcp_add`, `cli_parses_mcp_share`, `cli_parses_mcp_unshare`,
/// `cli_parses_mcp_remove_list_get`.
pub(crate) async fn dispatch(cmd: crate::cli::McpCmd) -> Result<()> {
    use crate::cli::McpCmd;
    match cmd {
        McpCmd::Add {
            name,
            transport,
            env,
            header,
            command_and_args,
            root,
            project,
            share_with_projects,
        } => add_cmd(
            root.as_deref(),
            &name,
            transport,
            &env,
            &header,
            &command_and_args,
            AddPlacement {
                project,
                share_with_projects,
            },
        ),
        McpCmd::Share { name, root } => share_cmd(root.as_deref(), &name, true),
        McpCmd::Unshare { name, root } => share_cmd(root.as_deref(), &name, false),
        McpCmd::Remove { name, root } => remove_cmd(root.as_deref(), &name),
        McpCmd::List { json, root } => list_cmd(root.as_deref(), json),
        McpCmd::Get { name, json, root } => get_cmd(root.as_deref(), &name, json),
        McpCmd::Test { name, json, root } => test_cmd(root.as_deref(), name.as_deref(), json).await,
    }
}

/// Where `tm mcp add` puts the server, and who may load it.
///
/// Why: clap hands these two independent booleans down together, and an
/// argument list of eight trips `clippy::too_many_arguments`. Grouping them
/// also keeps the two flags that decide WHO can load the server in one place.
/// What: `project` writes the project's own `.mcp.json` instead of the shared
/// user scope; `share_with_projects` records the #7672 content-match grant.
/// Test: `cli_parses_mcp_share`, `cli_parses_mcp_unshare`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AddPlacement {
    /// Write the server into THIS project's `.mcp.json` (`--project`, #7422).
    pub project: bool,
    /// Let an untrusted project match this user-scope entry by content
    /// (`--share-with-projects`, #7672).
    pub share_with_projects: bool,
}

pub(crate) fn add_cmd(
    root: Option<&str>,
    name: &str,
    transport: McpTransportArg,
    env: &[String],
    header: &[String],
    command_and_args: &[String],
    placement: AddPlacement,
) -> Result<()> {
    let AddPlacement {
        project,
        share_with_projects,
    } = placement;
    let config_dir = resolve_config_dir(root)?;
    let command_or_url = command_and_args.first().map(String::as_str);
    // Drop a leading `--` separator (clap's `trailing_var_arg` keeps it in the
    // captured tail after the command positional; persisting it verbatim breaks
    // every npx/uv-style stdio server). Only the first token is stripped — a
    // legitimate deeper `--` is preserved. See `mcp_config::strip_arg_separator`.
    let args = strip_arg_separator(command_and_args.get(1..).unwrap_or_default());

    let entry = match transport {
        McpTransportArg::Stdio => {
            if !header.is_empty() {
                bail!("--header is only valid for http/sse transports, not stdio");
            }
            let command = command_or_url
                .context("stdio transport requires a <command> positional argument")?;
            // Match `claude mcp add`: warn but do NOT block when the command is
            // not resolvable on PATH (it may be installed later or shadowed).
            if trusty_common::bin_resolve::resolve_binary(command).is_none() {
                tracing::warn!(
                    "command '{command}' for MCP server '{name}' was not found on PATH; \
                     adding anyway (it must resolve at session launch)"
                );
                eprintln!("warning: '{command}' not found on PATH — adding '{name}' anyway");
            }
            let env = parse_env(env)?;
            build_stdio_entry(command, args, &env)
        }
        McpTransportArg::Http | McpTransportArg::Sse => {
            if !env.is_empty() {
                bail!("--env is only valid for the stdio transport, not http/sse");
            }
            if !args.is_empty() {
                bail!("subprocess args (after `--`) are only valid for the stdio transport");
            }
            let url = command_or_url
                .context("http/sse transport requires a <url> positional argument")?;
            let headers = parse_headers(header)?;
            let t = if matches!(transport, McpTransportArg::Http) {
                McpTransport::Http
            } else {
                McpTransport::Sse
            };
            build_remote_entry(t, url, &headers)
        }
    };

    // #7422: `--project` writes the project's own `.mcp.json`, which a session
    // loads unconditionally — one declaration point, per ADR-0042, instead of a
    // shared definition plus a separate opt-in naming it.
    if project {
        let cwd = std::env::current_dir().context("cannot resolve the current directory")?;
        // #7422: the trust store answers for the WHOLE directory, so writing one
        // server must not mint a grant covering every other declaration already
        // in that `.mcp.json`. Require the grant instead of recording it.
        if !trusty_mpm::core::project_trust::is_project_trusted(&cwd) {
            anyhow::bail!(
                "{} is not a trusted project, so a server declared in its .mcp.json \
                 would not load.\n  Review what this repository already declares, then \
                 grant it once:\n    tm project trust {}",
                cwd.display(),
                cwd.display()
            );
        }
        let target = cwd.join(mcp_config::MCP_JSON);
        let changed = mcp_config::add_project_server(&cwd, name, entry)?;
        if changed {
            println!("Added MCP server '{name}' to {}", target.display());
            println!("  Sessions started in this trusted project load it with no opt-in needed.");
        } else {
            println!(
                "MCP server '{name}' already present in {} (no change)",
                target.display()
            );
        }
        return Ok(());
    }

    let outcome = register_at(
        &config_dir,
        share_store_root().as_deref(),
        name,
        entry,
        share_with_projects,
    )?;
    if outcome.changed {
        println!(
            "Added MCP server '{name}' to {}",
            config_dir.join(".claude.json").display()
        );
        println!(
            "  This is the SHARED user scope. Since #7422 a session loads it only in a \n               TRUSTED project whose .trusty-mpm.toml names it: [session] mcp_servers = [\"{name}\"]"
        );
    } else {
        println!("MCP server '{name}' already present (no change)");
    }
    if outcome.shared {
        println!(
            "  Shared with projects: an untrusted project whose .mcp.json declares exactly \n  this server now loads it, until the server's command, args or env change."
        );
    }
    if outcome.grant_dropped {
        println!(
            "  Dropped the earlier `tm mcp share` grant for '{name}': it was recorded against \n  the entry this write replaced. Re-share it with `tm mcp share {name}` if still wanted."
        );
    }
    Ok(())
}

/// What one registry write did to the server and to its share grant (#7672).
///
/// Why: a grant is recorded against CONTENT, so every write that changes the
/// content has to say what became of the grant — leaving that to the caller is
/// how a replaced entry kept a grant describing the entry before it.
/// What: `changed` is [`mcp_config::add_server`]'s own answer; exactly one of
/// `shared` and `grant_dropped` can be set, and only for a write that changed
/// something.
/// Test: `add_drops_a_stale_grant_unless_it_reshares`.
pub(crate) struct RegisterOutcome {
    /// The registry entry was added or replaced.
    pub changed: bool,
    /// A grant bound to the entry just written was recorded.
    pub shared: bool,
    /// A grant recorded against the REPLACED entry was dropped.
    pub grant_dropped: bool,
}

/// Write one user-scope registry entry and settle its share grant (#7672).
///
/// Why: the hermetic half of [`add_cmd`] — both roots are arguments, so the
/// lifecycle can be tested without redirecting `$HOME` (banned in this binary,
/// see the `env_isolation_tests` ratchet).
/// What: writes the entry, then either re-records the grant against what was
/// just written (`share_with_projects`) or drops any grant the replaced entry
/// carried. A `store_root` of `None` means the home directory would not resolve:
/// `--share-with-projects` is then an error, and the drop is logged and skipped.
///
/// # Errors
///
/// The registry write, or a `--share-with-projects` that cannot be recorded.
/// Test: `add_drops_a_stale_grant_unless_it_reshares`.
pub(crate) fn register_at(
    config_dir: &Path,
    store_root: Option<&Path>,
    name: &str,
    entry: Value,
    share_with_projects: bool,
) -> Result<RegisterOutcome> {
    let changed = mcp_config::add_server(config_dir, name, entry)?;
    if share_with_projects {
        let root =
            store_root.context("cannot resolve the home directory holding ~/.trusty-tools")?;
        record_share_at(config_dir, root, name)?;
        return Ok(RegisterOutcome {
            changed,
            shared: true,
            grant_dropped: false,
        });
    }
    // #7672: this write replaced the content a grant described, so the grant
    // goes with it. A digest mismatch already stops the stale grant from
    // matching; dropping it keeps the record honest about what is shared.
    let grant_dropped = changed && drop_grant_quietly(store_root, name);
    Ok(RegisterOutcome {
        changed,
        shared: false,
        grant_dropped,
    })
}

/// Handle `tm mcp share <name>` and `tm mcp unshare <name>` (#7672).
///
/// Why: content equivalence lets an UNTRUSTED project's `.mcp.json` load a
/// server whose spec it reproduces exactly. Registering a server is not that
/// consent — this workspace ships credential-bearing servers with an empty
/// `env`, so their spec is public and reproducible — so the operator names the
/// servers projects may reach that way.
/// What: for a share, REFUSES a name with no registered server — a grant for an
/// absent name is a dangling one that would attach to whatever is registered
/// under that name next — and otherwise records the grant against the entry's
/// current [`trusty_mpm::core::mcp_content_trust::spec_digest`]. An unshare
/// needs no registered server: dropping a grant is never the harmful direction.
///
/// # Errors
///
/// An unresolvable state root, an unregistered name on a share, or an
/// unreadable, malformed or unwritable store.
/// Test: `cli_parses_mcp_share`, `cli_parses_mcp_unshare`,
/// `share_refuses_a_name_with_no_registered_server`; the store itself in
/// `core::mcp_share`.
pub(crate) fn share_cmd(root: Option<&str>, name: &str, share: bool) -> Result<()> {
    let config_dir = resolve_config_dir(root)?;
    let store_root =
        share_store_root().context("cannot resolve the home directory holding ~/.trusty-tools")?;
    if !share {
        if drop_share_at(&store_root, name)? {
            println!("Unshared MCP server '{name}': only a TRUSTED project loads it from now on.");
        } else {
            println!("MCP server '{name}' is not shared with projects (no change)");
        }
        return Ok(());
    }
    if record_share_at(&config_dir, &store_root, name)? {
        println!(
            "Shared MCP server '{name}' with projects: an untrusted project whose \n  \
             .mcp.json declares exactly this server's command, args and env now loads it.\n  \
             The grant covers THAT content — change the server and share it again."
        );
    } else {
        println!(
            "MCP server '{name}' is already shared with projects at this exact content (no change)"
        );
    }
    Ok(())
}

/// Record `name`'s grant against its CURRENT registry content (#7672).
///
/// Why: the hermetic core of `tm mcp share`, and the one place a grant is
/// created. Both roots are arguments so the refusal can be tested without
/// redirecting `$HOME`.
/// What: `true` when the store changed. Bails when `name` is not registered, and
/// when its entry will not normalize — a grant for either could never match a
/// project declaration, so recording one would only mislead.
///
/// # Errors
///
/// An unregistered or unmodellable name, or an unreadable/unwritable store.
/// Test: `share_refuses_a_name_with_no_registered_server`.
pub(crate) fn record_share_at(config_dir: &Path, store_root: &Path, name: &str) -> Result<bool> {
    let Some(entry) = mcp_config::get_server(config_dir, name)? else {
        bail!(
            "'{name}' is not a registered MCP server in {}, so there is nothing to \
             share.\n  Register it first, then share it:\n    tm mcp add {name} -- \
             <command> [args...]\n    tm mcp share {name}",
            config_dir.display()
        );
    };
    let Some(digest) = trusty_mpm::core::mcp_content_trust::spec_digest(&entry) else {
        bail!(
            "MCP server '{name}' cannot be shared: its registry entry carries a shape tm's \
             content comparison does not model, so no project declaration could ever match it"
        );
    };
    let mut store = McpShareStore::load(store_root)?;
    let changed = store.share(name, &digest);
    if changed {
        store.save()?;
    }
    Ok(changed)
}

/// Drop `name`'s grant; `true` when one was there.
///
/// Why: the counterpart to [`record_share_at`], and the one place a grant is
/// revoked — `tm mcp unshare`, `tm mcp remove`, and a `tm mcp add` that replaces
/// an entry all go through it.
///
/// # Errors
///
/// An unreadable, malformed or unwritable store.
/// Test: `remove_drops_the_grant_so_a_reused_name_is_not_known`.
pub(crate) fn drop_share_at(store_root: &Path, name: &str) -> Result<bool> {
    let mut store = McpShareStore::load(store_root)?;
    if !store.unshare(name) {
        return Ok(false);
    }
    store.save()?;
    Ok(true)
}

/// [`drop_share_at`], for a caller whose own operation must not fail with it.
///
/// Why: `tm mcp remove` and `tm mcp add` are registry operations. A corrupt or
/// unwritable share store must not block the write the operator asked for, but
/// the failure has to be visible rather than swallowed. // See #7672
/// What: `true` when a grant was dropped; `false`, with a warning, on any
/// failure or unresolvable root.
/// Test: `remove_drops_the_grant_so_a_reused_name_is_not_known`.
fn drop_grant_quietly(store_root: Option<&Path>, name: &str) -> bool {
    let Some(root) = store_root else {
        tracing::warn!(
            "cannot resolve ~/.trusty-tools, so '{name}' keeps any `tm mcp share` grant"
        );
        return false;
    };
    match drop_share_at(root, name) {
        Ok(dropped) => dropped,
        Err(err) => {
            tracing::warn!("could not drop the `tm mcp share` grant for '{name}': {err}");
            false
        }
    }
}

/// Handle `tm mcp remove <name>`.
///
/// Why: drop a server the operator no longer wants injected into managed sessions.
/// What: calls [`mcp_config::remove_server`] and drops the server's share grant
/// with it — a grant left behind would apply to whatever is registered under
/// that name next (#7672). Prints what each half did.
/// Test: `cli_parses_mcp_remove`, `remove_drops_the_grant_so_a_reused_name_is_not_known`;
/// CRUD in `core::mcp_config`.
pub(crate) fn remove_cmd(root: Option<&str>, name: &str) -> Result<()> {
    let config_dir = resolve_config_dir(root)?;
    let outcome = remove_at(&config_dir, share_store_root().as_deref(), name)?;
    if outcome.removed {
        println!("Removed MCP server '{name}'");
    } else {
        println!("MCP server '{name}' not found (no change)");
    }
    if outcome.grant_dropped {
        println!("  Also dropped its `tm mcp share` grant with projects.");
    }
    Ok(())
}

/// What one `tm mcp remove` did to the registry and to the share grant (#7672).
///
/// Why: the two halves can disagree — a name with no registered server can
/// still carry a grant — so the caller prints what each one actually did.
/// Test: `remove_drops_the_grant_so_a_reused_name_is_not_known`.
pub(crate) struct RemoveOutcome {
    /// The registry entry was there and is gone.
    pub removed: bool,
    /// A share grant for that name was there and is gone.
    pub grant_dropped: bool,
}

/// Drop one registry entry and the grant that described it (#7672).
///
/// Why: the hermetic half of [`remove_cmd`], and the direction the PR #7692
/// re-review named: a grant left behind after a remove applies to whatever is
/// registered under that name next, which is an unrelated server.
/// What: removes the entry, then drops the grant best-effort — a broken share
/// store is logged, never allowed to fail the removal the operator asked for.
///
/// # Errors
///
/// The registry write.
/// Test: `remove_drops_the_grant_so_a_reused_name_is_not_known`.
pub(crate) fn remove_at(
    config_dir: &Path,
    store_root: Option<&Path>,
    name: &str,
) -> Result<RemoveOutcome> {
    let removed = mcp_config::remove_server(config_dir, name)?;
    Ok(RemoveOutcome {
        removed,
        grant_dropped: drop_grant_quietly(store_root, name),
    })
}

/// Handle `tm mcp list [--json]`.
///
/// Why: operators need an overview of which user-scope servers managed sessions
/// will load.
/// What: lists the top-level `mcpServers` map as a table (name → type + target)
/// or, with `--json`, the raw map. Each row is marked `opted-in` or
/// `scoped-out` for the current directory, and when the scope came back
/// degraded — an untrusted project, or an unreadable `.mcp.json` — that reason
/// prints above the opt-in hint, because until it is resolved the suggested
/// `[session] mcp_servers` edit changes nothing (#7422).
/// Test: `cli_parses_mcp_list`; CRUD in `core::mcp_config`.
pub(crate) fn list_cmd(root: Option<&str>, json: bool) -> Result<()> {
    let config_dir = resolve_config_dir(root)?;
    let servers = mcp_config::list_servers(&config_dir)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&servers)?);
        return Ok(());
    }
    if servers.is_empty() {
        println!(
            "No user-scope MCP servers registered in {}",
            config_dir.display()
        );
        println!("  Add one with: tm mcp add <name> -- <command> [args...]");
        return Ok(());
    }
    let name_w = servers.keys().map(String::len).max().unwrap_or(4).max(4);
    println!(
        "MCP servers in {} ({}):",
        config_dir.display(),
        servers.len()
    );
    // #7422: a shared declaration is no longer the same thing as a session
    // loading it, so each row says which it is FOR THIS PROJECT.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let scope = trusty_mpm::core::session_mcp_scope::resolve_scope(&cwd, &config_dir);
    println!("  {:<name_w$}  TYPE   SCOPE       TARGET", "NAME");
    for (name, entry) in &servers {
        let marker = if scope.excluded.iter().any(|n| n == name) {
            "scoped-out"
        } else {
            "opted-in"
        };
        println!(
            "  {:<name_w$}  {:<5}  {:<10}  {}",
            name,
            entry_type(entry),
            marker,
            entry_target(entry)
        );
    }
    // #7422: in an untrusted project the opt-in hint below changes nothing until
    // the grant exists, so the reason that made it a no-op prints above it.
    if let Some(reason) = &scope.degraded {
        println!();
        println!("  {reason}");
    }
    if !scope.excluded.is_empty() {
        println!();
        println!(
            "  {} server(s) are scoped-out for {} — add them to .trusty-mpm.toml:",
            scope.excluded.len(),
            cwd.display()
        );
        println!("    [session]");
        println!("    mcp_servers = {:?}", scope.excluded);
    }
    Ok(())
}

/// Handle `tm mcp get <name> [--json]`.
///
/// Why: inspect a single server's full definition.
/// What: prints the entry as pretty JSON (`--json`) or a short field summary;
/// errors if the server is absent.
/// Test: `cli_parses_mcp_get`; CRUD in `core::mcp_config`.
pub(crate) fn get_cmd(root: Option<&str>, name: &str, json: bool) -> Result<()> {
    let config_dir = resolve_config_dir(root)?;
    let Some(entry) = mcp_config::get_server(&config_dir, name)? else {
        bail!("MCP server '{name}' not found in {}", config_dir.display());
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&entry)?);
    } else {
        println!("{name}:");
        println!("  Type:   {}", entry_type(&entry));
        println!("  Target: {}", entry_target(&entry));
        println!("  Scope:  User config");
        // #2739 (+ follow-up): the "available everywhere" claim was misleading
        // for fleet sessions until the follow-up landed. As of the follow-up,
        // EVERY user-scope server here bridges into daemon-managed/fleet
        // sessions' `.mcp.json` too (native trusty servers via the allowlist
        // injector; everything else via the custom-server bridge) — EXCEPT a
        // remote (http/sse) server that declares `headers`, which is rejected
        // at bridge time (no established secret-delivery channel for HTTP
        // auth headers into a git-tracked `.mcp.json` yet; see
        // `session_launch::custom_mcp` docs).
        println!(
            "          Bridges into managed/fleet sessions too, unless this is a remote \
             (http/sse) server with `headers` set (not bridged — no safe delivery channel \
             for header secrets into .mcp.json yet)."
        );
    }
    Ok(())
}

/// Handle `tm mcp test [<name>] [--json]`.
///
/// Why: registration only proves a server's config is well-formed, not that it
/// starts and speaks MCP. `test` spawns each server and runs the real handshake
/// so operators (and CI) get a definitive pass/fail.
/// What: resolves the config dir, selects targets (one named server, or the
/// full user-scope + built-in union when `name` is `None`), probes each with a
/// bounded timeout via [`mcp_test::run_all`], then prints a table (or `--json`).
/// Exits with status 1 if ANY server failed (so it is usable as a CI gate);
/// a not-found named server is a hard error. Async because the stdio handshake
/// and http reachability check are I/O-bound.
/// Test: `cli_parses_mcp_test` in `tests.rs`; probe/selection logic in
/// `core::mcp_test`.
pub(crate) async fn test_cmd(root: Option<&str>, name: Option<&str>, json: bool) -> Result<()> {
    let config_dir = resolve_config_dir(root)?;
    let servers = mcp_config::list_servers(&config_dir)?;
    let targets = mcp_test::select_targets(&servers, name)?;
    let results = mcp_test::run_all(targets, mcp_test::DEFAULT_PROBE_TIMEOUT).await;

    if json {
        let arr: Vec<Value> = results.iter().map(McpTestResult::to_json).collect();
        println!("{}", serde_json::to_string_pretty(&Value::Array(arr))?);
    } else {
        print_test_table(&results, &config_dir);
    }

    // Non-zero exit if any server failed, matching `tm services`' convention so
    // `tm mcp test` slots straight into CI. Exit here (not a returned Err) to
    // avoid printing anyhow's error chrome after the report.
    if mcp_test::any_failed(&results) {
        std::process::exit(1);
    }
    Ok(())
}

/// Render the human-readable results table for `tm mcp test`.
fn print_test_table(results: &[McpTestResult], config_dir: &Path) {
    if results.is_empty() {
        println!("No MCP servers to test in {}", config_dir.display());
        return;
    }
    println!(
        "Testing {} MCP server(s) from {}:",
        results.len(),
        config_dir.display()
    );
    let name_w = results
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    println!("  {:<name_w$}  TRANSPORT  RESULT  DETAIL", "NAME");
    for r in results {
        let (label, detail) = r.render();
        println!(
            "  {:<name_w$}  {:<9}  {:<6}  {}",
            r.name,
            r.transport.as_str(),
            label,
            detail
        );
    }
    let passed = results.iter().filter(|r| r.passed()).count();
    println!("\n{passed}/{} passed", results.len());
}

/// The `type` discriminant of an entry, defaulting to `stdio` when absent.
fn entry_type(entry: &Value) -> &str {
    entry.get("type").and_then(Value::as_str).unwrap_or("stdio")
}

/// A one-line human summary of an entry's connection target (command+args or URL).
fn entry_target(entry: &Value) -> String {
    if let Some(url) = entry.get("url").and_then(Value::as_str) {
        return url.to_string();
    }
    let cmd = entry.get("command").and_then(Value::as_str).unwrap_or("");
    let mut args: Vec<&str> = entry
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    // Show the EFFECTIVE argv: drop a legacy stored leading `--` separator so
    // `tm mcp list/get` reflect what actually gets spawned (mirrors the spawn-path
    // and write-path normalisation in `mcp_config::strip_arg_separator`).
    if args.first() == Some(&"--") {
        args.remove(0);
    }
    if args.is_empty() {
        cmd.to_string()
    } else {
        format!("{cmd} {}", args.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kv_env_splits_on_first_equals() {
        let m = parse_env(&["A=1".into(), "B=x=y".into()]).unwrap();
        assert_eq!(m["A"], "1");
        assert_eq!(m["B"], "x=y", "only the first '=' splits");
    }

    #[test]
    fn parse_env_rejects_missing_equals() {
        assert!(parse_env(&["NOEQ".into()]).is_err());
    }

    #[test]
    fn parse_headers_splits_on_first_colon() {
        let m = parse_headers(&["Authorization: Bearer a:b".into()]).unwrap();
        assert_eq!(
            m["Authorization"], "Bearer a:b",
            "value trimmed, first ':' splits"
        );
    }

    #[test]
    fn parse_headers_rejects_missing_colon() {
        assert!(parse_headers(&["NoColon".into()]).is_err());
    }

    #[test]
    fn entry_target_formats_command_and_url() {
        let stdio = serde_json::json!({"type":"stdio","command":"echo","args":["hi","there"]});
        assert_eq!(entry_target(&stdio), "echo hi there");
        let http = serde_json::json!({"type":"http","url":"https://x/mcp"});
        assert_eq!(entry_target(&http), "https://x/mcp");
    }

    #[test]
    fn entry_target_shows_effective_argv_dropping_legacy_separator() {
        // A registry entry written before the fix still displays the effective
        // argv (no leading `--`) in `tm mcp list/get`.
        let legacy = serde_json::json!({
            "type":"stdio","command":"npx",
            "args":["--","-y","@modelcontextprotocol/server-github"]
        });
        assert_eq!(
            entry_target(&legacy),
            "npx -y @modelcontextprotocol/server-github"
        );
    }

    /// The npx shape `tm mcp add github npx -- -y @scope/pkg` must persist WITHOUT
    /// the `--` sentinel (clap's `trailing_var_arg` captures it as the first tail
    /// token). `--root` is tier-1 precedence so the write lands in
    /// `<tempdir>/claude-config`, letting us assert the stored args round-trip.
    #[test]
    fn add_strips_leading_separator_before_persisting() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_str().unwrap();
        let command_and_args = vec![
            "npx".to_string(),
            "--".to_string(),
            "-y".to_string(),
            "@modelcontextprotocol/server-github".to_string(),
        ];
        add_cmd(
            Some(root),
            "github",
            McpTransportArg::Stdio,
            &[],
            &[],
            &command_and_args,
            AddPlacement::default(),
        )
        .unwrap();

        let cfg = std::path::Path::new(root).join("claude-config");
        let entry = mcp_config::get_server(&cfg, "github")
            .unwrap()
            .expect("server persisted");
        assert_eq!(entry["command"], "npx");
        assert_eq!(
            entry["args"],
            serde_json::json!(["-y", "@modelcontextprotocol/server-github"]),
            "the `--` sentinel must not be persisted into stored args"
        );
    }

    /// Round-trip proof that only the LEADING `--` is stripped: a deeper `--`
    /// (a real inner separator some CLIs need) survives into the stored args.
    #[test]
    fn add_preserves_deeper_separator() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_str().unwrap();
        // `tm mcp add x uv -- run -- tool` → tail = ["uv","--","run","--","tool"].
        let command_and_args = vec![
            "uv".to_string(),
            "--".to_string(),
            "run".to_string(),
            "--".to_string(),
            "tool".to_string(),
        ];
        add_cmd(
            Some(root),
            "x",
            McpTransportArg::Stdio,
            &[],
            &[],
            &command_and_args,
            AddPlacement::default(),
        )
        .unwrap();

        let cfg = std::path::Path::new(root).join("claude-config");
        let entry = mcp_config::get_server(&cfg, "x").unwrap().unwrap();
        assert_eq!(entry["command"], "uv");
        assert_eq!(
            entry["args"],
            serde_json::json!(["run", "--", "tool"]),
            "leading `--` stripped once; the deeper `--` is preserved"
        );
    }

    /// A registry dir and a share-store root, both under one tempdir. Neither
    /// is `$HOME`-derived: this binary's `env_isolation_tests` ratchet bans a
    /// `set_var` here, so the lifecycle functions take both roots instead.
    fn roots(tmp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let cfg = tmp.path().join("claude-config");
        let store = tmp.path().join("state");
        std::fs::create_dir_all(&store).unwrap();
        (cfg, store)
    }

    /// Is this registry entry, under this name, content-trusted for an
    /// untrusted project — the question the launch path asks (#7672)?
    fn is_known(cfg: &Path, store: &Path, name: &str, entry: &Value) -> bool {
        use trusty_mpm::core::mcp_content_trust::{KnownServers, Verdict};

        let registry = mcp_config::list_servers(cfg).unwrap();
        let grants = McpShareStore::load(store).unwrap().grants();
        KnownServers::from_registry(&registry, &grants).classify(name, entry) == Verdict::Known
    }

    /// PR #7692 re-review, HIGH (a): a grant for a name with no registered
    /// server is dangling — it would attach to whatever gets registered under
    /// that name later. Refuse it, and write nothing.
    #[test]
    fn share_refuses_a_name_with_no_registered_server() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (cfg, store) = roots(&tmp);

        let err = record_share_at(&cfg, &store, "ghost").unwrap_err();

        assert!(
            err.to_string().contains("is not a registered MCP server"),
            "{err}"
        );
        assert!(
            !store.join("mcp-shared.json").exists(),
            "a refused share must not write a grant record"
        );
        // Revoking is the harmless direction, so it needs no registered server.
        assert!(!drop_share_at(&store, "ghost").unwrap());
    }

    /// PR #7692 re-review, HIGH (b): share, remove, then register something
    /// unrelated under the same name. The stale grant must not make that
    /// unrelated, unshared server matchable by content.
    #[test]
    fn remove_drops_the_grant_so_a_reused_name_is_not_known() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (cfg, store) = roots(&tmp);
        let original = serde_json::json!({"type": "stdio", "command": "echo", "args": ["one"]});
        mcp_config::add_server(&cfg, "helper", original.clone()).unwrap();
        assert!(record_share_at(&cfg, &store, "helper").unwrap());
        assert!(is_known(&cfg, &store, "helper", &original), "sanity");

        let removed = remove_at(&cfg, Some(store.as_path()), "helper").unwrap();

        assert!(removed.removed && removed.grant_dropped);
        // The name is reused by a different server, added the DEFAULT way.
        let unrelated = serde_json::json!({"type": "stdio", "command": "echo", "args": ["two"]});
        register_at(
            &cfg,
            Some(store.as_path()),
            "helper",
            unrelated.clone(),
            false,
        )
        .unwrap();

        assert!(
            !is_known(&cfg, &store, "helper", &unrelated),
            "an unshared server must not inherit the removed server's grant"
        );
    }

    /// PR #7692 re-review, HIGH (b), the other direction: `tm mcp add` over an
    /// existing name replaces the content a grant described, so the grant goes
    /// — unless the operator re-grants it in the same breath.
    #[test]
    fn add_drops_a_stale_grant_unless_it_reshares() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (cfg, store) = roots(&tmp);
        let original = serde_json::json!({"type": "stdio", "command": "echo", "args": ["one"]});
        mcp_config::add_server(&cfg, "helper", original).unwrap();
        record_share_at(&cfg, &store, "helper").unwrap();

        let replacement = serde_json::json!({"type": "stdio", "command": "echo", "args": ["two"]});
        let outcome = register_at(
            &cfg,
            Some(store.as_path()),
            "helper",
            replacement.clone(),
            false,
        )
        .unwrap();

        assert!(outcome.changed && outcome.grant_dropped && !outcome.shared);
        assert!(
            !is_known(&cfg, &store, "helper", &replacement),
            "the replacement was never shared, so it must not load untrusted"
        );

        // `--share-with-projects` re-records the grant against what it wrote.
        let third = serde_json::json!({"type": "stdio", "command": "echo", "args": ["three"]});
        let reshared =
            register_at(&cfg, Some(store.as_path()), "helper", third.clone(), true).unwrap();

        assert!(reshared.shared && !reshared.grant_dropped);
        assert!(is_known(&cfg, &store, "helper", &third));
    }
}
