//! The two side files the instruction-compression producer keeps beside the
//! savings ledger (#7245).
//!
//! Why: [`crate::core::savings_instructions::record_instruction_compression`]
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
//!   [`crate::core::savings::append_row`], which stays the ledger's one writer.
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
//! `the_no_fold_warning_is_emitted_at_warn_level`.

use std::path::{Path, PathBuf};

use crate::core::savings::{SavingsRow, append_row};

/// Directory holding staged rows, under the framework root's `usage/`.
const PENDING_DIR: &str = "pending-savings";

/// Directory holding the per-project "nothing folded" warning markers.
const NO_FOLD_WARNED_DIR: &str = "no-fold-warned";

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
        let line = serde_json::to_string(row).ok()?;
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
/// [`append_row`]. The claim file is removed only once the append has landed;
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
    let session_id = claude_session_id.trim();
    if session_id.is_empty() {
        return false;
    }
    let path = pending_row_path_in(root, compiled_prompt);
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
    let mut row: SavingsRow = match serde_json::from_str(&text) {
        Ok(row) => row,
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
    if let Err(source) = append_row(ledger, &row) {
        let restored = std::fs::rename(&claim, &path).is_ok();
        let row_json = serde_json::to_string(&row).unwrap_or(text);
        tracing::warn!(
            ledger = %ledger.display(),
            %source,
            restored,
            row = %row_json,
            "could not append the staged instruction-compression savings row"
        );
        return false;
    }
    let _ = std::fs::remove_file(&claim);
    true
}

/// [`emit_staged_row`] against the ambient framework root, working directory
/// and managed session scope.
///
/// Why: the `tm hook` handler knows only the Claude session id Claude Code sent
/// it on stdin. Every other input — which ledger, which project, which session
/// scope — is ambient, and resolving it here keeps the handler's addition to
/// one call. The framework root is
/// [`crate::core::paths::FrameworkPaths::default`], the same root the producer
/// staged under, so the two cannot disagree about where the file is.
/// What: rebuilds the compiled-prompt path from the session scope
/// ([`crate::core::harness_root::session_scope`], i.e. `TM_MANAGED_SESSION_ID`
/// or `local`) and tries it against two project directories — the checkout that
/// owns this working directory's harness state, then the working directory
/// itself, which differ when the session runs in a worktree. Returns whether a
/// row was appended.
/// Test: `a_staged_row_emits_once_under_the_claude_session_id` covers the
/// resolved-path form this delegates to; the ambient reads themselves are
/// exercised by the `tm hook` SessionStart path.
pub fn emit_staged_row_for_session(claude_session_id: &str) -> bool {
    let root = crate::core::paths::FrameworkPaths::default().root;
    let ledger = crate::core::savings::savings_log_in(&root);
    let scope = crate::core::harness_root::session_scope(None);
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    let owner = crate::core::harness_root::harness_root(&cwd);
    let mut candidates = vec![owner.clone()];
    if owner != cwd {
        candidates.push(cwd);
    }
    candidates.iter().any(|project_dir| {
        let compiled = crate::core::instruction_pipeline::compiled_prompt_path(project_dir, &scope);
        emit_staged_row(&ledger, &root, &compiled, claude_session_id)
    })
}

/// Where the "nothing folded" warning marker for `project_dir` lives.
///
/// What: `<root>/usage/no-fold-warned/<digest>`.
/// Test: `the_no_fold_warning_fires_once_per_project`.
fn no_fold_marker_path(root: &Path, project_dir: &Path) -> PathBuf {
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
