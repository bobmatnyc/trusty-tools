//! Compile-time embedded bundled agent set + version-stamped deploy/refresh.
//!
//! Why (#3405/#3406 follow-up): `tagent` launched from OUTSIDE this repo
//! checkout (e.g. `cd ~ && tagent --plain`, the owner's reported clean-shell
//! failure) has no project-tier `.trusty-agents/agents/` at all, and — until
//! this module — nothing ever populated the resolver's `$HOME` fallback tier
//! ([`super::loader`]'s `agents_dir_candidates`, and
//! [`super::registry::agent_search_paths`]) either. The bundled roster
//! (`assistant`, `ctrl`, `pm`, the specialist set, …) only ever existed as
//! plain files under THIS crate's own `.trusty-agents/agents/` — reachable
//! only when the process happens to run inside (or was built to auto-detect)
//! this checkout. A `cargo install`ed binary run from any other directory had
//! no bundled roster to fall back to, so `/switch assistant` (and the plain
//! CLI's default-persona switch) failed with a bare "file not found" and
//! silently degraded to `ctrl` (local Ollama) with no explanation.
//!
//! Why v2 (#3556): the original fix deployed the bundled roster exactly
//! ONCE per machine — [`deploy_bundled_agents`]'s `if dest.exists() {
//! continue; }` never revisits a file that is already there. A binary
//! upgrade that changes a bundled template (e.g. PR-A's `delegate_to_agent`
//! grant added to the base `assistant` agent) never reaches a machine that
//! already deployed the OLD copy — the compiled/source template is correct,
//! only the on-disk deployed copy goes stale, and the user silently loses a
//! capability until a human manually deletes the stale file. This module now
//! stamps every deploy with a content hash of the embedded bundle
//! ([`stamp`]) and, on a hash mismatch, refreshes exactly the bundled files
//! whose content actually changed — never a user's own non-bundled agent,
//! and never silently: a bundled file whose ON-DISK content differs from
//! what is about to be written is archived to a sibling `<file>.stale.bak`
//! first, so a user who hand-edited a bundled file can recover it.
//!
//! trusty-mpm solves the identical problem for ITS bundled agents/skills via
//! `include_str!` + an explicit `install` deploy step
//! (`crates/trusty-mpm/src/core/bundle.rs`); this module is the same pattern
//! adapted to trusty-agents' directory-package + flat-`.toml` agent formats
//! (too varied in shape for one `include_str!` constant per file, so this
//! uses `rust-embed` — already a dependency for the web UI bundle, see
//! `api/server/ui.rs` — to embed the whole `.trusty-agents/agents/` tree at
//! once).
//!
//! What: [`deploy_bundled_agents`] is the hermetic, "write missing files
//! only" core — writes every embedded file under `target_dir`, but ONLY when
//! that path doesn't already exist there, so a fresh deploy never clobbers a
//! prior one. [`ensure_bundled_agents_deployed`] is the production entry
//! point: resolves `target_dir` to `$HOME/.trusty-agents/agents` and, when
//! the on-disk `.bundled-stamp` file is missing or stale relative to the
//! CURRENT binary's embedded content hash, refreshes every bundled file
//! whose bytes changed (backing up any that differ from what's on disk)
//! before rewriting the stamp; otherwise it's the same fast no-op as before.
//! [`repair_bundled_agents`] is the explicit manual escape hatch (`tagent
//! agents repair`) — force-refreshes regardless of the stamp.
//!
//! Concurrency (#3556 code-critic follow-up, HIGH + MEDIUM): `delegate_to_agent`
//! routinely spawns a fresh `tagent` subprocess per delegation, so multiple
//! processes can enter the stale-stamp refresh path concurrently right after
//! a binary upgrade. Every write in this module now goes through
//! `crate::state_writer::atomic_write` (tmp-file + rename — never a
//! truncate-in-place, so a crash mid-write can't leave a torn file on disk).
//! The ENTIRE read-decide-write sequence for a stamp-aware pass —
//! `stamp::read`, the is-stale decision, the refresh loop, and the
//! subsequent `stamp::write` — is one critical section under a SINGLE
//! pass-level advisory lock ([`lock`]), acquired ONCE by the caller
//! ([`ensure_bundled_agents_deployed_in`], [`force_reprovision_bundled_agents`])
//! and threaded down into [`reprovision_bundled_agents_locked`] (the MEDIUM
//! follow-up: an earlier version acquired the lock only INSIDE the refresh
//! loop, leaving the stamp read/decide/write outside it — two concurrent
//! passes could both observe the same stale stamp, both refresh, then race
//! writing the stamp). Two concurrent passes over the same `target_dir` are
//! now fully serialized end to end, not just per-file. A `<file>.stale.bak`
//! is only ever created when one doesn't already exist, so a later pass —
//! even one recovering from a prior crash — can never clobber a genuine
//! backup with torn or already-refreshed content.
//!
//! Test: see `tests.rs` in this module, plus `stamp`'s and `lock`'s own
//! tests.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

mod lock;
mod stamp;

/// Embeds this crate's own `.trusty-agents/agents/` tree — the shared
/// persona set (`assistant`, `ctrl`, `pm`, `cto-assistant`, `izzie`) and the
/// specialist roster (`python-engineer`, `qa-agent`, …) — into the compiled
/// binary at build time.
#[derive(rust_embed::RustEmbed)]
#[folder = ".trusty-agents/agents/"]
struct BundledAgents;

/// Outcome of one (re)provision pass over the bundled agent set (#3556).
///
/// Why: callers (the interactive startup banner, `tagent agents
/// deploy`/`repair`) give the user different, accurate messaging for a
/// freshly-written file vs. a refreshed (content-changed) one vs. a stale
/// user copy that got archived before being overwritten — three independent
/// counters make that distinction possible without re-walking the file set.
/// What: `written` counts brand-new files, `refreshed` counts existing
/// bundled files whose content was updated to match the current embedded
/// template, `backed_up` counts how many of those refreshes archived a
/// differing prior copy to `<file>.stale.bak` first.
/// Test: `deploy_writes_missing_files_only`, `stale_stamp_triggers_refresh`,
/// `refresh_backs_up_differing_content` (tests.rs).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReprovisionReport {
    pub written: usize,
    pub refreshed: usize,
    pub backed_up: usize,
}

impl ReprovisionReport {
    /// Total files actually written to disk this pass (new + refreshed).
    ///
    /// Why: callers that only care about "did anything change" (the startup
    /// banner's `if … > 0` gate) shouldn't have to sum two fields themselves.
    /// What: `written + refreshed`.
    /// Test: exercised via every `ReprovisionReport`-returning test in
    /// `tests.rs`.
    pub fn total_touched(&self) -> usize {
        self.written + self.refreshed
    }
}

/// Idempotently materialise every embedded bundled-agent file under
/// `target_dir`, without ever overwriting a file that is already there.
///
/// Why: the hermetic core — takes `target_dir` as a parameter (rather than
/// resolving `$HOME` itself) so tests can point it at a tempdir instead of
/// touching the real filesystem outside the sandbox. Preserved as its own
/// entry point (rather than folded into the stamp-aware path) because it is
/// the exact "never touch an existing file" contract a few callers still
/// want explicitly (and every existing test pins this behavior).
/// What: for each path `rust-embed` walked out of `.trusty-agents/agents/`,
/// skips it when `target_dir.join(path)` already exists, otherwise creates
/// parent directories and writes the embedded bytes verbatim. Returns the
/// count of newly-written files.
/// Test: `deploy_writes_missing_files_only`, `deploy_is_idempotent_on_rerun`,
/// `deploy_never_overwrites_existing_file`,
/// `deploy_writes_directory_package_agents` (tests.rs).
pub fn deploy_bundled_agents(target_dir: &Path) -> Result<usize> {
    Ok(reprovision_bundled_agents(target_dir, false)?.written)
}

/// Shared core of every deploy/refresh entry point in this module (#3556),
/// acquiring its OWN pass-level lock — for callers with no additional
/// state (the stamp) to guard alongside the loop.
///
/// Why: `deploy_bundled_agents` has no stamp read/decide/write step around
/// it, so acquiring and immediately delegating is the simplest correct form.
/// Callers that DO have a stamp step to keep in the same critical section
/// (`ensure_bundled_agents_deployed_in`, `force_reprovision_bundled_agents`)
/// acquire the lock themselves and call
/// [`reprovision_bundled_agents_locked`] directly instead of this wrapper —
/// see that function's docs for why (#3556 code-critic follow-up, MEDIUM).
/// What: acquires the pass-level lock, then delegates to
/// [`reprovision_bundled_agents_locked`].
/// Test: `deploy_writes_missing_files_only`, `deploy_never_overwrites_existing_file`,
/// `deploy_is_idempotent_on_rerun`, `deploy_writes_directory_package_agents` (tests.rs).
fn reprovision_bundled_agents(target_dir: &Path, refresh_stale: bool) -> Result<ReprovisionReport> {
    let guard = lock::acquire(target_dir)?;
    reprovision_bundled_agents_locked(&guard, target_dir, refresh_stale)
}

/// The actual write-side loop over `BundledAgents::iter()`, REQUIRING an
/// already-held pass-level lock (#3556 code-critic follow-up, MEDIUM).
///
/// Why: takes `_guard: &lock::ProvisionLock` as a parameter — not because the
/// loop body touches it, but so the type system enforces that no caller can
/// run this loop without holding the lock. This is what lets
/// `ensure_bundled_agents_deployed_in` and `force_reprovision_bundled_agents`
/// fold the stamp `read`/is-stale-decide/`write` into the SAME critical
/// section as the refresh loop: they call `lock::acquire` once themselves,
/// pass the guard down here, and only release it (via `Drop`) after the
/// stamp has been rewritten. An earlier version had this loop acquire its
/// own lock internally and release it before returning — leaving the stamp
/// read/decide/write OUTSIDE any lock, so two concurrent passes could both
/// observe the same stale stamp, both refresh, then race writing the stamp
/// (harmless for two processes running the SAME binary, but two DIFFERENT
/// binary versions racing — e.g. a `cargo install` finishing in one
/// terminal while an older `tagent` is mid-startup in another — could leave
/// a stamp that doesn't match the content actually on disk).
/// What: for each embedded file, writes it when missing (`written`); when
/// present and `refresh_stale` is `false`, skips it untouched; when present
/// and `refresh_stale` is `true`, compares on-disk bytes to the embedded
/// bytes — identical content is skipped, differing content is archived to
/// `<dest>.stale.bak` (`backed_up`) ONLY WHEN that backup doesn't already
/// exist (so a later pass recovering from an earlier crash can never
/// clobber a genuine prior backup with torn or already-refreshed content),
/// THEN overwritten (`refreshed`) via `crate::state_writer::atomic_write`
/// (tmp-file + rename, so a crash mid-write leaves the original file/backup
/// intact rather than torn). Only ever touches paths that are part of the
/// embedded bundle — a user's own non-bundled file at `target_dir` is never
/// read, backed up, or written.
/// Test: `stale_stamp_triggers_refresh`, `refresh_backs_up_differing_content`,
/// `non_bundled_user_file_untouched_by_refresh`,
/// `existing_stale_backup_is_never_clobbered` (tests.rs);
/// `pass_lock_serializes_concurrent_reprovision_calls` (`lock`'s own tests)
/// pins the underlying mutual-exclusion primitive.
fn reprovision_bundled_agents_locked(
    _guard: &lock::ProvisionLock,
    target_dir: &Path,
    refresh_stale: bool,
) -> Result<ReprovisionReport> {
    let mut report = ReprovisionReport::default();
    for rel in BundledAgents::iter() {
        let dest = target_dir.join(rel.as_ref());
        let file = BundledAgents::get(&rel)
            .with_context(|| format!("embedded bundled-agent asset vanished: {rel}"))?;

        if dest.exists() {
            if !refresh_stale {
                continue;
            }
            let current = std::fs::read(&dest)
                .with_context(|| format!("failed to read existing {}", dest.display()))?;
            if current == file.data.as_ref() {
                continue;
            }
            let backup = stale_backup_path(&dest);
            if !backup.exists() {
                crate::state_writer::atomic_write(&backup, &current).with_context(|| {
                    format!(
                        "failed to back up stale bundled file to {}",
                        backup.display()
                    )
                })?;
                report.backed_up += 1;
            }
            crate::state_writer::atomic_write(&dest, file.data.as_ref())
                .with_context(|| format!("failed to refresh {}", dest.display()))?;
            report.refreshed += 1;
            continue;
        }

        crate::state_writer::atomic_write(&dest, file.data.as_ref())
            .with_context(|| format!("failed to write {}", dest.display()))?;
        report.written += 1;
    }
    Ok(report)
}

/// Sibling backup path for a bundled file about to be overwritten (#3556).
///
/// Why: constraints call for a fixed, non-wall-clock suffix (the environment
/// this ships in forbids `Date::now()`-style timestamps in library code, and
/// a fixed name is also simpler to document/find than a rotating one) — a
/// single generation of backup is enough to recover from an unintentional
/// refresh; it intentionally overwrites any PRIOR `.stale.bak` rather than
/// accumulating an unbounded history.
/// What: appends the literal `.stale.bak` suffix to the full file name
/// (e.g. `assistant/agent.toml` -> `assistant/agent.toml.stale.bak`).
/// Test: `refresh_backs_up_differing_content` (tests.rs).
fn stale_backup_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".stale.bak");
    PathBuf::from(name)
}

/// Compute the current binary's content stamp over its embedded bundle.
///
/// Why: pulled out of the two call sites (`ensure_bundled_agents_deployed_in`,
/// `force_reprovision_bundled_agents`) that both need "what does THIS
/// binary's bundle hash to right now" so they can never compute it
/// differently.
/// What: reads every embedded `(path, bytes)` pair and delegates to
/// [`stamp::compute`] for the order-independent hash.
/// Test: `stamp_changes_when_content_changes`,
/// `stamp_stable_regardless_of_input_order` (`stamp`'s own tests).
fn current_bundle_stamp() -> Result<String> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for rel in BundledAgents::iter() {
        let file = BundledAgents::get(&rel)
            .with_context(|| format!("embedded bundled-agent asset vanished: {rel}"))?;
        entries.push((rel.to_string(), file.data.into_owned()));
    }
    Ok(stamp::compute(entries))
}

/// Hermetic core of [`ensure_bundled_agents_deployed`] — takes `target_dir`
/// explicitly so tests can point it at a tempdir (#3556).
///
/// Why: the production entry point resolves `$HOME` itself, which routine
/// `cargo test` runs must never touch; this is the same
/// hermetic-core-plus-thin-wrapper split every other function in this module
/// already uses. Acquires the pass-level lock ONCE, before `stamp::read`
/// (#3556 code-critic follow-up, MEDIUM), and holds it across the ENTIRE
/// read-decide-refresh-write sequence so two concurrent callers over the
/// same `target_dir` can never both observe the same stale stamp and race
/// writing it back — see [`reprovision_bundled_agents_locked`]'s docs for
/// the full rationale.
/// What: computes the current binary's bundle stamp, compares it to the
/// stamp on disk (`target_dir/.bundled-stamp`, via [`stamp::read`]) — a
/// missing or differing stamp means "stale", triggering
/// `reprovision_bundled_agents_locked(&guard, target_dir, true)` (refresh
/// path) followed by writing the new stamp; a matching stamp takes the fast
/// never-overwrite path (`refresh_stale = false`) and leaves the stamp file
/// untouched. A lock-acquire failure propagates as `Err` (unchanged
/// contract: the caller, `ensure_bundled_agents_deployed`, is the one that
/// fails soft on `$HOME` resolution — this function itself is not best
/// effort).
/// Test: `stale_stamp_triggers_refresh`, `matching_stamp_is_a_fast_noop`,
/// `missing_stamp_establishes_baseline_without_rewriting_matching_content`,
/// `concurrent_ensure_calls_over_stale_target_converge_to_one_consistent_refresh`,
/// `ensure_deployed_blocks_on_externally_held_pass_lock` (tests.rs).
pub fn ensure_bundled_agents_deployed_in(target_dir: &Path) -> Result<ReprovisionReport> {
    let guard = lock::acquire(target_dir)?;

    let current_stamp = current_bundle_stamp()?;
    let on_disk_stamp = stamp::read(target_dir);
    let is_stale = on_disk_stamp.as_deref() != Some(current_stamp.as_str());

    let report = reprovision_bundled_agents_locked(&guard, target_dir, is_stale)?;
    if is_stale {
        stamp::write(target_dir, &current_stamp)?;
    }
    Ok(report)
}

/// Production entry point: deploy/refresh the bundled agent set at
/// `$HOME/.trusty-agents/agents/` — the resolver's `$HOME` fallback tier —
/// so `tagent` launched from ANY directory resolves `assistant` (and the
/// rest of the bundled roster) with the CURRENT binary's template content,
/// not whatever was deployed there by a previous version (#3556).
///
/// Why: called once per process from `runtime::startup::run_startup_init`,
/// mirroring where the credential-onboarding banner already runs — this is
/// the single choke point every interactive/one-shot invocation passes
/// through, but NOT `TrustyAgentsRepl::new` (many unit tests construct a REPL
/// directly without sandboxing `$HOME`; hooking there would make routine
/// `cargo test` runs write into a developer's real home directory).
/// What: no-op (`Ok(default)`) when `$HOME` cannot be resolved at all —
/// matches every other best-effort `dirs::home_dir()` operation in this
/// crate (the REPL log dir, the user-profile save). Never fatal: a deploy
/// failure (e.g. a read-only `$HOME`) is reported to the caller as an `Err`
/// so it can log a `warn` and continue — a stale or missing bundled roster
/// degrades to the pre-fix behavior, it does not crash the harness.
/// Test: `ensure_bundled_agents_deployed_in` (this function's hermetic
/// core) carries the unit coverage; this thin `$HOME`-resolving wrapper is
/// exercised end-to-end by real `tagent` invocations (mirrors every other
/// best-effort `dirs::home_dir()` operation in this crate).
pub fn ensure_bundled_agents_deployed() -> Result<ReprovisionReport> {
    let Some(home) = dirs::home_dir() else {
        return Ok(ReprovisionReport::default());
    };
    let target = home.join(".trusty-agents").join("agents");
    ensure_bundled_agents_deployed_in(&target)
}

/// Force-refresh every bundled agent file under `target_dir` regardless of
/// the stamp (#3556) — the hermetic core of [`repair_bundled_agents`].
///
/// Why: an explicit manual escape hatch is needed for the cases the
/// automatic stamp check can't cover — e.g. a user suspects a deploy was
/// interrupted, or wants to confirm the roster matches the installed binary
/// without waiting for (or trusting) the stamp comparison. Acquires the
/// pass-level lock ONCE, before the refresh loop, and holds it through the
/// trailing `stamp::write` (#3556 code-critic follow-up, MEDIUM) — the same
/// one-critical-section shape as `ensure_bundled_agents_deployed_in`, so a
/// concurrent `ensure_*`/`repair` pair over the same `target_dir` can never
/// interleave.
/// What: unconditionally runs the refresh path
/// (`reprovision_bundled_agents_locked(&guard, target_dir, true)`), then
/// writes the current bundle stamp so a subsequent automatic startup check
/// treats the roster as up to date. Errors (including a lock-acquire
/// failure) propagate loudly to the caller — this function has no fail-soft
/// path (unchanged contract: `repair_bundled_agents` is an explicit,
/// user-invoked command, not a best-effort background step).
/// Test: `force_reprovision_overwrites_even_when_stamp_matches`,
/// `existing_stale_backup_is_never_clobbered` (tests.rs).
pub fn force_reprovision_bundled_agents(target_dir: &Path) -> Result<ReprovisionReport> {
    let guard = lock::acquire(target_dir)?;
    let report = reprovision_bundled_agents_locked(&guard, target_dir, true)?;
    let current_stamp = current_bundle_stamp()?;
    stamp::write(target_dir, &current_stamp)?;
    Ok(report)
}

/// Production entry point for the explicit manual escape hatch — `tagent
/// agents repair` (#3556).
///
/// Why: unlike [`ensure_bundled_agents_deployed`] (a best-effort background
/// startup step that must never be fatal), this is a command the user
/// explicitly invoked to fix a problem — an unresolvable `$HOME` here is a
/// real failure the user needs to see, not something to silently no-op.
/// What: resolves `$HOME/.trusty-agents/agents` and delegates to
/// [`force_reprovision_bundled_agents`]; errors loudly (rather than
/// returning `Ok(default)`) when `$HOME` cannot be resolved.
/// Test: covered via `force_reprovision_bundled_agents`'s tempdir-based
/// tests; the `$HOME` resolution itself mirrors
/// `ensure_bundled_agents_deployed`'s (already-established) pattern.
pub fn repair_bundled_agents() -> Result<ReprovisionReport> {
    let home = dirs::home_dir()
        .context("cannot resolve $HOME to repair bundled agents in ~/.trusty-agents/agents")?;
    let target = home.join(".trusty-agents").join("agents");
    force_reprovision_bundled_agents(&target)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
