//! Bridge CUSTOM (non-native, non-builtin) MCP servers into a fleet session's
//! workspace `.mcp.json`, at USER and PROJECT scope (issue #2739 follow-up).
//!
//! Why: [`super::native_mcp`] closed the gap for the trusty-family NATIVE
//! allowlist only (`slack-mcp`, `telegram-mcp`, `gworkspace-mcp`,
//! `trusty-analyze`) — a deliberate, narrow first cut. Bob's follow-up
//! directive is broader: "support custom remote and local MCPs at a project or
//! user level", i.e. bridge servers that are NOT on that allowlist too, so an
//! operator-registered server like a Duetto HTTP endpoint reaches fleet
//! sessions the same way the native servers do. Two independent sources feed
//! this, each matching how trusty-mpm already models that scope:
//!
//! - **USER scope**: the SAME managed `.claude.json` `mcpServers` map
//!   [`super::native_mcp`] reads (`tm mcp add`, see [`crate::core::mcp_config`]).
//!   Any entry there that is NOT on the native allowlist and NOT one of the
//!   three dedicated-injector builtins (`trusty-memory`/`trusty-search`/
//!   `trusty-review`) is a "custom" user-scope server. The operator registered
//!   it deliberately via `tm mcp add`, so bridging it into every fleet session
//!   mirrors the native precedent — no new consent gate is needed for THIS
//!   half.
//! - **PROJECT scope**: `<project>/.trusty-mpm/manifest.toml`'s `[mcp.custom]`
//!   table — trusty-mpm's ALREADY-established per-project override file (see
//!   `core::manifest`; the same layer `[agents]`/`[skills]` use), resolved with
//!   the manifest's existing project > user > catalog > default precedence.
//!   `session_launch::mod::prepare_session_inner` threads the resolved
//!   `plan.custom_mcp_servers` in here.
//!
//! **Precedence when a name exists at both scopes: PROJECT wins.** This
//! function applies the user-scope registry FIRST, then the project-scope
//! manifest table SECOND — [`super::settings::inject_mcp_server`] is an
//! upsert-by-name, so a project entry silently overwrites a same-named user
//! entry. This is deliberate: a project's own declared MCP requirements are
//! more specific than the operator's blanket user registry, matching HR-2's
//! established "project override > user config" precedence used everywhere
//! else in the manifest.
//!
//! **Security decision (flagged for owner review, not guessed silently):**
//! bridging a REMOTE (http/sse) server is a real trust-surface expansion — the
//! URL/headers of a remote endpoint can carry a live credential (empirically
//! confirmed against the operator's own managed registry: an existing
//! `duetto-code-intelligence` entry carries its API key AS A URL QUERY
//! PARAMETER, not in `headers`). `.mcp.json` is git-TRACKED in real target
//! repos, so writing that verbatim would be the exact secret-leak class
//! [`super::native_mcp`] was built to prevent for stdio `env` — except a URL
//! has no structured "secret field" we can reliably strip the way `env`/
//! `headers` can be split out. This module's answer: [`validate_custom_remote_server`]
//! injects a remote entry ONLY when it carries NO `headers` (the clean-URL case
//! this issue's acceptance example — `duetto-memory` — is); an entry WITH
//! `headers` is REJECTED wholesale (fail-closed, `warn!`-logged), because there
//! is no established out-of-band delivery channel for an HTTP auth header
//! analogous to `.env.local` for stdio `env` today. A URL-embedded secret (the
//! `duetto-code-intelligence` shape) is NOT detected or stripped — there is no
//! reliable general way to do so without a false-negative that would still leak
//! it, so this is the ACTUAL open question flagged back to Bob: whether/how to
//! safely deliver a remote server's credential (URL- or header-borne) into a
//! git-tracked `.mcp.json`, e.g. via a documented `${VAR}`-expansion convention
//! Claude Code's own `.mcp.json` reader may support. Until that is decided, a
//! remote server that needs a credential (in the URL or in headers) simply does
//! not bridge — the fail-closed default matches the native module's existing
//! posture. Custom STDIO servers carry no such gap: their `env` is routed to
//! `.env.local` via the SAME [`super::native_mcp::route_mcp_secrets_to_env_local`]
//! machinery natives use.
//!
//! What: [`inject_custom_trusty_mcps`] is the `prepare_session_inner` call
//! site. For each candidate entry it dispatches on the declared transport:
//! `stdio` (or a bare command/no `type`) goes through
//! [`super::native_mcp::split_public_and_secret_env`] (the SAME positive-field
//! validator/secret-splitter natives use — its rules are name-agnostic);
//! `http`/`sse` goes through [`validate_custom_remote_server`]. Collected `env`
//! secrets are merged into `.env.local` via the shared native helper. Runs
//! BEFORE `preseed_workspace_trust_home` (same ordering requirement as the
//! native injector) so injected names flow into `enabledMcpjsonServers` — EXCEPT
//! the names that came from the PROJECT manifest scope, which
//! [`inject_custom_trusty_mcps`] returns to the caller so they can be excluded
//! from that auto-approval pre-seed (see the "consent" paragraph below).
//! Test: `custom_mcp_tests.rs`.
//!
//! **Reserved-name enforcement applies to BOTH scopes.** A project manifest
//! `[mcp.custom]` entry named e.g. `trusty-memory` or `slack-mcp` must be
//! rejected exactly like a same-named user-scope registry entry would be —
//! [`inject_mcp_server`] is an upsert-by-name and the dedicated/native
//! injectors for those reserved names run BEFORE this one
//! (`session_launch/mod.rs`), so an unfiltered project loop could silently
//! clobber a legitimate entry (arbitrary command execution via a spoofed
//! stdio command, or redirecting `trusty-memory`/`trusty-search` calls to an
//! attacker-controlled endpoint). Both scope loops call [`is_reserved_name`]
//! before injecting.
//!
//! **Name charset enforcement (issue #3033, MEDIUM finding 1).**
//! [`is_reserved_name`]'s exact-string comparison does NOT catch a
//! case-variant (`Trusty-Memory`) or Unicode-confusable (a non-ASCII hyphen in
//! place of ASCII `-`) typosquat of a reserved name — either shape sails past
//! it untouched. [`inject_one_custom_entry`] additionally requires every
//! candidate name (both scopes, since both loops route through it) to match
//! `^[a-z0-9][a-z0-9-]*$` via [`is_valid_custom_mcp_name`], rejecting the
//! typosquat class wholesale rather than chasing individual confusables.
//! Test: `custom_project_scope_rejects_case_variant_of_reserved_name`,
//! `custom_project_scope_rejects_unicode_hyphen_variant_of_reserved_name`,
//! `custom_project_scope_accepts_valid_lowercase_hyphen_name`.
//!
//! **Consent gate for project-scope servers.** A project-scope `[mcp.custom]`
//! entry ships with the CLONED REPO itself — unlike a user-scope registry
//! entry, which the operator personally added via `tm mcp add` on their OWN
//! machine. The PRIMARY safeguard (owner decision, superseding the original
//! interim design) is [`crate::core::project_trust::is_project_trusted`]:
//! [`inject_custom_trusty_mcps`] consults it before honoring ANY entry in
//! `project_custom` and, when the project is untrusted, skips the ENTIRE
//! project-scope loop with a single `warn!` naming the project path and
//! hinting `tm project trust <path>` (never logging entry values). Trust is
//! recorded in USER-scope state (`core::project_trust`, under
//! `~/.trusty-tools/trusty-mpm/`), never inside the repo, so a cloned repo
//! cannot self-trust; `tm project trust` / `tm project trust --revoke`
//! (`bin/tm/commands/project.rs`) are the only way to flip it. As
//! DEFENSE-IN-DEPTH this module additionally keeps the earlier no-preseed
//! safeguard: even for a TRUSTED project, [`inject_custom_trusty_mcps`]
//! returns the set of names it injected from the PROJECT scope so the caller
//! ([`super::settings::preseed_workspace_trust_home`]) can exclude them from
//! the `enabledMcpjsonServers` auto-approval — the server still bridges into
//! `.mcp.json` once trusted, but Claude Code's own "new MCP servers found"
//! consent dialog still fires for it in-session rather than being silently
//! pre-enabled. This is a deliberate simplification, not an oversight: it
//! costs the operator one extra in-session click even after explicitly
//! trusting the project, in exchange for never having to reason about a
//! separate "trusted AND pre-approved" code path. User-scope registry names
//! are unaffected by either gate (still pre-approved, matching the existing
//! native-allowlist posture) — the operator already consented to those by
//! registering them locally.
//! Test: `custom_project_scope_cannot_override_reserved_name`,
//! `custom_project_scope_cannot_override_trusty_memory`,
//! `custom_project_scope_cannot_override_trusty_search`,
//! `custom_project_scope_cannot_override_slack_mcp`,
//! `custom_project_scope_skipped_when_project_untrusted`,
//! `custom_project_scope_bridges_when_project_trusted`,
//! `custom_project_scope_skipped_again_after_revoke`.
//!
//! **Known limitation — no pruning of stale bridged entries (tracked in
//! issue #3045).** Once a custom server (user- or project-scope) is injected
//! into a workspace's `.mcp.json`, nothing ever removes it again if the
//! registry entry or manifest declaration is later revoked or deleted —
//! every past bridge target accumulates in `.mcp.json` forever, growing the
//! trust surface over time. A tracked-name-set approach (recording which
//! names THIS module injected, then pruning any that no longer appear in
//! either source on the next `prepare_session`) would close this, but is
//! deliberately NOT implemented here; see issue #3045 and the CHANGELOG
//! entry for this change.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use super::PrepError;
use super::native_mcp::{
    NATIVE_TRUSTY_MCP_SERVERS, route_mcp_secrets_to_env_local, split_public_and_secret_env,
};
use super::settings::inject_mcp_server;
use crate::core::manifest::CustomMcpServer;
use crate::core::mcp_config::{self, BUILTIN_MANAGED_MCP_SERVERS};
use crate::core::project_trust::is_project_trusted;
use crate::core::trusty_tools_config::managed_claude_config_dir;

/// The ONLY top-level fields a custom remote (http/sse) registration may carry
/// for [`validate_custom_remote_server`] to accept it.
///
/// Why: mirrors [`super::native_mcp`]'s positive-field-allowlist philosophy —
/// enumerate what IS safe rather than blocklist what isn't, so a new
/// secret-bearing field invented upstream fails closed by default.
/// What: `type`, `url`. `headers` is deliberately EXCLUDED from the injectable
/// set (see the module docs' security-decision section) — a registration
/// carrying `headers` is rejected wholesale rather than having `headers`
/// silently dropped, so the operator can see (via the `warn!`) exactly why
/// their server did not bridge.
const INJECTABLE_REMOTE_FIELDS: &[&str] = &["type", "url"];

/// Bridge CUSTOM (non-native, non-builtin) MCP servers from BOTH the
/// user-scope managed registry and the resolved project-scope manifest table
/// into the workspace `.mcp.json` (issue #2739 follow-up).
///
/// Why: this is the `prepare_session_inner` call site, run alongside (and
/// after) [`super::native_mcp::inject_native_trusty_mcps`] so the native
/// allowlist and the broader custom set are both bridged before trust preseed.
/// What: reads the managed `.claude.json` registry (best-effort — a
/// stripped-environment or read failure is logged and treated as "no user-scope
/// entries", never fatal to the launch) and injects every entry that is not a
/// native or builtin reserved name. THEN, only when
/// [`crate::core::project_trust::is_project_trusted`] returns `true` for
/// `project_path`, injects every entry in `project_custom` (overwriting a
/// same-named user entry — see the module docs for the precedence rationale).
/// An untrusted project's `project_custom` table is skipped in its entirety
/// (logged once at `warn!`, never fatal). Collected stdio secrets are merged
/// into `.env.local` once at the end. Non-fatal: an injection or
/// secret-routing failure for one entry never aborts the rest.
///
/// Returns the set of names actually injected from the PROJECT scope (not the
/// user scope) so the caller can exclude them from the workspace-trust
/// auto-approval pre-seed (see the module docs' "Consent gate" section) —
/// only names for which [`inject_one_custom_entry`] actually wrote an entry
/// are included, so a project-scope entry that itself fails validation (e.g.
/// a headers-bearing remote server) is never wrongly excluded from
/// pre-approval when it left a pre-existing user-scope entry of the same name
/// untouched. Always empty when the project is untrusted.
/// Test: `custom_user_scope_http_server_bridges`,
/// `custom_user_scope_stdio_server_bridges_with_secret_routing`,
/// `custom_project_scope_server_bridges`,
/// `custom_project_scope_overrides_user_scope_on_name_collision`,
/// `custom_bridging_skips_native_and_builtin_names`,
/// `custom_project_scope_cannot_override_reserved_name`,
/// `custom_project_scope_cannot_override_trusty_memory`,
/// `custom_project_scope_cannot_override_trusty_search`,
/// `custom_project_scope_cannot_override_slack_mcp`,
/// `custom_project_scope_skipped_when_project_untrusted`,
/// `custom_project_scope_bridges_when_project_trusted`,
/// `custom_project_scope_skipped_again_after_revoke`,
/// `custom_project_scope_rejects_case_variant_of_reserved_name`,
/// `custom_project_scope_rejects_unicode_hyphen_variant_of_reserved_name`,
/// `custom_project_scope_accepts_valid_lowercase_hyphen_name`.
pub(super) fn inject_custom_trusty_mcps(
    project_path: &Path,
    project_custom: &BTreeMap<String, CustomMcpServer>,
) -> Result<BTreeSet<String>, PrepError> {
    let mut secret_env: BTreeMap<String, String> = BTreeMap::new();
    let mut project_scope_injected: BTreeSet<String> = BTreeSet::new();

    // USER SCOPE (lower precedence): every managed-registry entry that is
    // neither a native trusty server (owned by `native_mcp`) nor one of the
    // three dedicated-injector builtins.
    match managed_claude_config_dir() {
        Some(config_dir) => match mcp_config::list_servers(&config_dir) {
            Ok(servers) => {
                for (name, entry) in &servers {
                    if is_reserved_name(name) {
                        continue;
                    }
                    inject_one_custom_entry(project_path, name, entry, &mut secret_env)?;
                }
            }
            Err(err) => {
                tracing::warn!(
                    "failed to read managed MCP registry for custom-server bridging: {err} — \
                     user-scope custom servers will be absent from this session"
                );
            }
        },
        None => {
            tracing::warn!(
                "skipping user-scope custom MCP bridging: cannot resolve the home directory"
            );
        }
    }

    // PROJECT SCOPE (higher precedence — overwrites a same-named user entry):
    // the resolved manifest's `[mcp.custom]` table. GATED on operator consent
    // (see the module docs' "Consent gate" paragraph): a project manifest
    // ships with the cloned repo, so its `[mcp.custom]` entries are honored
    // ONLY when the operator has explicitly run `tm project trust` for this
    // project path. An untrusted project contributes NOTHING here — a single
    // `warn!` names the project and hints the trust command; entry values are
    // never logged.
    if project_custom.is_empty() {
        // Nothing declared — skip the trust lookup entirely (no I/O, no log).
    } else if !is_project_trusted(project_path) {
        tracing::warn!(
            project = %project_path.display(),
            "skipping project-scope custom MCP servers: this project is not trusted — run \
             `tm project trust {}` to allow its [mcp.custom] manifest entries to bridge",
            project_path.display()
        );
    } else {
        // Reserved names (native or builtin) are rejected here EXACTLY like
        // the user-scope loop above — a TRUSTED project manifest must still
        // never be able to shadow `trusty-memory`, `trusty-search`,
        // `trusty-review`, or a native-allowlist server (see the module docs'
        // reserved-name-enforcement paragraph; this was previously unguarded
        // and let a project manifest silently overwrite those entries).
        for (name, def) in project_custom {
            if is_reserved_name(name) {
                tracing::warn!(
                    server = %name,
                    "refusing to bridge project-scope custom MCP server: name is reserved by a \
                     native or builtin injector and cannot be overridden by a project manifest"
                );
                continue;
            }
            let entry = custom_manifest_entry_to_value(def);
            if inject_one_custom_entry(project_path, name, &entry, &mut secret_env)? {
                project_scope_injected.insert(name.clone());
            }
        }
    }

    if !secret_env.is_empty() {
        route_mcp_secrets_to_env_local(project_path, &secret_env)?;
    }
    Ok(project_scope_injected)
}

/// Whether `name` is already owned by another injector and must never be
/// treated as a "custom" server here.
///
/// Why: the native allowlist has its own injector
/// ([`super::native_mcp::inject_native_trusty_mcps`]), and the three builtins
/// (`trusty-memory`/`trusty-search`/`trusty-review`) have their own dedicated,
/// pinning-aware injectors — re-injecting any of them here (unpinned, or
/// duplicated) would clobber that ownership.
/// What: `true` iff `name` is in [`NATIVE_TRUSTY_MCP_SERVERS`] or
/// [`BUILTIN_MANAGED_MCP_SERVERS`]. Applied identically to BOTH the
/// user-scope registry loop and the project-scope manifest loop in
/// [`inject_custom_trusty_mcps`] — a project manifest must not be able to
/// bypass this check the user-scope loop already enforced.
/// Test: `custom_bridging_skips_native_and_builtin_names`,
/// `custom_project_scope_cannot_override_reserved_name`,
/// `custom_project_scope_cannot_override_trusty_memory`,
/// `custom_project_scope_cannot_override_trusty_search`,
/// `custom_project_scope_cannot_override_slack_mcp`.
fn is_reserved_name(name: &str) -> bool {
    NATIVE_TRUSTY_MCP_SERVERS.contains(&name) || BUILTIN_MANAGED_MCP_SERVERS.contains(&name)
}

/// Whether `name` matches the strict custom-MCP-server charset
/// `^[a-z0-9][a-z0-9-]*$` (issue #3033, MEDIUM finding 1).
///
/// Why: [`is_reserved_name`] does an exact, case-sensitive, codepoint-exact
/// string comparison against the reserved-name tables — it does NOT catch a
/// case-variant (`Trusty-Memory`) or a Unicode confusable (a non-ASCII hyphen
/// U+2011 in place of ASCII `-`) typosquat of a reserved name. Either shape
/// sails past `is_reserved_name` untouched, then renders in a UI or log line
/// close enough to the real name to fool an operator. Restricting the
/// ENTIRE custom-server namespace to lowercase ASCII letters, digits, and
/// hyphens closes that typosquat class wholesale, independent of the
/// reserved-name table's exact contents.
/// What: `true` iff `name` is non-empty, its first character is an ASCII
/// lowercase letter or digit, and every subsequent character is an ASCII
/// lowercase letter, digit, or `-` (equivalent to the regex
/// `^[a-z0-9][a-z0-9-]*$`, checked without a `regex` dependency).
/// Test: `custom_project_scope_rejects_case_variant_of_reserved_name`,
/// `custom_project_scope_rejects_unicode_hyphen_variant_of_reserved_name`,
/// `custom_project_scope_accepts_valid_lowercase_hyphen_name`.
fn is_valid_custom_mcp_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Validate and inject a single custom entry, dispatching on its declared
/// transport.
///
/// Why: branching on the declared `type` BEFORE validating (rather than trying
/// the stdio validator and falling back to the remote one) avoids a spurious
/// "not stdio" warning firing for every legitimate http/sse entry —
/// [`split_public_and_secret_env`] always logs on rejection, so calling it
/// speculatively against a remote entry would double-warn.
/// What: `http`/`sse` entries go through [`validate_custom_remote_server`];
/// everything else (`stdio`, or no `type` at all — the bare-command shape) goes
/// through [`split_public_and_secret_env`], with any `env` secrets folded into
/// `secret_env` for the caller to route. A rejected entry is simply not
/// injected (already `warn!`-logged by the validator). Returns `true` iff an
/// entry was actually written to `.mcp.json`, so callers (specifically the
/// project-scope loop in [`inject_custom_trusty_mcps`]) can track exactly
/// which names this call caused to appear, rather than which names were merely
/// attempted.
/// Test: covered via `inject_custom_trusty_mcps`'s own test suite.
fn inject_one_custom_entry(
    project_path: &Path,
    name: &str,
    entry: &Value,
    secret_env: &mut BTreeMap<String, String>,
) -> Result<bool, PrepError> {
    if !is_valid_custom_mcp_name(name) {
        tracing::warn!(
            server = name,
            "refusing to bridge custom MCP server: name must match ^[a-z0-9][a-z0-9-]*$ \
             (lowercase ASCII letters, digits, hyphens only, starting with a letter or digit) — \
             this rejects case-variant and Unicode look-alike (e.g. non-ASCII hyphen) \
             typosquats of a reserved name"
        );
        return Ok(false);
    }

    let transport = entry.get("type").and_then(Value::as_str);
    match transport {
        Some("http") | Some("sse") => {
            if let Some(public) = validate_custom_remote_server(entry, name) {
                inject_mcp_server(project_path, name, public)?;
                return Ok(true);
            }
        }
        _ => {
            if let Some((public, env)) = split_public_and_secret_env(entry, name) {
                inject_mcp_server(project_path, name, public)?;
                secret_env.extend(env);
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Validate a custom remote (http/sse) registration and return its injectable
/// public shape, or `None` (logged) to reject.
///
/// Why (the module docs' security-decision section): a remote server's
/// credential may live in `headers` OR directly in the `url` itself — the
/// operator's own managed registry has a real example of the latter
/// (`duetto-code-intelligence`, an API key as a URL query parameter). There is
/// no safe out-of-band delivery channel for either today (unlike stdio `env`,
/// which routes to `.env.local`), so this fails closed on `headers` and
/// otherwise trusts the URL verbatim — the same posture the acceptance
/// example (`duetto-memory`, a clean URL with no headers) is designed to pass.
/// What: returns `Some({"type": <http|sse>, "url": <url>})` when `entry` is a
/// JSON object carrying ONLY fields in [`INJECTABLE_REMOTE_FIELDS`] (i.e. no
/// `headers` key at all — even an empty `headers: {}` is rejected, so an
/// operator who registered headers sees a consistent, unsurprising rejection
/// rather than a silent partial injection) and `url` is a non-empty string.
/// Any other shape (missing/blank `url`, a non-object entry, or any field
/// outside the allowlist) is rejected with a `warn!` explaining why.
/// Test: `remote_custom_server_without_headers_is_injected`,
/// `remote_custom_server_with_headers_is_rejected`,
/// `remote_custom_server_rejects_unknown_field`,
/// `remote_custom_server_rejects_missing_url`.
pub(super) fn validate_custom_remote_server(entry: &Value, server_name: &str) -> Option<Value> {
    let Some(obj) = entry.as_object() else {
        tracing::warn!(
            server = server_name,
            "refusing to bridge custom MCP server: registration is not a JSON object"
        );
        return None;
    };

    if let Some(bad_field) = obj
        .keys()
        .find(|k| !INJECTABLE_REMOTE_FIELDS.contains(&k.as_str()))
    {
        tracing::warn!(
            server = server_name,
            field = %bad_field,
            "refusing to bridge custom remote MCP server: no established secret-delivery \
             channel exists for HTTP headers today (see session_launch::custom_mcp docs) — \
             register a server with no `headers` to bridge it, or ask the operator to add \
             out-of-band header delivery"
        );
        return None;
    }

    let Some(url) = obj
        .get("url")
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
    else {
        tracing::warn!(
            server = server_name,
            "refusing to bridge custom remote MCP server: no non-empty string `url`"
        );
        return None;
    };

    let transport = obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("http")
        .to_string();

    Some(serde_json::json!({ "type": transport, "url": url }))
}

/// Convert a project-manifest [`CustomMcpServer`] definition into the same
/// `.claude.json`-registry-entry JSON shape [`crate::core::mcp_config`]
/// produces, so [`inject_one_custom_entry`] can validate/inject it through the
/// exact same code path as a user-scope registry entry.
///
/// Why: keeping ONE validation/injection path (rather than a parallel one for
/// the manifest's typed enum) means the security rules in this module
/// (positive field allowlist, header rejection, secret routing) apply
/// identically no matter which scope a server came from.
/// What: `Stdio` → `{"type":"stdio","command":..,"args":[..],"env":{...}}`
/// (omitting `env` when empty, matching [`crate::core::mcp_config::build_stdio_entry`]);
/// `Http`/`Sse` → `{"type":<http|sse>,"url":..,"headers":{...}}` (omitting
/// `headers` when empty).
/// Test: `custom_project_scope_server_bridges` (stdio and http both, via the
/// end-to-end injector test).
fn custom_manifest_entry_to_value(def: &CustomMcpServer) -> Value {
    match def {
        CustomMcpServer::Stdio { command, args, env } => {
            let mut obj = serde_json::Map::new();
            obj.insert("type".to_string(), Value::String("stdio".to_string()));
            obj.insert("command".to_string(), Value::String(command.clone()));
            obj.insert(
                "args".to_string(),
                Value::Array(args.iter().cloned().map(Value::String).collect()),
            );
            if !env.is_empty() {
                obj.insert(
                    "env".to_string(),
                    Value::Object(
                        env.iter()
                            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                            .collect(),
                    ),
                );
            }
            Value::Object(obj)
        }
        CustomMcpServer::Http { url, headers } => remote_entry_to_value("http", url, headers),
        CustomMcpServer::Sse { url, headers } => remote_entry_to_value("sse", url, headers),
    }
}

/// Shared builder for the `Http`/`Sse` arms of [`custom_manifest_entry_to_value`].
///
/// Why: the two remote variants differ only in the `type` discriminant;
/// factoring the shape construction avoids duplicating it.
/// What: `{"type": <transport>, "url": <url>}`, adding `"headers": {<headers>}`
/// only when non-empty (the resulting value still flows through
/// [`validate_custom_remote_server`], which rejects any `headers`-bearing
/// entry — this builder does not itself enforce that policy).
/// Test: covered transitively by `custom_project_scope_server_bridges`.
fn remote_entry_to_value(transport: &str, url: &str, headers: &BTreeMap<String, String>) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("type".to_string(), Value::String(transport.to_string()));
    obj.insert("url".to_string(), Value::String(url.to_string()));
    if !headers.is_empty() {
        obj.insert(
            "headers".to_string(),
            Value::Object(
                headers
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect(),
            ),
        );
    }
    Value::Object(obj)
}
