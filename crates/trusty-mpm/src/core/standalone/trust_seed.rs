//! Project trust + MCP-server pre-seeding for the managed CLAUDE_CONFIG_DIR.
//!
//! Why: WI-3 (sub-parts 2+3) requires that the managed Claude Code session never
//! sees a blocking "Do you trust this folder?" or "New MCP servers found" dialog.
//! Claude Code reads per-directory trust from `$CLAUDE_CONFIG_DIR/.claude.json` when
//! `CLAUDE_CONFIG_DIR` is set — NOT from `$HOME/.claude.json`.  All trust seeding
//! for the managed path MUST target `<claude_config_dir>/.claude.json`.
//!
//! ISOLATION INVARIANT: this module NEVER writes to `~/.claude.json` or
//! `~/.claude/`.  It only writes to files under `<claude_config_dir>` (the
//! managed CLAUDE_CONFIG_DIR — typically `~/.trusty-mpm/claude-config/`).
//!
//! What: [`preseed_managed_trust`] merges `projects.<workspace>` trust keys and
//! `enabledMcpjsonServers` into `<claude_config_dir>/.claude.json`. The MCP
//! server list is derived via
//! [`crate::core::mcp_config::managed_mcp_server_names`] — the two
//! UNCONDITIONAL framework builtins (`trusty-mpm`, `trusty-review`), the two
//! CONDITIONAL framework builtins (`trusty-memory`, `trusty-search`) gated by
//! the `inject_trusty_memory`/`inject_trusty_search` parameters below, UNION
//! any user-scope servers registered via `tm mcp add` (read from the same
//! file's top-level `mcpServers`) — so a `tm mcp add`-ed server is trusted on
//! the next session start with no per-project bookkeeping.
//!
//! **Security note (issue #3918 follow-up):** this derivation deliberately
//! does NOT read the target workspace's own `<workspace>/.mcp.json`. That
//! file is git-tracked content that arrives WITH a cloned repo — an earlier
//! version of this fix read it directly (mirroring
//! `session_launch::settings::preseed_workspace_trust`, the interactive `tm
//! launch` path) and a code-critic review caught that this let a hostile or
//! compromised repo's `.mcp.json` get silently auto-approved and connected in
//! a DAEMON-managed session, with no human present to decline it. See
//! [`crate::core::mcp_config::managed_mcp_server_names`]'s doc for the full
//! reasoning on why its remaining sources (the two unconditional builtins and
//! the operator's own `tm mcp add` registry) are safe against that vector.
//!
//! **Security note (issue #3934 follow-up):** the two CONDITIONAL builtins
//! looked safe to trust unconditionally too, on the assumption their
//! force-overwrite injector always ran this session — but that assumption is
//! controlled by a manifest `[mcp]` toggle that is itself untrusted,
//! project-scope, cloned-with-the-repo content (see
//! [`crate::core::mcp_config::CONDITIONAL_BUILTIN_MCP_SERVERS`]'s doc for the
//! full exploit). [`preseed_managed_trust`] now takes the resolved toggle
//! values as explicit parameters instead of assuming them — callers MUST
//! supply the actual value in effect for `workspace` this run, via
//! [`crate::core::mcp_config::resolve_conditional_mcp_toggles`].
//! Test: `test_preseed_managed_trust_marks_directory`,
//!   `test_preseed_managed_trust_is_idempotent`,
//!   `test_preseed_managed_trust_enables_mcp_servers`,
//!   `test_preseed_managed_trust_includes_trusty_mpm` (#3918),
//!   `test_preseed_managed_trust_excludes_foreign_mcp_json_entries` (#3918 follow-up,
//!   hostile-clone regression),
//!   `test_preseed_managed_trust_no_home_write` (isolation guard),
//!   `test_preseed_managed_trust_quarantines_malformed_json` (corrupt-file quarantine),
//!   `test_preseed_managed_trust_excludes_conditional_builtin_when_toggle_off`
//!   (#3934 regression — attack reproduction),
//!   `test_preseed_managed_trust_legitimate_toggle_disable_is_harmless`
//!   (#3934 — legitimate operator toggle still launches cleanly).

use std::path::Path;

use crate::core::mcp_config::managed_mcp_server_names;

/// Pre-seed project trust and MCP-server approval into `<claude_config_dir>/.claude.json`.
///
/// Why (WI-3 sub-parts 2+3): Claude Code shows two blocking dialogs for unfamiliar
/// projects — (1) "Do you trust the files in this folder?" and (2) "New MCP servers
/// found". Both are keyed to the absolute workspace path under
/// `projects.<workspace>` in `$CLAUDE_CONFIG_DIR/.claude.json`. Writing the trust
/// keys before launch means managed sessions start immediately without any dialog.
///
/// ISOLATION: writes ONLY to `<claude_config_dir>/.claude.json` — never to
/// `$HOME/.claude.json` or anything under `~/.claude/`. This is the central
/// isolation invariant for the managed driver (WI-7 support).
///
/// What: reads `<claude_config_dir>/.claude.json` (starts from `{}` when absent;
/// when the file exists but contains malformed JSON the corrupt file is renamed
/// to `.claude.json.corrupt` (preserving bytes for post-mortem) and seeding
/// proceeds from a fresh `{}`), then ensures `projects.<workspace>` carries
/// `hasTrustDialogAccepted: true`, `hasCompletedProjectOnboarding: true`,
/// `projectOnboardingSeenCount: 1` (if not already ≥ 1), and
/// `enabledMcpjsonServers` set to
/// [`crate::core::mcp_config::managed_mcp_server_names`]`(&config, inject_trusty_memory, inject_trusty_search)`
/// — the two unconditional builtins, the two conditional builtins gated by
/// the caller-supplied toggle values, UNION the `tm mcp add` registry (see
/// that function's doc for why raw `.mcp.json` content is deliberately
/// excluded, issue #3918 follow-up). Note this list does NOT read
/// `workspace`'s own `.mcp.json` or require `session_launch::prepare_session`
/// to have run for it — `workspace` is used only as the
/// `projects.<workspace>` trust key.
///
/// **`inject_trusty_memory` / `inject_trusty_search` (issue #3934):** the
/// caller MUST pass the toggle values actually resolved for `workspace` this
/// run — e.g. via [`crate::core::mcp_config::resolve_conditional_mcp_toggles`]
/// or an equivalent already-resolved `HarnessPlan`. Passing a hardcoded
/// `true, true` reopens the exact vulnerability this parameter closes: a
/// manifest that disabled the force-overwrite injector would then have its
/// (possibly attacker-spoofed) `.mcp.json` entry pre-approved anyway.
/// Writes back pretty-printed; idempotent: if all fields already match, the
/// file is NOT rewritten. All other keys in the file are preserved.
/// Test: `test_preseed_managed_trust_marks_directory`,
///   `test_preseed_managed_trust_is_idempotent`,
///   `test_preseed_managed_trust_enables_mcp_servers`,
///   `test_preseed_managed_trust_no_home_write`,
///   `test_preseed_managed_trust_quarantines_malformed_json`,
///   `test_preseed_managed_trust_excludes_conditional_builtin_when_toggle_off`.
pub fn preseed_managed_trust(
    claude_config_dir: &Path,
    workspace: &Path,
    inject_trusty_memory: bool,
    inject_trusty_search: bool,
) -> anyhow::Result<()> {
    use serde_json::Value;

    let claude_json = claude_config_dir.join(".claude.json");

    // Read existing config. If the file exists but contains malformed JSON,
    // quarantine it by renaming to `.claude.json.corrupt` (the file holds no
    // valid OAuth state when it cannot be parsed) and proceed from `{}`.
    let mut config: Value = match std::fs::read_to_string(&claude_json) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(val) if val.is_object() => val,
            Ok(_) => {
                tracing::warn!(
                    "skipping managed trust pre-seed: {} is valid JSON but not an object",
                    claude_json.display()
                );
                return Ok(());
            }
            Err(err) => {
                // Malformed JSON holds no valid OAuth state — quarantine the
                // corrupt file so trust seeding can proceed from a fresh `{}`.
                // A malformed .claude.json would otherwise permanently block
                // trust seeding (managed sessions see the trust dialog forever).
                let corrupt_path = claude_json.with_extension("json.corrupt");
                match std::fs::rename(&claude_json, &corrupt_path) {
                    Ok(()) => tracing::warn!(
                        "managed trust pre-seed: {} is not valid JSON ({err}); \
                         quarantined to {} — proceeding with fresh config",
                        claude_json.display(),
                        corrupt_path.display()
                    ),
                    Err(rename_err) => tracing::warn!(
                        "managed trust pre-seed: {} is not valid JSON ({err}); \
                         quarantine rename failed ({rename_err}) — proceeding with fresh config",
                        claude_json.display()
                    ),
                }
                Value::Object(serde_json::Map::new())
            }
        },
        // Missing file is expected on first seed — start fresh.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(serde_json::Map::new()),
        Err(err) => {
            tracing::warn!(
                "skipping managed trust pre-seed: failed to read {}: {err}",
                claude_json.display()
            );
            return Ok(());
        }
    };

    let workspace_key = workspace.to_string_lossy().to_string();

    // Build the expected enabledMcpjsonServers list from the two
    // unconditional framework builtins, the two conditional builtins gated
    // by the caller-resolved toggles, UNION the operator's own `tm mcp add`
    // registry — computed from the immutable config BEFORE the mutable
    // navigation below borrows it. Deliberately NOT derived from
    // `workspace`'s own `.mcp.json` — see the module doc's security note
    // (issue #3918 follow-up): that file is git-tracked content that arrives
    // with a cloned repo, and auto-approving whatever it declares would let a
    // hostile clone smuggle an arbitrary MCP server into a daemon-managed
    // session with no human present to decline it. The two conditional names
    // are gated by `inject_trusty_memory`/`inject_trusty_search` (issue
    // #3934) rather than unioned in unconditionally, for the identical
    // reason applied one layer deeper: a manifest toggle disabling their
    // force-overwrite injector is ALSO untrusted, cloned-with-the-repo input.
    let enabled_mcp = Value::Array(
        managed_mcp_server_names(&config, inject_trusty_memory, inject_trusty_search)
            .into_iter()
            .map(Value::String)
            .collect(),
    );

    // Navigate to (or create) projects.<workspace>.
    let projects = config
        .as_object_mut()
        .expect("config is an object")
        .entry("projects")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !projects.is_object() {
        *projects = Value::Object(serde_json::Map::new());
    }
    let projects = projects.as_object_mut().expect("projects is an object");

    let entry = projects
        .entry(workspace_key)
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(serde_json::Map::new());
    }
    let entry = entry.as_object_mut().expect("project entry is an object");

    // Idempotent: skip the write when all fields are already fully set AND the
    // MCP list already matches the canonical server set.
    let already_seeded = entry.get("hasTrustDialogAccepted") == Some(&Value::Bool(true))
        && entry.get("hasCompletedProjectOnboarding") == Some(&Value::Bool(true))
        && entry
            .get("projectOnboardingSeenCount")
            .and_then(|v| v.as_i64())
            .is_some_and(|n| n >= 1)
        && entry.get("enabledMcpjsonServers") == Some(&enabled_mcp);
    if already_seeded {
        return Ok(());
    }

    entry.insert("hasTrustDialogAccepted".to_string(), Value::Bool(true));
    entry.insert(
        "hasCompletedProjectOnboarding".to_string(),
        Value::Bool(true),
    );
    // Only set the count to 1 if it is not already >= 1 (avoid overwriting
    // a higher value the user may have accumulated in a previous session).
    entry
        .entry("projectOnboardingSeenCount")
        .or_insert_with(|| Value::from(1));
    entry.insert("enabledMcpjsonServers".to_string(), enabled_mcp);

    // Use atomic write so a crash mid-write never produces a torn .claude.json
    // (which may hold OAuth state). `write_json_atomic` also creates parent
    // directories and backs up the previous file to `.claude.json.bak`.
    trusty_common::claude_config::write_json_atomic(&claude_json, &config)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", claude_json.display()))?;

    Ok(())
}

#[cfg(test)]
#[path = "trust_seed_tests.rs"]
mod tests;
