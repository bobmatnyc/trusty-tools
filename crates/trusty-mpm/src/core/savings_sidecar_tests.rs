//! Tests for the instruction-compression producer's side files (#7245).
//!
//! Why: both files exist to survive a process boundary, so the properties worth
//! asserting are the ones a single-process implementation would get wrong — a
//! staged row that emits twice, a hook that writes a row nobody staged, and a
//! decline warning that repeats on every launch until an operator stops reading
//! it.
//! What: drives the staging, claiming and warning functions against temp
//! directories, with the warning's level and text read back off a real
//! subscriber rather than assumed.
//! Test: this file.

use super::*;

use std::sync::{Arc, Mutex};

/// A row with a known, foldable measurement.
///
/// Why: every emit assertion below turns on the row surviving the round trip
/// intact, so the figures are hand-set rather than measured.
/// What: `session_id` is the compile-time placeholder the emit path replaces.
fn staged_row(session_id: &str) -> SavingsRow {
    SavingsRow {
        ts: "2026-09-09T00:00:00Z".to_string(),
        session_id: session_id.to_string(),
        technique: crate::core::savings::TECHNIQUE_INSTRUCTION_COMPRESSION.to_string(),
        tokens_saved: 600,
        tokens_before: 6_800,
        cost_saved_usd: 0.0018,
        basis: "hand-built".to_string(),
        model_source: crate::core::session_model::MODEL_SOURCE_LAUNCH_CONFIG.to_string(),
    }
}

/// How many rows the ledger holds.
fn rows_in(ledger: &Path) -> usize {
    std::fs::read_to_string(ledger)
        .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
        .unwrap_or(0)
}

/// Why (#7245): the compiling process cannot key the row, so the whole fix is
/// this hand-off — the first hook invocation appends it under the id the
/// statusline folds by, and a second invocation must not append it again.
/// Test: itself.
#[test]
fn a_staged_row_emits_once_under_the_claude_session_id() {
    let root = tempfile::tempdir().expect("temp root");
    let compiled = root
        .path()
        .join("project/.trusty-mpm/sessions/m1/INSTRUCTIONS-COMPILED.md");
    let ledger = root.path().join("ledger.jsonl");

    stage_row(root.path(), &compiled, &staged_row("m1"));
    assert!(
        pending_row_path_in(root.path(), &compiled).exists(),
        "the compiling process must leave a staged row behind"
    );

    assert!(
        emit_staged_row(&ledger, root.path(), &compiled, "c1"),
        "the first hook invocation must append the staged row"
    );
    assert!(
        !crate::core::savings::fold_session(&ledger, "c1").is_zero(),
        "the statusline fold under the Claude session id must find the row"
    );
    assert!(
        crate::core::savings::fold_session(&ledger, "m1").is_zero(),
        "nothing may stay attributed to the compile-time id"
    );

    assert!(
        !emit_staged_row(&ledger, root.path(), &compiled, "c1"),
        "a second hook invocation must append nothing"
    );
    assert_eq!(
        rows_in(&ledger),
        1,
        "the staged row must reach the ledger exactly once"
    );
}

/// Why (#7245): the hook runs on every session start, and almost none of those
/// sessions have a staged row waiting. Writing anything in that case would put a
/// fabricated figure on the status bar.
/// Test: itself.
#[test]
fn emitting_without_a_staged_row_writes_nothing() {
    let root = tempfile::tempdir().expect("temp root");
    let compiled = root
        .path()
        .join("project/.trusty-mpm/sessions/m1/INSTRUCTIONS-COMPILED.md");
    let ledger = root.path().join("ledger.jsonl");

    assert!(
        !emit_staged_row(&ledger, root.path(), &compiled, "c1"),
        "with nothing staged the hook must report no append"
    );
    assert!(
        !ledger.exists(),
        "with nothing staged the hook must not create the ledger"
    );
}

/// Why (#7245): Claude Code sends an empty `session_id` before a session has
/// one. Claiming the staged row against that would consume it and key the row by
/// nothing, losing the measurement for the session that actually needs it.
/// Test: itself.
#[test]
fn emitting_without_a_session_id_leaves_the_row_staged() {
    let root = tempfile::tempdir().expect("temp root");
    let compiled = root
        .path()
        .join("project/.trusty-mpm/sessions/m1/INSTRUCTIONS-COMPILED.md");
    let ledger = root.path().join("ledger.jsonl");

    stage_row(root.path(), &compiled, &staged_row("m1"));
    assert!(!emit_staged_row(&ledger, root.path(), &compiled, "   "));
    assert!(
        pending_row_path_in(root.path(), &compiled).exists(),
        "a blank session id must leave the staged row for the next hook"
    );

    assert!(emit_staged_row(&ledger, root.path(), &compiled, "c1"));
    assert_eq!(rows_in(&ledger), 1);
}

/// Why (#7245): two projects' unmanaged launches share the single `local`
/// session scope, so a file named after the scope would let one project's hook
/// claim the other's row. The compiled prompt's full path is what separates
/// them.
/// Test: itself.
#[test]
fn two_compiled_prompts_stage_to_different_files() {
    let root = tempfile::tempdir().expect("temp root");
    let left = root
        .path()
        .join("alpha/.trusty-mpm/sessions/local/INSTRUCTIONS-COMPILED.md");
    let right = root
        .path()
        .join("beta/.trusty-mpm/sessions/local/INSTRUCTIONS-COMPILED.md");

    assert_ne!(
        pending_row_path_in(root.path(), &left),
        pending_row_path_in(root.path(), &right),
        "two projects on the `local` scope must not share a staging file"
    );
}

/// Why (#7245): the decline is permanent for a project that overrides no
/// instruction section, so a warning on every launch is one an operator stops
/// reading. It has to be visible once and then quiet.
/// Test: itself.
#[test]
fn the_no_fold_warning_fires_once_per_project() {
    let root = tempfile::tempdir().expect("temp root");
    let project = root.path().join("project");

    assert!(
        warn_no_fold_once(root.path(), &project, 26_741, 29_074),
        "the first decline must warn"
    );
    assert!(
        !warn_no_fold_once(root.path(), &project, 26_741, 29_074),
        "a second compile with the same figures must stay quiet"
    );
}

/// Why (#7245): an operator who edits `CLAUDE.md` and still sees no segment
/// needs the new figures, not silence left over from the old ones.
/// Test: itself.
#[test]
fn the_no_fold_warning_fires_again_when_the_byte_pair_moves() {
    let root = tempfile::tempdir().expect("temp root");
    let project = root.path().join("project");

    assert!(warn_no_fold_once(root.path(), &project, 26_741, 29_074));
    assert!(
        warn_no_fold_once(root.path(), &project, 26_741, 28_000),
        "a changed byte pair must warn again"
    );
}

/// Collects a subscriber's output so a test can read back what was logged.
///
/// Why: `warn_no_fold_once`'s contract is the LEVEL as much as the count — at
/// `debug!` (where #7209 left it) the decline was invisible, which is the defect
/// #7245 names. Asserting the return value alone would not catch a regression to
/// `debug!`.
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

/// Why (#7245): the whole point of the second arm of this fix is that an
/// operator can SEE why the segment is absent. A `debug!` they never enable is
/// not seeing it, and two identical warnings per project is the noise the marker
/// exists to prevent.
/// Test: itself.
#[test]
fn the_no_fold_warning_is_emitted_at_warn_level() {
    let root = tempfile::tempdir().expect("temp root");
    let project = root.path().join("project");
    let capture = CaptureWriter::default();

    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        warn_no_fold_once(root.path(), &project, 26_741, 29_074);
        warn_no_fold_once(root.path(), &project, 26_741, 29_074);
    });

    let logged = String::from_utf8(capture.0.lock().expect("capture lock").clone())
        .expect("the captured log must be utf-8");
    assert_eq!(
        logged
            .matches("not smaller than the instruction sources")
            .count(),
        1,
        "two compiles must produce exactly one warning: {logged}"
    );
    assert!(
        logged.contains("26741") && logged.contains("29074"),
        "the warning must name both byte counts: {logged}"
    );
}
