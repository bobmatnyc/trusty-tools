//! Move untracked project-tier files that SHADOW a bundled agent (issue #4448).
//!
//! Why: `retract_framework_agents` (#4409) can only delete what its ownership
//! ledger names. A copy an older binary wrote before the ledger existed, or one
//! a stale deploy left behind, is invisible to it — and the project tier
//! outranks the canonical user tier in Claude Code's resolution, so that copy
//! wins forever and nothing refreshes it. #4408 is that failure observed live: a
//! 32-byte project-tier stub beat the real 25 KB `rust-engineer`, delegation
//! kept "working", and no check could fail.
//!
//! What: [`quarantine_shadowing_agents`] renames such a file to an inert
//! `.md.disabled` sibling AFTER taking a verified byte-identical backup, and
//! returns a [`QuarantineReport`] accounting for every file it examined.
//!
//! # The movability predicate
//!
//! A file is movable only when ALL FOUR gates agree. Each is independently
//! fail-closed, so a new [`TierResidentClass`] variant, an unreadable ledger, a
//! missing `git`, or an unfamiliar frontmatter key each default to REFUSING.
//!
//! | # | Gate | Refuses when | Source |
//! |---|---|---|---|
//! | 1 | Name | the file does not resolve to a name tm ships | [`classify_tier_resident`] (#4442) |
//! | 2 | Ledger | the ledger records the operator as owner | [`ownership_of`] (#4442) |
//! | 3 | VCS | git tracks the file, or git cannot be asked | [`VcsIndex`] (#4448) |
//! | 4 | Schema | the frontmatter is not tm's composer output | [`agent_schema`] (#4448) |
//!
//! Gates 3 and 4 are what #4526 lacked, and each answers a distinct hazard that
//! blocked it:
//!
//! - **Gate 3 replaces the closed `Origin::Project` (#4443).** A
//!   `ShadowsBundled` classification together with an `Untracked` ledger cannot
//!   separate a stale leftover from a project's deliberate override. A
//!   repository that COMMITS a project-tier agent is declaring it, and
//!   `git ls-files` is that declaration — reachable without any new schema.
//!   #4526 would have renamed this repository's own committed agent files; that
//!   is the failure this gate closes.
//! - **Gate 4 keeps another live project's files safe.** claude-mpm deploys
//!   into the same `.claude/agents` convention reusing tm's exact filenames
//!   (`engineer.md`, `qa.md`, `ops.md`, `research.md`, `ticketing.md`, …) with a
//!   different schema. A filename-keyed sweep moves them; three of six
//!   independent inventory passes on 2026-08-03 made exactly that error.
//!
//! # What this module will not do
//!
//! IT NEVER DELETES. There is no `remove_file` and no `remove_dir_all` on any
//! path, success or failure. Quarantine is a copy followed by a rename; a
//! failure leaves both the original and its backup where they are. A quarantine
//! that deletes is a worse defect than the shadowing it fixes (2026-07-21
//! incident). `never_deletes_on_any_path` pins it.
//!
//! It also never runs against the CANONICAL deploy tier — every file there is
//! tm-owned, so a sweep would move the entire roster. Callers bind the target
//! to the workspace path explicitly; see
//! `prepare_session_never_quarantines_the_operator_home_agents_tier` in
//! `trusty-mpm`.
//!
//! Test: `quarantine_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::agents::agent_schema::{AgentSchema, agent_schema};
use crate::agents::deployer::is_agent_file;
use crate::agents::manifest::{
    AgentManifest, ManifestError, ManifestLoad, with_agent_manifest_lock,
};
use crate::agents::quarantine_receipt::{
    FailStage, FailedAgent, QuarantineReport, QuarantinedAgent, SkipReason, SkippedAgent,
    render_receipt,
};
use crate::agents::tier_audit::{
    TierOwnership, TierResidentClass, agent_identity, classify_tier_resident, ownership_of,
};
use crate::agents::vcs_claim::{VcsClaim, VcsIndex};

/// Suffix that makes a file inert without renaming it beyond recognition.
///
/// Every agent loader keys on a `.md` extension — `is_agent_file` is
/// `ends_with(".md")`, and trusty-agents' claude-mpm loaders (sync and async)
/// both test `path.extension() == Some("md")`. `qa.md.disabled` has extension
/// `disabled`, so no loader sees it, while the original name stays legible.
const DISABLED_SUFFIX: &str = ".disabled";

/// How many numbered fallbacks to try before giving up on a free name.
const MAX_COLLISION_ATTEMPTS: u32 = 100;

/// Filename of the per-run receipt, written beside that run's backups.
const RECEIPT_FILE: &str = "RECEIPT.md";

/// Why a whole sweep refused to run.
///
/// Why: these are the conditions under which acting at all would be reckless,
/// as distinct from a per-file refusal (which is a [`SkipReason`], not an
/// error). Every variant means NOTHING was touched.
/// What: [`CorruptLedger`](Self::CorruptLedger) — the ownership ledger did not
/// parse, or could not be read at all (#5626), so the operator-owned exemption
/// cannot be honoured;
/// [`EmptyRoster`](Self::EmptyRoster) — the bundled roster resolved empty, which
/// would make every file classify as custom (a silent no-op that hides a broken
/// install); [`Manifest`](Self::Manifest) — the ledger lock could not be taken.
/// Test: `quarantine_refuses_on_a_corrupt_ledger`,
/// `quarantine_refuses_when_the_ledger_cannot_be_read`,
/// `quarantine_refuses_an_empty_roster`.
#[derive(Debug, thiserror::Error)]
pub enum QuarantineError {
    /// The ownership ledger was not established — corrupt, or unreadable.
    /// Nothing was moved.
    #[error(
        "agent ownership ledger could not be established; refusing to move any file on the \
         strength of a ledger this run never read, because the operator-owned exemption lives \
         in it. Detail: {0}"
    )]
    CorruptLedger(String),
    /// The bundled roster is empty. Nothing was moved.
    #[error(
        "the bundled agent roster resolved empty; refusing to sweep, because every file would \
         classify as a custom agent and the run would report a false clean"
    )]
    EmptyRoster,
    /// The ledger lock could not be acquired.
    #[error("agent ledger lock failed: {0}")]
    Manifest(#[from] ManifestError),
}

/// Quarantine every file in `tier_dir` that shadows a bundled agent name.
///
/// Why: the repair half of #4442's report/repair pair. `tm doctor`'s
/// `asset_tier` probe flags these files and can only tell the operator to
/// delete them by hand; this moves them, reversibly, with a receipt.
///
/// What: takes the SAME exclusive ledger lock the deploy and retract paths take
/// (`with_agent_manifest_lock`) and holds it across the whole
/// read-classify-move sequence, so two concurrent session launches serialise and
/// the second finds the work already done. For each `.md` file directly under
/// `tier_dir` — never recursing, the tier is flat — it evaluates the four gates
/// in the module doc's table and either records a [`SkipReason`] or moves the
/// file: copy into `backup_root/<run-id>/`, re-read the copy and compare bytes,
/// then rename the original to `<name>.md.disabled`, then confirm from disk that
/// the destination exists and the source is gone. `run_id` distinguishes runs so
/// a second sweep never overwrites the first run's backups.
///
/// A missing `tier_dir` is a no-op that does NOT create the directory — the
/// lock would, and materialising an empty `.claude/agents/` in every project on
/// every launch is a visible regression of its own.
///
/// EVERY EXIT PATH RETURNS THE FULL REPORT. A per-file I/O failure is recorded
/// in [`QuarantineReport::failed`] and the sweep continues; the receipt is
/// rendered from the report afterwards, so a partially failed run still
/// produces an accurate account of what moved and what did not. Only the
/// whole-sweep refusals in [`QuarantineError`] return `Err`, and each of those
/// means nothing was touched.
///
/// Test: `quarantine_moves_an_untracked_trusty_mpm_shadow`,
/// `quarantine_never_moves_a_git_tracked_file`,
/// `quarantine_never_moves_a_claude_mpm_file_on_a_colliding_name`,
/// `quarantine_never_moves_a_user_owned_entry`,
/// `quarantine_never_moves_a_custom_agent`,
/// `a_partial_failure_still_reports_every_candidate`,
/// `never_deletes_on_any_path`, `quarantine_missing_dir_is_a_noop`,
/// `quarantine_refuses_on_a_corrupt_ledger`, `quarantine_refuses_an_empty_roster`.
pub fn quarantine_shadowing_agents(
    tier_dir: &Path,
    backup_root: &Path,
    bundled: &BTreeSet<String>,
    run_id: &str,
) -> Result<QuarantineReport, QuarantineError> {
    // Refuse before locking: an empty roster cannot classify anything, and a
    // no-op that looks clean is how a broken install stays invisible.
    if bundled.is_empty() {
        return Err(QuarantineError::EmptyRoster);
    }
    // Checked before the lock so the common no-op neither blocks nor CREATES
    // the directory — mirroring `retract_framework_agents_filtered`.
    if !tier_dir.is_dir() {
        return Ok(QuarantineReport::default());
    }

    with_agent_manifest_lock(tier_dir, || {
        sweep_locked(tier_dir, backup_root, bundled, run_id)
    })
}

/// The body of [`quarantine_shadowing_agents`], run holding the ledger lock.
///
/// Why/What: mirrors `deployer::retract_locked` — the critical section is one
/// call so the lock's scope cannot be misread, and the ledger is loaded INSIDE
/// it (a read taken before the lock could be stale by the time a file moves).
/// Never call it directly.
/// Test: covered by every `quarantine_*` test through the public wrapper, plus
/// `quarantine_blocks_while_the_ledger_lock_is_held`.
fn sweep_locked(
    tier_dir: &Path,
    backup_root: &Path,
    bundled: &BTreeSet<String>,
    run_id: &str,
) -> Result<QuarantineReport, QuarantineError> {
    let manifest = match AgentManifest::load_checked(tier_dir) {
        ManifestLoad::Ok(m) => m,
        // #5626: an unreadable ledger refuses exactly like a corrupt one — the
        // user-owned exemption it holds is the reason nothing may move, and a
        // read that failed is no more evidence of a file's owner than a parse
        // that failed.
        ManifestLoad::Corrupt(detail) | ManifestLoad::Unreadable(detail) => {
            return Err(QuarantineError::CorruptLedger(detail));
        }
    };
    let vcs = VcsIndex::probe(tier_dir);
    let backup_dir = backup_root.join(run_id);

    let mut report = QuarantineReport::default();
    for (file_name, path) in candidates(tier_dir) {
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let name = agent_identity(&content, &file_name);
        let ownership = ownership_of(&manifest, &file_name);

        match refusal(&name, &content, ownership, bundled, vcs.claim(&file_name)) {
            Some(reason) => report.skipped.push(SkippedAgent { name, path, reason }),
            None => move_one(&mut report, name, &path, &file_name, &backup_dir),
        }
    }

    // Rendered from the report, after every file is accounted for — including
    // a run that failed part way. See `quarantine_receipt`'s module doc.
    if report.wrote_anything() {
        let body = render_receipt(&report, tier_dir, run_id);
        // #4448: routed through `free_path` like the backups, not a bare write.
        // `run_id` is second-resolution, so two sweeps inside one UTC second
        // would otherwise truncate the first run's receipt — the only record of
        // what that run moved.
        let written = std::fs::create_dir_all(&backup_dir).and_then(|()| {
            let path = free_path(&backup_dir.join(RECEIPT_FILE)).ok_or_else(|| {
                std::io::Error::other(format!(
                    "no free receipt name after {MAX_COLLISION_ATTEMPTS} attempts"
                ))
            })?;
            std::fs::write(&path, body).map(|()| path)
        });
        match written {
            Ok(path) => report.receipt = Some(path),
            // Never an `Err`: the moves already happened and the caller must
            // still learn about them.
            Err(e) => report.receipt_error = Some(e.to_string()),
        }
    }

    Ok(report)
}

/// The `.md` files directly under `dir`, sorted for deterministic output.
///
/// Why: `read_dir` order is filesystem-defined, and a sweep whose report order
/// varies run to run is not diffable.
/// What: `(file_name, path)` pairs for regular files whose name
/// [`is_agent_file`] accepts. `symlink_metadata` is deliberate — a symlink into
/// the canonical tier must not be followed and treated as a resident file.
/// Test: covered by `quarantine_moves_an_untracked_trusty_mpm_shadow` and
/// `a_symlink_is_never_a_candidate`.
fn candidates(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name().to_str()?.to_owned();
            if !is_agent_file(&file_name) {
                return None;
            }
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).ok()?;
            meta.is_file().then_some((file_name, path))
        })
        .collect();
    out.sort();
    out
}

/// The FIRST gate that refuses this file, or `None` when all four agree.
///
/// Why: the conjunction stated once, in gate order, so the receipt names the
/// most specific owner of the file rather than whichever check happened to run
/// last. Splitting it out also makes every branch unit-testable without
/// staging a directory.
/// What: gate 1+2 come from [`classify_tier_resident`] — a `UserOwned` ledger
/// entry wins before any name is compared, and everything that is not
/// [`TierResidentClass::ShadowsBundled`] is refused, which deliberately
/// includes [`TierResidentClass::StrandedFrameworkOwned`] (that tier belongs to
/// `retract_framework_agents`, and two writers on one file is a race, not a
/// repair). Gate 3 is the VCS claim; gate 4 is the frontmatter schema. Only
/// [`AgentSchema::TrustyMpm`] and [`VcsClaim::Unclaimed`] pass.
///
/// PURE — the caller resolves the [`VcsClaim`] and passes it in, so every row
/// of the conjunction (including the `Unknown` VCS row, which needs a machine
/// without git to reach through [`VcsIndex`]) is directly testable.
/// Test: `movable_only_when_all_four_gates_agree`, plus one
/// `quarantine_never_moves_*` test per gate.
fn refusal(
    name: &str,
    content: &str,
    ownership: TierOwnership,
    bundled: &BTreeSet<String>,
    claim: VcsClaim,
) -> Option<SkipReason> {
    // #4448 gates 1 and 2 — the shared #4442 classifier, never re-derived here.
    if classify_tier_resident(name, ownership, bundled) != TierResidentClass::ShadowsBundled {
        return Some(if ownership == TierOwnership::UserOwned {
            SkipReason::UserOwned
        } else {
            SkipReason::NotShadowingBundled
        });
    }
    // #4448 gate 3 — the repository's own claim, standing in for the closed
    // `Origin::Project` (#4443).
    match claim {
        VcsClaim::Claimed => return Some(SkipReason::GitTracked),
        VcsClaim::Unknown => return Some(SkipReason::VcsUnknown),
        VcsClaim::Unclaimed => {}
    }
    // #4448 gate 4 — never the filename; claude-mpm reuses tm's exactly.
    match agent_schema(content) {
        AgentSchema::TrustyMpm => None,
        AgentSchema::ClaudeMpm => Some(SkipReason::ClaudeMpmSchema),
        AgentSchema::Unrecognized => Some(SkipReason::UnrecognizedSchema),
    }
}

/// Back up, move, and verify one file, recording the outcome on `report`.
///
/// Why: the ordering is the safety property. The backup is taken and VERIFIED
/// before the original is touched, so every failure mode leaves at least one
/// intact copy — and no branch here removes anything.
/// What: copy → re-read the copy and compare bytes → rename the original to a
/// free `.md.disabled` name → confirm from disk that the destination exists and
/// the source is gone. Any step failing pushes a [`FailedAgent`] naming the
/// [`FailStage`] and returns; the sweep continues with the next file.
/// Verification re-reads from the filesystem rather than trusting the syscall's
/// return, because an exit code is not evidence.
/// Test: `quarantine_moves_an_untracked_trusty_mpm_shadow`,
/// `a_failed_backup_leaves_the_original_in_place`,
/// `a_second_run_does_not_overwrite_the_first_backup`.
fn move_one(
    report: &mut QuarantineReport,
    name: String,
    path: &Path,
    file_name: &str,
    backup_dir: &Path,
) {
    let mut fail = |stage: FailStage, detail: String| {
        report.failed.push(FailedAgent {
            name: name.clone(),
            path: path.to_path_buf(),
            stage,
            detail,
        });
    };

    if let Err(e) = std::fs::create_dir_all(backup_dir) {
        fail(FailStage::Backup, e.to_string());
        return;
    }
    let Some(backup) = free_path(&backup_dir.join(file_name)) else {
        fail(
            FailStage::Backup,
            format!("no free backup name after {MAX_COLLISION_ATTEMPTS} attempts"),
        );
        return;
    };
    if let Err(e) = std::fs::copy(path, &backup) {
        fail(FailStage::Backup, e.to_string());
        return;
    }

    // Evidence, not an exit code: read both back and compare bytes.
    match (std::fs::read(path), std::fs::read(&backup)) {
        (Ok(original), Ok(copied)) if original == copied => {}
        (Ok(_), Ok(_)) => {
            fail(
                FailStage::Verify,
                format!("backup {} is not byte-identical", backup.display()),
            );
            return;
        }
        (a, b) => {
            let detail = a.err().or(b.err()).map_or_else(
                || "unreadable after copy".to_owned(),
                |e: std::io::Error| e.to_string(),
            );
            fail(FailStage::Verify, detail);
            return;
        }
    }

    let Some(to) = free_path(&path.with_file_name(format!("{file_name}{DISABLED_SUFFIX}"))) else {
        fail(
            FailStage::Rename,
            format!("no free `{DISABLED_SUFFIX}` name after {MAX_COLLISION_ATTEMPTS} attempts"),
        );
        return;
    };
    if let Err(e) = std::fs::rename(path, &to) {
        fail(FailStage::Rename, e.to_string());
        return;
    }

    // Confirm the move actually landed, from disk.
    if std::fs::symlink_metadata(&to).is_err() || std::fs::symlink_metadata(path).is_ok() {
        fail(
            FailStage::Verify,
            format!(
                "after rename, {} is not in its expected state",
                to.display()
            ),
        );
        return;
    }

    report.moved.push(QuarantinedAgent {
        name,
        from: path.to_path_buf(),
        to,
        backup,
    });
}

/// `preferred`, or the first free `preferred.N`, or `None`.
///
/// Why: a quarantine that overwrites an occupied destination destroys the very
/// thing it exists to preserve — including a previous run's `.md.disabled`,
/// which is an operator's only in-place copy.
/// What: freeness is `symlink_metadata().is_err()`, so a DANGLING SYMLINK
/// counts as occupied; `metadata()` would follow it and report the path free,
/// and the subsequent rename would then write through the link to wherever it
/// points. Gives up after [`MAX_COLLISION_ATTEMPTS`] rather than looping.
/// Test: `a_second_run_does_not_overwrite_the_first_backup`,
/// `a_dangling_symlink_is_not_free`.
fn free_path(preferred: &Path) -> Option<PathBuf> {
    if std::fs::symlink_metadata(preferred).is_err() {
        return Some(preferred.to_path_buf());
    }
    let base = preferred.as_os_str().to_string_lossy().into_owned();
    (1..=MAX_COLLISION_ATTEMPTS).find_map(|n| {
        let candidate = PathBuf::from(format!("{base}.{n}"));
        std::fs::symlink_metadata(&candidate)
            .is_err()
            .then_some(candidate)
    })
}

#[cfg(test)]
#[path = "quarantine_tests.rs"]
mod tests;
