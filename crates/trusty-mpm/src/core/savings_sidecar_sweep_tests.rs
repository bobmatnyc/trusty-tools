//! Tests for the staged-row sweep that stops a savings row being stranded
//! (#7411).
//!
//! Why: #7245's claim only ever probed the one staging file name the hook could
//! rebuild, and the compiling process and the hook do not agree on that name —
//! the compile exports `TM_MANAGED_SESSION_ID` and the hook Claude Code spawns
//! does not, so they derive different session scopes, different compiled-prompt
//! paths and different digests. Rows sat in `pending-savings/` forever and the
//! `💸` segment under-reported by exactly them. The properties below are the
//! ones a probe-one-name implementation gets wrong.
//! What: drives the sweep and the ambient emit against temp directories, with
//! no process-environment or working-directory mutation — every input the
//! production path reads from the ambient is passed in.
//! Test: this file.

use super::*;

use std::sync::{Arc, Mutex};

/// A row with a known, foldable measurement.
///
/// What: `session_id` is the compile-time placeholder the claim replaces.
fn a_row(session_id: &str) -> SavingsRow {
    SavingsRow {
        ts: "2026-09-10T21:08:49Z".to_string(),
        session_id: session_id.to_string(),
        technique: crate::core::savings::TECHNIQUE_INSTRUCTION_COMPRESSION.to_string(),
        tokens_saved: 6_699,
        tokens_before: 6_702,
        cost_saved_usd: 0.020_097,
        basis: "hand-built".to_string(),
        model_source: crate::core::session_model::MODEL_SOURCE_LAUNCH_CONFIG.to_string(),
    }
}

/// A project directory holding a compiled prompt at `scope`, written to disk.
///
/// What: returns the compiled prompt's path. The body is far smaller than the
/// bundled source set so the producer's fold measurement comes out positive when
/// a test re-derives, and — since #7491 — at least
/// [`crate::core::savings_instructions::min_plausible_compiled_bytes`], below
/// which the producer treats the file as a stub and writes no row at all.
fn project_with_compiled_prompt(under: &Path, name: &str, scope: &str) -> PathBuf {
    let project = under.join(name);
    let compiled = crate::core::instruction_pipeline::compiled_prompt_path(&project, scope);
    std::fs::create_dir_all(compiled.parent().expect("session dir")).expect("session dir");
    let body = "x".repeat(crate::core::savings_instructions::min_plausible_compiled_bytes().max(1));
    std::fs::write(&compiled, &body).expect("compiled prompt");
    compiled
}

/// How many rows the ledger holds.
fn rows_in(ledger: &Path) -> usize {
    std::fs::read_to_string(ledger)
        .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
        .unwrap_or(0)
}

/// Move `path`'s modification time `hours` into the past.
///
/// Why: the sweep ages a staged file by its mtime, so a test of the discard
/// bound has to be able to write an old file. Backdating beats waiting.
fn backdate(path: &Path, hours: u64) {
    let when = SystemTime::now() - Duration::from_secs(hours * 60 * 60);
    std::fs::File::options()
        .write(true)
        .open(path)
        .expect("staged file")
        .set_modified(when)
        .expect("backdate the staged file");
}

/// Why (#7411): this is the live defect. The compile stages under the managed
/// session scope it exports; the hook resolves `local` because Claude Code does
/// not pass that variable on, probes one name, finds nothing, and the row is
/// stranded. The hook must claim the row that is actually there.
/// Test: itself.
#[test]
fn the_sweep_claims_a_row_staged_under_another_session_scope() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path().join("framework");
    let compiled = project_with_compiled_prompt(tmp.path(), "project", "managed-7411");
    let project = tmp.path().join("project");
    let ledger = crate::core::savings::savings_log_in(&root);

    stage_row(&root, &compiled, &a_row("managed-7411"));

    assert!(
        emit_staged_row_for_session_in(&root, &project, "claude-1"),
        "a hook that resolves a different session scope must still claim the staged row"
    );
    assert!(
        !crate::core::savings::fold_session(&ledger, "claude-1").is_zero(),
        "the statusline fold under the Claude session id must find the row"
    );
    assert!(
        !pending_row_path_in(&root, &compiled).exists(),
        "a claimed row must not stay staged"
    );

    assert!(
        !emit_staged_row_for_session_in(&root, &project, "claude-1"),
        "a second SessionStart must append nothing"
    );
    assert_eq!(rows_in(&ledger), 1, "the row must land exactly once");
}

/// Why (#7658): THE live regression — 312 byte-identical
/// `instruction-compression` rows for one Claude session. The sweep runs before
/// the re-derivation's `has_row` guard and consults nothing itself, so a
/// producer that re-stages the same measurement got it appended on every hook.
/// The row's identity, not the staging file's presence, is what must bound the
/// ledger.
/// Test: itself.
#[test]
fn n_hooks_sweeping_a_restaged_row_append_exactly_one() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path().join("framework");
    let compiled = project_with_compiled_prompt(tmp.path(), "project", "managed-7658");
    let project = tmp.path().join("project");
    let ledger = crate::core::savings::savings_log_in(&root);

    // 26 hooks, each preceded by a re-stage — the shape the live ledger shows,
    // whichever producer does the re-staging.
    for _ in 0..26 {
        stage_row(&root, &compiled, &a_row("managed-7658"));
        emit_staged_row_for_session_in(&root, &project, "claude-7658");
    }

    assert_eq!(
        rows_in(&ledger),
        1,
        "26 hooks over one re-staged measurement must leave one row, got:\n{}",
        std::fs::read_to_string(&ledger).unwrap_or_default()
    );
    assert!(
        !pending_row_path_in(&root, &compiled).exists(),
        "a redundant staged row must be dropped, not left to be claimed again"
    );
}

/// Why (#7658) — the Fail-Open Check at the sweep. A ledger that cannot be READ
/// answers neither "present" nor "absent", so the claim must put the row back
/// rather than append blind. The opposite reading is the fail-open that makes an
/// IO fault an unbounded append loop.
/// Test: itself.
#[test]
fn an_unreadable_ledger_leaves_the_staged_row_alone() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path().join("framework");
    let compiled = project_with_compiled_prompt(tmp.path(), "project", "managed-7658b");
    let project = tmp.path().join("project");
    let ledger = crate::core::savings::savings_log_in(&root);
    // A directory at the ledger path: readable as a path, unreadable as a file.
    std::fs::create_dir_all(&ledger).expect("occupy the ledger path");

    stage_row(&root, &compiled, &a_row("managed-7658b"));
    assert!(
        !emit_staged_row_for_session_in(&root, &project, "claude-7658b"),
        "no row can be reported against a ledger that cannot be read"
    );
    assert!(
        pending_row_path_in(&root, &compiled).exists(),
        "the measurement must stay staged for a hook that can read the ledger"
    );
}

/// Why (#7411): staging is the only route this producer has to the ledger, so a
/// staged file that was never written — an unwritable directory, a `tm`
/// upgraded between the compile and the hook — left the session with a blank
/// `💸` segment for its whole life. The compiled prompt is still on disk and is
/// the same input the producer measured, so the hook can redo the measurement.
/// Test: itself.
#[test]
fn a_hook_with_nothing_staged_rederives_from_the_compiled_prompt() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path().join("framework");
    project_with_compiled_prompt(tmp.path(), "project", "local");
    let project = tmp.path().join("project");
    let ledger = crate::core::savings::savings_log_in(&root);

    assert!(
        emit_staged_row_for_session_in(&root, &project, "claude-2"),
        "with nothing staged the hook must re-measure the fold from the compiled prompt"
    );
    let folded = crate::core::savings::fold_session(&ledger, "claude-2");
    assert!(
        !folded.is_zero(),
        "the re-derived row must be foldable under the Claude session id: {folded:?}"
    );
}

/// Why (#7411): a Claude session raises `SessionStart` again on every resume and
/// compact. Re-deriving on each one would append the same measurement several
/// times and inflate the segment.
/// Test: itself.
#[test]
fn a_second_session_start_does_not_append_a_second_rederived_row() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path().join("framework");
    project_with_compiled_prompt(tmp.path(), "project", "local");
    let project = tmp.path().join("project");
    let ledger = crate::core::savings::savings_log_in(&root);

    emit_staged_row_for_session_in(&root, &project, "claude-3");
    emit_staged_row_for_session_in(&root, &project, "claude-3");

    assert_eq!(
        rows_in(&ledger),
        1,
        "two SessionStart events for one session must leave one row"
    );
}

/// Why (#7411): the defect is a file that outlives every hook that could have
/// resolved it. A row whose compiled prompt is gone can never be attributed to
/// a session, so leaving it is a permanent leak — it has to go, and the
/// measurement has to survive somewhere an operator can read.
/// Test: itself.
#[test]
fn a_stranded_orphan_is_discarded_with_its_measurement_logged() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path().join("framework");
    let project = tmp.path().join("project");
    // Staged for a compiled prompt that does not exist — the shape the two live
    // rows on the owner's host had.
    let vanished = project.join(".trusty-mpm/sessions/gone/INSTRUCTIONS-COMPILED.md");
    let staged = pending_row_path_in(&root, &vanished);
    let ledger = crate::core::savings::savings_log_in(&root);
    stage_row(&root, &vanished, &a_row("gone"));

    assert_eq!(
        sweep_pending_rows(&ledger, &root, &project, "claude-4"),
        0,
        "a fresh orphan is not yet the sweep's to judge"
    );
    assert!(staged.exists(), "a fresh orphan must be left alone");

    backdate(&staged, 13);
    let capture = CaptureWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        sweep_pending_rows(&ledger, &root, &project, "claude-4");
    });

    assert!(
        !staged.exists(),
        "a stranded orphan must not survive the next hook"
    );
    assert_eq!(rows_in(&ledger), 0, "a discarded row is not a ledger row");
    let logged = String::from_utf8(capture.0.lock().expect("capture lock").clone())
        .expect("the captured log must be utf-8");
    assert!(
        logged.contains("compiled prompt no longer exists"),
        "the discard must state its reason: {logged}"
    );
    assert!(
        logged.contains("6699"),
        "the discard must preserve the measurement: {logged}"
    );
}

/// Why (#7411): the sweep reads one shared directory, so a hook for project A
/// sees project B's staged rows. Claiming one while B's own launch is still
/// coming would attribute B's saving to A's session.
/// Test: itself.
#[test]
fn a_fresh_row_for_another_project_is_left_for_its_own_hook() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path().join("framework");
    let theirs = project_with_compiled_prompt(tmp.path(), "theirs", "local");
    let ours = tmp.path().join("ours");
    std::fs::create_dir_all(&ours).expect("our project");
    let ledger = crate::core::savings::savings_log_in(&root);

    stage_row(&root, &theirs, &a_row("local"));

    assert_eq!(
        sweep_pending_rows(&ledger, &root, &ours, "claude-5"),
        0,
        "another project's fresh row is not ours to claim"
    );
    assert!(
        pending_row_path_in(&root, &theirs).exists(),
        "it must still be there for its own hook"
    );
    assert_eq!(
        sweep_pending_rows(&ledger, &root, &tmp.path().join("theirs"), "claude-6"),
        1,
        "its own project's hook must claim it"
    );
}

/// Why (#7411): a row whose project never launches again would otherwise sit
/// forever behind the ownership check above. Once it has outlived
/// [`STRANDED_AFTER`] the next hook adopts it — a row attributed to a nearby
/// session beats a row attributed to none.
/// Test: itself.
#[test]
fn a_stranded_row_for_another_project_is_adopted_rather_than_stranded() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path().join("framework");
    let theirs = project_with_compiled_prompt(tmp.path(), "theirs", "local");
    let ours = tmp.path().join("ours");
    std::fs::create_dir_all(&ours).expect("our project");
    let ledger = crate::core::savings::savings_log_in(&root);

    stage_row(&root, &theirs, &a_row("local"));
    backdate(&pending_row_path_in(&root, &theirs), 13);

    assert_eq!(
        sweep_pending_rows(&ledger, &root, &ours, "claude-7"),
        1,
        "a row nobody has claimed in half a day must be adopted, not stranded"
    );
    assert!(
        !crate::core::savings::fold_session(&ledger, "claude-7").is_zero(),
        "the adopted row must reach the ledger"
    );
}

/// Why (#7411): rows staged by a `tm` from before this fix record no compiled
/// prompt, and an upgrade must not strand the very rows it exists to rescue.
/// Test: itself.
#[test]
fn a_row_staged_without_a_path_is_still_claimed_by_digest() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path().join("framework");
    let compiled = project_with_compiled_prompt(tmp.path(), "project", "local");
    let project = tmp.path().join("project");
    let ledger = crate::core::savings::savings_log_in(&root);

    // The pre-#7411 on-disk shape: the bare row, no `compiled_prompt` field.
    let staged = pending_row_path_in(&root, &compiled);
    std::fs::create_dir_all(staged.parent().expect("pending dir")).expect("pending dir");
    std::fs::write(
        &staged,
        serde_json::to_string(&a_row("local")).expect("serialise"),
    )
    .expect("write the pre-#7411 staged row");

    assert_eq!(
        sweep_pending_rows(&ledger, &root, &project, "claude-8"),
        1,
        "a row staged by an older tm must still be claimed"
    );
    assert!(!crate::core::savings::fold_session(&ledger, "claude-8").is_zero());
}

/// Why (#7411): the sweep widened what a hook touches, and two hooks can run at
/// once — a `SessionStart` and a resume land together often enough. The rename
/// has to stay the claim, or the widened reach doubles rows instead of
/// recovering them.
/// Test: itself.
#[test]
fn two_racing_sweeps_append_exactly_one_row() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path().join("framework");
    let compiled = project_with_compiled_prompt(tmp.path(), "project", "local");
    let project = tmp.path().join("project");
    let ledger = crate::core::savings::savings_log_in(&root);

    stage_row(&root, &compiled, &a_row("local"));

    let claimed: usize = std::thread::scope(|scope| {
        let racers: Vec<_> = (0..2)
            .map(|_| scope.spawn(|| sweep_pending_rows(&ledger, &root, &project, "claude-9")))
            .collect();
        racers
            .into_iter()
            .map(|racer| racer.join().expect("racer"))
            .sum()
    });

    assert_eq!(claimed, 1, "exactly one racer may claim the staged row");
    assert_eq!(rows_in(&ledger), 1, "the ledger must hold exactly one row");
}

/// Collects a subscriber's output so a test can read back what was logged.
///
/// Why: the discard's contract is that the measurement survives in the log, so
/// the test has to read the log rather than trust the return value.
/// What: an `io::Write` over a shared buffer, and the `MakeWriter` that hands it
/// to `tracing_subscriber::fmt`.
#[derive(Clone, Default)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
