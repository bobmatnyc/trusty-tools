//! Managed-session hook definitions and idempotent merge into settings.json.
//!
//! Why: WI-3 requires the managed CLAUDE_CONFIG_DIR (`~/.trusty-mpm/claude-config/`)
//! to ship the full trusty-mpm hook triad (PreToolUse / PostToolUse / Stop) so
//! the daemon receives Claude Code lifecycle events from managed sessions. Without
//! hooks the circuit breaker, audit log, and dashboard are blind to managed sessions.
//! Centralising the definition here (rather than duplicating it in the `tm install`
//! binary) lets `ensure_global_config_dir` call it from the library crate, and
//! lets the binary re-use the same literal for `tm install`.
//! What: [`mpm_hook_command`] resolves the absolute binary path (falling back to
//! the bare name when resolution fails); [`mpm_hook_additions`] returns the hook
//! triad JSON block; [`ensure_managed_hooks`] reads `<claude_config_dir>/settings.json`,
//! deep-merges the triad idempotently, and writes back; [`remove_global_trusty_mpm_hooks`]
//! strips MPM hook entries from all global settings files; [`write_project_hooks`]
//! writes project-scoped hooks into a given settings file.
//! Test: `test_ensure_managed_hooks_writes_triad`,
//! `test_ensure_managed_hooks_is_idempotent`,
//! `test_hook_command_uses_absolute_path`,
//! `test_remove_global_trusty_mpm_hooks_removes_only_mpm_entries`,
//! `test_write_project_hooks_targets_project_dir`,
//! `test_is_mpm_hook_command_recognises_tm_bin_name`,
//! `test_write_project_hooks_replaces_stale_exe_path_group` in `tests`.

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

/// The bare fallback command name when the binary path cannot be resolved.
///
/// Why: if `current_exe()` fails (e.g. under `cargo test` without a real
/// install), we must still produce a working hook definition rather than
/// panicking. The bare name is what Claude Code would previously use
/// (resolved via PATH), so it degrades gracefully.
/// What: `"trusty-mpm hook"`.
const BARE_HOOK_COMMAND: &str = "trusty-mpm hook";

/// Resolve the absolute path for the `trusty-mpm hook` command.
///
/// Why: using a bare binary name means Claude Code resolves the hook via
/// `$PATH` at fire-time. In build environments or fresh shells where
/// `~/.cargo/bin` is absent from `PATH`, this silently fails to launch the
/// process — the hook executor throws a not-found error and the event is
/// lost. Using the absolute path of the running binary eliminates the PATH
/// dependency entirely and is correct by construction: the binary being
/// installed is the same binary running `tm install`.
/// What: delegates to [`resolve_stable_hook_exe`] and renders `"<abs-path>
/// hook"`; falls back to `BARE_HOOK_COMMAND` when no stable binary resolves so
/// the install never panics. If a caller already knows the exe path (e.g. from
/// `current_exe()` cached at startup), they can pass it via `exe_override` to
/// skip the syscall.
/// Test: `test_hook_command_uses_absolute_path`.
pub fn mpm_hook_command(exe_override: Option<&Path>) -> String {
    match resolve_stable_hook_exe(exe_override) {
        Some(p) => format!("{} hook", p.display()),
        None => BARE_HOOK_COMMAND.to_string(),
    }
}

/// Resolve a STABLE, installed absolute binary path for baking into managed
/// hook commands — never an ephemeral build/worktree path.
///
/// Why (#2229): `std::env::current_exe()` returns a
/// `target/debug/deps/trusty_mpm-<hash>` (or worktree) path when `tm` runs from
/// a debug build. Persisting that path into the SHARED global `settings.json`
/// breaks every managed session's hooks once the artifact is rebuilt away. The
/// hook command must instead point at a stable installed binary (`~/.cargo/bin/tm`)
/// or degrade to the PATH-resolved bare name — anything but the transient path.
/// What: prefers `exe_override` (canonicalized), then `current_exe()`
/// (canonicalized), but only when the result is absolute AND not an ephemeral
/// build path per [`trusty_common::bin_resolve::is_ephemeral_build_path`]. When
/// the running binary is ephemeral or unresolved, PATH-resolves the installed
/// `tm`/`trusty-mpm` binary via [`trusty_common::bin_resolve::resolve_binary`].
/// Returns `None` only when no stable absolute path can be found (caller then
/// uses the bare command name).
/// Test: covered by `test_hook_command_uses_absolute_path`,
/// `test_hook_command_rejects_ephemeral_exe_override`.
fn resolve_stable_hook_exe(exe_override: Option<&Path>) -> Option<PathBuf> {
    let running = exe_override
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()))
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.canonicalize().ok().or(Some(p)))
        });

    if let Some(p) = running
        && p.is_absolute()
        && !trusty_common::bin_resolve::is_ephemeral_build_path(&p)
    {
        return Some(p);
    }

    // Ephemeral or unresolved running path: fall back to a PATH-resolved
    // installed binary so the hook command survives worktree/debug rebuilds.
    MPM_BIN_NAMES
        .iter()
        .find_map(|name| trusty_common::bin_resolve::resolve_binary(name))
        .filter(|p| p.is_absolute())
}

/// Build the MPM lifecycle hook additions JSON block (six events).
///
/// Why: every call site — the managed global config writer AND `tm install` — must
/// use the exact same shape so [`trusty_common::claude_config::merge_hook_entries`]
/// can dedup by deep equality without producing duplicates. Centralising the literal
/// here means the managed path and the `install` path can never silently diverge.
/// `SessionStart` and `SessionEnd` (#1744) are required so the daemon can capture
/// the Claude Code internal session UUID (for `--resume`) and immediately mark a
/// session Stopped when Claude Code exits, even on ungraceful exits. Without these
/// two events wired, `correlate_session_start` and `handle_session_end` in
/// `daemon/api.rs` receive no traffic and `claude_session_id` stays `None` forever.
/// The `merge_hook_entries` dedup logic preserves any existing `SessionStart` entry
/// (e.g. `trusty-memory inbox-check` from the project-level config) — adding the
/// `trusty-mpm hook` entry alongside it does NOT clobber the memory hook.
/// What: returns a JSON object with six `hooks` arrays:
/// `PreToolUse`, `PostToolUse` (async), `Stop`, `SubagentStop`, `SessionStart`,
/// `SessionEnd`. `SubagentStop` (#2610) fires when a delegated Task-tool
/// subagent ends its turn, letting the hook handler flag an idle-parking final
/// message (see `commands::misc::hook` / `core::idle_parking`).
/// `PostToolUse` is marked `async: true` so Claude Code does not block waiting for
/// the daemon to ingest tool results; the other five use short synchronous timeouts.
/// `exe_override` pins the binary path for the hook command; pass `None` to resolve
/// via `mpm_hook_command(None)`.
/// Test: `test_mpm_hook_additions_has_six_events`,
/// covered by `test_ensure_managed_hooks_writes_triad`.
pub fn mpm_hook_additions_with_exe(exe_override: Option<&Path>) -> serde_json::Value {
    let cmd = mpm_hook_command(exe_override);
    serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": cmd,
                    "timeout": 5
                }]
            }],
            "PostToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": cmd,
                    "timeout": 60,
                    "async": true
                }]
            }],
            "Stop": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": cmd,
                    "timeout": 5
                }]
            }],
            "SubagentStop": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": cmd,
                    "timeout": 5
                }]
            }],
            "SessionStart": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": cmd,
                    "timeout": 5
                }]
            }],
            "SessionEnd": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": cmd,
                    "timeout": 5
                }]
            }]
        }
    })
}

/// Build the MPM lifecycle hook additions JSON block with the default exe resolution.
///
/// Why: convenience wrapper that calls `mpm_hook_additions_with_exe(None)` so
/// existing call sites that do not need to pin the exe path stay concise.
/// What: delegates to [`mpm_hook_additions_with_exe`] with `None`.
/// Test: covered by `test_mpm_hook_additions_has_six_events`.
pub fn mpm_hook_additions() -> serde_json::Value {
    mpm_hook_additions_with_exe(None)
}

/// Canonical binary names this crate ships as `[[bin]]` targets (`Cargo.toml`):
/// the full name and the short everyday alias used by `tm run`/`tm load`/`tm login`.
const MPM_BIN_NAMES: &[&str] = &["trusty-mpm", "tm"];

/// File-name STEMS that identify an mpm-owned binary once any Cargo
/// build-artifact `-<hash>` suffix is stripped.
///
/// Why (#2235): managed hooks resolved from a debug/worktree build embed a
/// `target/debug/deps/<stem>-<hash>` path whose file name is NOT one of
/// [`MPM_BIN_NAMES`] — the Cargo dep artifact uses the underscore crate name
/// (`trusty_mpm`) or the `[[bin]]` alias (`tm`) with a trailing content hash,
/// and the long-defunct pre-rename binary was `session_manager_mvp`. All of
/// these must be recognised as the SAME hook owner so stale entries collapse.
/// What: the canonical stems (dash + underscore spellings) plus the retired MVP
/// name. Matched by [`is_mpm_binary_filename`] against the hash-stripped stem.
const MPM_BIN_STEMS: &[&str] = &["trusty-mpm", "trusty_mpm", "tm", "session_manager_mvp"];

/// Recognise an mpm-owned binary by its file-name component, tolerating Cargo
/// build-artifact hash suffixes and the defunct `session_manager_mvp` name.
///
/// Why (#2235): dedup that keyed identity on the exact file name ∈
/// {`trusty-mpm`,`tm`} could never strip a stale entry whose command carried a
/// build-artifact path (`.../deps/trusty_mpm-<hash> hook`, `.../tm-<hash> hook`)
/// or the retired `session_manager_mvp-<hash>` name — those file names are not
/// in the recognised set, so every managed launch appended a fresh entry beside
/// the un-strippable stale ones and `settings.json` grew without bound. Matching
/// the whole binary family (canonical names + hash-suffixed artifacts + the MVP
/// name) makes the strip remove ALL prior managed variants before the merge, so
/// exactly one canonical entry per (event, matcher) survives.
/// What: returns `true` when `name` is a canonical [`MPM_BIN_NAMES`] entry, the
/// bare defunct `session_manager_mvp` name, OR a Cargo build-artifact of the
/// form `<stem>-<hexhash>` where `stem ∈ MPM_BIN_STEMS` and `<hexhash>` is an
/// all-hex-digit suffix of length ≥ 8 (the shape Cargo emits under
/// `target/*/deps/`). The hex-length guard keeps unrelated binaries that merely
/// share a stem prefix (e.g. `tm-cli`) from being mis-identified.
/// Test: `test_is_mpm_hook_command_recognises_tm_bin_name`,
/// `test_is_mpm_hook_command_recognises_stale_hash_and_mvp_variants`.
fn is_mpm_binary_filename(name: &str) -> bool {
    if MPM_BIN_NAMES.contains(&name) || name == "session_manager_mvp" {
        return true;
    }
    // Cargo build-artifact form: `<stem>-<hexhash>` (e.g. `trusty_mpm-1a2b3c4d`).
    if let Some((stem, hash)) = name.rsplit_once('-')
        && hash.len() >= 8
        && hash.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return MPM_BIN_STEMS.contains(&stem);
    }
    false
}

/// Check if a command string is one of the trusty-mpm hook command variants.
///
/// Why (#2015, #2235): the crate ships TWO `[[bin]]` targets that share one
/// binary — `trusty-mpm` and the short alias `tm` (the everyday entry point for
/// `tm run`/`tm load`/`tm login`). `mpm_hook_command` resolves whichever binary
/// is currently running via `current_exe()`, so a hook group's command can be
/// `.../tm hook` on one write and `.../trusty-mpm hook` on the next (or vice
/// versa) purely from which entry point launched the process. Worse, a
/// debug/worktree build resolves to a hash-suffixed artifact
/// (`.../deps/trusty_mpm-<hash> hook`) and legacy configs still carry the
/// retired `session_manager_mvp-<hash>` name — none of which the original
/// file-name-exact predicate recognised, so those stale groups were invisible
/// to the replace-by-identity strip in [`write_project_hooks`] and survived
/// every merge: exactly the unbounded-growth bug #2235 reports. The whole
/// binary family must be recognised as the SAME hook owner.
/// What: returns `true` when `cmd` ends with ` hook` (scoping the match to an
/// actual MPM hook invocation, not just any binary that happens to share a
/// name) AND the remaining prefix's file-name component is recognised by
/// [`is_mpm_binary_filename`] — covering bare names (`"tm hook"`), absolute
/// paths (`"/opt/bin/trusty-mpm hook"`), and hash-suffixed build artifacts
/// (`".../deps/trusty_mpm-<hash> hook"`, `"session_manager_mvp-<hash> hook"`).
/// Test: `test_remove_global_trusty_mpm_hooks_removes_only_mpm_entries`,
/// `test_is_mpm_hook_command_recognises_tm_bin_name`,
/// `test_is_mpm_hook_command_recognises_stale_hash_and_mvp_variants`,
/// `test_write_project_hooks_replaces_stale_exe_path_group`,
/// `test_write_project_hooks_collapses_stale_hash_and_mvp_entries`.
fn is_mpm_hook_command(cmd: &str) -> bool {
    // The command must end with " hook" (with exactly one trailing sub-command word).
    let Some(binary) = cmd.strip_suffix(" hook") else {
        return false;
    };
    Path::new(binary)
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(is_mpm_binary_filename)
}

/// Strip trusty-mpm hook entries from every global Claude settings file.
///
/// Why: after switching from global hooks to project-scoped hooks, any
/// previously installed global hook triad must be cleaned up so it does not
/// fire in unrelated projects. This mirrors the `remove_global_trusty_memory_hooks`
/// pattern from trusty-memory — strip first, then write project-scoped hooks.
/// What: discovers all `.claude/settings*.json` files under `$HOME` via
/// [`trusty_common::claude_config::discover_claude_settings`], and for each file
/// removes any hook group whose `command` field matches a trusty-mpm hook pattern
/// (see [`is_mpm_hook_command`]). Writes back atomically only when something
/// actually changed. Returns the count of files modified.
/// Test: `test_remove_global_trusty_mpm_hooks_removes_only_mpm_entries`.
pub fn remove_global_trusty_mpm_hooks() -> anyhow::Result<usize> {
    use trusty_common::claude_config::{
        default_settings_max_depth, discover_claude_settings, write_json_atomic,
    };

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    let files = discover_claude_settings(&home, default_settings_max_depth());

    let mut changed = 0usize;
    for path in &files {
        let text = match std::fs::read_to_string(path) {
            Ok(s) if s.trim().is_empty() => continue,
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut val: serde_json::Value = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) if v.is_object() => v,
            _ => continue,
        };

        if strip_mpm_hook_entries(&mut val) {
            if let Err(e) = write_json_atomic(path, &val) {
                eprintln!(
                    "warning: could not remove MPM hooks from {}: {e}",
                    path.display()
                );
            } else {
                changed += 1;
            }
        }
    }
    Ok(changed)
}

/// Remove every trusty-mpm hook entry from a settings JSON value in-place.
///
/// Why: shared logic between `remove_global_trusty_mpm_hooks` and tests.
/// What: delegates to [`strip_mpm_hook_entries_for_events`] with `events =
/// None`, which strips MPM-owned groups from every event key present.
/// Test: `test_remove_global_trusty_mpm_hooks_removes_only_mpm_entries`.
fn strip_mpm_hook_entries(val: &mut serde_json::Value) -> bool {
    strip_mpm_hook_entries_for_events(val, None)
}

/// Remove MPM-owned hook groups for the given events (or all events) in-place.
///
/// Why (#2015): [`trusty_common::claude_config::merge_hook_entries`] dedups a
/// hook group only by byte-for-byte JSON equality. When the resolved absolute
/// exe path changes (bin name `tm` vs `trusty-mpm`, worktree rebuilds,
/// reinstalls) the `command` string differs, so a NEW MPM hook group is
/// appended beside the stale one on every merge — MPM groups accumulate and
/// each fires on every lifecycle event. Replacing the existing MPM-owned
/// group for an event *before* merging enforces replace-by-identity (event
/// name + "is this an MPM hook"), not full-value equality, so exactly one
/// MPM group per event survives regardless of exe-path churn.
/// What: when `events` is `Some(list)`, only those event keys are inspected;
/// when `None`, every event key under `hooks` is inspected (used by
/// [`remove_global_trusty_mpm_hooks`] / [`strip_mpm_hook_entries`], which must
/// clean up all events, not just a fresh-additions subset). Within scope,
/// filters out matcher groups whose `hooks[*].command` all match
/// [`is_mpm_hook_command`], removing the event key entirely once its array is
/// empty. Non-MPM groups and out-of-scope events are left untouched. Returns
/// `true` if anything was removed.
/// Test: `test_remove_global_trusty_mpm_hooks_removes_only_mpm_entries`,
/// `test_write_project_hooks_replaces_stale_exe_path_group`.
fn strip_mpm_hook_entries_for_events(
    val: &mut serde_json::Value,
    events: Option<&[String]>,
) -> bool {
    let Some(hooks_map) = val.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return false;
    };

    let scope: Vec<String> = match events {
        Some(evs) => evs.to_vec(),
        None => hooks_map.keys().cloned().collect(),
    };

    let mut any_changed = false;
    let mut events_to_remove: Vec<String> = Vec::new();

    for event_key in scope {
        let Some(arr) = hooks_map.get_mut(&event_key).and_then(|v| v.as_array_mut()) else {
            continue;
        };
        let before_len = arr.len();
        arr.retain(|group| {
            // Retain groups that do NOT contain exclusively mpm hook commands.
            let Some(inner_hooks) = group.get("hooks").and_then(|h| h.as_array()) else {
                return true; // keep unknown shapes
            };
            !inner_hooks.iter().all(|entry| {
                entry
                    .get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(is_mpm_hook_command)
            })
        });
        if arr.len() != before_len {
            any_changed = true;
            if arr.is_empty() {
                events_to_remove.push(event_key);
            }
        }
    }

    // Remove now-empty event keys entirely.
    for key in events_to_remove {
        hooks_map.remove(&key);
        any_changed = true;
    }

    // Remove the `hooks` key itself if the map is now empty.
    if hooks_map.is_empty() {
        val.as_object_mut().unwrap().remove("hooks");
    }

    any_changed
}

/// Write project-scoped MPM hooks into a single Claude settings file.
///
/// Why: global hooks fire in every project and can break unrelated build
/// environments that have a stripped PATH. Project-scoped hooks only fire
/// inside the specific project directory, matching trusty-memory's approach.
/// Before merging, any EXISTING MPM-owned hook group for each event about to
/// be (re-)added is stripped first (#2015): `merge_hook_entries` dedups only
/// by byte-for-byte JSON equality, so when the resolved exe path differs from
/// a previous write (bin name `tm` vs `trusty-mpm`, worktree rebuild,
/// reinstall) the stale group would otherwise survive alongside the fresh
/// one, and MPM hook groups accumulate — each one firing on every lifecycle
/// event. Stripping first enforces replace-by-identity (event name + "is this
/// an MPM hook") rather than full-value equality.
/// What: reads `settings_path` (tolerates missing/empty — starts from `{}`),
/// strips any MPM-owned hook group for the events present in the fresh
/// additions via [`strip_mpm_hook_entries_for_events`], deep-merges the MPM
/// hook additions, and writes back atomically only when something actually
/// changed. Returns `true` when the file was updated. `exe_override` is
/// forwarded to [`mpm_hook_additions_with_exe`] so the caller can pin the
/// binary path at install time.
/// Test: `test_write_project_hooks_targets_project_dir`,
/// `test_write_project_hooks_replaces_stale_exe_path_group`.
pub fn write_project_hooks(
    settings_path: &Path,
    exe_override: Option<&Path>,
) -> anyhow::Result<bool> {
    use trusty_common::claude_config::{merge_hook_entries, write_json_atomic};

    let original: serde_json::Value = match std::fs::read_to_string(settings_path) {
        Ok(s) if s.trim().is_empty() => serde_json::Value::Object(serde_json::Map::new()),
        Ok(s) => serde_json::from_str::<serde_json::Value>(&s)
            .ok()
            .filter(|v| v.is_object())
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            serde_json::Value::Object(serde_json::Map::new())
        }
        Err(e) => {
            return Err(anyhow::Error::new(e))
                .map_err(|e| anyhow::anyhow!("read {}: {e}", settings_path.display()));
        }
    };

    let additions = mpm_hook_additions_with_exe(exe_override);

    // Replace-by-identity: drop any stale MPM-owned group for each event we
    // are about to add, so the merge below can never leave two MPM groups
    // (old exe path + new exe path) side-by-side for the same event.
    let mut base = original.clone();
    if let Some(events) = additions.get("hooks").and_then(|h| h.as_object()) {
        let event_keys: Vec<String> = events.keys().cloned().collect();
        strip_mpm_hook_entries_for_events(&mut base, Some(&event_keys));
    }

    let merged = merge_hook_entries(&base, &additions);

    if merged == original {
        return Ok(false);
    }

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_json_atomic(settings_path, &merged)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", settings_path.display()))?;
    Ok(true)
}

/// Idempotently merge the MPM hook triad into `<claude_config_dir>/settings.json`.
///
/// Why: the managed CLAUDE_CONFIG_DIR starts with an empty `settings.json` (`{}`),
/// so without this call managed sessions have NO hooks and the daemon is blind to
/// their lifecycle events. Called from [`super::global_config::ensure_global_config_dir`]
/// after the initial settings.json seed so the file is always wired on every
/// managed launch.
/// What: reads `<claude_config_dir>/settings.json` (tolerates missing / empty /
/// malformed by starting from `{}`), deep-merges [`mpm_hook_additions`] using
/// [`trusty_common::claude_config::merge_hook_entries`], and writes back only when
/// the merged value differs — so calling this twice produces identical files
/// (idempotency requirement). Uses [`mpm_hook_additions_with_exe`] to embed the
/// absolute binary path rather than a bare name.
/// Test: `test_ensure_managed_hooks_writes_triad`, `test_ensure_managed_hooks_is_idempotent`.
pub fn ensure_managed_hooks(claude_config_dir: &Path) -> anyhow::Result<()> {
    let settings_path = claude_config_dir.join("settings.json");
    write_project_hooks(&settings_path, None).map(|_| ())
}

/// Resolve the resolved-exe path from `current_exe()` for use at install time.
///
/// Why: callers that run `tm install` should pin the absolute path once
/// (at install entry, before any `cd`) and pass it through. This helper
/// centralises resolution and, crucially, refuses to pin an ephemeral
/// build/worktree path (#2229) that would 404 after a rebuild — falling back to
/// the PATH-resolved installed binary instead.
/// What: delegates to [`resolve_stable_hook_exe`] with no override, returning a
/// stable installed absolute path, or `None` when none can be found.
/// Test: covered indirectly by `test_hook_command_uses_absolute_path`,
/// `test_hook_command_rejects_ephemeral_exe_override`.
pub fn resolve_current_exe() -> Option<PathBuf> {
    resolve_stable_hook_exe(None)
}
