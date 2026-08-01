//! Quarantine untracked agent files that shadow a bundled name (issue #4448).
//!
//! Why: [`crate::agents::deployer::retract_framework_agents`] is the repair for
//! a workspace `.claude/agents/` that an older binary deployed into — but it can
//! only see LEDGER ENTRIES. A file no manifest names is invisible to it, and
//! since #4437 nothing deploys into a workspace tier either, so the deployer's
//! old skip-and-warn branch never runs there. The result is a permanent hole:
//! a claude-mpm-era `qa.md` sits in the project tier, the project tier outranks
//! the canonical user tier in Claude Code's resolution, and every delegation to
//! `qa` silently loads the stale definition. Observed live on 2026-07-30 in 4 of
//! 16 sessions; 22 files were quarantined BY HAND on 2026-07-31 because no code
//! path could reach them. This module is that hand-sweep, made mechanical.
//!
//! What: [`quarantine_shadowing_agents`] renames each offender to
//! `<file>.md.disabled` — a rename, never a delete — and drops a plain-text
//! receipt beside it. It is deliberately the NARROWEST possible sweep:
//!
//! - Classification is [`crate::agents::tier_audit`]'s and only its. This module
//!   contains no "is this bundled" predicate of its own; it filters that
//!   module's verdicts. A second predicate here is the drift the shared
//!   classifier exists to prevent.
//! - It moves only [`TierResidentClass::ShadowsBundled`] AND
//!   [`TierOwnership::Untracked`] files. Every other combination is refused —
//!   see [`quarantine_shadowing_agents`] for the per-case reasoning.
//! - It NEVER writes the ownership ledger. Because only untracked files move,
//!   no manifest entry can be left pointing at a renamed file; that invariant is
//!   structural, not a step that could be forgotten.
//! - Every ambiguity is a refusal, not a sweep: an empty roster, a corrupt
//!   ledger, or an occupied destination name each stop the move. This is a
//!   deliberate guard against the fail-open shape (the failure branch falling
//!   through to the action anyway) that this repo keeps re-growing.
//!
//! Pairing: run it alongside
//! [`crate::agents::deployer::retract_framework_agents`] on the same directory
//! — conventionally after, matching both call sites. Retraction owns the
//! tracked tier (it deletes framework-owned entries and prunes the ledger);
//! this owns the untracked remainder retraction structurally cannot reach.
//!
//! The order is a convention, NOT a correctness requirement, and the
//! distinction is measured rather than assumed: swapping the two call sites
//! leaves the whole suite green. They converge because the `Untracked`
//! narrowing refuses every file retraction will delete, so neither operation
//! can reach the other's tier whichever runs first. Keep the narrowing even if
//! the order ever changes — the narrowing is what carries the guarantee.
//!
//! Test: `quarantine_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::agents::manifest::{AgentManifest, ManifestLoad, with_agent_manifest_lock};
use crate::agents::tier_audit::{TierOwnership, TierResidentClass, audit_agent_tier_with_manifest};

/// Suffix appended to a quarantined agent file.
///
/// Why: it must (a) stop the harness resolving the file as an agent and (b)
/// stop [`crate::agents::deployer::is_agent_file`] re-scanning it on the next
/// run, or a second pass would quarantine the quarantine. Appending rather than
/// replacing `.md` keeps the original filename readable in `ls` and makes the
/// undo a literal suffix strip.
/// What: `".disabled"`, appended whole — `qa.md` becomes `qa.md.disabled`.
/// Test: `quarantine_renames_an_untracked_shadowing_file`.
pub const QUARANTINE_SUFFIX: &str = ".disabled";

/// Filename of the human-readable recovery receipt.
///
/// Why: the WARN log this replaces fired on eight separate days through
/// 2026-07-30 with zero operator follow-up, so a log line is not a delivery
/// mechanism. The receipt lands in the directory the operator is already
/// looking at when they wonder where their agent went, and carries the exact
/// command to undo the move.
/// What: a `.txt` file — deliberately not `.md`, so the harness never resolves
/// it as an agent and [`crate::agents::tier_audit`] never classifies it.
/// Test: `quarantine_writes_a_recovery_receipt`,
/// `receipt_is_not_an_agent_file`.
pub const RECEIPT_FILE: &str = "TRUSTY-MPM-QUARANTINE.txt";

/// How many `.disabled.<n>` fallbacks are tried before a file is refused.
const MAX_COLLISION_ATTEMPTS: u32 = 100;

/// A refusal or failure from [`quarantine_shadowing_agents`].
///
/// Why: this operation MOVES operator files, so "could not establish the facts"
/// must be a distinct, loud outcome rather than an empty success — an empty
/// `Ok` would read as "nothing was shadowing" and the shadow would persist
/// unreported. A crate-local `thiserror` enum keeps that distinction typed
/// instead of stringly, and keeps the deployer's [`crate::agents::builder::AgentBuildError`]
/// (whose variants are all about composing agents) from acquiring meanings it
/// has no business carrying.
/// What: [`EmptyRoster`](Self::EmptyRoster) and
/// [`CorruptLedger`](Self::CorruptLedger) are REFUSALS — nothing on disk was
/// touched. [`Io`](Self::Io) is a genuine filesystem failure, which may occur
/// after some files have already been renamed (those are reported in the
/// receipt regardless).
/// Test: `quarantine_refuses_on_empty_roster`,
/// `quarantine_refuses_on_corrupt_manifest`.
#[derive(Debug, Error)]
pub enum QuarantineError {
    /// The caller supplied no bundled names, so nothing can be classified.
    #[error(
        "refusing to quarantine: the bundled agent roster is empty, so no file can be \
         proven to shadow a bundled name. An empty roster means the roster could not be \
         built, never that nothing is bundled — acting on it would sweep every agent in \
         the directory (issue #4448)"
    )]
    EmptyRoster,
    /// The directory's ownership ledger exists but cannot be parsed.
    #[error(
        "refusing to quarantine: this directory's ownership ledger is corrupt, so a \
         user-owned file cannot be told apart from an untracked one and the operator's \
         own agents would be swept. Delete `.trusty-mpm-manifest.json` in this directory \
         by hand to clear it. Detail: {0}"
    )]
    CorruptLedger(String),
    /// A filesystem operation failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// The directory's exclusive ledger lock could not be taken.
    ///
    /// A refusal: the lock guards this sweep against a concurrent retraction
    /// deciding the fate of the same files, so failing to take it means nothing
    /// may move.
    #[error("could not take the agent ledger lock, so nothing was quarantined: {0}")]
    Lock(#[from] crate::agents::manifest::ManifestError),
}

/// One file this run renamed out of the way.
///
/// Why: the caller reports what moved, and recovery needs both endpoints of the
/// rename — the receipt is generated from exactly these fields.
/// What: the path as it was, the path it now has, and the agent name it was
/// shadowing per [`crate::agents::tier_audit::agent_identity`] (which is the
/// frontmatter `name:` when declared, NOT necessarily the filename stem).
/// Test: `quarantine_renames_an_untracked_shadowing_file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedAgent {
    /// Where the file was before this run.
    pub from: PathBuf,
    /// Where the file is now.
    pub to: PathBuf,
    /// The bundled agent name this file was shadowing.
    pub name: String,
}

/// Summary of one [`quarantine_shadowing_agents`] run.
///
/// Why: a caller needs to distinguish "swept N files" from "sweep ran, found
/// nothing" without re-scanning, and needs the refused set to report a partial
/// sweep honestly.
/// What: the files renamed, and the files that CLASSIFIED as movable but were
/// left alone because every candidate destination name was occupied.
/// Test: `quarantine_renames_an_untracked_shadowing_file`,
/// `quarantine_refuses_when_the_destination_is_occupied`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuarantineResult {
    /// Files renamed to `<name>.md.disabled` this run.
    pub quarantined: Vec<QuarantinedAgent>,
    /// Filenames that qualified but could not be given a free destination name.
    pub refused: Vec<String>,
}

impl QuarantineResult {
    /// Whether this run changed anything on disk.
    ///
    /// Why: every caller's first question, and spelling it once keeps the two
    /// call sites from disagreeing about whether `refused` counts as activity.
    /// What: `true` when at least one file was renamed. A non-empty
    /// [`refused`](Self::refused) alone is NOT a change — nothing moved.
    /// Test: `empty_result_reports_no_change`.
    pub fn changed(&self) -> bool {
        !self.quarantined.is_empty()
    }
}

/// Rename untracked files in `dir` that shadow a bundled agent name.
///
/// Why: see the module doc — this is the only code path that can reach a
/// project-tier agent file no ledger names, and those are the files that
/// silently win agent resolution.
///
/// What: takes `dir`'s exclusive ledger lock (the same
/// [`with_agent_manifest_lock`] the deployer and retraction take, because a
/// concurrent retraction on the same directory is deciding the fate of the same
/// files), loads the ledger STRICTLY, classifies every `.md` file with
/// [`audit_agent_tier_with_manifest`], and renames those that are BOTH
/// [`TierResidentClass::ShadowsBundled`] and [`TierOwnership::Untracked`] to
/// `<file>.md.disabled`. The ledger is never written.
///
/// What it REFUSES to move, and why each refusal is load-bearing:
///
/// - `bundled` is empty → [`QuarantineError::EmptyRoster`]. An empty roster
///   makes `ShadowsBundled` unreachable, so a naive run would look like a clean
///   no-op while the shadowing persisted; worse, a future roster-inversion bug
///   would sweep everything. The roster is built by the caller (it unions the
///   on-disk agent source with the binary's embedded bundle), so empty means
///   "could not be built".
/// - The ledger is corrupt → [`QuarantineError::CorruptLedger`], moving
///   nothing. [`crate::agents::tier_audit::audit_agent_tier`] degrades a corrupt
///   ledger to empty, which is correct for a REPORT and catastrophic for a move:
///   every [`TierOwnership::UserOwned`] file would read as `Untracked` and a
///   name collision would then condemn the operator's own agent.
/// - [`TierOwnership::UserOwned`] → never moved, at two independent layers:
///   [`crate::agents::tier_audit::classify_tier_resident`] already returns
///   [`TierResidentClass::Custom`] for it, and the ownership filter here would
///   reject it even if that check regressed.
/// - [`TierResidentClass::Custom`] → never moved. Not tm's file. It never
///   reaches this function: the audit filters it out.
/// - [`TierResidentClass::StrandedFrameworkOwned`] → deliberately OUT OF SCOPE.
///   A stranded file is ledger-tracked and framework-owned, which is precisely
///   what `retract_framework_agents` DELETES on the very same directory at the
///   very same call sites. Quarantining it would duplicate that repair with a
///   weaker one and desynchronise the ledger (which would keep naming a file
///   that no longer exists under that name). Leaving it to retraction keeps one
///   owner per case.
/// - The destination name is occupied → that file is refused and reported in
///   [`QuarantineResult::refused`], never overwritten. `std::fs::rename`
///   clobbers on Unix, so an already-quarantined `qa.md.disabled` would be
///   destroyed by a second sweep; the numbered fallbacks and this refusal make
///   the operation non-destructive by construction.
///
/// A missing `dir` is an empty no-op — checked BEFORE the lock, since
/// [`with_agent_manifest_lock`] would otherwise materialise the directory just
/// to place its sidecar.
///
/// Test: `quarantine_renames_an_untracked_shadowing_file`,
/// `quarantine_never_moves_a_user_owned_file_on_a_bundled_name`,
/// `quarantine_never_moves_an_untracked_custom_agent`,
/// `quarantine_never_moves_a_stranded_framework_file`,
/// `quarantine_refuses_on_corrupt_manifest`, `quarantine_refuses_on_empty_roster`,
/// `quarantine_missing_dir_is_a_noop`, `quarantine_is_idempotent`,
/// `quarantine_follows_the_frontmatter_name_not_the_stem`,
/// `quarantine_spares_a_bundled_stem_declaring_a_custom_name`,
/// `quarantine_leaves_the_ledger_untouched`.
pub fn quarantine_shadowing_agents(
    dir: &Path,
    bundled: &BTreeSet<String>,
) -> Result<QuarantineResult, QuarantineError> {
    // FAIL CLOSED. Checked first and with its own early return so no later edit
    // can add a path that reaches the rename loop with an unusable roster.
    if bundled.is_empty() {
        return Err(QuarantineError::EmptyRoster);
    }
    // A directory that does not exist holds nothing to quarantine. Before the
    // lock: taking it would CREATE the directory for the sidecar.
    if !dir.is_dir() {
        return Ok(QuarantineResult::default());
    }

    with_agent_manifest_lock(dir, || quarantine_locked(dir, bundled))
}

/// The body of [`quarantine_shadowing_agents`], run holding the ledger lock.
///
/// Why/What: mirrors the deployer's `*_locked` split — the critical section is
/// one expression so the lock's scope cannot be misread. Never call it
/// directly; it is unsafe against a concurrent retraction by construction.
/// Test: covered by every `quarantine_*` test through the public wrapper.
fn quarantine_locked(
    dir: &Path,
    bundled: &BTreeSet<String>,
) -> Result<QuarantineResult, QuarantineError> {
    // STRICT load — the opposite policy from the read-only probe's. See the
    // corrupt-ledger bullet on `quarantine_shadowing_agents`.
    let manifest = match AgentManifest::load_checked(dir) {
        ManifestLoad::Ok(m) => m,
        ManifestLoad::Corrupt(detail) => return Err(QuarantineError::CorruptLedger(detail)),
    };

    let mut result = QuarantineResult::default();
    for found in audit_agent_tier_with_manifest(dir, bundled, &manifest) {
        if !is_movable(found.class, found.ownership) {
            continue;
        }
        let Some(to) = free_quarantine_path(&found.path) else {
            // Every candidate name is taken. Refuse — renaming anyway would
            // destroy a file an earlier sweep already quarantined.
            result.refused.push(file_name_of(&found.path));
            continue;
        };
        std::fs::rename(&found.path, &to)?;
        result.quarantined.push(QuarantinedAgent {
            from: found.path,
            to,
            name: found.name,
        });
    }

    if result.changed() {
        write_receipt(dir, &result)?;
        tracing::warn!(
            dir = %dir.display(),
            count = result.quarantined.len(),
            "quarantined project-tier agent file(s) that were shadowing bundled agents — \
             renamed to *.md.disabled, nothing deleted. See {RECEIPT_FILE} in that \
             directory to review or undo (issue #4448)"
        );
    }
    if !result.refused.is_empty() {
        tracing::warn!(
            dir = %dir.display(),
            files = ?result.refused,
            "left shadowing agent file(s) in place: every quarantine destination name is \
             already taken. Clear the existing *.md.disabled files to let the sweep \
             proceed (issue #4448)"
        );
    }
    Ok(result)
}

/// Whether a classified file may be renamed out of the resolution path.
///
/// Why: this is THE predicate that decides whether an operator's file moves, so
/// it is named and given an exhaustive truth table rather than left inline
/// where only the combinations a staged directory happens to produce get
/// tested. Both conjuncts are load-bearing and neither is currently redundant
/// for the reason it looks: today's classifier cannot emit
/// [`TierResidentClass::StrandedFrameworkOwned`] with
/// [`TierOwnership::Untracked`] (stranded IS the framework-owned ledger case),
/// so a filesystem test cannot distinguish "we check the class" from "we only
/// check ownership". The moment `tier_audit` grows the further variant its own
/// doc anticipates — a registry-sourced project agent, say — an
/// ownership-only filter would start sweeping it. The truth table below is what
/// keeps that from being discovered in production.
/// What: `true` only for [`TierResidentClass::ShadowsBundled`] AND
/// [`TierOwnership::Untracked`]. Every other pair is `false`:
/// - user-owned → the ledger PROVES the file is the operator's;
/// - framework-owned → ledger-tracked, hence
///   [`crate::agents::deployer::retract_framework_agents`]'s to delete; moving
///   it here would leave the ledger naming a file that no longer exists;
/// - stranded → the framework-owned case again, by another name;
/// - custom → not tm's file at all (and filtered out before it reaches here).
///
/// Test: `movable_truth_table_is_exhaustive`.
fn is_movable(class: TierResidentClass, ownership: TierOwnership) -> bool {
    class == TierResidentClass::ShadowsBundled && ownership == TierOwnership::Untracked
}

/// The first unused `<path>.disabled[.N]`, or `None` if all are taken.
///
/// Why: `std::fs::rename` silently replaces the destination on Unix, so
/// renaming onto an existing `qa.md.disabled` would DESTROY the file a previous
/// sweep preserved — turning a reversible quarantine into data loss on the
/// second run. Probing first is what makes "nothing is ever deleted" true.
/// What: tries `<path>.disabled`, then `<path>.disabled.1` …
/// `<path>.disabled.{MAX_COLLISION_ATTEMPTS-1}`, returning the first whose
/// [`std::fs::symlink_metadata`] reports `NotFound`. Any OTHER metadata error
/// counts as OCCUPIED, not free — an unreadable destination is exactly when a
/// blind rename is least safe. `symlink_metadata` rather than `Path::exists`
/// because the latter follows links and reports a dangling symlink as absent,
/// while `rename` would happily clobber the link itself.
/// Test: `quarantine_is_idempotent`,
/// `quarantine_refuses_when_the_destination_is_occupied`.
fn free_quarantine_path(path: &Path) -> Option<PathBuf> {
    let base = {
        let mut s = path.as_os_str().to_os_string();
        s.push(QUARANTINE_SUFFIX);
        PathBuf::from(s)
    };
    if is_free(&base) {
        return Some(base);
    }
    (1..MAX_COLLISION_ATTEMPTS).find_map(|n| {
        let candidate = {
            let mut s = base.as_os_str().to_os_string();
            s.push(format!(".{n}"));
            PathBuf::from(s)
        };
        is_free(&candidate).then_some(candidate)
    })
}

/// Whether `path` is safe to rename onto — i.e. provably absent.
fn is_free(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    )
}

/// `path`'s file name, or its full display form if it somehow has none.
fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Quote `s` so a POSIX shell reads it as exactly one literal word.
///
/// Why: the receipt prints an `mv` an operator is invited to paste into a
/// shell, and the paths in it are attacker-influenced — a filename is whatever
/// landed in the directory. Rust's `{:?}` looked like quoting but emits DOUBLE
/// quotes, and a shell still expands `$(…)`, `${…}` and backticks inside those.
/// A file named `evil$(rm -rf ~).md` would then execute on paste. The undo path
/// for a destructive operation is the last place that may be approximately
/// correct.
/// What: wraps `s` in SINGLE quotes, inside which POSIX shells treat every
/// character literally, and rewrites each embedded `'` as `'\''` (close, escape
/// a literal quote, reopen) — the only sequence single quotes cannot contain.
/// A non-UTF-8 path is lossily converted before quoting, so the printed command
/// may not resolve for such a file; that is a legibility limit, never an
/// injection one, since the lossy replacement character is inert.
/// Test: `shell_quote_neutralises_expansion`, `shell_quote_escapes_a_quote`,
/// `receipt_undo_command_survives_a_hostile_filename`.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Append this run's moves to the directory's recovery receipt.
///
/// Why: a rename an operator cannot explain or undo is worse than the shadow it
/// fixed. The receipt sits in the directory they will actually open, states what
/// was moved and why, and gives a copy-pasteable undo — none of which a
/// `tracing::warn!` into a daemon log delivers.
/// What: appends (never truncates — an earlier sweep's record must survive) a
/// timestamped stanza naming every rename and the `mv` that reverses it. Every
/// path is [`shell_quote`]d, never `{:?}`-formatted: Rust's `Debug` produces
/// DOUBLE quotes, inside which a shell still expands `$(…)` and backticks, so a
/// filename could execute code the moment the operator pasted the undo. An undo
/// command for a destructive operation that can run arbitrary code is worse than
/// offering no undo at all.
/// Test: `quarantine_writes_a_recovery_receipt`,
/// `receipt_appends_across_runs`,
/// `receipt_undo_command_survives_a_hostile_filename`.
fn write_receipt(dir: &Path, result: &QuarantineResult) -> std::io::Result<()> {
    use std::io::Write;

    let mut body = String::new();
    body.push_str(&format!(
        "\n=== trusty-mpm agent quarantine — {} ===\n\
         These files resolved to names trusty-mpm ships as bundled agents, and this\n\
         directory outranks the canonical agent tier in Claude Code's resolution — so\n\
         they were silently winning over the real agents. They were RENAMED, not\n\
         deleted, and no file tracked as yours was touched. See\n\
         https://github.com/bobmatnyc/trusty-tools/issues/4448\n\n\
         To restore any of them, run the matching command:\n",
        chrono::Utc::now().to_rfc3339()
    ));
    for moved in &result.quarantined {
        body.push_str(&format!(
            "  # shadowed bundled agent: {}\n  mv {} {}\n",
            // The name is attacker-influenced too (it is frontmatter), and it
            // sits on a `#` comment line where only a newline could escape.
            moved.name.replace(['\n', '\r'], " "),
            shell_quote(&moved.to.to_string_lossy()),
            shell_quote(&moved.from.to_string_lossy()),
        ));
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(RECEIPT_FILE))?;
    file.write_all(body.as_bytes())
}

#[cfg(test)]
#[path = "quarantine_tests.rs"]
mod tests;
