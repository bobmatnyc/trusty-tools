//! Project-trust seeding and stale-entry pruning for the managed CLAUDE_CONFIG_DIR.
//!
//! Why: a managed Claude Code session must not block on "Do you trust the files
//! in this folder?". Claude Code reads per-directory trust from
//! `$CLAUDE_CONFIG_DIR/.claude.json` when `CLAUDE_CONFIG_DIR` is set — NOT from
//! `$HOME/.claude.json` — so managed-path seeding must target
//! `<claude_config_dir>/.claude.json` (WI-3 sub-part 2).
//!
//! ISOLATION INVARIANT: this module NEVER writes to `~/.claude.json` or
//! `~/.claude/`. It writes only under `<claude_config_dir>` (the managed
//! CLAUDE_CONFIG_DIR — typically `~/.trusty-mpm/claude-config/`).
//!
//! What: [`preseed_managed_trust`] sets the three trust keys on
//! `projects.<workspace>` and REMOVES `enabledMcpjsonServers` from that entry.
//! [`prune_stale_project_entries`] deletes `projects` entries that are pure
//! seeder droppings for directories that are definitively gone (#4206).
//!
//! **#4181 / ADR-0042 — this module no longer derives an MCP approval.** It used
//! to write `enabledMcpjsonServers`, computed from the framework builtins gated
//! on four per-run `_pinned` results plus the operator's `tm mcp add` registry.
//! Those four parameters and that derivation went with the injectors that fed
//! them: MCP servers are now declared once in user scope, and a project-scope
//! approval is exactly what lets a workspace `.mcp.json` entry displace that
//! declaration. So the #3918 → #3934 → #3950 hardening chain — excluding a
//! cloned repo's own `.mcp.json` from the approval set, gating the conditional
//! builtins on their manifest toggle, then gating every builtin on its actual
//! per-run write result — hardened a mechanism this module no longer has. The
//! key is REMOVED from the entry rather than merely left unwritten, because
//! ceasing to write it would leave it in place on every machine a prior tm
//! launched; see [`preseed_managed_trust`]'s own doc.
//!
//! Test: `test_preseed_managed_trust_marks_directory`,
//!   `test_preseed_managed_trust_is_idempotent`,
//!   `test_preseed_managed_trust_writes_no_mcp_approval` (#4181),
//!   `test_preseed_managed_trust_strips_a_stale_mcp_approval` (#4181 migration),
//!   `test_preseed_managed_trust_preserves_other_keys`,
//!   `test_preseed_managed_trust_no_home_write` (isolation guard),
//!   `test_preseed_managed_trust_quarantines_malformed_json` (corrupt-file quarantine).

use std::path::Path;

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
/// What: the three keys [`preseed_managed_trust`] writes
/// (`hasTrustDialogAccepted`, `hasCompletedProjectOnboarding`,
/// `projectOnboardingSeenCount`), plus `enabledMcpjsonServers`, which #4181
/// stopped writing but which a dropping left by an older tm still carries — so
/// dropping it from this list would make those legacy entries stop looking pure
/// and never be pruned. MUST be kept in sync with the writes at the end
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

/// Pre-seed project trust into `<claude_config_dir>/.claude.json`, and strip any
/// MCP approval a prior version left behind.
///
/// Why (WI-3 sub-part 2): Claude Code blocks an unfamiliar project on "Do you
/// trust the files in this folder?", keyed to the absolute workspace path under
/// `projects.<workspace>`. Writing the trust keys before launch means managed
/// sessions start immediately without that dialog.
///
/// ISOLATION: writes ONLY to `<claude_config_dir>/.claude.json` — never to
/// `$HOME/.claude.json` or anything under `~/.claude/`. This is the central
/// isolation invariant for the managed driver (WI-7 support).
///
/// **#4181 / ADR-0042 — why `enabledMcpjsonServers` is REMOVED, not merely left
/// unwritten.** The approval is name-based and content-blind, and an approved
/// name makes the workspace `.mcp.json` entry of that name WIN over the
/// operator's own user-scope declaration — measured, and the whole hinge of the
/// #3918→#3950 name-squatting chain. tm no longer writes MCP config into a
/// workspace, so it approves no project-scope name either, and a repo's
/// colliding entry falls through to Claude Code's own consent dialog. Ceasing to
/// write would leave the key on every machine a prior tm launched, keeping the
/// displacement alive exactly where a cloned repo could reach it.
///
/// This also closes the widening #5398 shipped on the owner's explicit
/// understanding that this change removes the mechanism: this seeder applied no
/// `project_scope_mcp_names` subtraction (#2739), so a repo `[mcp.custom]` name
/// colliding with an operator registry name was pre-approved on `tm launch` /
/// `tm connect`. With no approval written, there is nothing to collide with.
///
/// What: reads `<claude_config_dir>/.claude.json` (starts from `{}` when absent;
/// a malformed file is quarantined to a timestamped path — #4206 — and seeding
/// proceeds from a fresh `{}`), then ensures `projects.<workspace>` carries
/// `hasTrustDialogAccepted: true`, `hasCompletedProjectOnboarding: true` and
/// `projectOnboardingSeenCount >= 1`, and REMOVES `enabledMcpjsonServers`.
/// `workspace` is used only as the `projects.<workspace>` key — this never reads
/// the workspace's own `.mcp.json`. Writes back atomically; idempotent once the
/// entry is trusted and the approval key is gone. All other keys are preserved.
/// Test: `test_preseed_managed_trust_marks_directory`,
///   `test_preseed_managed_trust_is_idempotent`,
///   `test_preseed_managed_trust_writes_no_mcp_approval`,
///   `test_preseed_managed_trust_strips_a_stale_mcp_approval`,
///   `test_preseed_managed_trust_no_home_write`,
///   `test_preseed_managed_trust_quarantines_malformed_json`.
pub fn preseed_managed_trust(claude_config_dir: &Path, workspace: &Path) -> anyhow::Result<()> {
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
        && !entry.contains_key("enabledMcpjsonServers");
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
    // #4181: drop the project-scope MCP approval, on this run and on every
    // machine a prior version already seeded.
    entry.remove("enabledMcpjsonServers");

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
