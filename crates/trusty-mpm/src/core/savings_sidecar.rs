//! The two side files the instruction-compression producer keeps beside the
//! savings ledger (#7245).
//!
//! Why: [`crate::core::savings_instructions::record_instruction_compression_in`]
//! runs in the `tm` process that compiles the prompt, BEFORE `claude` is
//! spawned. The id the `💸` statusline segment folds by does not exist yet —
//! Claude Code exports `CLAUDE_CODE_SESSION_ID` into its own children only. So
//! #7209's fix, which keys a row by that variable when it is set, repaired
//! `tm divert` (a hook, and therefore a child of Claude Code) and could never
//! repair this producer: the variable is always absent at compile time, and the
//! directory-derived fallback key it drops to is one the statusline never
//! matches. This module carries the row across that process boundary, and holds
//! the marker that stops the "nothing folded" decline logging on every launch.
//!
//! What: two files under `<framework-root>/usage/`.
//!
//! - `pending-savings/<key>.json` — one staged
//!   [`crate::core::savings::SavingsRow`], written by the compiling process
//!   because it cannot key it. [`emit_staged_row`] claims it and appends it
//!   under the Claude session id through
//!   [`crate::core::savings::append_row_once`], which stays the ledger's one
//!   writer for this technique and, since #7658, appends only a measurement the
//!   ledger does not already carry.
//!   The claim is an atomic rename onto a per-attempt path, so of two racing
//!   hook processes only the one whose rename succeeds appends. The claim file
//!   is deleted once the append has landed, and renamed back to the staging
//!   path when the append fails: the row lands exactly once, and a failed
//!   append loses nothing.
//! - `no-fold-warned/<key>` — the byte pair the last "nothing folded" warning
//!   named. A project whose fold is steady warns once; a project whose numbers
//!   move warns again with the new pair.
//!
//! `<key>` digests the compiled prompt's own path, which both processes can
//! compute: the compiling one holds it already, and the hook rebuilds it from
//! its working directory and `TM_MANAGED_SESSION_ID`. Digesting the path rather
//! than naming the file after the session scope is what stops two projects'
//! unmanaged launches — which share the single `local` scope — from claiming
//! each other's rows.
//!
//! The trade #7245 makes: a compile that no `tm hook` invocation ever follows
//! writes no row at all, where before it wrote one under a key nothing could
//! fold. The staged file survives, so a later hook in the same scope still
//! emits it; what is lost until then is that row's contribution to the
//! machine-wide [`crate::core::savings::fold_all`] — a contribution no
//! per-session surface could ever have read.
//!
//! **Why "in the same scope" was not good enough (#7411).** The compiling
//! process exports `TM_MANAGED_SESSION_ID`; the hook Claude Code spawns does
//! not. The two therefore derive different compiled-prompt paths, different
//! digests, and different staging file names, and the claim — which only ever
//! tried the one digest the hook could rebuild — matched nothing. Rows sat in
//! `pending-savings/` indefinitely and the `💸` segment under-reported by
//! exactly them. Three changes close it:
//!
//! - the staged file records the compiled prompt it measures, so a claimer that
//!   derives a different path can still tell what the file is for;
//! - [`sweep_pending_rows`] walks the whole directory instead of probing one
//!   name, claiming every row whose compiled prompt still exists and deleting —
//!   with the measurement logged — only the ones whose prompt is gone and which
//!   have passed [`STRANDED_AFTER`];
//! - a hook that finds nothing staged re-measures the fold from the compiled
//!   prompt on disk, so the ledger is not left empty for the session.
//!
//! Everything here is best-effort, like the producer it serves: an unwritable
//! directory, an unparseable staged row, or a lost claim each skip silently. A
//! missing savings row must never cost a session its launch.
//!
//! Test: the inline suite in `savings_sidecar_tests.rs` —
//! `a_staged_row_emits_once_under_the_claude_session_id`,
//! `emitting_without_a_staged_row_writes_nothing`,
//! `emitting_without_a_session_id_leaves_the_row_staged`,
//! `a_failed_append_leaves_the_row_staged_for_the_next_hook`,
//! `two_racing_claims_append_exactly_one_row`,
//! `two_compiled_prompts_stage_to_different_files`,
//! `the_no_fold_warning_fires_once_per_project`,
//! `the_no_fold_warning_fires_again_when_the_byte_pair_moves`,
//! `the_no_fold_warning_is_emitted_at_warn_level`,
//! `a_staged_row_remembers_its_compiled_prompt` — plus the #7411 suite in
//! `savings_sidecar_sweep_tests.rs`:
//! `a_stranded_row_for_another_project_is_adopted_rather_than_stranded`,
//! `a_second_session_start_does_not_append_a_second_rederived_row`,
//! `the_sweep_claims_a_row_staged_under_another_session_scope`,
//! `a_hook_with_nothing_staged_rederives_from_the_compiled_prompt`,
//! `a_stranded_orphan_is_discarded_with_its_measurement_logged`,
//! `a_fresh_row_for_another_project_is_left_for_its_own_hook`,
//! `a_row_staged_without_a_path_is_still_claimed_by_digest`,
//! `two_racing_sweeps_append_exactly_one_row`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::core::savings::{AppendOnce, SavingsRow};

/// Directory holding staged rows, under the framework root's `usage/`.
const PENDING_DIR: &str = "pending-savings";

/// How long a staged row whose compiled prompt is gone waits before the next
/// hook discards it (#7411).
///
/// Why: two failure shapes need separating, and only time separates them. A
/// staged row is normally claimed seconds later — the compile stages it and the
/// launch it is part of raises `SessionStart` immediately — so anything still
/// unclaimed after half a day belongs to a launch that never happened, or to a
/// compiled prompt that has since been deleted. Twelve hours is long enough to
/// cover a machine suspended mid-launch and an overnight gap between a compile
/// and the session it was for, and short enough that the ledger does not carry
/// a growing tail of files nothing will ever attribute. Days would defeat the
/// point: the whole defect is a file that outlives every hook that could have
/// claimed it.
///
/// It never causes a row to be LOST while its compiled prompt still exists —
/// such a row is claimed at any age. Age only decides two things: when an
/// orphan is discarded, and when a row staged for another project's compiled
/// prompt may be adopted rather than left forever.
const STRANDED_AFTER: Duration = Duration::from_secs(12 * 60 * 60);

/// Directory holding the per-project "nothing folded" warning markers.
///
/// #7569: `crate::core::savings_repair::MARKER_DIR` aliases this rather than
/// re-spelling it, so a rename here cannot leave the repair sweeping a
/// directory nothing writes.
pub(crate) const NO_FOLD_WARNED_DIR: &str = "no-fold-warned";

/// FNV-1a (64-bit) over `bytes`.
///
/// Why: the file name has to be derivable from a path by two independent
/// processes and safe to write, which a raw path is not. FNV-1a is the same
/// scheme [`crate::core::policy_labels`] already uses for the same reason — a
/// short, allocation-free, dependency-free digest for a name, never a security
/// claim.
/// What: the standard 64-bit FNV-1a offset basis and prime.
/// Test: `two_compiled_prompts_stage_to_different_files`.
fn digest(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The `usage/<dir>/<digest of path>` file name both processes agree on.
///
/// Why: one derivation for both side files, so a rename of the scheme cannot
/// desynchronise the stager from the claimer.
/// What: `<root>/usage/<dir>/<16 hex digits><suffix>`.
/// Test: `two_compiled_prompts_stage_to_different_files`.
fn keyed_path(root: &Path, dir: &str, keyed_on: &Path, suffix: &str) -> PathBuf {
    let key = digest(keyed_on.as_os_str().as_encoded_bytes());
    root.join("usage")
        .join(dir)
        .join(format!("{key:016x}{suffix}"))
}

/// Where the row staged for `compiled_prompt` lives.
///
/// What: `<root>/usage/pending-savings/<digest>.json`.
/// Test: `a_staged_row_emits_once_under_the_claude_session_id`.
pub fn pending_row_path_in(root: &Path, compiled_prompt: &Path) -> PathBuf {
    keyed_path(root, PENDING_DIR, compiled_prompt, ".json")
}

/// A staged row plus the compiled prompt it measures (#7411).
///
/// Why: the file name is a digest, which is one-way — a sweeping hook can see
/// that a staged file exists and cannot tell which compiled prompt it belongs
/// to, whether that prompt still exists, or whether it is this project's. That
/// is the whole defect: the claim only ever matched a path the hook happened to
/// reconstruct identically, and a row staged under a managed session scope the
/// hook process does not export sat unclaimed forever.
/// What: `#[serde(flatten)]` keeps the on-disk object exactly the row's own
/// fields plus `compiled_prompt`, so a file staged before this field existed
/// still parses (the path reads back empty and the sweep falls back to matching
/// the digest), and a file staged now still deserialises as a bare
/// [`SavingsRow`] for anything that reads it that way.
/// Test: `a_staged_row_remembers_its_compiled_prompt`,
/// `a_row_staged_without_a_path_is_still_claimed_by_digest`.
#[derive(serde::Serialize, serde::Deserialize)]
struct StagedRow {
    /// The compiled prompt whose fold this row measures; empty in a file
    /// staged before #7411.
    #[serde(default)]
    compiled_prompt: String,
    /// The row itself, inlined as the object's own fields.
    #[serde(flatten)]
    row: SavingsRow,
}

/// Stage `row` for the hook that will learn this session's Claude id.
///
/// Why: see the module header — the compiling process has the measurement and
/// not the key, and the hook has the key and not the measurement.
/// What: writes `row` as one JSON object through a temp file renamed into
/// place, so a hook reading concurrently sees either no file or a whole one.
/// Overwrites any row staged by an earlier compile of the same prompt: one
/// launch, one row. Silent on every failure.
/// Test: `a_staged_row_emits_once_under_the_claude_session_id`,
/// `two_compiled_prompts_stage_to_different_files`.
pub fn stage_row(root: &Path, compiled_prompt: &Path, row: &SavingsRow) {
    use std::io::Write as _;

    let path = pending_row_path_in(root, compiled_prompt);
    let staged = (|| -> Option<()> {
        let dir = path.parent()?;
        std::fs::create_dir_all(dir).ok()?;
        // #7411: record which compiled prompt this measures, so a hook that
        // reconstructs a different path can still claim it and a sweep can tell
        // an orphan from a row still waiting for its own launch.
        let line = serde_json::to_string(&StagedRow {
            compiled_prompt: compiled_prompt.to_string_lossy().into_owned(),
            row: row.clone(),
        })
        .ok()?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir).ok()?;
        tmp.write_all(line.as_bytes()).ok()?;
        // On failure `PersistError::Drop` removes the temp file.
        tmp.persist(&path).ok()?;
        Some(())
    })();
    if staged.is_none() {
        tracing::warn!(
            path = %path.display(),
            "could not stage the instruction-compression savings row; the 💸 \
             segment will have nothing to fold for this session"
        );
    }
}

/// A claim path no other attempt can pick, beside the staging path.
///
/// Why: the claim has to be a rename, and a rename needs a destination that no
/// concurrent attempt — in this process or another — is also renaming onto.
/// What: the staging path plus this process's id and a per-attempt counter, so
/// the name is unique across processes and across threads within one.
/// Test: `two_racing_claims_append_exactly_one_row`.
fn claim_path_for(staged: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static ATTEMPT: AtomicU64 = AtomicU64::new(0);
    let attempt = ATTEMPT.fetch_add(1, Ordering::Relaxed);
    let mut name = staged.as_os_str().to_os_string();
    name.push(format!(".claim-{}-{attempt:x}", std::process::id()));
    PathBuf::from(name)
}

/// Claim the row staged for `compiled_prompt` and append it under
/// `claude_session_id`.
///
/// Why: this is the whole point of the staging file — the first process that
/// knows the id the statusline folds by writes the row the compiling process
/// could not. Running this on every hook event, or from two hooks at once, must
/// still put exactly one row in the ledger and must never destroy a row it
/// failed to append.
/// What: renames the staged file onto a per-attempt claim path — the rename is
/// the claim, and the racer whose rename fails reads nothing and appends
/// nothing — then replaces the row's `session_id` and appends through
/// [`crate::core::savings::append_row_once`], which since #7658 appends only a
/// measurement the ledger does not already carry. The claim file is removed once
/// the append has landed — or once the ledger is found to hold the row already;
/// an append that fails goes back to the staging path for the next hook, and
/// the warning carries the row's JSON so the measurement survives in the log
/// even if the rename back also fails. Returns whether a row was appended:
/// `false` for a blank id, no staged file, a lost claim race, an unparseable
/// staged row, or a failed append — and in every one of those cases nothing is
/// written to the ledger.
/// Test: `a_staged_row_emits_once_under_the_claude_session_id`,
/// `emitting_without_a_staged_row_writes_nothing`,
/// `emitting_without_a_session_id_leaves_the_row_staged`,
/// `a_failed_append_leaves_the_row_staged_for_the_next_hook`,
/// `two_racing_claims_append_exactly_one_row`.
pub fn emit_staged_row(
    ledger: &Path,
    root: &Path,
    compiled_prompt: &Path,
    claude_session_id: &str,
) -> bool {
    claim_staged_row(
        ledger,
        &pending_row_path_in(root, compiled_prompt),
        claude_session_id,
    )
}

/// Claim the staged row at `path` and append it under `claude_session_id`.
///
/// Why (#7411): the claim used to be reachable only through the digest of a
/// compiled-prompt path the caller had reconstructed, which is exactly the
/// reconstruction that fails when the hook resolves a different session scope
/// than the compile did. The sweep needs to claim a file it found by reading
/// the directory, so the atomic rename lives here — one claim primitive, still
/// the only writer, so two hooks racing on one staged file still produce one
/// ledger row.
/// What: see [`emit_staged_row`], which is this against a digested path. The
/// append goes through [`crate::core::savings::append_row_once`] since #7658, so
/// a claim of a measurement the ledger already carries deletes the staged file
/// and reports `false` — the row is accounted for, and leaving the file would
/// only have it claimed again on the next hook. A ledger that cannot be READ
/// leaves the row staged, exactly as a failed append does.
/// Test: `two_racing_claims_append_exactly_one_row`,
/// `a_failed_append_leaves_the_row_staged_for_the_next_hook`,
/// `n_hooks_sweeping_a_restaged_row_append_exactly_one`,
/// `an_unreadable_ledger_leaves_the_staged_row_alone`.
fn claim_staged_row(ledger: &Path, path: &Path, claude_session_id: &str) -> bool {
    let session_id = claude_session_id.trim();
    if session_id.is_empty() {
        return false;
    }
    let path = path.to_path_buf();
    // #7245: the rename IS the claim. A second hook — or a concurrent one —
    // finds nothing to rename and appends nothing, which is what makes this
    // idempotent, and the claimed file still exists until the append lands.
    let claim = claim_path_for(&path);
    if std::fs::rename(&path, &claim).is_err() {
        return false;
    }
    let text = match std::fs::read_to_string(&claim) {
        Ok(text) => text,
        Err(source) => {
            let restored = std::fs::rename(&claim, &path).is_ok();
            tracing::warn!(
                path = %path.display(),
                %source,
                restored,
                "could not read the claimed staged savings row"
            );
            return false;
        }
    };
    let mut row: SavingsRow = match serde_json::from_str::<StagedRow>(&text) {
        Ok(staged) => staged.row,
        Err(source) => {
            tracing::warn!(
                path = %path.display(),
                %source,
                "discarding an unparseable staged savings row"
            );
            let _ = std::fs::remove_file(&claim);
            return false;
        }
    };
    row.session_id = session_id.to_string();
    // #7658: the sweep re-runs on every hook, and a producer that re-stages
    // hands it the same measurement again. `append_row_once` is what makes a
    // second claim of an identical row a no-op instead of a second ledger line.
    match crate::core::savings::append_row_once(ledger, &row) {
        Ok(AppendOnce::Appended) => {
            let _ = std::fs::remove_file(&claim);
            true
        }
        // The measurement is already on the ledger, so this staged copy is
        // redundant: drop it rather than leave it to be claimed again forever.
        Ok(AppendOnce::AlreadyPresent) => {
            let _ = std::fs::remove_file(&claim);
            false
        }
        // #7658: nothing was written and nothing is known, so the staged row
        // goes back for a hook that can read the ledger — the same treatment a
        // failed append gets, for the same reason.
        Ok(AppendOnce::LedgerUnreadable) => {
            let restored = std::fs::rename(&claim, &path).is_ok();
            tracing::warn!(
                ledger = %ledger.display(),
                restored,
                "left the staged instruction-compression savings row unwritten because \
                 the ledger could not be read"
            );
            false
        }
        Err(source) => {
            let restored = std::fs::rename(&claim, &path).is_ok();
            let row_json = serde_json::to_string(&row).unwrap_or(text);
            tracing::warn!(
                ledger = %ledger.display(),
                %source,
                restored,
                row = %row_json,
                "could not append the staged instruction-compression savings row"
            );
            false
        }
    }
}

/// Every compiled prompt that exists under `project_dir`, newest first.
///
/// Why (#7411): the hook cannot reconstruct the session scope the compiling
/// process used. `TM_MANAGED_SESSION_ID` is set for the compile and absent in
/// the hook Claude Code spawns, so the two derive different paths, different
/// digests, and different staging file names — which is how a row is staged
/// that no hook ever claims. Reading the sessions directory answers the same
/// question without guessing at the scope, and a file found this way is by
/// construction a compiled prompt that still exists.
/// What: the `INSTRUCTIONS-COMPILED.md` under each
/// `<project>/.trusty-mpm/sessions/<scope>/`, ordered by modification time with
/// the most recent first, so a caller wanting this launch's prompt takes the
/// head. Empty when the directory is absent.
/// Test: `the_sweep_claims_a_row_staged_under_another_session_scope`,
/// `a_hook_with_nothing_staged_rederives_from_the_compiled_prompt`.
///
/// `pub(crate)` since #7616: the `instruction_compression` `tm doctor` check
/// measures the same prompt this sweep does, and a second copy of the lookup
/// would let the check and the producer disagree about which file is current.
pub(crate) fn compiled_prompts_in(project_dir: &Path) -> Vec<PathBuf> {
    let sessions = crate::core::harness_root::harness_dir(project_dir)
        .join(crate::core::harness_root::SESSIONS_DIR);
    let Ok(entries) = std::fs::read_dir(&sessions) else {
        return Vec::new();
    };
    let mut found: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .map(|entry| {
            entry
                .path()
                .join(crate::core::instruction_pipeline::COMPILED_PROMPT_FILE)
        })
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    found.sort_by_key(|left| std::cmp::Reverse(left.0));
    found.into_iter().map(|(_, path)| path).collect()
}

/// How long ago `path` was last written.
///
/// Why: the staging file's own modification time is the moment the compile
/// staged it, which is the age the sweep's discard bound is measured against.
/// Reading it rather than the row's `ts` field means an unparseable file — the
/// one shape with no readable timestamp — still ages out.
/// What: `None` when the file is gone or the clock ran backwards, which both
/// read as "not stranded" and leave the file alone.
/// Test: `a_stranded_orphan_is_discarded_with_its_measurement_logged`.
fn age_of(path: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    SystemTime::now().duration_since(modified).ok()
}

/// The compiled prompt the file staged at `staged` measures, if it still
/// exists.
///
/// Why (#7411): the sweep's three-way decision — claim, leave, discard — turns
/// entirely on this. A staged row whose prompt exists is live and must never be
/// dropped; one whose prompt is gone can never be attributed to a session and
/// is the only thing the sweep is allowed to discard.
/// What: the path the file recorded, when that file is still there. A file
/// staged before #7411 records none, so it falls back to the digest match the
/// pre-#7411 claim used — no row is lost across the upgrade.
/// Test: `a_row_staged_without_a_path_is_still_claimed_by_digest`,
/// `a_stranded_orphan_is_discarded_with_its_measurement_logged`.
fn live_compiled_prompt(
    root: &Path,
    staged: &Path,
    project_prompts: &[PathBuf],
) -> Option<PathBuf> {
    let recorded = std::fs::read_to_string(staged)
        .ok()
        .and_then(|text| serde_json::from_str::<StagedRow>(&text).ok())
        .map(|parsed| parsed.compiled_prompt)
        .filter(|recorded| !recorded.is_empty())
        .map(PathBuf::from);
    if let Some(recorded) = recorded {
        return recorded.is_file().then_some(recorded);
    }
    project_prompts
        .iter()
        .find(|compiled| pending_row_path_in(root, compiled) == staged)
        .cloned()
}

/// Claim, discard, or leave every row staged under `root`, on behalf of
/// `project_dir`.
///
/// Why (#7411): claiming only the one digest the hook could reconstruct left
/// every other staged row on disk forever, under-reporting the `💸` segment by
/// exactly the rows it stranded. A sweep reaches them all, and gives each file
/// one of two terminal outcomes — appended to the ledger, or deleted with its
/// measurement in the log — so no file can outlive the hooks that could have
/// resolved it.
/// What: for each `*.json` under `usage/pending-savings/`, resolves the
/// compiled prompt it measures and then:
///
/// - prompt exists and is this project's → claim it, at any age;
/// - prompt exists and is another project's → leave it, until it passes
///   [`STRANDED_AFTER`], after which this hook adopts it rather than let it sit
///   forever;
/// - prompt is gone (or the file is unreadable) and it has passed
///   [`STRANDED_AFTER`] → delete it and log the row's JSON at `warn`;
/// - otherwise → leave it for a hook that can do better.
///
/// Claiming goes through [`claim_staged_row`], so the rename-then-append still
/// makes two racing sweeps append exactly one row. Returns how many rows
/// reached the ledger.
/// Test: `the_sweep_claims_a_row_staged_under_another_session_scope`,
/// `a_stranded_orphan_is_discarded_with_its_measurement_logged`,
/// `a_fresh_row_for_another_project_is_left_for_its_own_hook`,
/// `a_stranded_row_for_another_project_is_adopted_rather_than_stranded`,
/// `a_row_staged_without_a_path_is_still_claimed_by_digest`,
/// `two_racing_sweeps_append_exactly_one_row`.
pub fn sweep_pending_rows(
    ledger: &Path,
    root: &Path,
    project_dir: &Path,
    claude_session_id: &str,
) -> usize {
    let Ok(entries) = std::fs::read_dir(root.join("usage").join(PENDING_DIR)) else {
        return 0;
    };
    let prompts = compiled_prompts_in(project_dir);
    let harness = crate::core::harness_root::harness_dir(project_dir);
    let mut appended = 0;
    for staged in entries.flatten().map(|entry| entry.path()) {
        // Skip the `.claim-<pid>-<n>` files a concurrent claim is holding.
        if staged.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let stranded = age_of(&staged).is_some_and(|age| age >= STRANDED_AFTER);
        match live_compiled_prompt(root, &staged, &prompts) {
            Some(compiled) if compiled.starts_with(&harness) || stranded => {
                if claim_staged_row(ledger, &staged, claude_session_id) {
                    appended += 1;
                }
            }
            Some(_) => {}
            None if stranded => {
                let row = std::fs::read_to_string(&staged).unwrap_or_default();
                if std::fs::remove_file(&staged).is_ok() {
                    tracing::warn!(
                        path = %staged.display(),
                        row = %row.trim(),
                        stranded_hours = STRANDED_AFTER.as_secs() / 3600,
                        "discarding a staged instruction-compression savings row whose \
                         compiled prompt no longer exists; no session can be attributed \
                         it, and this line preserves the measurement"
                    );
                }
            }
            None => {}
        }
    }
    appended
}

/// Put an instruction-compression row in the ledger for `claude_session_id`,
/// from whatever this machine has on disk.
///
/// Why: the `tm hook` handler knows only the Claude session id Claude Code sent
/// it on stdin. Every other input — which ledger, which project, which compiled
/// prompt — is ambient, and resolving it here keeps the handler's addition to
/// one call. The framework root is
/// [`crate::core::paths::FrameworkPaths::default`], the same root the producer
/// staged under, so the two cannot disagree about where the files are.
/// What: sweeps the staged rows ([`sweep_pending_rows`]) for two project
/// directories — the checkout that owns this working directory's harness state,
/// then the working directory itself, which differ when the session runs in a
/// worktree. When that appends nothing and the ledger holds no
/// instruction-compression row for this session yet, re-measures the fold from
/// the newest compiled prompt on disk (#7411), so a session whose staged row
/// was never written still gets its figure. Returns whether a row was appended.
/// Test: `the_sweep_claims_a_row_staged_under_another_session_scope` and
/// `a_hook_with_nothing_staged_rederives_from_the_compiled_prompt` cover the
/// two resolved-path forms this delegates to; the ambient reads themselves are
/// exercised by the `tm hook` SessionStart path.
pub fn emit_staged_row_for_session(claude_session_id: &str) -> bool {
    let root = crate::core::paths::FrameworkPaths::default().root;
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    emit_staged_row_for_session_in(&root, &cwd, claude_session_id)
}

/// [`emit_staged_row_for_session`] against an explicit framework root and
/// working directory.
///
/// Why: the two ambient reads the entry point makes — the framework root under
/// the operator's home and the process working directory — are the whole reason
/// the sweep and the re-derivation were untestable in place. Passing them in
/// keeps every test on a tempdir with no process-env or working-directory
/// mutation, which parallel test binaries require. Same split as
/// [`crate::core::harness_root::session_scope_from`] and
/// `savings_instructions::record_instruction_compression_to`.
/// What: see [`emit_staged_row_for_session`]; this is its body.
/// Test: `the_sweep_claims_a_row_staged_under_another_session_scope`,
/// `a_hook_with_nothing_staged_rederives_from_the_compiled_prompt`,
/// `a_second_session_start_does_not_append_a_second_rederived_row`.
pub fn emit_staged_row_for_session_in(root: &Path, cwd: &Path, claude_session_id: &str) -> bool {
    let session_id = claude_session_id.trim();
    if session_id.is_empty() {
        return false;
    }
    let ledger = crate::core::savings::savings_log_in(root);
    let cwd = cwd.to_path_buf();
    let owner = crate::core::harness_root::harness_root(&cwd);
    let mut projects = vec![owner.clone()];
    if owner != cwd {
        projects.push(cwd);
    }
    let appended: usize = projects
        .iter()
        .map(|project| sweep_pending_rows(&ledger, root, project, session_id))
        .sum();
    if appended > 0 {
        return true;
    }
    // #7411: nothing was staged for this session, so re-measure the fold from
    // the compiled prompt this launch is actually running on. Guarded by the
    // ledger read because a Claude session raises SessionStart again on every
    // resume and compact.
    if crate::core::savings::has_row(
        &ledger,
        session_id,
        crate::core::savings::TECHNIQUE_INSTRUCTION_COMPRESSION,
    ) {
        return false;
    }
    projects
        .iter()
        .filter_map(|project| compiled_prompts_in(project).into_iter().next())
        .any(|compiled| {
            crate::core::savings_instructions::rederive_from_compiled_prompt(
                root, &compiled, session_id,
            )
        })
}

/// Where the "nothing folded" warning marker for `project_dir` lives.
///
/// What: `<root>/usage/no-fold-warned/<digest>`.
/// Test: `the_no_fold_warning_fires_once_per_project`,
/// `the_repair_sweeps_the_directory_the_producer_writes`.
pub(crate) fn no_fold_marker_path(root: &Path, project_dir: &Path) -> PathBuf {
    keyed_path(root, NO_FOLD_WARNED_DIR, project_dir, "")
}

/// Warn — once per project — that the compiled prompt was not smaller than the
/// sources that fed it, so no savings row can be written.
///
/// Why (#7245): this decline is the reason the `💸` segment is structurally
/// absent for a project that overrides no instruction section, and at `debug!`
/// it was invisible: an operator saw a missing segment with nothing anywhere
/// saying why. It cannot log on every launch either — the condition is
/// permanent for such a project, and a warning that repeats every session is
/// one an operator learns to skip. So it names both byte counts and the reason
/// once, and repeats only when the numbers move.
/// What: compares the `<source> <compiled>` pair against the marker file for
/// `project_dir`; when it differs (or no marker exists) emits one `warn!` and
/// records the pair. Returns whether it warned. A marker that cannot be written
/// still warns — an operator seeing the message twice is strictly better than
/// never seeing it.
/// Test: `the_no_fold_warning_fires_once_per_project`,
/// `the_no_fold_warning_fires_again_when_the_byte_pair_moves`,
/// `the_no_fold_warning_is_emitted_at_warn_level`.
pub fn warn_no_fold_once(
    root: &Path,
    project_dir: &Path,
    source_bytes: usize,
    compiled_bytes: usize,
) -> bool {
    let path = no_fold_marker_path(root, project_dir);
    let stamp = format!("{source_bytes} {compiled_bytes}");
    if std::fs::read_to_string(&path).is_ok_and(|seen| seen.trim() == stamp) {
        return false;
    }
    tracing::warn!(
        project = %project_dir.display(),
        source_bytes,
        compiled_bytes,
        "the compiled prompt is not smaller than the instruction sources it was \
         built from, so no instruction-compression savings row is written and \
         the 💸 statusline segment stays absent for this project; this project \
         configures no CLAUDE.md section overrides, and only an override folds \
         a bundled section away"
    );
    let _ = (|| -> Option<()> {
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, &stamp).ok()
    })();
    true
}

#[cfg(test)]
#[path = "savings_sidecar_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "savings_sidecar_sweep_tests.rs"]
mod sweep_tests;
