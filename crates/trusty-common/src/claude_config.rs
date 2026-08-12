//! Claude Code configuration discovery and patching.
//!
//! Why: trusty-search, trusty-analyze, and trusty-memory each grew their own
//! "setup" command that scans `$HOME` for `.claude/settings*.json`, upserts an
//! MCP server entry, and writes JSON atomically. Three divergent copies meant
//! three subtly different skip-lists, three backup strategies, and three sets
//! of bugs. This module is the single shared implementation.
//!
//! What: pure-ish helpers — directory scanning, idempotent JSON upsert, atomic
//! writes with backup, and Claude Code hook merging. The one piece of process
//! state is [`staging_stamp`]'s counter, which exists so two concurrent writers
//! cannot pick the same staging filename (#4077).
//!
//! Test: `cargo test -p trusty-common` covers `mcp_server_entry` shape,
//! `merge_hook_entries` idempotency, and `discover_claude_settings` skip-dir
//! behaviour. Filesystem-touching tests are `#[ignore]`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

/// Directory names that are never recursed into while scanning for Claude
/// settings files.
///
/// Why: `$HOME` contains huge, irrelevant subtrees (`node_modules`, `target`,
/// `Library`). Walking them is slow and pollutes results. A shared const keeps
/// every trusty-* setup command skipping exactly the same directories.
/// What: a flat slice of directory base-names compared case-sensitively.
/// Test: `discover_claude_settings_skips_blacklisted_dirs` plants a settings
/// file inside `node_modules` and asserts it is not returned.
pub const SCAN_SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    "Library",
    "Applications",
    ".Trash",
    "build",
    "dist",
    ".cache",
    ".npm",
    ".cargo",
];

/// Default recursion depth for [`discover_claude_settings`].
const DEFAULT_SETTINGS_MAX_DEPTH: usize = 8;

/// Environment variable name stamped on every MPM-spawned sub-agent process.
///
/// Why: MPM spawns nested Claude Code sessions ("sub-agents") whose stdout
/// and audit traffic should not feed back into the parent's hook pipeline.
/// Specifically, the `trusty-mpm hook` command short-circuits when this var
/// is set so PreToolUse / PostToolUse / Stop events from a sub-agent never
/// double-post to the daemon. Memory enrichment (`trusty-memory
/// prompt-context`) deliberately does **not** guard on this var — sub-agents
/// benefit from the parent palace's prompt-fact block as much as the PM does.
/// Centralising the literal here keeps the spawn side (`trusty-agents`) and the
/// consumer side (`trusty-mpm-cli`) referencing the exact same string so a
/// rename never silently breaks the guard.
/// What: the literal `"CLAUDE_MPM_SUB_AGENT"`. Presence is what matters; the
/// canonical value used by spawn helpers is `"1"`.
/// Test: covered by `trusty-mpm-cli::tests::hook_guard_short_circuits`.
pub const CLAUDE_MPM_SUB_AGENT_ENV_VAR: &str = "CLAUDE_MPM_SUB_AGENT";

/// Scan `home` for every `.claude/settings.json` and `.claude/settings.local.json`.
///
/// Why: a Claude Code user can have settings files scattered across many
/// project directories. Setup commands need to find them all to offer the user
/// a choice of where to install an MCP server. Each sibling project reinvented
/// this walk; centralising it fixes the skip-list once.
/// What: recursively walks `home` up to `max_depth` directories deep, skipping
/// any directory whose base-name is in [`SCAN_SKIP_DIRS`]. For every `.claude`
/// directory found, checks for `settings.json` and `settings.local.json` and
/// collects the ones that exist. A `max_depth` of 0 inspects only `home`
/// itself. Use [`DEFAULT_SETTINGS_MAX_DEPTH`] (8) as a sensible default.
/// Returns paths sorted for deterministic output.
/// Test: `discover_claude_settings_skips_blacklisted_dirs` (`#[ignore]`, real
/// filesystem) verifies both discovery and skip behaviour.
pub fn discover_claude_settings(home: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect_claude_settings(home, max_depth, &mut found);
    found.sort();
    found
}

/// Recursive worker for [`discover_claude_settings`].
fn collect_claude_settings(dir: &Path, depth_remaining: usize, out: &mut Vec<PathBuf>) {
    // If this directory is a `.claude` dir, harvest its settings files.
    if dir.file_name().and_then(|n| n.to_str()) == Some(".claude") {
        for name in ["settings.json", "settings.local.json"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                out.push(candidate);
            }
        }
        // `.claude` directories don't contain nested projects worth scanning.
        return;
    }

    if depth_remaining == 0 {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return, // permission denied / not a dir — skip silently
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Always allow `.claude` itself; otherwise honour the skip-list.
        if name != ".claude" && SCAN_SKIP_DIRS.contains(&name) {
            continue;
        }
        collect_claude_settings(&path, depth_remaining.saturating_sub(1), out);
    }
}

/// Default [`discover_claude_settings`] depth, exposed for callers that want
/// the library default without hard-coding the number.
///
/// Why: keeps the "8" in one place so a future tuning change is one edit.
/// What: returns [`DEFAULT_SETTINGS_MAX_DEPTH`].
/// Test: compile-time constant; no runtime test needed.
pub const fn default_settings_max_depth() -> usize {
    DEFAULT_SETTINGS_MAX_DEPTH
}

/// Build a standard MCP server entry JSON object.
///
/// Why: every trusty-* MCP server is registered with the same `{command, args}`
/// shape. A constructor avoids hand-built `json!` literals drifting in field
/// names or omitting `args`.
/// What: returns `{"command": <command>, "args": [<args...>]}`. `args` is always
/// present (an empty array when no args are supplied) because Claude Code
/// expects the key.
/// Test: `mcp_server_entry_has_expected_shape`.
pub fn mcp_server_entry(command: &str, args: &[&str]) -> Value {
    json!({
        "command": command,
        "args": args,
    })
}

/// Atomically write `value` as pretty-printed JSON to `path`.
///
/// Why: a crash or `^C` mid-write must never leave a half-written settings
/// file — that would brick the user's Claude Code config. Writing to a temp
/// file then renaming makes the swap atomic on every supported OS.
///
/// Why the staging names are per-call (issue #4077): they used to be the FIXED
/// `<path>.tmp` and `<path>.bak`, which every writer of `path` shared. Two
/// concurrent writers then truncated and filled the SAME `<path>.tmp`, and
/// whichever renamed first published whatever bytes were in it at that instant
/// — the other writer's payload, or half of it. That is durable torn-file
/// corruption of the exact file this function exists to protect, and it is not
/// confined to one process: `tm launch` seeds `~/.claude.json` from the CLI
/// while the daemon seeds it from its own, so no in-process mutex can cover it.
/// The backup carried the same defect one line up — two `fs::copy` calls into
/// one `<path>.bak` interleave into a torn backup, destroying the recovery
/// artifact at the moment it is needed.
///
/// What: serialises `value` to pretty JSON, then publishes through per-call
/// staging paths — `<path>.bak.<pid>.<seq>` renamed onto `<path>.bak` (skipped
/// when `path` does not yet exist), and `<path>.tmp.<pid>.<seq>` renamed onto
/// `path`. Parent directories are created if missing. Every writer therefore
/// fills a name no other live writer can hold, and `rename` publishes one
/// writer's COMPLETE bytes; a reader of `path` sees one writer's payload or
/// the other's, never a splice. Concurrent writers can still LOSE an update —
/// serialising a read-modify-write cycle is the caller's job (`trusty-mpm`'s
/// `core::claude_json_guard` does it in-process) — but they can no longer
/// corrupt. A staged file is removed if its own write or rename fails, so a
/// failed call leaves `path` byte-for-byte as it was and drops no litter.
/// Test: `write_json_atomic_creates_and_backs_up` (`#[ignore]`, real fs),
/// `concurrent_writers_never_publish_a_torn_file` (#4077),
/// `concurrent_writers_never_tear_the_backup` (#4077),
/// `failed_rename_leaves_destination_and_no_staging_file`,
/// `staging_paths_are_unique_per_call`.
pub fn write_json_atomic(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir {}", parent.display()))?;
    }

    let serialized =
        serde_json::to_string_pretty(value).context("serialize JSON for atomic write")?;

    // #4077: stage under a name no other live writer can hold, then rename.
    let stamp = staging_stamp();

    if path.exists() {
        let backup = backup_path(path);
        let staged = append_extension(path, &format!("bak.{stamp}"));
        stage_then_publish(&staged, &backup, |dest| {
            std::fs::copy(path, dest).map(|_| ())
        })
        .with_context(|| format!("back up {} to {}", path.display(), backup.display()))?;
    }

    let staged = append_extension(path, &format!("tmp.{stamp}"));
    stage_then_publish(&staged, path, |dest| {
        std::fs::write(dest, serialized.as_bytes())
    })
    .with_context(|| format!("atomically write {}", path.display()))
}

/// Fill `staged` via `fill`, then rename it onto `dest`, removing `staged` if
/// either step fails.
///
/// Why (issue #4077): the rename is what makes the publish atomic, and the
/// removal is what keeps per-call staging names from turning every failed
/// write into permanent litter in the operator's home directory. Both callers
/// in [`write_json_atomic`] need exactly this sequence, and a copy of it in
/// each is how the two would drift.
/// What: calls `fill(staged)`, then `rename(staged, dest)`. On either error the
/// staged file is removed best-effort (its own removal failure is not worth
/// masking the real error) and the original error is returned, leaving `dest`
/// untouched. The context names the stage that actually failed — a fill error
/// never reached the publish, so reporting it as "publish X onto Y" pointed a
/// reader at a rename that was never attempted.
/// Test: `concurrent_writers_never_publish_a_torn_file`,
/// `failed_write_leaves_no_staging_file` (fill branch),
/// `failed_rename_leaves_destination_and_no_staging_file` (rename branch).
fn stage_then_publish(
    staged: &Path,
    dest: &Path,
    fill: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<()> {
    let filled = fill(staged);
    let reached_publish = filled.is_ok();
    let result = filled.and_then(|()| std::fs::rename(staged, dest));
    if result.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    result.with_context(|| {
        if reached_publish {
            format!("publish {} onto {}", staged.display(), dest.display())
        } else {
            format!("fill staging file {}", staged.display())
        }
    })
}

/// A suffix unique to this call among every live writer on the machine.
///
/// Why (issue #4077): uniqueness must hold across THREADS and across
/// PROCESSES, because the racing writers are `tm launch` and the daemon as
/// often as they are two tokio tasks. The pid separates processes; the counter
/// separates calls within one process and, unlike a wall-clock reading, cannot
/// repeat under a coarse timer. A leftover staging file from a crashed process
/// whose pid is later reused is harmless: the new writer truncates it, fills it
/// completely, and renames — the collision that matters is only between two
/// writers that are both live, and pid plus counter makes that impossible.
/// What: `<process id>.<monotonically increasing counter>`.
/// Test: `staging_paths_are_unique_per_call`.
fn staging_stamp() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Idempotently upsert a single entry into the `mcpServers` object of a JSON
/// config file.
///
/// Why: setup commands must be safe to re-run. Running `setup` twice should not
/// duplicate the server entry, clobber unrelated config, or rewrite the file
/// when nothing changed. All three sibling projects needed this exact contract.
/// What: loads `path` (treating a missing file as an empty `{}` object),
/// ensures a `mcpServers` object exists, and sets `mcpServers[server_key] =
/// entry`. If the key already maps to a value equal to `entry`, nothing is
/// written and `Ok(false)` is returned. Otherwise the merged config is written
/// via [`write_json_atomic`] (which backs the original up to `<path>.bak`) and
/// `Ok(true)` is returned. Creates the file if it does not exist.
/// Test: `patch_mcp_server_is_idempotent` (`#[ignore]`, real fs).
pub fn patch_mcp_server(path: &Path, server_key: &str, entry: &Value) -> Result<bool> {
    let mut root = load_json_object(path)?;

    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));

    // If `mcpServers` exists but isn't an object, replace it with one.
    if !servers.is_object() {
        *servers = Value::Object(Map::new());
    }
    let servers_obj = servers
        .as_object_mut()
        .expect("mcpServers coerced to object above");

    if servers_obj.get(server_key) == Some(entry) {
        return Ok(false); // already present and identical — no write
    }

    servers_obj.insert(server_key.to_string(), entry.clone());
    write_json_atomic(path, &Value::Object(root))?;
    Ok(true)
}

/// Merge Claude Code hook entries from `additions` into `existing`.
///
/// Why: trusty-* setup commands install Stop / PostToolUse / UserPromptSubmit
/// hooks. A naive overwrite would destroy hooks the user (or another tool)
/// already configured. Merging additively is the safe, shared behaviour.
/// What: returns a new `Value` that is `existing` deep-cloned with the `hooks`
/// object merged. For each known hook event (`Stop`, `PostToolUse`,
/// `UserPromptSubmit`) the arrays from `additions.hooks.<event>` are appended
/// to `existing.hooks.<event>`, skipping any addition already present (deep
/// equality) so the operation is idempotent. Hook events outside the known set
/// are also merged the same way so callers are not blocked by this list.
/// `existing` entries are never removed or reordered.
/// Test: `merge_hook_entries_is_idempotent` and
/// `merge_hook_entries_preserves_existing`.
pub fn merge_hook_entries(existing: &Value, additions: &Value) -> Value {
    let mut result = existing.clone();

    let Some(add_hooks) = additions.get("hooks").and_then(Value::as_object) else {
        return result; // nothing to merge
    };

    // Ensure result is an object with a `hooks` object.
    if !result.is_object() {
        result = Value::Object(Map::new());
    }
    let root = result
        .as_object_mut()
        .expect("result coerced to object above");
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    let hooks_obj = hooks
        .as_object_mut()
        .expect("hooks coerced to object above");

    for (event, add_value) in add_hooks {
        let Some(add_array) = add_value.as_array() else {
            continue; // hook events are arrays in Claude Code config
        };
        let target = hooks_obj
            .entry(event.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        if !target.is_array() {
            *target = Value::Array(Vec::new());
        }
        let target_array = target
            .as_array_mut()
            .expect("target coerced to array above");
        for item in add_array {
            if !target_array.contains(item) {
                target_array.push(item.clone());
            }
        }
    }

    result
}

/// Reserve a unique, timestamped quarantine path for a corrupt config file.
///
/// Why (issue #4206): two independent writers in trusty-mpm
/// (`core::standalone::trust_seed` and `core::mcp_config`) each renamed a
/// malformed `.claude.json` to the FIXED name `.claude.json.corrupt`. The
/// second quarantine therefore silently overwrote the first — destroying the
/// only record of the first failure, in the exact file that also holds
/// `oauthAccount` and every project's trust state. A post-mortem could see
/// that corruption had happened at least once and nothing more. Centralising
/// the naming here (rather than fixing it twice) means the two writers can
/// never drift back apart, and any future third writer inherits the same
/// scheme.
/// What: `<path>.corrupt-<UTC timestamp>`, e.g.
/// `.claude.json.corrupt-20260728T025723.418Z`. Millisecond precision makes a
/// same-second collision unlikely; the function additionally probes for an
/// unused name (appending `-1`, `-2`, …, bounded) so two quarantine events can
/// NEVER collapse onto one file even under a coarse clock. Purely a name
/// computation — it does not create, rename, or touch anything, so the tiny
/// TOCTOU window between probing and the caller's `rename` is acceptable: the
/// worst case is the same overwrite that was previously guaranteed, and only
/// under a race that the timestamp already makes vanishingly rare.
/// Test: `quarantine_path_is_timestamped_and_unique`,
///   `quarantine_path_avoids_existing_file`.
pub fn quarantine_path(path: &Path) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    quarantine_path_with_stamp(path, &stamp)
}

/// Stamp-injected core of [`quarantine_path`].
///
/// Why: the collision-probe loop is the only part of the naming scheme that can
/// actually lose a quarantine record, and it is unreachable from a test that
/// cannot control the timestamp — with a wall clock the caller can never plant
/// a file at the name the next call will compute, so the probe would only ever
/// be exercised by an unreproducible same-millisecond race. Taking the stamp as
/// a parameter makes the loop deterministically testable without weakening the
/// public signature or introducing a clock abstraction.
/// What: identical behaviour to [`quarantine_path`], with `stamp` supplied by
/// the caller instead of read from `Utc::now`. Private — the public entry point
/// remains the only supported way to reserve a quarantine name.
/// Test: `quarantine_path_avoids_existing_file` drives this directly with a
///   fixed stamp; `quarantine_path_is_timestamped_and_unique` covers the public
///   wrapper.
fn quarantine_path_with_stamp(path: &Path, stamp: &str) -> PathBuf {
    let base = append_extension(path, &format!("corrupt-{stamp}"));
    if !base.exists() {
        return base;
    }
    // Coarse clock or a genuine same-millisecond race: find an unused sibling
    // rather than clobbering the earlier record. Bounded so a pathological
    // filesystem can never spin here; the final fallback still returns a
    // distinct-by-timestamp name.
    for n in 1..1000 {
        let candidate = append_extension(path, &format!("corrupt-{stamp}-{n}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    base
}

// ─── internal helpers ─────────────────────────────────────────────────────

/// Path of the backup file written before an atomic JSON write: `<path>.bak`.
fn backup_path(path: &Path) -> PathBuf {
    append_extension(path, "bak")
}

/// Append `suffix` to a path's file name, preserving the existing extension
/// (`settings.json` → `settings.json.bak`).
fn append_extension(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

/// Load `path` as a JSON object map, returning an empty map when the file is
/// absent. Errors on malformed JSON or a non-object top-level value.
fn load_json_object(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            if text.trim().is_empty() {
                return Ok(Map::new());
            }
            let value: Value = serde_json::from_str(&text)
                .with_context(|| format!("parse JSON config {}", path.display()))?;
            match value {
                Value::Object(map) => Ok(map),
                other => anyhow::bail!(
                    "config {} is not a JSON object (found {})",
                    path.display(),
                    json_type_name(&other)
                ),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => {
            Err(anyhow::Error::new(e)).with_context(|| format!("read config {}", path.display()))
        }
    }
}

/// Human-readable JSON type name for error messages.
fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("trusty-claude-config-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn mcp_server_entry_has_expected_shape() {
        let e = mcp_server_entry("trusty-search", &["mcp", "--stdio"]);
        assert_eq!(e["command"], "trusty-search");
        assert_eq!(e["args"], json!(["mcp", "--stdio"]));
    }

    #[test]
    fn mcp_server_entry_always_includes_args_array() {
        let e = mcp_server_entry("foo", &[]);
        assert!(e["args"].is_array());
        assert_eq!(e["args"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn merge_hook_entries_preserves_existing() {
        let existing = json!({
            "hooks": { "Stop": [{ "command": "user-hook" }] },
            "other": "untouched"
        });
        let additions = json!({
            "hooks": { "Stop": [{ "command": "trusty-hook" }] }
        });
        let merged = merge_hook_entries(&existing, &additions);
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert!(stop.contains(&json!({ "command": "user-hook" })));
        assert!(stop.contains(&json!({ "command": "trusty-hook" })));
        assert_eq!(merged["other"], "untouched");
    }

    #[test]
    fn merge_hook_entries_is_idempotent() {
        let existing = json!({ "hooks": {} });
        let additions = json!({
            "hooks": {
                "PostToolUse": [{ "command": "trusty" }],
                "UserPromptSubmit": [{ "command": "trusty-prompt" }]
            }
        });
        let once = merge_hook_entries(&existing, &additions);
        let twice = merge_hook_entries(&once, &additions);
        assert_eq!(
            once, twice,
            "merging the same additions twice must be a no-op"
        );
        assert_eq!(once["hooks"]["PostToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(
            once["hooks"]["UserPromptSubmit"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn merge_hook_entries_handles_missing_hooks_block() {
        let existing = json!({ "model": "claude" });
        let additions = json!({ "hooks": { "Stop": [{ "command": "trusty" }] } });
        let merged = merge_hook_entries(&existing, &additions);
        assert_eq!(merged["model"], "claude");
        assert_eq!(merged["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn merge_hook_entries_noop_when_no_additions() {
        let existing = json!({ "hooks": { "Stop": [{ "command": "x" }] } });
        let merged = merge_hook_entries(&existing, &json!({}));
        assert_eq!(merged, existing);
    }

    /// The name must carry a UTC timestamp, preserve the original file name,
    /// and never repeat across two calls — the whole point of #4206's fix is
    /// that a second quarantine cannot land on the first one's bytes.
    #[test]
    fn quarantine_path_is_timestamped_and_unique() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");

        let first = quarantine_path(&path);
        let name = first.file_name().unwrap().to_string_lossy().into_owned();

        assert!(
            name.starts_with(".claude.json.corrupt-"),
            "quarantine name must extend the original file name: {name}"
        );
        let stamp = name
            .strip_prefix(".claude.json.corrupt-")
            .expect("prefix asserted above");
        assert!(
            stamp.len() >= 20 && stamp.ends_with('Z') && stamp.contains('T'),
            "expected a `YYYYmmddTHHMMSS.sssZ` UTC stamp, got {stamp:?}"
        );
        assert_eq!(
            first.parent(),
            path.parent(),
            "quarantine must stay a sibling of the file it replaces"
        );

        // Simulate the first rename actually happening, then quarantine again:
        // whether the clock advanced or the probe loop fired, the second name
        // must not be the first one.
        std::fs::write(&first, b"{ corrupt").unwrap();
        let second = quarantine_path(&path);
        assert_ne!(
            first, second,
            "a second quarantine must never reuse an occupied name"
        );
    }

    /// The collision probe, driven with a fixed stamp so it is exercised
    /// deterministically rather than only under a same-millisecond race.
    #[test]
    fn quarantine_path_avoids_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        let stamp = "20260728T025723.418Z";

        let base = quarantine_path_with_stamp(&path, stamp);
        assert_eq!(
            base.file_name().unwrap(),
            ".claude.json.corrupt-20260728T025723.418Z"
        );

        std::fs::write(&base, b"first corruption").unwrap();
        let second = quarantine_path_with_stamp(&path, stamp);
        assert_eq!(
            second.file_name().unwrap(),
            ".claude.json.corrupt-20260728T025723.418Z-1",
            "an occupied name must be sidestepped, not overwritten"
        );

        std::fs::write(&second, b"second corruption").unwrap();
        let third = quarantine_path_with_stamp(&path, stamp);
        assert_eq!(
            third.file_name().unwrap(),
            ".claude.json.corrupt-20260728T025723.418Z-2",
            "the probe must keep walking, not stop at -1"
        );

        // The earlier records survive untouched — the defect being fixed.
        assert_eq!(std::fs::read(&base).unwrap(), b"first corruption");
        assert_eq!(std::fs::read(&second).unwrap(), b"second corruption");
    }

    #[test]
    fn append_extension_preserves_original() {
        let p = Path::new("/tmp/.claude/settings.json");
        assert_eq!(backup_path(p), Path::new("/tmp/.claude/settings.json.bak"));
        assert_eq!(
            append_extension(p, "tmp.7.0"),
            Path::new("/tmp/.claude/settings.json.tmp.7.0")
        );
    }

    /// Why (#4077): the fixed `<path>.tmp` was what two writers collided on.
    /// Two calls must never compute the same staging name, and the name must
    /// still be a sibling of the target so the publish is a same-filesystem
    /// rename rather than a copy.
    #[test]
    fn staging_paths_are_unique_per_call() {
        let p = Path::new("/tmp/.claude/settings.json");
        let first = append_extension(p, &format!("tmp.{}", staging_stamp()));
        let second = append_extension(p, &format!("tmp.{}", staging_stamp()));

        assert_ne!(
            first, second,
            "two writers must never stage through the same filename"
        );
        assert_eq!(first.parent(), p.parent(), "staging must stay a sibling");
        assert_eq!(second.parent(), p.parent(), "staging must stay a sibling");
    }

    /// Why (#4077): with per-call staging names, a failed publish that left its
    /// staged file behind would litter the operator's home with one dead file
    /// per failure instead of reusing one.
    #[test]
    fn failed_write_leaves_no_staging_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = dir.path();
        let dest = dir.join("settings.json");
        let staged = dir.join("settings.json.tmp.test");

        let err = stage_then_publish(&staged, &dest, |p| {
            std::fs::write(p, b"partial")?;
            Err(std::io::Error::other("simulated fill failure"))
        })
        .expect_err("a failing fill must propagate");

        assert!(
            !staged.exists(),
            "the staged file must be removed when the publish fails: {err}"
        );
        assert!(
            !dest.exists(),
            "a failed publish must not create the target"
        );
        let chain = format!("{err:#}");
        assert!(
            chain.contains("fill staging file"),
            "a fill failure must not be reported as a publish that never happened: {chain}"
        );
    }

    /// Why: `stage_then_publish` promises the staged file is removed on EITHER
    /// failure, and `write_json_atomic` promises a failed call leaves its target
    /// byte-for-byte as it was. Only the fill-fails branch was covered; this is
    /// the other one — fill SUCCEEDS and the rename fails.
    ///
    /// The failure is forced structurally, not by timing: `<path>.bak` is
    /// planted as a non-empty DIRECTORY, and renaming a non-directory onto a
    /// directory is required to fail by POSIX (`EISDIR` here, `ENOTDIR` on some
    /// platforms) and fails on Windows too. So `fs::copy` fills the staged
    /// backup, the rename onto `<path>.bak` cannot succeed, and the error
    /// propagates before the target is ever touched. No sleep, no race, no
    /// dependence on file permissions or on the test's uid.
    #[test]
    fn failed_rename_leaves_destination_and_no_staging_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = dir.path();
        let path = dir.join("settings.json");

        write_json_atomic(&path, &json!({ "v": 1 })).expect("seed target");
        let before = std::fs::read(&path).expect("read seeded target");

        // Occupy `<path>.bak` with a non-empty directory: the backup publish
        // fills its staging file, then cannot rename onto this.
        let backup = backup_path(&path);
        std::fs::create_dir(&backup).expect("plant backup directory");
        std::fs::write(backup.join("occupant"), b"not a backup").expect("occupy it");

        let err = write_json_atomic(&path, &json!({ "v": 2 }))
            .expect_err("a rename that cannot succeed must propagate");

        let chain = format!("{err:#}");
        assert!(
            chain.contains("publish ") && chain.contains(" onto "),
            "a rename failure must name the publish stage: {chain}"
        );

        assert_eq!(
            std::fs::read(&path).expect("target must survive"),
            before,
            "a failed publish must leave the target byte-for-byte unchanged"
        );

        let leftovers: Vec<String> = std::fs::read_dir(dir)
            .expect("list scratch dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("settings.json.bak.") || n.starts_with("settings.json.tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed publish must remove its own staging file, left: {leftovers:?}"
        );

        assert_eq!(
            std::fs::read(backup.join("occupant")).expect("occupant must survive"),
            b"not a backup",
            "the rename destination must be untouched by the failed publish"
        );
    }

    /// A writer's payload: `writer` and `pad` are consistent only WITHIN one
    /// writer's own value, so any splice of two writers either fails to parse
    /// or fails the cross-field equality check. Padded to ~4 KB so filling the
    /// staging file is not one atomic syscall's worth of bytes.
    fn racing_payload(writer: usize) -> Value {
        json!({ "writer": writer, "pad": "x".repeat(4096) })
    }

    /// Read `path` and classify it: `None` while it does not exist yet, `Some`
    /// of whether the bytes are exactly one writer's complete payload.
    fn observe_intact(path: &Path) -> Option<bool> {
        let text = std::fs::read_to_string(path).ok()?;
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return Some(false); // a splice that is not even valid JSON
        };
        let intact = value["writer"]
            .as_u64()
            .is_some_and(|w| value == racing_payload(w as usize));
        Some(intact)
    }

    /// Why (#4077): the defect is a TORN file, so the only proof is real
    /// writers contending on one path while a reader watches. Two directions
    /// are asserted — every write must SUCCEED, and every snapshot a reader
    /// ever observes must be exactly one writer's complete payload, never a
    /// splice. Watching throughout rather than only at the end is what catches
    /// the tear itself: the pre-fix code publishes a temp file another writer
    /// truncated, and the corrupt state is transient.
    ///
    /// Writers record failures instead of panicking so the storm runs to
    /// completion and the reader gets its evidence; the counts are asserted
    /// afterwards. Pre-fix, BOTH counters are non-zero.
    ///
    /// Reliability: 8 writers × 60 rounds = 480 contended publishes, watched by
    /// 2 readers spinning continuously. Every observed pre-fix run failed on
    /// both counters; see the PR body for the raw output. NOT `#[ignore]`d
    /// despite touching the filesystem — the module's other fs tests are, but a
    /// regression test for a corruption class that CI never runs proves
    /// nothing, and this one is a hermetic `tempdir` finishing in ~0.05s.
    #[test]
    fn concurrent_writers_never_publish_a_torn_file() {
        const WRITERS: usize = 8;
        const READERS: usize = 2;
        const ROUNDS: usize = 60;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        // Seed so readers never race the very first creation.
        write_json_atomic(&path, &racing_payload(0)).expect("seed write");

        let write_failures = AtomicU64::new(0);
        let torn_reads = AtomicU64::new(0);
        let done = std::sync::atomic::AtomicBool::new(false);

        std::thread::scope(|scope| {
            for _ in 0..READERS {
                let (path, torn_reads, done) = (&path, &torn_reads, &done);
                scope.spawn(move || {
                    while !done.load(Ordering::Relaxed) {
                        if observe_intact(path) == Some(false) {
                            torn_reads.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
            let writers: Vec<_> = (0..WRITERS)
                .map(|writer| {
                    let (path, write_failures) = (&path, &write_failures);
                    scope.spawn(move || {
                        for _ in 0..ROUNDS {
                            if write_json_atomic(path, &racing_payload(writer)).is_err() {
                                write_failures.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    })
                })
                .collect();

            // Join the WRITERS explicitly, then release the readers. Letting
            // the scope join everything would deadlock — the readers only stop
            // when `done` is set, and the scope sets nothing.
            for handle in writers {
                handle.join().expect("a writer thread must not panic");
            }
            done.store(true, Ordering::Relaxed);
        });

        assert_eq!(
            torn_reads.load(Ordering::Relaxed),
            0,
            "a reader observed .claude.json as a splice of two writers — concurrent \
             writers shared a staging file (issue #4077)"
        );
        assert_eq!(
            write_failures.load(Ordering::Relaxed),
            0,
            "concurrent atomic writes failed — two writers collided on one staging \
             filename (issue #4077)"
        );
        assert_eq!(
            observe_intact(&path),
            Some(true),
            "the final .claude.json is not any single writer's complete payload"
        );
    }

    /// Why (#4077): `<path>.bak` was published by a bare `fs::copy` into a
    /// shared fixed name, so two writers interleaved into a torn BACKUP — the
    /// recovery artifact corrupted exactly when it is needed. Same storm and
    /// same watched-snapshot assertion as the sibling test, aimed at the backup.
    #[test]
    fn concurrent_writers_never_tear_the_backup() {
        const WRITERS: usize = 8;
        const READERS: usize = 2;
        const ROUNDS: usize = 60;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        let backup = backup_path(&path);

        // Two seed writes: the first creates the target, the second creates the
        // backup, so every racing write takes the backup branch and the readers
        // never race the backup's first creation.
        write_json_atomic(&path, &racing_payload(0)).expect("seed target");
        write_json_atomic(&path, &racing_payload(0)).expect("seed backup");

        let torn_reads = AtomicU64::new(0);
        let done = std::sync::atomic::AtomicBool::new(false);

        std::thread::scope(|scope| {
            for _ in 0..READERS {
                let (backup, torn_reads, done) = (&backup, &torn_reads, &done);
                scope.spawn(move || {
                    while !done.load(Ordering::Relaxed) {
                        if observe_intact(backup) == Some(false) {
                            torn_reads.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
            let writers: Vec<_> = (0..WRITERS)
                .map(|writer| {
                    let path = &path;
                    scope.spawn(move || {
                        for _ in 0..ROUNDS {
                            // Errors are the sibling test's assertion; here the
                            // storm only has to keep the backup churning.
                            let _ = write_json_atomic(path, &racing_payload(writer));
                        }
                    })
                })
                .collect();
            for handle in writers {
                handle.join().expect("a writer thread must not panic");
            }
            done.store(true, Ordering::Relaxed);
        });

        assert_eq!(
            torn_reads.load(Ordering::Relaxed),
            0,
            "a reader observed .claude.json.bak as a splice of two writers — concurrent \
             backups shared one destination (issue #4077)"
        );
        assert_eq!(
            observe_intact(&backup),
            Some(true),
            "the final backup is not any single writer's complete payload"
        );
    }

    #[test]
    #[ignore = "touches the real filesystem"]
    fn write_json_atomic_creates_and_backs_up() {
        let dir = scratch_dir("atomic");
        let path = dir.join("settings.json");

        write_json_atomic(&path, &json!({ "v": 1 })).unwrap();
        assert!(path.exists());
        assert!(!backup_path(&path).exists(), "no backup on first write");

        write_json_atomic(&path, &json!({ "v": 2 })).unwrap();
        let backup = std::fs::read_to_string(backup_path(&path)).unwrap();
        assert!(backup.contains("\"v\": 1"));
        let current = std::fs::read_to_string(&path).unwrap();
        assert!(current.contains("\"v\": 2"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[ignore = "touches the real filesystem"]
    fn patch_mcp_server_is_idempotent() {
        let dir = scratch_dir("patch");
        let path = dir.join("settings.json");
        let entry = mcp_server_entry("trusty-search", &["mcp"]);

        let first = patch_mcp_server(&path, "trusty-search", &entry).unwrap();
        assert!(first, "first patch must modify the file");

        let second = patch_mcp_server(&path, "trusty-search", &entry).unwrap();
        assert!(!second, "re-patching identical entry must be a no-op");

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["mcpServers"]["trusty-search"], entry);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[ignore = "touches the real filesystem"]
    fn patch_mcp_server_preserves_other_keys() {
        let dir = scratch_dir("patch-preserve");
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"model":"claude","mcpServers":{"existing":{"command":"x"}}}"#,
        )
        .unwrap();

        let entry = mcp_server_entry("trusty-memory", &["mcp"]);
        patch_mcp_server(&path, "trusty-memory", &entry).unwrap();

        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["model"], "claude");
        assert_eq!(parsed["mcpServers"]["existing"]["command"], "x");
        assert_eq!(parsed["mcpServers"]["trusty-memory"], entry);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[ignore = "touches the real filesystem"]
    fn discover_claude_settings_skips_blacklisted_dirs() {
        let home = scratch_dir("discover");

        // A real project: home/proj/.claude/settings.json
        let real = home.join("proj").join(".claude");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("settings.json"), "{}").unwrap();
        std::fs::write(real.join("settings.local.json"), "{}").unwrap();

        // A buried one inside node_modules — must be skipped.
        let buried = home.join("node_modules").join("pkg").join(".claude");
        std::fs::create_dir_all(&buried).unwrap();
        std::fs::write(buried.join("settings.json"), "{}").unwrap();

        let found = discover_claude_settings(&home, default_settings_max_depth());
        assert_eq!(found.len(), 2, "should find only the two non-skipped files");
        assert!(
            found
                .iter()
                .all(|p| !p.to_string_lossy().contains("node_modules"))
        );

        std::fs::remove_dir_all(&home).ok();
    }
}
