//! Managed-session hook definitions and idempotent merge into settings.json.
//!
//! Why: WI-3 requires the managed CLAUDE_CONFIG_DIR (`~/.trusty-mpm/claude-config/`)
//! to ship the full trusty-mpm hook triad (PreToolUse / PostToolUse / Stop) so
//! the daemon receives Claude Code lifecycle events from managed sessions. Without
//! hooks the circuit breaker, audit log, and dashboard are blind to managed sessions.
//! Centralising the definition here (rather than duplicating it in the `tm install`
//! binary) lets `ensure_global_config_dir` call it from the library crate, and
//! lets the binary re-use the same literal for `tm install`.
//! What: [`mpm_hook_command`] resolves the absolute binary path, or returns
//! [`StableHookExeError`] when nothing stable resolves (#7244 — it used to fall
//! back to the bare name, which made a refusal look like a success at every
//! call site and let the write proceed); [`mpm_hook_additions`] returns the hook
//! triad JSON block; [`ensure_managed_hooks`] reads `<claude_config_dir>/settings.json`,
//! deep-merges the triad idempotently, and writes back;
//! [`remove_global_trusty_mpm_hooks_at`] strips MPM hook entries from the two
//! global settings files; [`write_project_hooks`] writes project-scoped hooks
//! into a given settings file.
//! Test: `test_ensure_managed_hooks_writes_triad`,
//! `test_ensure_managed_hooks_is_idempotent`,
//! `test_hook_command_uses_absolute_path`,
//! `remove_global_hooks_at_strips_only_the_two_global_files`,
//! `test_write_project_hooks_targets_project_dir`,
//! `test_is_mpm_hook_command_recognises_tm_bin_name`,
//! `test_write_project_hooks_replaces_stale_exe_path_group`,
//! `test_strip_mpm_hook_entries_removes_only_tm_entry_from_mixed_group` in `tests`.
//!
//! Issue #2940: [`cleanup`] (sibling module) builds `tm hooks clean` and the
//! `tm doctor` hook-hygiene probe on top of [`is_mpm_hook_command`] and
//! [`strip_mpm_hook_entries`] — see that module's doc for the contamination
//! background.
//!
//! Issue #2948: [`strip_hook_entries_matching_for_events`] generalises the
//! removal logic to per-entry (not per-group) granularity so a hand-mixed
//! group is handled correctly by both this module and [`cleanup`]; issue
//! #2003 reuses the same primitive from `session_launch::settings` for the
//! project-tier writer's broader trusty-owned predicate.

#[cfg(test)]
mod tests;

pub(crate) mod backup;
pub mod cleanup;

use std::path::{Path, PathBuf};

/// Why no stable hook binary could be resolved (#7244).
///
/// Why: the previous resolution answered `Option<PathBuf>` and every caller
/// turned `None` into the bare literal `"trusty-mpm hook"`, so a refusal and a
/// success both produced a hook command and both got WRITTEN. A refusal is a
/// fact the caller needs: it means the file about to be written would carry a
/// command tm cannot vouch for, and the correct response is to write nothing
/// and say why. Naming the reason (rather than a bare `None`) is what makes
/// the operator-facing message actionable — "install tm" and "you are running
/// a test binary" call for different fixes.
/// What: three refusal reasons, each carrying the path that was rejected where
/// there is one.
/// Test: `resolve_stable_hook_exe_with_refuses_a_foreign_binary_stem`,
/// `resolve_stable_hook_exe_with_refuses_an_ephemeral_exe`,
/// `write_project_hooks_writes_nothing_when_the_exe_cannot_be_resolved`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StableHookExeError {
    /// The running binary is a build artifact and nothing installed was found.
    #[error(
        "refusing to bake the ephemeral build path {0} into a persisted hook command, \
         and no installed tm/trusty-mpm binary was found on PATH"
    )]
    Ephemeral(PathBuf),
    /// The running binary is not one this crate ships, and nothing installed
    /// was found. A `cargo test` harness reaches this arm.
    #[error(
        "the running executable {0} is not a tm/trusty-mpm binary, and no installed \
         tm/trusty-mpm binary was found on PATH"
    )]
    ForeignBinary(PathBuf),
    /// Neither `current_exe()` nor a PATH lookup produced an absolute path.
    #[error("no installed tm/trusty-mpm binary could be resolved")]
    Unresolved,
}

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
/// hook"`. If a caller already knows the exe path (e.g. from `current_exe()`
/// cached at startup), they can pass it via `exe_override` to skip the syscall.
///
/// #7244: this used to fall back to the bare literal `"trusty-mpm hook"` when
/// nothing stable resolved, which made a refusal indistinguishable from a
/// success at every call site and let the write proceed regardless. The
/// refusal is now returned; the writers below return it without writing.
/// Test: `test_hook_command_uses_absolute_path`.
pub fn mpm_hook_command(exe_override: Option<&Path>) -> Result<String, StableHookExeError> {
    resolve_stable_hook_exe(exe_override).map(|p| format!("{} hook", p.display()))
}

/// Resolve a STABLE, installed absolute binary path for baking into managed
/// hook commands — never an ephemeral build/worktree path.
///
/// Why (#2229): `std::env::current_exe()` returns a
/// `target/debug/deps/trusty_mpm-<hash>` (or worktree) path when `tm` runs from
/// a debug build. Persisting that path into the SHARED global `settings.json`
/// breaks every managed session's hooks once the artifact is rebuilt away. The
/// hook command must instead point at a stable installed binary
/// (`~/.cargo/bin/tm`) — anything but the transient path.
/// What: prefers `exe_override` (canonicalized), then `current_exe()`
/// (canonicalized), but only when the result is absolute, is not an ephemeral
/// build path per [`trusty_common::bin_resolve::is_ephemeral_build_path`], AND
/// names a binary this crate ships (#7244 — see [`is_mpm_bin_stem_path`]). When
/// the running binary is refused or unresolved, PATH-resolves the installed
/// `tm`/`trusty-mpm` binary via [`trusty_common::bin_resolve::resolve_binary`].
/// Returns [`StableHookExeError`] when no stable absolute path can be found;
/// the caller writes nothing rather than persisting a command it cannot vouch
/// for (#7244).
/// Test: covered by `test_hook_command_uses_absolute_path`,
/// `test_hook_command_rejects_ephemeral_exe_override`,
/// `test_hook_command_rejects_system_temp_exe_override`.
fn resolve_stable_hook_exe(exe_override: Option<&Path>) -> Result<PathBuf, StableHookExeError> {
    let running = exe_override
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()))
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.canonicalize().ok().or(Some(p)))
        });

    resolve_stable_hook_exe_with(running, trusty_common::bin_resolve::resolve_binary)
}

/// Testable core of [`resolve_stable_hook_exe`] with its I/O sources injected.
///
/// Why (#7244): the resolution had no seam, so the two ways it can refuse the
/// running binary — an ephemeral build path, a binary that is not ours — could
/// not be exercised on a machine that has `tm` installed, because the PATH
/// fallback always rescued the call. That is the same machine every developer
/// and CI runner uses, so the refusal was untested where it mattered. Injecting
/// both sources makes each arm deterministic, mirroring the
/// `resolve_statusline_binary_with` pattern this crate already uses.
/// What: accepts `running` (the already-canonicalized `current_exe()` or
/// override) only when it is absolute, is not an ephemeral build path
/// ([`trusty_common::bin_resolve::is_ephemeral_build_path`]), AND names one of
/// [`MPM_BIN_STEMS`]. Otherwise falls back to the first `path_lookup` hit for a
/// [`MPM_BIN_NAMES`] entry that passes those SAME three gates, and reports why
/// the running binary was refused when that fallback also fails.
/// Test: `resolve_stable_hook_exe_with_refuses_a_foreign_binary_stem`,
/// `resolve_stable_hook_exe_with_refuses_an_ephemeral_exe`,
/// `resolve_stable_hook_exe_with_refuses_an_ephemeral_path_lookup_hit`,
/// `resolve_stable_hook_exe_with_falls_back_to_the_installed_binary`.
fn resolve_stable_hook_exe_with(
    running: Option<PathBuf>,
    path_lookup: impl Fn(&str) -> Option<PathBuf>,
) -> Result<PathBuf, StableHookExeError> {
    let mut refusal: Option<StableHookExeError> = None;
    if let Some(p) = running.filter(|p| p.is_absolute()) {
        // #4485: `is_ephemeral_build_path` also rejects anything under a system
        // temp root, so an agent harness's scratchpad binary
        // (`/private/tmp/claude-<uid>/…/scratchpad/…`) can no longer be
        // persisted here as if it were the installed binary. The check stays in
        // the guard — this site must not grow a second, divergent copy of it.
        if trusty_common::bin_resolve::is_ephemeral_build_path(&p) {
            refusal = Some(StableHookExeError::Ephemeral(p));
        } else if !is_mpm_bin_stem_path(&p) {
            // #7244: the path guard alone is one predicate away from a silent
            // catastrophe — when it misses (a build tree it does not recognise),
            // the name check still refuses `test_session_lifecycle-<hash>`. Two
            // independent reasons must both say "this is our installed binary".
            refusal = Some(StableHookExeError::ForeignBinary(p));
        } else {
            return Ok(p);
        }
    }

    // Refused or unresolved running path: fall back to a PATH-resolved
    // installed binary so the hook command survives worktree/debug rebuilds.
    // #7244 round 2: `$PATH` is not a trust boundary. A build-tree `tm` ahead
    // of the installed one — `target/debug/tm`, or one of this repo's
    // `target-<issue>/debug/tm` — is exactly the path the running-exe branch
    // above refuses, so the fallback applies the SAME two gates rather than
    // accepting through the side door what the front door just turned away.
    MPM_BIN_NAMES
        .iter()
        .find_map(|name| path_lookup(name))
        .filter(|p| {
            p.is_absolute()
                && !trusty_common::bin_resolve::is_ephemeral_build_path(p)
                && is_mpm_bin_stem_path(p)
        })
        .ok_or_else(|| refusal.unwrap_or(StableHookExeError::Unresolved))
}

/// Whether `path`'s file name names a binary THIS crate ships.
///
/// Why (#7244): `resolve_stable_hook_exe` judged the running executable purely
/// by WHERE it lived. A `cargo test` harness built into a build directory the
/// path guard did not recognise therefore passed as "the installed tm binary",
/// and `test_session_lifecycle-<hash>` was written into a real project's
/// `settings.json` as the command for pm-guard, Read/Bash diversion,
/// `PostToolUse` and `SessionEnd`. Nothing tm ships is ever named anything but
/// a [`MPM_BIN_STEMS`] entry, so asking WHAT the binary is costs one string
/// comparison and stops the entire class — including build layouts nobody has
/// invented yet.
/// What: strips a Cargo `-<hexhash>` build-artifact suffix (≥ 8 hex digits,
/// the same shape [`is_mpm_hash_suffixed_artifact`] recognises) when present,
/// then requires the remaining stem to be in [`MPM_BIN_STEMS`]. A non-UTF-8
/// file name, or no file name at all, is not ours.
/// Test: `resolve_stable_hook_exe_with_refuses_a_foreign_binary_stem`,
/// `is_mpm_bin_stem_path_accepts_the_shipped_names`.
fn is_mpm_bin_stem_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|f| f.to_str()) else {
        return false;
    };
    MPM_BIN_STEMS.contains(&hash_stripped_stem(name))
}

/// Strip a Cargo build-artifact `-<hexhash>` suffix from a binary file name.
///
/// Why (#7244): the same `<stem>-<hexhash>` shape is read in two places — the
/// dedup predicate [`is_mpm_hash_suffixed_artifact`] and the identity check
/// [`is_mpm_bin_stem_path`]. One implementation means the two cannot disagree
/// about what the stem of `trusty_mpm-1a2b3c4d` is.
/// What: returns the text before the final `-` when what follows is at least 8
/// ASCII hex digits, else `name` unchanged — so `trusty-mpm` (whose suffix
/// `mpm` is not a hash) keeps its full name.
/// Test: `is_mpm_bin_stem_path_accepts_the_shipped_names`.
fn hash_stripped_stem(name: &str) -> &str {
    match name.rsplit_once('-') {
        Some((stem, hash)) if hash.len() >= 8 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
            stem
        }
        _ => name,
    }
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
pub fn mpm_hook_additions_with_exe(
    exe_override: Option<&Path>,
) -> Result<serde_json::Value, StableHookExeError> {
    // #7244: resolved BEFORE any JSON is built, so a refusal reaches the caller
    // with nothing written and no half-formed block to merge.
    let cmd = mpm_hook_command(exe_override)?;
    Ok(serde_json::json!({
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
    }))
}

/// Build the MPM lifecycle hook additions JSON block with the default exe resolution.
///
/// Why: convenience wrapper that calls `mpm_hook_additions_with_exe(None)` so
/// existing call sites that do not need to pin the exe path stay concise.
/// What: delegates to [`mpm_hook_additions_with_exe`] with `None`.
/// Test: covered by `test_mpm_hook_additions_has_six_events`.
pub fn mpm_hook_additions() -> Result<serde_json::Value, StableHookExeError> {
    mpm_hook_additions_with_exe(None)
}

/// Canonical binary names this crate ships as `[[bin]]` targets (`Cargo.toml`):
/// `"trusty-mpm"` (the full name) then `"tm"` (the short everyday alias used
/// by `tm run`/`tm load`/`tm login`).
///
/// #4058 review round 1 MEDIUM finding 2: this is the SAME two-name SET as
/// [`crate::core::own_binary_names::OWN_BINARY_NAMES`], but kept as its own
/// array rather than an alias of it, because the ORDER here is load-bearing
/// and differs from that constant's order. [`resolve_stable_hook_exe`]
/// consumes this list via `.find_map(resolve_binary)` — first PATH hit
/// wins — so `"trusty-mpm"` must stay first to keep preferring the full name
/// over the `tm` alias when both happen to be on `PATH`, exactly as it did
/// before the #4058 consolidation. `OWN_BINARY_NAMES` is ordered `tm` first
/// instead, because ITS order-sensitive consumer
/// (`session_launch::settings::STATUSLINE_BIN_NAMES`) needs the opposite
/// preference. `"trusty-mpm"` is first on purpose: it is the unambiguous full
/// binary name, while `"tm"` is a short alias a user may have shadowed on
/// `PATH`, and the resolved exe is persisted into `settings.json` where a
/// wrong resolution survives across sessions.
///
/// Test: `test_mpm_bin_names_prefers_full_name_over_short_alias` pins this
/// array's exact ORDER (the only mechanical guard — `resolve_stable_hook_exe`
/// calls the real `resolve_binary` and is not injectable, so no behavioural
/// test can observe the preference), and
/// `test_mpm_bin_names_matches_own_binary_names_set` pins the two arrays to
/// the same SET so a future third `[[bin]]` target can't drift between them
/// unnoticed.
const MPM_BIN_NAMES: &[&str] = &["trusty-mpm", "tm"];

/// File-name STEMS that identify a binary this crate SHIPS TODAY, once any
/// Cargo build-artifact `-<hash>` suffix is stripped.
///
/// Why (#7244 round 2): this list is the WRITE-side identity check
/// ([`is_mpm_bin_stem_path`]), the second of two independent reasons a binary
/// may be persisted as a hook command. `session_manager_mvp` used to be here —
/// but `crates/trusty-mpm/tests/session_manager_mvp.rs` compiles to
/// `session_manager_mvp-<hash>` on every `cargo test`, so for that one name the
/// "two independent checks" collapsed to one: only the path guard stood between
/// a running test harness and a real project's `settings.json`. Nothing this
/// crate ships is named that any more, so the write side does not need it.
/// What: the two `[[bin]]` names plus the underscore crate-name spelling Cargo
/// uses for dep artifacts. The cleanup side keeps the retired name — see
/// [`MPM_STALE_BIN_STEMS`].
const MPM_BIN_STEMS: &[&str] = &["trusty-mpm", "trusty_mpm", "tm"];

/// [`MPM_BIN_STEMS`] plus the retired pre-rename binary name.
///
/// Why (#2235): a pre-rename install still carries
/// `…/deps/session_manager_mvp-<hash> hook` in its `settings.json`, and the
/// replace-by-identity strip in [`write_project_hooks`] can only collapse an
/// entry it recognises as the same hook owner. Removing the name from the WRITE
/// check (#7244 round 2) must not orphan those entries, so the CLEANUP check
/// keeps it. Recognising a name for removal is the safe direction; recognising
/// it for persistence is not.
/// What: the shipped stems plus `session_manager_mvp`. Read only by
/// [`is_mpm_hash_suffixed_artifact`].
const MPM_STALE_BIN_STEMS: &[&str] = &["trusty-mpm", "trusty_mpm", "tm", "session_manager_mvp"];

/// Recognise an mpm-owned binary by its EXACT file-name component: a
/// canonical [`MPM_BIN_NAMES`] entry or the defunct `session_manager_mvp` name.
///
/// Why: this is the low-risk branch of [`is_mpm_hook_command`] — an exact
/// bare-name or absolute-path match to a name tm itself ships. It does NOT
/// cover Cargo build-artifact hash suffixes; see [`is_mpm_hash_suffixed_artifact`]
/// for that (path-scoped, per #2940 review round 1 MEDIUM) branch.
/// What: returns `true` when `name` is a canonical [`MPM_BIN_NAMES`] entry or
/// the bare defunct `session_manager_mvp` name.
/// Test: `test_is_mpm_hook_command_recognises_tm_bin_name`.
fn is_mpm_binary_filename(name: &str) -> bool {
    MPM_BIN_NAMES.contains(&name) || name == "session_manager_mvp"
}

/// Recognise a Cargo build-artifact hash-suffixed mpm binary, SCOPED to a
/// path that actually looks like a `deps/` build-artifact directory.
///
/// Why (#2235, tightened #2940 review round 1 MEDIUM): dedup that keyed
/// identity on the exact file name ∈ {`trusty-mpm`,`tm`} could never strip a
/// stale entry whose command carried a build-artifact path
/// (`.../deps/trusty_mpm-<hash> hook`, `.../tm-<hash> hook`) — those file
/// names are not in [`MPM_BIN_NAMES`], so every managed launch appended a
/// fresh entry beside the un-strippable stale ones and `settings.json` grew
/// without bound (#2235's unbounded-growth bug). Recognising the hash-suffixed
/// shape fixed that — but issue #2940 wired this SAME predicate into
/// `tm hooks clean`'s `--force` DESTRUCTIVE deletion path, where a coincidental
/// foreign binary named e.g. `tm-a1b2c3d4` (an unrelated tool that happens to
/// share the `<stem>-<hexhash>` shape) would previously have been silently
/// deleted from a project's settings. Requiring a `deps` path component scopes
/// the match to the one shape `resolve_stable_hook_exe`/`current_exe()` can
/// ever actually produce for a hash-suffixed binary (Cargo always places build
/// artifacts under `target/<profile>/deps/`), closing that false-positive
/// window without touching the exact-name branch above (which carries the
/// same coincidental-collision risk pre-existing #2940 and is out of this
/// PR's scope — see `cleanup.rs`'s module doc for the residual risk note).
/// What: returns `true` when `path`'s file name is `<stem>-<hexhash>` with
/// `stem ∈ MPM_STALE_BIN_STEMS` and an all-hex-digit `<hexhash>` of length ≥ 8, AND
/// `path` has a `deps` component anywhere in it.
/// Test: `test_is_mpm_hook_command_recognises_stale_hash_and_mvp_variants`,
/// `test_is_mpm_hook_command_rejects_hash_suffixed_binary_outside_deps_dir`.
fn is_mpm_hash_suffixed_artifact(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|f| f.to_str()) else {
        return false;
    };
    // Cargo build-artifact form: `<stem>-<hexhash>` (e.g. `trusty_mpm-1a2b3c4d`).
    // #7244: one implementation of "what is the stem of `trusty_mpm-1a2b3c4d`",
    // shared with `is_mpm_bin_stem_path`. An unchanged return means the name
    // carried no hash suffix, which is the exact-name branch's business
    // ([`is_mpm_binary_filename`]), not this one's.
    let stem = hash_stripped_stem(name);
    if stem == name || !MPM_STALE_BIN_STEMS.contains(&stem) {
        return false;
    }
    path.components().any(|c| c.as_os_str() == "deps")
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
/// name) AND EITHER the remaining prefix's file-name component is recognised
/// by [`is_mpm_binary_filename`] (bare names — `"tm hook"`; absolute paths —
/// `"/opt/bin/trusty-mpm hook"`) OR the full prefix is recognised by
/// [`is_mpm_hash_suffixed_artifact`] (hash-suffixed build artifacts under a
/// `deps/` directory — `".../deps/trusty_mpm-<hash> hook"`).
/// Test: `remove_global_hooks_at_strips_only_the_two_global_files`,
/// `test_is_mpm_hook_command_recognises_tm_bin_name`,
/// `test_is_mpm_hook_command_recognises_stale_hash_and_mvp_variants`,
/// `test_is_mpm_hook_command_rejects_hash_suffixed_binary_outside_deps_dir`,
/// `test_write_project_hooks_replaces_stale_exe_path_group`,
/// `test_write_project_hooks_collapses_stale_hash_and_mvp_entries`.
///
/// `pub` (rather than `pub(crate)`) since issue #2940: [`cleanup`] and the
/// `tm doctor` hook-hygiene probe both need this exact predicate so the
/// contamination scan and the removal logic can never classify a command
/// differently.
pub fn is_mpm_hook_command(cmd: &str) -> bool {
    // The command must end with " hook" (with exactly one trailing sub-command word).
    let Some(binary) = cmd.strip_suffix(" hook") else {
        return false;
    };
    let path = Path::new(binary);
    if path
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(is_mpm_binary_filename)
    {
        return true;
    }
    is_mpm_hash_suffixed_artifact(path)
}

/// Recognise a foreign claude-mpm-owned hook command signature (issue #2940).
///
/// Why: a project that still carries claude-mpm's own hook wiring alongside
/// (or instead of) tm's would fire BOTH harnesses' hooks in the same tm
/// session, producing conflicting/undefined behaviour — `tm doctor` needs to
/// warn about this without ever touching the foreign entry (that call is the
/// operator's, not tm's). claude-mpm invokes its hooks via its own
/// `claude-mpm`/`claude_mpm` binary or a script under a `.claude-mpm/`
/// directory; neither ever produces a command [`is_mpm_hook_command`]
/// recognises, so the two predicates are mutually exclusive by construction
/// — checked here defensively so a command is NEVER double-classified.
/// What: returns `true` when `cmd` contains the substring `claude-mpm` or
/// `claude_mpm` (case-insensitive, covering both the installed CLI and a
/// `.claude-mpm/`-rooted script path) AND [`is_mpm_hook_command`] does not
/// already claim it.
/// Test: `test_is_claude_mpm_hook_command_recognises_foreign_signatures`,
/// `test_is_claude_mpm_hook_command_never_overlaps_tm`.
pub fn is_claude_mpm_hook_command(cmd: &str) -> bool {
    if is_mpm_hook_command(cmd) {
        return false;
    }
    let lower = cmd.to_ascii_lowercase();
    lower.contains("claude-mpm") || lower.contains("claude_mpm")
}

/// The two GLOBAL Claude settings files under `home`.
///
/// Why (#5875, #6070): Claude Code reads exactly `~/.claude/settings.json` and
/// `~/.claude/settings.local.json` as the user's GLOBAL settings. Everything
/// else a `$HOME` walk reaches is a PROJECT file that the project itself owns.
/// Naming the two paths directly costs two `open()` calls; discovering them by
/// recursive walk costs the whole home tree, and that walk is what
/// [`remove_global_trusty_mpm_hooks`] used to pay on every `tm launch`.
/// What: joins the two well-known file names under `<home>/.claude` without
/// touching the filesystem — existence is decided by the caller's read.
/// Test: `remove_global_hooks_at_strips_only_the_two_global_files`.
fn global_settings_files(home: &Path) -> Vec<PathBuf> {
    let claude = home.join(".claude");
    vec![
        claude.join("settings.json"),
        claude.join("settings.local.json"),
    ]
}

/// Strip trusty-mpm hook entries from the global Claude settings files.
///
/// Why: after switching from global hooks to project-scoped hooks, any
/// previously installed global hook triad must be cleaned up so it does not
/// fire in unrelated projects. This mirrors the `remove_global_trusty_memory_hooks`
/// pattern from trusty-memory — strip first, then write project-scoped hooks.
/// What: resolves the home directory and delegates to
/// [`remove_global_trusty_mpm_hooks_at`].
/// Test: `remove_global_hooks_at_strips_only_the_two_global_files`,
/// `remove_global_hooks_at_ignores_project_settings_below_home`.
pub fn remove_global_trusty_mpm_hooks() -> anyhow::Result<usize> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    remove_global_trusty_mpm_hooks_at(&home)
}

/// [`remove_global_trusty_mpm_hooks`] against an explicit home directory.
///
/// Why (#5875): the no-argument form resolved the real `$HOME` and recursively
/// walked it to depth 8, so `launch()` step 8 paid an O(entire home tree) scan
/// on every session start. On a real developer machine one `opendir()` inside
/// that walk blocked indefinitely, hanging `tm launch` — and every test that
/// drives it, which is how the eight `guided_fallback_*` tests came to run
/// forever (#6070). Issue #2940 already made this call for the WRITE side:
/// `tm install` stopped walking `$HOME` and writes only the managed config dir,
/// leaving the machine-wide sweep to the explicit, user-invoked `tm hooks
/// clean`. This is the symmetric fix for the REMOVE side. Taking `home` as a
/// parameter is also what makes the behaviour testable at all — every `Test:`
/// line in this module previously named a test that did not exist, because the
/// only entry point read a process-global.
/// What: for each of [`global_settings_files`], removes every hook group whose
/// `command` matches a trusty-mpm hook pattern (see [`is_mpm_hook_command`]) and
/// writes the file back atomically only when something actually changed.
/// Missing, empty, unparseable, and non-object files are skipped silently.
/// Returns the count of files modified. PROJECT settings files under `home` are
/// deliberately NOT touched — `tm hooks clean` owns that sweep.
/// Test: `remove_global_hooks_at_strips_only_the_two_global_files`,
/// `remove_global_hooks_at_ignores_project_settings_below_home`.
pub fn remove_global_trusty_mpm_hooks_at(home: &Path) -> anyhow::Result<usize> {
    use trusty_common::claude_config::write_json_atomic;

    let files = global_settings_files(home);

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
/// Why: shared logic between `remove_global_trusty_mpm_hooks`, [`cleanup`]
/// (issue #2940's `tm hooks clean`), and tests.
/// What: delegates to [`strip_mpm_hook_entries_for_events`] with `events =
/// None`, which strips MPM-owned groups from every event key present.
/// Test: `remove_global_hooks_at_strips_only_the_two_global_files`.
pub fn strip_mpm_hook_entries(val: &mut serde_json::Value) -> bool {
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
/// What: delegates to [`strip_hook_entries_matching_for_events`] with
/// [`is_mpm_hook_command`] as the predicate.
/// Test: `remove_global_hooks_at_strips_only_the_two_global_files`,
/// `test_write_project_hooks_replaces_stale_exe_path_group`,
/// `test_strip_mpm_hook_entries_removes_only_tm_entry_from_mixed_group`.
fn strip_mpm_hook_entries_for_events(
    val: &mut serde_json::Value,
    events: Option<&[String]>,
) -> bool {
    strip_hook_entries_matching_for_events(val, events, is_mpm_hook_command)
}

/// Remove hook group ENTRIES matching `matches_cmd` for the given events (or
/// all events) in-place, at PER-ENTRY (not per-group) granularity.
///
/// Why (issue #2948): the original group-level filter (`inner_hooks.iter().all(...)`
/// then drop-or-keep the WHOLE group) left a hand-mixed group — one entry this
/// predicate owns alongside one genuinely foreign entry in the SAME matcher
/// group — completely untouched, since not every entry matched. That silently
/// failed to strip the owned entry AND (via [`cleanup::event_names_matching`]'s
/// matching `.all()`) made the contamination invisible to `tm doctor` too.
/// Filtering each group's `hooks[*]` array individually strips exactly the
/// matched entries and leaves any foreign entry — and the group itself — in
/// place; only a group whose `hooks` array becomes empty (every entry matched,
/// or it started empty) is dropped. Generalised over an arbitrary predicate
/// (rather than hard-coding [`is_mpm_hook_command`]) so
/// `session_launch::settings::write_project_hooks` (issue #2003) can reuse the
/// exact same entry-level replace-by-identity logic for its broader
/// trusty-owned predicate (lifecycle triad + `trusty-memory` + PM-guard),
/// keeping the two writers' contamination-safety guarantees identical.
/// What: when `events` is `Some(list)`, only those event keys are inspected;
/// when `None`, every event key under `hooks` is inspected. Within scope, each
/// group's `hooks[*]` array is filtered to drop entries whose `command` matches
/// `matches_cmd`; a group whose array is left empty is dropped entirely, and an
/// event key emptied of all groups is removed. Groups with a non-array/absent
/// `hooks` field (unrecognised shape) are always retained untouched. Returns
/// `true` if anything was removed.
/// Test: `remove_global_hooks_at_strips_only_the_two_global_files`,
/// `test_write_project_hooks_replaces_stale_exe_path_group`,
/// `test_strip_mpm_hook_entries_removes_only_tm_entry_from_mixed_group`.
pub(crate) fn strip_hook_entries_matching_for_events(
    val: &mut serde_json::Value,
    events: Option<&[String]>,
    matches_cmd: impl Fn(&str) -> bool,
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
        let mut event_changed = false;

        // Strip matching entries WITHIN each group first, preserving any
        // sibling entry that does not match.
        for group in arr.iter_mut() {
            let Some(inner_hooks) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
                continue; // unknown shape: leave entirely untouched
            };
            let before_inner_len = inner_hooks.len();
            inner_hooks.retain(|entry| {
                !entry
                    .get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(&matches_cmd)
            });
            if inner_hooks.len() != before_inner_len {
                event_changed = true;
            }
        }

        // Now drop groups left with an empty `hooks` array (every entry
        // matched, or it started empty — same as the pre-#2948 behaviour for
        // a homogeneous group). Groups with no `hooks` array at all are kept.
        arr.retain(|group| {
            group
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_none_or(|inner| !inner.is_empty())
        });

        if arr.len() != before_len {
            event_changed = true;
        }
        if event_changed {
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
///
/// #7244: the hook command is resolved FIRST. When no stable installed binary
/// can be found the error is returned and the file is not read, created, or
/// written — a `settings.json` with no tm hooks is recoverable, one wired to a
/// `cargo test` harness silently disables pm-guard enforcement.
///
/// #7244 (round 3): a rewrite that actually changes the file first copies it to
/// `<path>.<YYYYMMDDTHHMMSSZ>.bak` via
/// [`backup::snapshot_then_prune`], keeping the newest
/// [`backup::HOOK_SETTINGS_SNAPSHOTS_KEPT`]. A snapshot failure aborts the
/// rewrite. The two earlier exits stay ahead of it, so a refused write and a
/// no-op rewrite both take no snapshot.
/// Test: `test_write_project_hooks_targets_project_dir`,
/// `test_write_project_hooks_replaces_stale_exe_path_group`,
/// `write_project_hooks_writes_nothing_when_the_exe_cannot_be_resolved`,
/// `write_project_hooks_snapshots_the_file_it_replaces`,
/// `write_project_hooks_takes_no_snapshot_when_the_exe_is_refused`,
/// `write_project_hooks_takes_no_snapshot_when_nothing_changes`,
/// `write_project_hooks_aborts_the_rewrite_when_the_snapshot_fails`.
pub fn write_project_hooks(
    settings_path: &Path,
    exe_override: Option<&Path>,
) -> anyhow::Result<bool> {
    write_project_hooks_with(settings_path, mpm_hook_additions_with_exe(exe_override))
}

/// [`write_project_hooks`] with the resolved additions supplied by the caller.
///
/// Why (#7244): the Fail-Open Check this fix owes — "a refusal writes nothing"
/// — cannot be exercised through [`write_project_hooks`] on a machine that has
/// `tm` installed, because the PATH fallback always resolves. Taking the
/// already-computed `Result` lets a test hand in the refusal directly and
/// assert the file is untouched, while production still routes through the one
/// resolution above.
/// What: returns `additions`'s error unchanged before touching the filesystem;
/// otherwise performs the read / strip / merge / snapshot / atomic-write
/// exactly as [`write_project_hooks`] documents.
/// Test: `write_project_hooks_writes_nothing_when_the_exe_cannot_be_resolved`,
/// `write_project_hooks_takes_no_snapshot_when_the_exe_is_refused`.
fn write_project_hooks_with(
    settings_path: &Path,
    additions: Result<serde_json::Value, StableHookExeError>,
) -> anyhow::Result<bool> {
    use trusty_common::claude_config::{merge_hook_entries, write_json_atomic};

    // Before the read: a refusal must leave a missing file missing.
    let additions = additions?;

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

    // #7244: snapshot BEFORE the rename that replaces the file, and fail
    // closed. The equality check above already returned for a no-op rewrite,
    // and the refusal at the top returned before any of this, so every
    // snapshot taken here corresponds to a real change of content.
    backup::snapshot_then_prune(settings_path, backup::HOOK_SETTINGS_SNAPSHOTS_KEPT).map_err(
        |e| {
            anyhow::anyhow!(
                "snapshot {} before rewriting it: {e}",
                settings_path.display()
            )
        },
    )?;

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
    ensure_managed_hooks_with_exe(claude_config_dir, None)
}

/// [`ensure_managed_hooks`] with the hook binary pinned by the caller.
///
/// Why (#7244): [`ensure_managed_hooks`] resolves the running binary, which
/// under `cargo test` is a build artifact the resolver now refuses — so its
/// tests would assert against a refusal on any host without `tm` installed
/// (every CI runner). Pinning the path keeps them testing the MERGE, which is
/// what they are about.
/// What: joins `settings.json` under `claude_config_dir` and forwards both
/// arguments to [`write_project_hooks`].
/// Test: `test_ensure_managed_hooks_writes_triad`,
/// `test_ensure_managed_hooks_is_idempotent`.
pub fn ensure_managed_hooks_with_exe(
    claude_config_dir: &Path,
    exe_override: Option<&Path>,
) -> anyhow::Result<()> {
    let settings_path = claude_config_dir.join("settings.json");
    write_project_hooks(&settings_path, exe_override).map(|_| ())
}

/// Resolve the resolved-exe path from `current_exe()` for use at install time.
///
/// Why: callers that run `tm install` should pin the absolute path once
/// (at install entry, before any `cd`) and pass it through. This helper
/// centralises resolution and, crucially, refuses to pin an ephemeral
/// build/worktree path (#2229) that would 404 after a rebuild — falling back to
/// the PATH-resolved installed binary instead.
/// What: delegates to [`resolve_stable_hook_exe`] with no override, returning a
/// stable installed absolute path, or `None` when none can be found. The
/// callers pass the result straight back in as an `exe_override`, so they need
/// the path or nothing — [`write_project_hooks`] raises the same refusal with
/// its reason a moment later (#7244).
/// Test: covered indirectly by `test_hook_command_uses_absolute_path`,
/// `test_hook_command_rejects_ephemeral_exe_override`.
pub fn resolve_current_exe() -> Option<PathBuf> {
    resolve_stable_hook_exe(None).ok()
}
