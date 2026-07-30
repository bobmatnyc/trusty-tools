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
//! UNCONDITIONAL framework builtins (`trusty-mpm`, `trusty-review`) and the
//! two CONDITIONAL framework builtins (`trusty-memory`, `trusty-search`),
//! EACH gated by its own `_pinned` parameter below, UNION any user-scope
//! servers registered via `tm mcp add` (read from the same file's top-level
//! `mcpServers`) — so a `tm mcp add`-ed server is trusted on the next
//! session start with no per-project bookkeeping.
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
//! full exploit). [`preseed_managed_trust`] takes the resolved toggle values
//! as explicit parameters instead of assuming them.
//!
//! **Security note (issue #3950 follow-up — fifth instance):** a toggle
//! being on is necessary but not sufficient — the force-overwrite injector's
//! WRITE can itself fail (disk full, permission error, transient I/O fault)
//! while a spoofed or stale `.mcp.json` entry is already present, in which
//! case the name must NOT be approved either, even though its toggle was on.
//! [`preseed_managed_trust`] now takes each of the four builtins' actual
//! per-run pin RESULT (toggle AND write success) — callers MUST supply the
//! value the SAME run's injectors actually observed
//! ([`super::load::load_alias`] reads it off the
//! [`crate::core::session_launch::PrepReport`] its own
//! `prepare_session_with_repo_url` call already produced), never a value
//! re-derived purely from the manifest toggle in isolation.
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
//!   (#3934 — legitimate operator toggle still launches cleanly),
//!   `test_preseed_managed_trust_excludes_unconditional_builtin_when_pin_failed`
//!   (#3950 regression — write-failure reproduction).

use std::path::Path;

use crate::core::mcp_config::managed_mcp_server_names;

/// The exact set of keys [`preseed_managed_trust`] itself writes into a
/// `projects.<workspace>` entry.
///
/// Why (issue #4206): [`prune_stale_project_entries`] must distinguish a
/// SEEDER DROPPING (an entry this function created for a directory that no
/// longer exists — 2,520 of 2,541 entries in the reporting operator's config)
/// from REAL USER STATE that Claude Code itself accumulated (`lastSessionId`,
/// `lastCost`, `mcpServers`, `history`, …). The observed distribution is
/// sharply bimodal — droppings carry exactly these four keys, real entries
/// carried 15–33 — so "the entry's key set is a subset of what the seeder
/// writes" is a precise, non-heuristic test for "nothing but this function has
/// ever touched this entry".
/// What: the four keys written below (`hasTrustDialogAccepted`,
/// `hasCompletedProjectOnboarding`, `projectOnboardingSeenCount`,
/// `enabledMcpjsonServers`). MUST be kept in sync with the writes at the end
/// of [`preseed_managed_trust`] — adding a key there without adding it here
/// only makes the prune MORE conservative (entries stop looking pure and are
/// kept), never more destructive, so the failure mode of drift is safe.
/// Test: `prune_keeps_entry_with_runtime_fields`.
const SEEDER_WRITTEN_KEYS: &[&str] = &[
    "hasTrustDialogAccepted",
    "hasCompletedProjectOnboarding",
    "projectOnboardingSeenCount",
    "enabledMcpjsonServers",
];

/// Whether `dir` is DEFINITIVELY absent from the filesystem.
///
/// Why (issue #4206): this is the load-bearing safety predicate of the prune.
/// Deleting an entry because its directory "looks missing" must never fire for
/// a directory that is merely temporarily unreachable — an unmounted volume, a
/// network mount that is down, a permission error, a filesystem returning
/// `EIO`. This project has repeatedly shipped the opposite bug (a failure
/// branch treating an ambiguous observation as a definite negative and
/// advancing state anyway), so the rule here is deliberately asymmetric:
/// `NotFound` is the ONLY error the OS reports that actually means "this path
/// does not exist", and every other outcome — including success — keeps the
/// entry.
/// What: `symlink_metadata` (NOT `metadata`) so a dangling symlink counts as
/// PRESENT: the link itself exists, and its target may simply be an unmounted
/// volume. Returns `true` only on `ErrorKind::NotFound`; any other error kind
/// (`PermissionDenied`, `NotADirectory`, or a platform-specific I/O fault)
/// returns `false` — keep the entry.
/// Test: `prune_keeps_entry_when_path_error_is_ambiguous`,
///   `prune_drops_entry_when_directory_definitively_absent`.
fn is_definitively_absent(dir: &Path) -> bool {
    match std::fs::symlink_metadata(dir) {
        Ok(_) => false,
        Err(err) => err.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Drop `projects` entries that are pure seeder droppings for directories that
/// are definitively gone, returning how many were removed.
///
/// Why (issue #4206): the seeder only ever ADDED entries, so a config that had
/// accumulated 2,541 `projects` keys — 2,445 of them `tempfile::TempDir`
/// paths — had no mechanism to ever shrink. Pruning during the read-modify-
/// write that is already happening makes the file self-healing with no new
/// command, no background job, and no extra I/O beyond one `symlink_metadata`
/// per entry.
/// What: removes an entry only when BOTH conditions hold —
/// (1) [`is_definitively_absent`] for its path key, AND
/// (2) its object carries no key outside [`SEEDER_WRITTEN_KEYS`].
/// The conjunction is the point: either test alone would be unsafe.
/// Condition (1) alone would delete a real, actively-used project sitting on a
/// temporarily-unmounted volume; condition (2) alone would delete a
/// freshly-seeded entry for a directory that still exists. `keep_key` (the
/// workspace being seeded this run) is never pruned. Non-object entries are
/// left untouched — this function only removes what it can positively
/// identify.
/// Test: `prune_drops_entry_when_directory_definitively_absent`,
///   `prune_keeps_entry_when_path_error_is_ambiguous`,
///   `prune_keeps_entry_with_runtime_fields`.
fn prune_stale_project_entries(
    projects: &mut serde_json::Map<String, serde_json::Value>,
    keep_key: &str,
) -> usize {
    let before = projects.len();
    projects.retain(|key, entry| {
        if key == keep_key {
            return true;
        }
        let Some(obj) = entry.as_object() else {
            // Not an object — not something this seeder wrote. Leave it alone.
            return true;
        };
        let is_pure_seeder_entry = obj
            .keys()
            .all(|k| SEEDER_WRITTEN_KEYS.contains(&k.as_str()));
        if !is_pure_seeder_entry {
            // Carries Claude Code's own runtime state (lastSessionId, lastCost,
            // mcpServers, …) — real user state, never a seeder dropping. Keep
            // it even when the path is gone.
            return true;
        }
        !is_definitively_absent(Path::new(key))
    });
    before - projects.len()
}

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
/// [`crate::core::mcp_config::managed_mcp_server_names`]`(&config, trusty_mpm_pinned, trusty_review_pinned, trusty_memory_pinned, trusty_search_pinned)`
/// — the two unconditional builtins and the two conditional builtins, EACH
/// gated by the caller-supplied per-name pin result, UNION the `tm mcp add`
/// registry (see that function's doc for why raw `.mcp.json` content is
/// deliberately excluded, issue #3918 follow-up). Note this list does NOT
/// read `workspace`'s own `.mcp.json` or require `session_launch::prepare_session`
/// to have run for it — `workspace` is used only as the
/// `projects.<workspace>` trust key.
///
/// **`trusty_mpm_pinned` / `trusty_review_pinned` / `trusty_memory_pinned` /
/// `trusty_search_pinned` (issues #3934/#3950):** each MUST reflect whether
/// that name's force-overwrite injector actually SUCCEEDED writing the
/// framework-controlled entry for `workspace` this run — not merely that its
/// manifest toggle (where one exists) was on. Passing a hardcoded
/// `true, true, true, true` reopens the exact vulnerability these parameters
/// close: a manifest that disabled a conditional injector, OR a write that
/// failed for any of the four (disk full, permission error, transient I/O
/// fault — issue #3950, the fifth instance of this class), would then have
/// its (possibly attacker-spoofed) `.mcp.json` entry pre-approved anyway.
/// [`super::load::load_alias`] derives all four from the
/// [`crate::core::session_launch::PrepReport`] the SAME run's
/// `prepare_session_with_repo_url` call already produced, rather than
/// re-deriving from the manifest toggle alone.
/// Writes back pretty-printed; idempotent: if all fields already match, the
/// file is NOT rewritten. All other keys in the file are preserved.
/// Test: `test_preseed_managed_trust_marks_directory`,
///   `test_preseed_managed_trust_is_idempotent`,
///   `test_preseed_managed_trust_enables_mcp_servers`,
///   `test_preseed_managed_trust_no_home_write`,
///   `test_preseed_managed_trust_quarantines_malformed_json`,
///   `test_preseed_managed_trust_excludes_conditional_builtin_when_toggle_off`,
///   `test_preseed_managed_trust_excludes_unconditional_builtin_when_pin_failed`
///   (issue #3950 regression).
pub fn preseed_managed_trust(
    claude_config_dir: &Path,
    workspace: &Path,
    trusty_mpm_pinned: bool,
    trusty_review_pinned: bool,
    trusty_memory_pinned: bool,
    trusty_search_pinned: bool,
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
                //
                // Issue #4206: the quarantine name is TIMESTAMPED
                // (`trusty_common::claude_config::quarantine_path`) rather than
                // the fixed `.claude.json.corrupt` this used to use. The fixed
                // name meant a second quarantine — here or in the sibling
                // writer `core::mcp_config::read_config` — silently overwrote
                // the first, destroying the only record of the first failure.
                let corrupt_path = trusty_common::claude_config::quarantine_path(&claude_json);
                // ERROR, not warn: losing a `.claude.json` (OAuth state + every
                // project's trust) is an operator-visible data-loss event. The
                // workspace's error-capture layer filters on `!= Level::ERROR`
                // and is only composed for the Daemon and Supervisor commands,
                // so a `warn!` here reached NO diagnostic surface at all —
                // verified: no `quarantin*` line has ever reached `daemon.log`.
                match std::fs::rename(&claude_json, &corrupt_path) {
                    Ok(()) => tracing::error!(
                        "managed trust pre-seed: {} is not valid JSON ({err}); \
                         quarantined to {} — proceeding with fresh config",
                        claude_json.display(),
                        corrupt_path.display()
                    ),
                    Err(rename_err) => tracing::error!(
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
    // unconditional framework builtins and the two conditional builtins,
    // EACH gated by whether its injector actually pinned the entry this run
    // (issues #3934/#3950), UNION the operator's own `tm mcp add` registry —
    // computed from the immutable config BEFORE the mutable navigation below
    // borrows it. Deliberately NOT derived from `workspace`'s own
    // `.mcp.json` — see the module doc's security note (issue #3918
    // follow-up): that file is git-tracked content that arrives with a
    // cloned repo, and auto-approving whatever it declares would let a
    // hostile clone smuggle an arbitrary MCP server into a daemon-managed
    // session with no human present to decline it.
    let enabled_mcp = Value::Array(
        managed_mcp_server_names(
            &config,
            trusty_mpm_pinned,
            trusty_review_pinned,
            trusty_memory_pinned,
            trusty_search_pinned,
        )
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

    // Issue #4206: bound the growth of this file during the read-modify-write
    // that is already in flight. Only entries that are BOTH definitively gone
    // from disk AND carry nothing but this seeder's own keys are removed — see
    // `prune_stale_project_entries` for why the conjunction, and why an
    // ambiguous filesystem error keeps the entry.
    let pruned = prune_stale_project_entries(projects, &workspace_key);
    if pruned > 0 {
        tracing::info!(
            "managed trust pre-seed: pruned {pruned} stale project entr{} from {} \
             (directory definitively absent and entry carried no Claude Code runtime state)",
            if pruned == 1 { "y" } else { "ies" },
            claude_json.display()
        );
    }

    let entry = projects
        .entry(workspace_key)
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(serde_json::Map::new());
    }
    let entry = entry.as_object_mut().expect("project entry is an object");

    // Idempotent: skip the write when all fields are already fully set AND the
    // MCP list already matches the canonical server set.
    //
    // Issue #4206: `pruned == 0` is part of the condition. Without it, a run
    // that pruned stale entries but found this workspace already seeded would
    // return here and DISCARD the prune — the file could then never shrink on
    // the (very common) repeat-launch path, which is exactly the path that let
    // it reach 2,541 entries.
    let already_seeded = pruned == 0
        && entry.get("hasTrustDialogAccepted") == Some(&Value::Bool(true))
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
