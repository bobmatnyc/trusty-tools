//! Inject NATIVE trusty MCP servers from the managed registry into a fleet
//! session's workspace `.mcp.json` (issue #2739), with secrets routed OUT of
//! that tracked file (issue #2739 follow-up — code-critic BLOCK).
//!
//! Why: `tm mcp add` writes user-scope MCP servers into the tm-owned
//! `CLAUDE_CONFIG_DIR/.claude.json` `mcpServers` map (see [`crate::core::mcp_config`]),
//! and its help text claims they are "available in all your projects". That is
//! TRUE for the standalone `tm run` driver but FALSE for daemon-managed fleet
//! sessions (`tm session new`): those launch `claude --setting-sources
//! project,local`, which excludes the `user` tier where that map lives, so the
//! managed servers are never read. The workspace `.mcp.json` is regenerated on
//! every spawn by `prepare_session` via two hardcoded injectors
//! (`trusty-memory`, `trusty-search`) and nothing bridges the `tm mcp` registry
//! into the project layer — so a `tm mcp add`-ed server (e.g. `slack-mcp`) is
//! invisible to fleet sessions. This module closes that gap.
//!
//! `<workspace>/.mcp.json` is git-TRACKED in every real project (confirmed: it
//! is tracked in this very repo). A fleet agent's mandated `git add -A &&
//! commit && push` workflow would therefore commit any secret written into it
//! straight into remote history. So a server's `env` block (e.g.
//! `SLACK_BOT_TOKEN`, `SLACK_USER_TOKEN`, `TELEGRAM_BOT_TOKEN`) is NEVER written
//! into `.mcp.json` — see [`split_public_and_secret_env`]. Instead it is
//! delivered out-of-band via a workspace-root `.env.local`
//! ([`route_mcp_secrets_to_env_local`]), which the native binaries' own credential
//! resolver (`trusty_common::inference::credentials::resolve_key`, confirmed at
//! `crates/trusty-channels/src/{slack,telegram}/api/client.rs`) already reads
//! via env → `.env.local` → secure-store precedence
//! (`trusty_common::inference::credentials::dotenv::load_env_local_once`,
//! `find_workspace_env_local`). That loader searches upward from the spawned
//! process's cwd and — critically — checks the STARTING directory itself before
//! any repo-root boundary test (`dotenv.rs`'s
//! `walk_finds_env_local_at_the_boundary_itself` covers exactly this), so an
//! `.env.local` written at the workspace root is found by an MCP server Claude
//! Code spawns with that workspace as cwd — the documented behaviour for a
//! project-scope `.mcp.json` entry, and the same premise
//! `inject_trusty_memory_mcp`'s cwd-based palace fallback already depends on.
//! Because `.gitignore`/`.git/info/exclude` do not retroactively protect an
//! ALREADY-tracked file (`.mcp.json`'s problem), but `.env.local` is USUALLY a
//! NEW, never-tracked file, excluding it via `.git/info/exclude`
//! ([`ensure_env_local_git_excluded`]) closes that case — mirroring the proven
//! pattern in `daemon::managed_routes::inproject::untracked_sync::append_to_git_exclude`
//! (that helper is daemon-private and unreachable from this `core`-layer
//! module, so `ensure_env_local_git_excluded` re-implements the same
//! `git rev-parse --git-path info/exclude` + idempotent-append technique rather
//! than introducing a `core` → `daemon` dependency; flagged here as a future
//! consolidation candidate). But `tm` ships into ARBITRARY target repos —
//! including ones that already TRACK a file literally named `.env.local` (a
//! second code-critic pass empirically reproduced exactly this: appending to
//! `info/exclude` is a no-op for an already-tracked path). So
//! [`route_mcp_secrets_to_env_local`] adds a second, independent gate AFTER the
//! exclude-file write: [`is_env_local_actually_ignored`] asks git itself —
//! `git check-ignore -q -- .env.local` — whether the path would actually be
//! staged, and skips secret delivery entirely (fail-closed, same "servers
//! launch without credentials this session" contract as the not-a-git-repo
//! case) unless git confirms it is ignored. Both gates run BEFORE any secret
//! content is written, so there is never a window where a live token sits in a
//! file git would stage.
//!
//! What: [`inject_native_trusty_mcps`] reads the managed `.claude.json`
//! `mcpServers` map (via the SAME [`crate::core::mcp_config`] reader `tm mcp`
//! uses), filters it to the [`NATIVE_TRUSTY_MCP_SERVERS`] allowlist, and for
//! each match splits the entry via [`split_public_and_secret_env`], which also
//! validates it as a stdio + env-auth server (a positive field allowlist —
//! `type`/`command`/`args`/`env` only — rejects any registration carrying a
//! secret-bearing field like `url`/`headers`): the
//! `env`-free command/args/type shape merges idempotently into
//! `<workspace>/.mcp.json` via [`super::settings::inject_mcp_server`]; any `env`
//! entries are collected and merged into `<workspace>/.env.local` via
//! [`route_mcp_secrets_to_env_local`]. Because it runs BEFORE
//! `preseed_workspace_trust_home`, the injected `.mcp.json` names still flow
//! into the `enabledMcpjsonServers` trust list, so no "new MCP servers found"
//! dialog fires.
//! Test: `native_mcp_tests.rs` (`inject_native_only_injects_allowlisted`,
//! `inject_native_strips_secret_env_from_mcp_json`,
//! `inject_native_delivers_secrets_via_env_local`,
//! `inject_native_env_local_is_git_excluded_and_unstaged`,
//! `inject_native_env_local_routing_is_idempotent`, `inject_native_skips_non_native`,
//! `inject_native_trust_preseed_covers_injected`,
//! `inject_native_absent_config_is_noop`,
//! `inject_native_no_secrets_writes_no_env_local`,
//! `inject_native_skips_secrets_when_env_local_is_tracked`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::PrepError;
use super::settings::inject_mcp_server;
use crate::core::mcp_config;
use crate::core::trusty_tools_config::managed_claude_config_dir;

/// Basename of the git-excluded file native MCP server secrets are delivered
/// through.
///
/// Why: must be exactly the filename
/// `trusty_common::inference::credentials::dotenv` searches for — that loader
/// hardcodes `.env.local`, so this cannot be an arbitrary/dedicated filename.
/// What: `.env.local`.
/// Test: `inject_native_delivers_secrets_via_env_local`.
const ENV_LOCAL_FILENAME: &str = ".env.local";

/// The allowlist of NATIVE trusty MCP server names eligible for fleet injection.
///
/// Why (owner decision, issue #2739): only trusty-family native MCP servers are
/// propagated into managed sessions — third-party/HTTP managed servers (github,
/// chrome-devtools, Duetto endpoints, …) are deliberately NOT injected, so a
/// remote/opaque endpoint added to the standalone `tm run` driver never silently
/// lands in every fleet session. The operator still controls PRESENCE via
/// `tm mcp add`/`tm mcp remove`; this allowlist only FILTERS the managed registry
/// down to the trusty natives. Keeping it an explicit, documented constant makes
/// it trivially extensible: adding a new native trusty MCP binary is a one-line
/// change here.
/// What: the fixed set of native trusty MCP server names to inject when present
/// in the managed registry. `trusty-memory` and `trusty-search` are EXCLUDED on
/// purpose — they already have dedicated, palace/index-pinning injectors
/// (`inject_trusty_memory_mcp` / `inject_trusty_search_mcp`) that must own their
/// entries; re-injecting them here (unpinned) would clobber that pinning.
/// `trusty-review` is likewise omitted (it is a built-in framework server the
/// managed `.mcp.json` provisions directly, not a `tm mcp add` native). Every
/// server here has its `env` block routed to `.env.local` instead of
/// `.mcp.json` — see the module docs — so adding a server whose credentials
/// should stay in `.mcp.json` (there is no such case today) would need an
/// explicit carve-out, not just an allowlist entry.
/// Test: `inject_native_only_injects_allowlisted`,
/// `inject_native_skips_non_native` in `native_mcp_tests.rs`.
pub(super) const NATIVE_TRUSTY_MCP_SERVERS: &[&str] = &[
    "slack-mcp",
    "telegram-mcp",
    "gworkspace-mcp",
    "trusty-analyze",
];

/// Inject the allowlisted native trusty MCP servers from the managed registry
/// into the workspace `.mcp.json` (issue #2739).
///
/// Why: this is the `prepare_session` call site. It resolves the tm-owned managed
/// config dir — the SAME `.claude.json` the `tm mcp` CLI reads and writes — from
/// the REAL operator home (`dirs::home_dir()`/`$HOME`), exactly as `tm mcp` does.
///
/// #2756: the original version resolved this relative to `fw.claude_home_dir()`,
/// which is a NO-OP in every real daemon-managed fleet session. Managed spawns
/// build `fw` via [`crate::core::paths::FrameworkPaths::for_managed_workspace`],
/// which by design (#1931, for `CLAUDE_CONFIG_DIR` isolation of agent/skill
/// deploy targets) makes `claude_home_dir()` return the WORKSPACE directory — NOT
/// real `$HOME`. So the injector looked for
/// `<workspace>/.trusty-tools/trusty-mpm/claude-config/.claude.json` — a path that
/// never exists in a freshly cloned workspace — [`mcp_config::list_servers`]
/// treated the missing file as an empty map, and nothing was ever injected. But
/// the managed `.claude.json` registry `tm mcp add` writes is ALWAYS
/// home-relative regardless of which workspace is being provisioned, so it must
/// be read from the real home too. The `tm mcp` CLI resolves it identically via
/// [`managed_claude_config_dir`] (which does not consult `CLAUDE_CONFIG_DIR`
/// either), so the two see the same file. The daemon process runs under the real
/// user, so real `$HOME` is available at spawn time.
/// What: resolves [`managed_claude_config_dir`] (real home) and delegates to
/// [`inject_native_trusty_mcps_from`]. If the home directory cannot be resolved
/// (a stripped environment), logs a `warn!` and no-ops so the session still
/// launches — just without the extra native servers. A missing/empty managed
/// `.claude.json` likewise makes the delegate a no-op.
/// Test: `inject_native_resolves_managed_registry_from_real_home` (#2756) drives
/// the ACTUAL `for_managed_workspace` → real-home resolution end-to-end; the
/// `inject_native_*` tests cover the hermetic `_from` core directly with a
/// tempdir config dir.
pub(super) fn inject_native_trusty_mcps(project_path: &Path) -> Result<(), PrepError> {
    let Some(config_dir) = managed_claude_config_dir() else {
        tracing::warn!(
            "skipping native trusty MCP injection: cannot resolve the home directory for the \
             managed claude config dir — native servers will be absent from this session"
        );
        return Ok(());
    };
    inject_native_trusty_mcps_from(project_path, &config_dir)
}

/// Hermetic core: inject allowlisted native trusty MCP servers from
/// `managed_config_dir/.claude.json` into `<project_path>/.mcp.json`, routing
/// any secret `env` values through `.env.local` instead.
///
/// Why: split from [`inject_native_trusty_mcps`] so tests can drive it against a
/// tempdir `CLAUDE_CONFIG_DIR` + tempdir workspace without touching the real
/// home or the daemon.
/// What: reads the managed `mcpServers` map via [`mcp_config::list_servers`]
/// (which quarantines a malformed `.claude.json` and yields an empty map when the
/// file is absent). For each entry whose NAME is in
/// [`NATIVE_TRUSTY_MCP_SERVERS`], [`split_public_and_secret_env`] validates it as
/// a stdio + env-auth server and separates the entry into an `env`-free public
/// shape (merged into `.mcp.json` via [`inject_mcp_server`]) and its `env` map
/// (collected across all matched servers); a registration that fails that
/// validation (non-stdio `type`, missing `command`, or a non-allowlisted field
/// like `url`/`headers`) is REJECTED wholesale — nothing is written for it.
/// When any secrets were collected, [`route_mcp_secrets_to_env_local`]
/// merges them into the workspace `.env.local`. Non-allowlisted
/// (third-party/HTTP) servers are skipped, and `trusty-memory` / `trusty-search`
/// are never touched here (their dedicated injectors own them). Injection order
/// is deterministic (allowlist order) so `.mcp.json` is stable across runs; the
/// secret map is a `BTreeMap` for the same reason. A read error on the registry
/// is surfaced as [`PrepError::Deploy`]; the `prepare_session` call site treats
/// the whole step as non-fatal.
/// Test: `inject_native_only_injects_allowlisted`,
/// `inject_native_strips_secret_env_from_mcp_json`,
/// `inject_native_delivers_secrets_via_env_local`,
/// `inject_native_env_local_routing_is_idempotent`,
/// `inject_native_skips_non_native`, `inject_native_absent_config_is_noop` in
/// `native_mcp_tests.rs`.
pub(super) fn inject_native_trusty_mcps_from(
    project_path: &Path,
    managed_config_dir: &Path,
) -> Result<(), PrepError> {
    let servers = mcp_config::list_servers(managed_config_dir)
        .map_err(|err| PrepError::Deploy(format!("reading managed MCP registry: {err}")))?;

    // Iterate the allowlist (not the registry) so injection order is deterministic
    // and only trusty natives are ever considered.
    let mut secret_env: BTreeMap<String, String> = BTreeMap::new();
    for &name in NATIVE_TRUSTY_MCP_SERVERS {
        if let Some(entry) = servers.get(name) {
            // `None` = the registration failed the stdio+env-auth validation
            // (non-stdio type, missing command, or a non-allowlisted field like
            // url/headers) and was rejected wholesale — nothing is written for it.
            if let Some((public_entry, env)) = split_public_and_secret_env(entry, name) {
                inject_mcp_server(project_path, name, public_entry)?;
                secret_env.extend(env);
            }
        }
    }

    if !secret_env.is_empty() {
        route_mcp_secrets_to_env_local(project_path, &secret_env)?;
    }
    Ok(())
}

/// The ONLY top-level fields an injected native registration may carry.
///
/// Why (code-critic residual HIGH on #2739): stripping just the `env` key is
/// not enough — a token can also ride in `url`, `headers`, or any other field
/// of a registration, and those would pass verbatim into the git-tracked
/// `.mcp.json`. Enumerating the permitted fields POSITIVELY (rather than
/// blocklisting `url`/`headers`) means a NEW secret-bearing field invented
/// upstream fails closed by default: it is not on this list, so the whole
/// registration is rejected rather than leaked. Injection is thereby narrowed
/// to exactly the stdio + env-auth shape — the only shape the four native
/// servers actually use.
/// What: `type`, `command`, `args`, `env`. `env` is extracted to `.env.local`
/// (never written to `.mcp.json`); the rest form the committable public shape.
/// Test: `split_public_and_secret_env_rejects_url_bearing_entry`,
/// `split_public_and_secret_env_rejects_header_bearing_entry`,
/// `split_public_and_secret_env_rejects_unknown_field`.
const INJECTABLE_STDIO_FIELDS: &[&str] = &["type", "command", "args", "env"];

/// Validate a registration as a stdio + env-auth server and split it into an
/// `env`-free public shape plus its secret `env` map — or reject it.
///
/// Why (code-critic BLOCK + residual HIGH on #2739): two distinct leak classes
/// are closed here. Shared by [`inject_native_trusty_mcps_from`] (which calls
/// this only for allowlist-matched entries) AND, as of the #2739 custom-server
/// follow-up, `session_launch::custom_mcp` (which calls this for ANY
/// non-native/non-builtin registered server, native or not — the validation
/// rules below are name-agnostic).
///   (1) The managed registry's `env` block is where live secrets
///   (`SLACK_BOT_TOKEN`, `TELEGRAM_BOT_TOKEN`, …) live. Every value there is
///   treated as a secret CATEGORICALLY — never split by a key-name heuristic
///   (`*_TOKEN`, `*_SECRET`, …) — because a heuristic has false negatives and a
///   single missed key is a real credential leak into a tracked file; a
///   category-wide rule can't have that failure mode.
///   (2) A token can also ride OUTSIDE `env` — in a `url`, `headers`, or any
///   other field of the registration. Rather than stripping only `env` (which
///   would pass such a field verbatim into `.mcp.json`), this constrains
///   injection to the stdio + env-auth shape via a POSITIVE field allowlist
///   ([`INJECTABLE_STDIO_FIELDS`]): a registration is REJECTED outright (skipped,
///   with a `warn!` to stderr — never partially injected) when it declares a
///   non-`stdio` `type`, omits a string `command`, or carries ANY field outside
///   the allowlist (e.g. `url`/`headers`, the http-server shape). This is the
///   actual guarantee: the ONLY bytes that ever reach `.mcp.json` are a
///   `type`/`command`/`args` stdio triple; every secret channel (`env`, and now
///   any out-of-band field) is either routed to `.env.local` or rejected
///   wholesale. `args` is the one field passed through verbatim: it defines the
///   stdio invocation itself, and argv is not a credential channel for these
///   native servers (they all take secrets via `env`/`resolve_key`, and argv is
///   world-readable via `ps` so no sane server accepts a token there) — a value
///   the operator placed in `args` via their own `tm mcp add` is their explicit
///   command shape, not a secret to strip.
/// What: returns `Some((public_shape, secret_env))` for a valid stdio server, or
/// `None` (logged) to reject. `public_shape` is built POSITIVELY from only the
/// non-secret allowlisted fields present (`type`/`command`/`args`), so no
/// unexpected field can survive even if the allowlist check is later loosened.
/// A non-string `env` value is skipped with a `warn!` (never propagated to
/// `.mcp.json` OR `.env.local`) — the `env` schema is always
/// `Record<string, string>`, so a non-string value is a malformed entry, not a
/// secret to route.
/// Test: `inject_native_strips_secret_env_from_mcp_json`,
/// `inject_native_no_env_key_when_source_has_none`,
/// `split_public_and_secret_env_separates_env_from_shape`,
/// `split_public_and_secret_env_skips_non_string_values`,
/// `split_public_and_secret_env_rejects_url_bearing_entry`,
/// `split_public_and_secret_env_rejects_header_bearing_entry`,
/// `split_public_and_secret_env_rejects_unknown_field`,
/// `split_public_and_secret_env_rejects_non_stdio_type`,
/// `split_public_and_secret_env_rejects_missing_command`,
/// `inject_native_rejects_url_bearing_allowlisted_server`.
pub(super) fn split_public_and_secret_env(
    entry: &Value,
    server_name: &str,
) -> Option<(Value, BTreeMap<String, String>)> {
    let Some(obj) = entry.as_object() else {
        tracing::warn!(
            server = server_name,
            "refusing to inject MCP server: registration is not a JSON object \
             (never routed to .mcp.json)"
        );
        return None;
    };

    // Positive field allowlist: any field outside the stdio + env-auth shape
    // (e.g. `url`/`headers` of an http server) means the registration could
    // carry a secret we do NOT strip — reject the whole entry rather than leak.
    if let Some(bad_field) = obj
        .keys()
        .find(|k| !INJECTABLE_STDIO_FIELDS.contains(&k.as_str()))
    {
        tracing::warn!(
            server = server_name,
            field = %bad_field,
            "refusing to inject MCP server: registration carries a field outside \
             the stdio+env-auth allowlist (e.g. url/headers) that could hold a secret; only \
             stdio command servers are injected (never routed to .mcp.json)"
        );
        return None;
    }

    // `type`, if present, must be exactly `stdio` (a token-bearing http/sse
    // server declares `type: "http"` and carries its secret in url/headers).
    if let Some(ty) = obj.get("type")
        && ty.as_str() != Some("stdio")
    {
        tracing::warn!(
            server = server_name,
            "refusing to inject MCP server: `type` is not `stdio` \
             (never routed to .mcp.json)"
        );
        return None;
    }

    // A stdio server MUST name a string command; without one there is nothing
    // safe to inject.
    let Some(command) = obj.get("command").filter(|c| c.is_string()) else {
        tracing::warn!(
            server = server_name,
            "refusing to inject MCP server: no string `command` \
             (never routed to .mcp.json)"
        );
        return None;
    };

    // Build the public shape POSITIVELY — only the non-secret allowlisted
    // fields, so nothing unexpected can ride along even if the guard above is
    // later relaxed.
    let mut public = serde_json::Map::new();
    if let Some(ty) = obj.get("type") {
        public.insert("type".to_string(), ty.clone());
    }
    public.insert("command".to_string(), command.clone());
    if let Some(args) = obj.get("args") {
        public.insert("args".to_string(), args.clone());
    }

    // Extract the secret env map (routed to `.env.local`, never `.mcp.json`).
    let mut secret_env = BTreeMap::new();
    if let Some(env_obj) = obj.get("env").and_then(Value::as_object) {
        for (key, value) in env_obj {
            match value.as_str() {
                Some(s) => {
                    secret_env.insert(key.clone(), s.to_string());
                }
                None => tracing::warn!(
                    server = server_name,
                    env_key = %key,
                    "native trusty MCP env value is not a string; skipping \
                     (never routed to .mcp.json or .env.local)"
                ),
            }
        }
    }

    Some((Value::Object(public), secret_env))
}

/// Merge collected MCP-server secrets into the workspace `.env.local`, after
/// confirming — via git itself, not just our own exclude-file write — that the
/// file is genuinely ignored.
///
/// Why: the delivery half of the #2739 secret-leak fix — this is the ONLY
/// place a raw token value from the managed registry is ever written to disk
/// inside the workspace, so it alone carries the exclusion guarantee. `pub(super)`
/// (rather than private) because the #2739 custom-server follow-up's
/// `session_launch::custom_mcp` calls this too for custom stdio servers' `env`
/// blocks — the guarantee (never write a secret into git-tracked `.mcp.json`)
/// applies identically regardless of whether the server is native or custom.
/// [`ensure_env_local_git_excluded`] appending a line to `info/exclude` is
/// necessary but NOT sufficient: a code-critic re-review (residual HIGH,
/// #2739) reproduced a second leak of the SAME class — if the target repo
/// already TRACKS a file literally named `.env.local` (empirically plausible;
/// `tm` ships to arbitrary repos, including ones that never adopted the
/// `.env.local`-is-gitignored convention), `info/exclude` does nothing for an
/// already-tracked path, so merging a token into it and a routine `git add -A`
/// would stage — and a commit would leak — the secret. Asking git directly
/// (`git check-ignore`) is the only check that actually reflects git's own
/// staging decision, catching both a tracked `.env.local` AND the case where
/// the exclude append silently didn't take effect (e.g. a `.gitignore`
/// negation pattern elsewhere overriding it).
/// What: resolves + ensures the `.env.local` git-exclude entry via
/// [`ensure_env_local_git_excluded`] FIRST; on failure (not a git repo, `git`
/// missing, a permissions error) logs a `warn!` and returns `Ok(())`. THEN —
/// even after that succeeds — runs `git -C <project_path> check-ignore -q --
/// .env.local` (git's own authority on whether a path would be staged) and
/// bails out the SAME way (`warn!` + `Ok(())`, no write) unless it exits `0`.
/// Only past both gates does it read any existing `<project_path>/.env.local`
/// (missing file → empty), merge in `secrets` via [`merge_env_file`], and skip
/// the write entirely when the merge is a byte-for-byte no-op (idempotency).
/// Test: `inject_native_delivers_secrets_via_env_local`,
/// `inject_native_env_local_is_git_excluded_and_unstaged`,
/// `inject_native_env_local_routing_is_idempotent`,
/// `inject_native_secrets_skipped_outside_git_repo`,
/// `inject_native_skips_secrets_when_env_local_is_tracked`.
pub(super) fn route_mcp_secrets_to_env_local(
    project_path: &Path,
    secrets: &BTreeMap<String, String>,
) -> Result<(), PrepError> {
    if let Err(err) = ensure_env_local_git_excluded(project_path) {
        tracing::warn!(
            "skipping native trusty MCP secret delivery: could not confirm `{}` is \
             git-excluded in {} ({err}) — those servers will launch without credentials \
             this session",
            ENV_LOCAL_FILENAME,
            project_path.display()
        );
        return Ok(());
    }

    if !is_env_local_actually_ignored(project_path) {
        tracing::warn!(
            "skipping native trusty MCP secret delivery: `{}` in {} is NOT recognised as \
             git-ignored (it may already be tracked in this repo) — those servers will \
             launch without credentials this session rather than risk staging a secret",
            ENV_LOCAL_FILENAME,
            project_path.display()
        );
        return Ok(());
    }

    let env_local_path = project_path.join(ENV_LOCAL_FILENAME);
    let existing = std::fs::read_to_string(&env_local_path).unwrap_or_default();
    let merged = merge_env_file(&existing, secrets);
    if merged == existing {
        return Ok(());
    }
    std::fs::write(&env_local_path, merged).map_err(|source| PrepError::Io {
        path: env_local_path,
        source,
    })
}

/// Ask git itself whether `.env.local` is ignored (and therefore would never
/// be staged by `git add -A`) in `project_path`.
///
/// Why: the fail-closed gate that catches an already-TRACKED `.env.local` —
/// the case [`ensure_env_local_git_excluded`] cannot detect on its own, since
/// appending a line to `info/exclude` succeeds unconditionally regardless of
/// whether the path is already tracked. `git check-ignore` is git's OWN
/// authority on whether a path would be excluded from `git add`, so this is
/// the same check a human would run to verify the guarantee, not a
/// reimplementation of git's ignore-resolution rules.
/// What: runs `git -C <project_path> check-ignore -q -- .env.local` via
/// `.output()` (NOT `.status()`) — capturing the child's stdout/stderr into
/// buffers instead of letting them inherit this process's file descriptors —
/// and returns whether it exited `0` (ignored). Capturing matters because this
/// runs inside the daemon/MCP process whose stdout must stay clean for JSON-RPC
/// framing (`-q` suppresses check-ignore's normal output, but `.output()`
/// guarantees nothing the child writes can reach our stdout regardless). Any
/// non-zero exit — including "not ignored" (which is what a tracked file, or a
/// file with no matching ignore rule, both report) and a hard error (not a repo,
/// no `git` binary) — is treated as `false`. Never panics.
/// Test: `inject_native_skips_secrets_when_env_local_is_tracked`,
/// `is_env_local_actually_ignored_true_when_excluded`,
/// `is_env_local_actually_ignored_false_when_tracked`.
pub(super) fn is_env_local_actually_ignored(project_path: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(project_path)
        .args(["check-ignore", "-q", "--", ENV_LOCAL_FILENAME])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Ensure `.env.local` is listed in the workspace's real `info/exclude` file.
///
/// Why: `.gitignore`/a tracked ignore file cannot protect a NEW file any
/// differently than `.git/info/exclude` can, but unlike a tracked `.gitignore`
/// change, appending to `info/exclude` is itself never committed — it is git's
/// own mechanism for a local, per-checkout ignore rule. Resolving the real path
/// via `git rev-parse --git-path info/exclude` (rather than assuming
/// `<project_path>/.git/info/exclude`) is required because a linked git
/// worktree's `.git` is a FILE (a gitlink), and `info/exclude` lives in the
/// SHARED common git dir — this mirrors
/// `daemon::managed_routes::inproject::untracked_sync::append_to_git_exclude`,
/// which solves the identical problem for a different call site (see the
/// module docs for why that helper isn't called directly).
/// What: runs `git -C <project_path> rev-parse --git-path info/exclude`;
/// resolves a relative result against `project_path` (an absolute result, the
/// linked-worktree case, is used as-is); creates the exclude file's parent dir;
/// and appends a `.env.local` line only when not already present (idempotent).
/// Returns `Err` (a descriptive string, never a panic) on any failure —
/// `project_path` is not a git repo, `git` is not on `PATH`, or the exclude
/// file could not be created/written. The caller treats `Err` as "skip secret
/// delivery entirely", never as license to write the secret file unexcluded.
/// Test: `inject_native_env_local_is_git_excluded_and_unstaged`,
/// `inject_native_secrets_skipped_outside_git_repo`,
/// `ensure_env_local_git_excluded_is_idempotent`,
/// `ensure_env_local_git_excluded_fails_outside_git_repo`.
pub(super) fn ensure_env_local_git_excluded(project_path: &Path) -> Result<(), String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(project_path)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()
        .map_err(|e| format!("git rev-parse failed to spawn: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git rev-parse --git-path info/exclude failed ({}): {stderr}",
            output.status
        ));
    }

    let raw_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw_path.is_empty() {
        return Err("git rev-parse returned an empty path".to_string());
    }
    let exclude_path = {
        let p = PathBuf::from(&raw_path);
        if p.is_absolute() {
            p
        } else {
            project_path.join(p)
        }
    };

    if let Some(parent) = exclude_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }

    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    if existing
        .lines()
        .any(|line| line.trim() == ENV_LOCAL_FILENAME)
    {
        return Ok(());
    }

    let entry = if existing.is_empty() || existing.ends_with('\n') {
        format!("{ENV_LOCAL_FILENAME}\n")
    } else {
        format!("\n{ENV_LOCAL_FILENAME}\n")
    };

    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)
        .map_err(|e| format!("could not open {}: {e}", exclude_path.display()))?;
    file.write_all(entry.as_bytes())
        .map_err(|e| format!("could not write {}: {e}", exclude_path.display()))
}

/// Merge `updates` into an existing `.env.local`'s textual content, preserving
/// every other line untouched.
///
/// Why: `.env.local` may already carry the operator's own values (hand-edited,
/// or copied in by
/// `daemon::managed_routes::inproject::untracked_sync::sync_untracked_files`
/// for in-project spawns) — a blind overwrite would destroy them. Per-key
/// replacement keeps this additive and idempotent: re-running with the same
/// `updates` against already-merged content reproduces the identical string, so
/// the caller's byte-equality check can skip the write.
/// What: walks `existing` line by line; a non-comment `KEY=...` line whose
/// `KEY` is in `updates` is replaced with a freshly quoted `KEY="value"` line
/// (quoting guards against a value containing `#`/whitespace/`=` being
/// misparsed); every other line (including comments and unrelated keys) passes
/// through verbatim. Any `updates` key not found in `existing` is appended at
/// the end. Embedded `\n`/`\r` in a value are stripped (a token is always
/// single-line; keeping them out prevents a malformed value from injecting an
/// extra line) and `\`/`"` are backslash-escaped. Returns a string always
/// ending in a single trailing newline when non-empty.
/// Test: `merge_env_file_adds_new_keys`, `merge_env_file_replaces_existing_key`,
/// `merge_env_file_preserves_unrelated_lines`,
/// `merge_env_file_is_idempotent_when_unchanged`.
pub(super) fn merge_env_file(existing: &str, updates: &BTreeMap<String, String>) -> String {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut lines: Vec<String> = Vec::new();

    for line in existing.lines() {
        let is_comment = line.trim_start().starts_with('#');
        if !is_comment
            && let Some((key, _)) = line.split_once('=')
            && let Some(new_val) = updates.get(key.trim())
        {
            let key = key.trim();
            lines.push(format!("{key}={}", quote_env_value(new_val)));
            seen.insert(key);
            continue;
        }
        lines.push(line.to_string());
    }

    for (key, val) in updates {
        if !seen.contains(key.as_str()) {
            lines.push(format!("{key}={}", quote_env_value(val)));
        }
    }

    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Render a single `.env.local` value as a double-quoted, escaped literal.
///
/// Why: shared by every [`merge_env_file`] write path so quoting rules never
/// drift between the "replace" and "append" branches.
/// What: strips embedded `\n`/`\r`, backslash-escapes `\` and `"`, and wraps the
/// result in `"…"`.
/// Test: covered via `merge_env_file_*` (no separate unit test — this is a
/// private one-line formatting helper with no independent branch logic).
fn quote_env_value(value: &str) -> String {
    let sanitized: String = value.chars().filter(|c| *c != '\n' && *c != '\r').collect();
    let escaped = sanitized.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}
