//! The bundled agent set, read from instructional content, plus its
//! version-stamped deploy/refresh into `~/.trusty-agents/` (#9014).
//!
//! Why (#3405/#3406 follow-up): `tagent` launched from OUTSIDE this repo
//! checkout (e.g. `cd ~ && tagent --plain`, the owner's reported clean-shell
//! failure) has no project-tier `.trusty-agents/agents/` at all, and — until
//! this module — nothing ever populated the resolver's `$HOME` fallback tier
//! ([`super::loader`]'s `agents_dir_candidates`, and
//! [`super::registry::agent_search_paths`]) either. A `cargo install`ed binary
//! run from any other directory had no bundled roster to fall back to, so
//! `/switch assistant` (and the plain CLI's default-persona switch) failed with
//! a bare "file not found" and silently degraded to `ctrl` (local Ollama) with
//! no explanation.
//!
//! Why v2 (#3556): the original fix deployed the bundled roster exactly
//! ONCE per machine — [`deploy_bundled_agents`]'s `if dest.exists() {
//! continue; }` never revisits a file that is already there. A template
//! change never reached a machine that already deployed the OLD copy. This
//! module stamps every deploy with a content hash of the bundled tree
//! ([`stamp`]) and, on a hash mismatch, refreshes exactly the bundled files
//! whose content actually changed — never a user's own non-bundled agent,
//! and never silently: a bundled file whose ON-DISK content differs from
//! what is about to be written is archived to a sibling
//! `<file>.stale.<digest>.bak` first, so a user who hand-edited a bundled
//! file can recover it — see [`stale_backup_path`] for why the name carries
//! the content digest and what bounds the resulting set (#4461).
//!
//! Why v3 (#9014, ADR-0064): the two trees are no longer compiled in. They
//! live under `content/agents/trusty-agents/{agents,workflows}/` and
//! [`content::BundledTree`] reads them at runtime through the shared content
//! resolver. With no content the production entry points fail with the
//! resolver's typed error naming the remedy; nothing deploys an empty set.
//!
//! What: [`deploy_bundled_agents`] is the hermetic, "write missing files
//! only" core — writes every file of a [`content::BundledTree`] under
//! `target_dir`, but ONLY when that path doesn't already exist there.
//! [`ensure_bundled_agents_deployed`] is the production entry point: reads the
//! agent tree, resolves `target_dir` to `$HOME/.trusty-agents/agents` and,
//! when the on-disk `.bundled-stamp` file is missing or stale relative to the
//! tree's content hash, refreshes every bundled file whose bytes changed
//! (backing up any that differ from what's on disk) before rewriting the
//! stamp; otherwise it's the same fast no-op as before.
//! [`repair_bundled_agents`] is the explicit manual escape hatch (`tagent
//! agents repair`) — force-refreshes regardless of the stamp.
//!
//! Concurrency (#3556 code-critic follow-up, HIGH + MEDIUM): `delegate_to_agent`
//! routinely spawns a fresh `tagent` subprocess per delegation, so multiple
//! processes can enter the stale-stamp refresh path concurrently. Every write
//! in this module goes through `crate::state_writer::atomic_write` (tmp-file +
//! rename). The ENTIRE read-decide-write sequence for a stamp-aware pass —
//! `stamp::read`, the is-stale decision, the refresh loop, and the subsequent
//! `stamp::write` — is one critical section under a SINGLE pass-level
//! advisory lock ([`lock`]), acquired ONCE by the caller
//! ([`ensure_bundled_agents_deployed_in`], [`force_reprovision_bundled_agents`])
//! and threaded down into [`reprovision_tree_locked`]. A backup path is
//! derived from the bytes it holds, so a later pass can never clobber a
//! genuine backup (#4461).
//!
//! Test: see `tests.rs` and `content_tests.rs` in this module, plus `stamp`'s
//! and `lock`'s own tests.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

pub mod content;
mod lock;
mod provenance;
mod stamp;

pub use content::{BundledTree, BundledTreeKind};

/// Is `rel` a file the bundle is allowed to ship and deploy? (#5226)
///
/// Why: a stray local file left in the bundled source tree (a `.env`, the
/// `<file>.lock` sidecar `state_writer::atomic_write` leaves) would otherwise
/// be written into every user's home. This is an ALLOWLIST, not a denylist,
/// so a stray file of a kind nobody anticipated is excluded by default: a
/// bundled file that fails to ship is loud (the agent does not resolve), a
/// secret that ships is silent.
/// What: accepts a path whose extension is one of the three the bundle
/// actually uses — `toml`, `md`, `json` — and whose every path component is
/// dotless. [`content::BundledTree::from_content`] applies it when reading the
/// tree, and [`bundled_files`] repeats it at deploy time.
/// Test: `bundled_asset_predicate_fails_closed`,
/// `stray_files_in_the_asset_tree_are_never_deployed`,
/// `every_source_file_is_a_bundled_asset_or_known_junk` (tests.rs).
fn is_bundled_asset(rel: &str) -> bool {
    let mut components = rel.split('/').peekable();
    let mut last = "";
    for component in components.by_ref() {
        if component.is_empty() || component.starts_with('.') {
            return false;
        }
        last = component;
    }
    matches!(
        last.rsplit_once('.').map(|(_, ext)| ext),
        Some("toml") | Some("md") | Some("json")
    )
}

/// Every `(path, bytes)` pair `tree` actually deploys (#5226).
///
/// Why: the stamp and the write loop both have to agree on what "the bundle"
/// is — a stray file that changed the stamp but was never written would make
/// every startup see a stale stamp and refresh forever. One filtered view
/// keeps them from drifting.
/// What: drops anything [`is_bundled_asset`] rejects with a `warn` naming it,
/// and returns the rest.
/// Test: `stray_files_in_the_asset_tree_are_never_deployed`,
/// `stray_files_do_not_move_the_bundle_stamp` (tests.rs).
fn bundled_files(tree: &BundledTree) -> Vec<(String, Vec<u8>)> {
    tree.files()
        .iter()
        .filter(|(rel, _)| {
            let keep = is_bundled_asset(rel);
            if !keep {
                tracing::warn!(
                    asset = %rel,
                    "ignoring a file in the bundled tree that is not a bundled asset"
                );
            }
            keep
        })
        .cloned()
        .collect()
}

/// Outcome of one (re)provision pass over the bundled agent set (#3556).
///
/// Why: callers (the interactive startup banner, `tagent agents
/// deploy`/`repair`) give the user different, accurate messaging for a
/// freshly-written file vs. a refreshed (content-changed) one vs. a stale
/// user copy that got archived before being overwritten — three independent
/// counters make that distinction possible without re-walking the file set.
/// What: `written` counts brand-new files, `refreshed` counts existing
/// bundled files whose content was updated to match the current bundled
/// template, `backed_up` counts how many of those refreshes archived a
/// differing prior copy to `<file>.stale.<digest>.bak` first, and `preserved`
/// counts files the automatic path declined to overwrite because they no longer
/// match what this module last wrote there (#3844) — a hand edit the operator
/// gets told about instead of losing.
/// Test: `deploy_writes_missing_files_only`, `stale_stamp_triggers_refresh`,
/// `refresh_backs_up_differing_content`,
/// `a_hand_edited_bundled_file_survives_a_stale_stamp_refresh` (tests.rs).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReprovisionReport {
    pub written: usize,
    pub refreshed: usize,
    pub backed_up: usize,
    /// #3844: bundled files left alone because they carry a local edit.
    pub preserved: usize,
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

/// Idempotently materialise every file of `tree` under `target_dir`, without
/// ever overwriting a file that is already there.
///
/// Why: the hermetic core — takes the tree and `target_dir` as parameters
/// (rather than resolving content and `$HOME` itself) so tests can point it at
/// a tempdir instead of touching the real filesystem outside the sandbox.
/// Preserved as its own entry point (rather than folded into the stamp-aware
/// path) because it is the exact "never touch an existing file" contract a
/// few callers still want explicitly (and every existing test pins this
/// behavior).
/// What: for each bundled path, skips it when `target_dir.join(path)` already
/// exists, otherwise creates parent directories and writes the bytes
/// verbatim. Returns the count of newly-written files.
/// Test: `deploy_writes_missing_files_only`, `deploy_is_idempotent_on_rerun`,
/// `deploy_never_overwrites_existing_file`,
/// `deploy_writes_directory_package_agents` (tests.rs).
// #9014: takes the tree read from content; nothing is compiled in.
pub fn deploy_bundled_agents(tree: &BundledTree, target_dir: &Path) -> Result<usize> {
    let guard = lock::acquire(target_dir)?;
    Ok(reprovision_tree_locked(&guard, tree, target_dir, RefreshPolicy::WriteMissingOnly)?.written)
}

/// What a reprovision pass may do to a bundled file that is already on disk
/// (#3844).
///
/// Why: the pass used to take a `refresh_stale: bool`, which could express only
/// "leave every existing file alone" or "overwrite every divergent one". The
/// automatic upgrade path needs the third case in between — refresh a file this
/// module itself last wrote, keep one the operator edited — and `tagent agents
/// repair` still needs the unconditional overwrite, so the two can no longer
/// share one flag.
/// What: three variants, applied only to a path that already exists (a missing
/// file is always written).
/// Test: `deploy_never_overwrites_existing_file` (`WriteMissingOnly`),
/// `a_hand_edited_bundled_file_survives_a_stale_stamp_refresh` and
/// `a_stale_deploy_this_tool_wrote_is_still_refreshed` (`KeepHandEdits`),
/// `force_reprovision_overwrites_even_when_stamp_matches` (`Overwrite`) (tests.rs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshPolicy {
    /// Never revisit a file that is already there.
    WriteMissingOnly,
    /// Refresh a divergent file only when the provenance manifest vouches that
    /// its current content is what this module last wrote at that path.
    KeepHandEdits,
    /// Overwrite every divergent file, archiving its content first — the
    /// explicit `tagent agents repair` escape hatch.
    Overwrite,
}

/// The actual write-side loop over one [`BundledTree`], REQUIRING an
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
/// What: for each bundled file, writes it when missing (`written`); when
/// present, acts on `policy`. [`RefreshPolicy::WriteMissingOnly`] skips it
/// untouched. The other two compare on-disk bytes to the bundled bytes —
/// identical content is skipped, and differing content is archived to the
/// content-addressed [`stale_backup_path`] (`backed_up`) and then overwritten
/// (`refreshed`) via `crate::state_writer::atomic_write` (tmp-file + rename, so
/// a crash mid-write leaves the original file/backup intact rather than torn).
/// The backup write is skipped only when that exact path already exists, which —
/// because the path is derived from the bytes — means those bytes are already
/// archived. Only ever touches paths that are part of the bundled tree — a
/// user's own non-bundled file at `target_dir` is never read, backed up, or
/// written.
///
/// #3844: under [`RefreshPolicy::KeepHandEdits`] — every automatic pass — a
/// divergent file is overwritten ONLY when [`provenance`]'s manifest vouches
/// that its current content is what this module last wrote there. Content it
/// cannot vouch for is the operator's, so the file is left exactly as it is and
/// counted in `preserved`, with a `warn` naming the file and the one command
/// that does overwrite it. [`RefreshPolicy::Overwrite`] is that command's path
/// and asks no such question.
///
/// The manifest is rewritten at the end of the pass, still inside the caller's
/// lock, and only when the pass actually changed it — so the matching-stamp fast
/// path stays a true no-op with zero disk writes.
/// Test: `stale_stamp_triggers_refresh`, `refresh_backs_up_differing_content`,
/// `non_bundled_user_file_untouched_by_refresh`,
/// `existing_stale_backup_is_never_clobbered`,
/// `repeated_divergence_is_recoverable_across_reprovisions`,
/// `pristine_files_are_reprovisioned_without_littering_backups`,
/// `a_hand_edited_bundled_file_survives_a_stale_stamp_refresh`,
/// `a_stale_deploy_this_tool_wrote_is_still_refreshed`,
/// `a_preserved_hand_edit_is_never_archived_or_rewritten`,
/// `repair_still_overwrites_a_hand_edit_the_automatic_pass_kept` (tests.rs);
/// `pass_lock_serializes_concurrent_reprovision_calls` (`lock`'s own tests)
/// pins the underlying mutual-exclusion primitive.
fn reprovision_tree_locked(
    _guard: &lock::ProvisionLock,
    tree: &BundledTree,
    target_dir: &Path,
    policy: RefreshPolicy,
) -> Result<ReprovisionReport> {
    let mut report = ReprovisionReport::default();
    // #3844: what this module last wrote at each path, read once per pass.
    let known = provenance::read(target_dir);
    let mut recorded = known.clone();
    // #5226: a path the bundle does not own is never materialised here.
    for (rel, bytes) in bundled_files(tree) {
        let dest = target_dir.join(&rel);

        if dest.exists() {
            if policy == RefreshPolicy::WriteMissingOnly {
                continue;
            }
            let current = std::fs::read(&dest)
                .with_context(|| format!("failed to read existing {}", dest.display()))?;
            if current == bytes {
                // Already the shipped content, so this path is ours whatever a
                // pre-#3844 manifest says (or fails to say) about it.
                recorded.insert(rel.clone(), provenance::digest(&bytes));
                continue;
            }
            // #3844: the divergence is the operator's unless the manifest says
            // this module wrote it. Refusing here is what stopped four
            // consecutive `cargo install`s from reverting live configuration.
            if policy == RefreshPolicy::KeepHandEdits
                && !provenance::vouches_for(&known, &rel, &current)
            {
                tracing::warn!(
                    file = %dest.display(),
                    "keeping the on-disk bundled file: its content is not what was \
                     last deployed there, so it carries a local edit. The shipped \
                     template was NOT applied to it — run `tagent agents repair` to \
                     overwrite it (the current content is archived first)"
                );
                report.preserved += 1;
                continue;
            }
            // #4461: the backup name carries the digest of the bytes being
            // archived, so a second divergence lands beside the first rather
            // than being skipped as "already backed up".
            let backup = stale_backup_path(&dest, &current);
            if !backup.exists() {
                crate::state_writer::atomic_write(&backup, &current).with_context(|| {
                    format!(
                        "failed to back up stale bundled file to {}",
                        backup.display()
                    )
                })?;
                tracing::warn!(
                    file = %dest.display(),
                    backup = %backup.display(),
                    "on-disk bundled file differs from the shipped template; \
                     archived its content before overwriting"
                );
                report.backed_up += 1;
            }
            crate::state_writer::atomic_write(&dest, &bytes)
                .with_context(|| format!("failed to refresh {}", dest.display()))?;
            recorded.insert(rel.clone(), provenance::digest(&bytes));
            report.refreshed += 1;
            continue;
        }

        crate::state_writer::atomic_write(&dest, &bytes)
            .with_context(|| format!("failed to write {}", dest.display()))?;
        recorded.insert(rel.clone(), provenance::digest(&bytes));
        report.written += 1;
    }
    if recorded != known {
        provenance::write(target_dir, &recorded)?;
    }
    Ok(report)
}

/// Sibling backup path for a bundled file about to be overwritten, named
/// after the CONTENT being archived (#3556, #4461).
///
/// Why: the original fixed `.stale.bak` name gave each file exactly one
/// backup slot, and the caller wrote it only when that slot was empty — so
/// the first reprovision preserved a hand-edit and every later one discarded
/// the user's work silently, because a backup was already sitting there
/// (#4461). Deriving the name from the archived bytes makes the write both
/// safe and complete: two different contents can never target the same path,
/// so no pass can clobber another's backup (the property the fixed name was
/// protecting), and the same content re-archived resolves to the same path,
/// so nothing accumulates from repeated no-change passes. A wall-clock
/// suffix would give the same safety but a new file on every pass; the
/// digest is what bounds the set.
///
/// What bounds the set: one file per DISTINCT content ever overwritten at
/// this path. Reprovisioning N times over an unchanged file adds nothing —
/// an unchanged file is never overwritten at all, and re-archiving identical
/// bytes hits the existing path. The set grows only when the file genuinely
/// diverges again with content not already archived, so its size is the
/// number of real divergences, never the number of reprovisions. Nothing
/// prunes it: every entry is content a human may still need, and an
/// automatic sweep would be free to delete the exact hand-edit this exists
/// to preserve.
///
/// What: appends `.stale.<digest>.bak` to the full file name, where
/// `<digest>` is the first 16 hex characters of the SHA-256 of `content`
/// (e.g. `assistant/agent.toml` -> `assistant/agent.toml.stale.9f2c….bak`).
/// The `.bak` tail is kept because the agent listers filter on it —
/// `repl::agent_commands::list_assistant_agents_into` and
/// `api::server::projects`'s catalog scan both skip `*.bak`, so a backup
/// never surfaces as a bogus agent.
/// Test: `refresh_backs_up_differing_content`,
/// `repeated_divergence_is_recoverable_across_reprovisions`,
/// `identical_content_reuses_one_backup_path` (tests.rs).
fn stale_backup_path(dest: &Path, content: &[u8]) -> PathBuf {
    let digest = format!("{:x}", Sha256::digest(content));
    let mut name = dest.as_os_str().to_os_string();
    name.push(format!(".stale.{}.bak", &digest[..16]));
    PathBuf::from(name)
}

/// The content stamp of `tree`.
///
/// Why: pulled out of the two call sites (`ensure_tree_deployed_in`,
/// `force_reprovision_bundled_agents`) that both need "what does this tree
/// hash to right now" so they can never compute it differently.
/// What: hashes the [`bundled_files`] view with [`stamp::compute`].
/// Test: `stamp_changes_when_content_changes`,
/// `stamp_stable_regardless_of_input_order` (`stamp`'s own tests).
fn current_bundle_stamp(tree: &BundledTree) -> String {
    // #5226: the same filtered view the write loop uses, so a stray file can
    // never make the stamp disagree with what actually gets deployed.
    stamp::compute(bundled_files(tree))
}

/// Hermetic core of [`ensure_bundled_agents_deployed`] — takes the tree and
/// `target_dir` explicitly so tests can point it at a tempdir (#3556).
///
/// Why: the production entry point resolves content and `$HOME` itself, which
/// routine `cargo test` runs must never touch.
/// What: [`ensure_tree_deployed_in`] over `tree`.
/// Test: `stale_stamp_triggers_refresh`, `matching_stamp_is_a_fast_noop`,
/// `missing_stamp_establishes_baseline_without_rewriting_matching_content`,
/// `concurrent_ensure_calls_over_stale_target_converge_to_one_consistent_refresh`,
/// `ensure_deployed_blocks_on_externally_held_pass_lock` (tests.rs).
// #9014: takes the tree read from content; nothing is compiled in.
pub fn ensure_bundled_agents_deployed_in(
    tree: &BundledTree,
    target_dir: &Path,
) -> Result<ReprovisionReport> {
    ensure_tree_deployed_in(tree, target_dir)
}

/// Stamp-aware deploy/refresh of one bundled tree into `target_dir` (#5227).
///
/// Why: agents and workflows need byte-identical deploy semantics — the
/// pass-level lock, the content-hash staleness check, the
/// `.stale.<digest>.bak` archive of a differing on-disk copy. One
/// implementation keeps the two from drifting. `stamp` and `lock` are
/// per-`target_dir`, so the two trees never share a stamp file. Acquires the
/// pass-level lock ONCE, before `stamp::read` (#3556), and holds it across the
/// whole read-decide-refresh-write sequence — see [`reprovision_tree_locked`].
/// What: compares the tree's stamp to `target_dir/.bundled-stamp`; a missing
/// or differing stamp refreshes under [`RefreshPolicy::KeepHandEdits`] and
/// rewrites the stamp, a matching one takes the
/// [`RefreshPolicy::WriteMissingOnly`] fast path and leaves it untouched.
/// Test: the bundled-agent suite through `ensure_bundled_agents_deployed_in`;
/// `bundled_workflows_deploy_to_target` pins the workflows tree.
fn ensure_tree_deployed_in(tree: &BundledTree, target_dir: &Path) -> Result<ReprovisionReport> {
    let guard = lock::acquire(target_dir)?;

    let current_stamp = current_bundle_stamp(tree);
    let on_disk_stamp = stamp::read(target_dir);
    let is_stale = on_disk_stamp.as_deref() != Some(current_stamp.as_str());

    // #3844: a stale stamp refreshes what this module last wrote and keeps what
    // the operator edited; a matching stamp still never revisits an existing file.
    let policy = if is_stale {
        RefreshPolicy::KeepHandEdits
    } else {
        RefreshPolicy::WriteMissingOnly
    };
    let report = reprovision_tree_locked(&guard, tree, target_dir, policy)?;
    if is_stale {
        stamp::write(target_dir, &current_stamp)?;
    }
    Ok(report)
}

/// `~/.trusty-agents/<kind dir>`, or `None` when `$HOME` is unknown.
fn home_target(kind: BundledTreeKind) -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".trusty-agents").join(kind.dir_name()))
}

/// Reads the `kind` tree for this process (#9014).
///
/// What: [`BundledTree::load`], its typed error kept as the source so a
/// caller can still downcast it (see [`log_deploy_failure`]).
fn load_tree(kind: BundledTreeKind) -> Result<BundledTree> {
    BundledTree::load(kind).with_context(|| {
        format!(
            "cannot read the bundled {} from instructional content",
            kind.dir_name()
        )
    })
}

/// Production entry point: deploy/refresh the bundled workflow definitions at
/// `$HOME/.trusty-agents/workflows/` (#5227).
///
/// Why: `--workflow prescriptive` reads `<workflows-dir>/prescriptive.json`.
/// A `cargo install`ed `tagent` — or the GUI's `.app` sidecar, whose CWD is
/// `/` — has no workflow definition anywhere on its search hierarchy until
/// this deploys one.
/// What: mirrors [`ensure_bundled_agents_deployed`] — no-op when `$HOME` is
/// unresolvable, otherwise reads the workflow tree from content (an `Err`
/// naming the remedy when there is none) and deploys it stamp-aware into
/// `$HOME/.trusty-agents/workflows`. Best-effort at the call site.
/// Test: `bundled_workflows_deploy_to_target` covers the hermetic core.
pub fn ensure_bundled_workflows_deployed() -> Result<ReprovisionReport> {
    let Some(target) = home_target(BundledTreeKind::Workflows) else {
        return Ok(ReprovisionReport::default());
    };
    ensure_tree_deployed_in(&load_tree(BundledTreeKind::Workflows)?, &target)
}

/// Production entry point: deploy/refresh the bundled agent set at
/// `$HOME/.trusty-agents/agents/` — the resolver's `$HOME` fallback tier —
/// so `tagent` launched from ANY directory resolves `assistant` (and the
/// rest of the bundled roster) with the current content, not whatever was
/// deployed there before (#3556).
///
/// Why: called once per process from `runtime::startup::run_startup_init` —
/// the single choke point every interactive/one-shot invocation passes
/// through, but NOT `TrustyAgentsRepl::new` (many unit tests construct a REPL
/// directly without sandboxing `$HOME`).
/// What: no-op (`Ok(default)`) when `$HOME` cannot be resolved. Otherwise
/// reads the agent tree from content and deploys it. With no content
/// installed, or a release missing a required file, it returns that typed
/// error (#9014) and deploys nothing; a write failure (a read-only `$HOME`)
/// is an `Err` too. The caller logs it and continues — it never crashes the
/// harness.
/// Test: `ensure_bundled_agents_deployed_in` (this function's hermetic
/// core) carries the unit coverage; `an_absent_tree_fails_closed_naming_its_first_required_file`
/// (content_tests.rs) pins the no-content arm.
pub fn ensure_bundled_agents_deployed() -> Result<ReprovisionReport> {
    let Some(target) = home_target(BundledTreeKind::Agents) else {
        return Ok(ReprovisionReport::default());
    };
    ensure_tree_deployed_in(&load_tree(BundledTreeKind::Agents)?, &target)
}

/// The remedy text when `err` is a not-installed content error (#9014).
///
/// Why: `run_startup_init` deploys before tracing exists and the REPL sends
/// tracing to a file, so it prints this one line itself.
/// What: `Some(<error text>)` for a not-installed
/// [`trusty_agents_common::agent_content::AgentContentError`], else `None`.
/// Test: `no_content_startup_names_tm_content_update` (tests/no_content_e2e.rs).
pub fn not_installed_message(err: &anyhow::Error) -> Option<String> {
    err.downcast_ref::<trusty_agents_common::agent_content::AgentContentError>()
        .filter(|e| e.is_not_installed())
        .map(ToString::to_string)
}

/// The stderr text for a content error from a deploy (#9014).
///
/// Why: old or unusable content (`Missing`, `Invalid`, `EmptyRoster`) fails
/// the load as surely as none does, and its remedy is the same `tm content
/// update`; the REPL's tracing goes to a file, so startup prints it.
/// What: `Some(<error text>)`, which carries the remedy and the path or origin,
/// for every [`trusty_agents_common::agent_content::AgentContentError`] except
/// `Write` (a plain I/O failure, not a content problem); else `None`.
/// Test: `stale_content_startup_names_tm_content_update_and_the_missing_file`
/// (tests/stale_content_e2e.rs).
pub fn deploy_failure_message(err: &anyhow::Error) -> Option<String> {
    use trusty_agents_common::agent_content::AgentContentError;
    err.downcast_ref::<AgentContentError>()
        .filter(|e| !matches!(e, AgentContentError::Write { .. }))
        .map(ToString::to_string)
}

/// Logs a failed best-effort deploy of `what` (#9014).
///
/// Why: with no content installed, the agent and workflow deploys fail for
/// the same reason; one ERROR line naming `tm content update` tells the
/// operator what to do, two warnings bury it.
/// What: a not-installed [`trusty_agents_common::agent_content::AgentContentError`]
/// goes through `report_not_installed` (one ERROR line per process); any other
/// error is a `warn`.
/// Test: `no_content_startup_names_tm_content_update` (tests/no_content_e2e.rs);
/// the one-line rule is pinned in trusty-agents-common's `agent_content` tests.
pub fn log_deploy_failure(what: &str, err: &anyhow::Error) {
    use trusty_agents_common::agent_content::{AgentContentError, report_not_installed};
    let claimed = err
        .downcast_ref::<AgentContentError>()
        .is_some_and(|e| report_not_installed(e, what));
    if !claimed {
        tracing::warn!(error = %format!("{err:#}"), "{what}");
    }
}

/// Force-refresh every file of `tree` under `target_dir` regardless of the
/// stamp (#3556) — the hermetic core of [`repair_bundled_agents`].
///
/// Why: an explicit manual escape hatch for the cases the automatic stamp
/// check can't cover — e.g. a user suspects a deploy was interrupted. Acquires
/// the pass-level lock ONCE and holds it through the trailing `stamp::write`
/// (#3556), so a concurrent `ensure_*`/`repair` pair can never interleave.
/// What: runs the refresh loop under [`RefreshPolicy::Overwrite`], then writes
/// the tree's stamp so a later automatic startup check treats the roster as
/// up to date. Errors propagate loudly — no fail-soft path.
/// Test: `force_reprovision_overwrites_even_when_stamp_matches`,
/// `existing_stale_backup_is_never_clobbered` (tests.rs).
// #9014: takes the tree read from content; nothing is compiled in.
pub fn force_reprovision_bundled_agents(
    tree: &BundledTree,
    target_dir: &Path,
) -> Result<ReprovisionReport> {
    let guard = lock::acquire(target_dir)?;
    let report = reprovision_tree_locked(&guard, tree, target_dir, RefreshPolicy::Overwrite)?;
    stamp::write(target_dir, &current_bundle_stamp(tree))?;
    Ok(report)
}

/// Production entry point for the explicit manual escape hatch — `tagent
/// agents repair` (#3556).
///
/// Why: unlike [`ensure_bundled_agents_deployed`], this is a command the user
/// explicitly invoked to fix a problem — an unresolvable `$HOME` or absent
/// content is a real failure the user needs to see.
/// What: resolves `$HOME/.trusty-agents/agents`, reads the agent tree from
/// content and delegates to [`force_reprovision_bundled_agents`]; errors
/// loudly on either failure.
/// Test: covered via `force_reprovision_bundled_agents`'s tempdir-based tests.
pub fn repair_bundled_agents() -> Result<ReprovisionReport> {
    let target = home_target(BundledTreeKind::Agents)
        .context("cannot resolve $HOME to repair bundled agents in ~/.trusty-agents/agents")?;
    force_reprovision_bundled_agents(&load_tree(BundledTreeKind::Agents)?, &target)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
