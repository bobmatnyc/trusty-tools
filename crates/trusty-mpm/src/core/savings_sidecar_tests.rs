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

/// Why (#7245): the claim and the append are two steps, and the second one can
/// fail on its own — an unwritable ledger, a full disk, a path component that is
/// a file. A claim that consumed the staged file before knowing the append
/// landed would destroy the only copy of the measurement. Exactly-once has to
/// mean "at most one row AND no lost row".
/// What: makes the ledger's parent component a regular file, so `append_row`'s
/// `create_dir_all` fails after the claim, then retries against a writable
/// ledger.
/// Test: itself.
#[test]
fn a_failed_append_leaves_the_row_staged_for_the_next_hook() {
    let root = tempfile::tempdir().expect("temp root");
    let compiled = root
        .path()
        .join("project/.trusty-mpm/sessions/m1/INSTRUCTIONS-COMPILED.md");

    let blocker = root.path().join("blocker");
    std::fs::write(&blocker, b"a regular file, not a directory").expect("write blocker");
    let unwritable = blocker.join("ledger.jsonl");

    stage_row(root.path(), &compiled, &staged_row("m1"));
    assert!(
        !emit_staged_row(&unwritable, root.path(), &compiled, "c1"),
        "an append that fails must report no append"
    );
    assert!(
        pending_row_path_in(root.path(), &compiled).exists(),
        "a failed append must return the row to its staging path for the next hook"
    );

    let ledger = root.path().join("ledger.jsonl");
    assert!(
        emit_staged_row(&ledger, root.path(), &compiled, "c1"),
        "the retry against a writable ledger must append the recovered row"
    );
    assert_eq!(
        rows_in(&ledger),
        1,
        "the recovered row must reach the ledger exactly once"
    );
    assert!(
        !crate::core::savings::fold_session(&ledger, "c1").is_zero(),
        "the recovered row must fold under the Claude session id"
    );
}

/// Why (#7245): `tm hook` fires on several events, and two of them can overlap.
/// The claim is what decides the race, so drive it from two threads at once
/// rather than trusting the single-threaded ordering above.
/// What: two threads call `emit_staged_row` against one staged file; exactly one
/// may report an append, and the ledger holds one row.
/// Test: itself.
#[test]
fn two_racing_claims_append_exactly_one_row() {
    let root = tempfile::tempdir().expect("temp root");
    let compiled = root
        .path()
        .join("project/.trusty-mpm/sessions/m1/INSTRUCTIONS-COMPILED.md");
    let ledger = root.path().join("ledger.jsonl");

    stage_row(root.path(), &compiled, &staged_row("m1"));

    let racers: Vec<_> = (0..2)
        .map(|_| {
            let ledger = ledger.clone();
            let root = root.path().to_path_buf();
            let compiled = compiled.clone();
            std::thread::spawn(move || emit_staged_row(&ledger, &root, &compiled, "c1"))
        })
        .collect();
    let appended = racers
        .into_iter()
        .map(|racer| racer.join().expect("racer thread"))
        .filter(|appended| *appended)
        .count();

    assert_eq!(appended, 1, "exactly one racer may claim the staged row");
    assert_eq!(
        rows_in(&ledger),
        1,
        "two racing hooks must leave exactly one row"
    );
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

/// Why (#7411): the staging file's name is a one-way digest, so a hook that
/// derives a different compiled-prompt path than the compile did cannot tell
/// what the file is for, whether its prompt still exists, or whose project it
/// belongs to. That is why rows staged under a managed session scope sat
/// unclaimed forever. The path has to be IN the file — and the file has to stay
/// readable as a bare row, so nothing that reads it that way breaks.
/// Test: itself.
#[test]
fn a_staged_row_remembers_its_compiled_prompt() {
    let root = tempfile::tempdir().expect("temp root");
    let compiled = root
        .path()
        .join("project/.trusty-mpm/sessions/m1/INSTRUCTIONS-COMPILED.md");

    stage_row(root.path(), &compiled, &staged_row("m1"));

    let text = std::fs::read_to_string(pending_row_path_in(root.path(), &compiled))
        .expect("the staged file must be readable");
    let parsed: serde_json::Value =
        serde_json::from_str(&text).expect("the staged file must be one JSON object");
    assert_eq!(
        parsed.get("compiled_prompt").and_then(|v| v.as_str()),
        Some(compiled.to_string_lossy().as_ref()),
        "the staged row must name the compiled prompt it measures: {text}"
    );
    let row: SavingsRow =
        serde_json::from_str(&text).expect("a staged file must still read back as a bare row");
    assert_eq!(row.tokens_saved, 600, "the measurement must survive intact");
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

/// Why (#7617): the warning's first wording told an operator the `💸` segment
/// "stays absent" for this project. Since #7617 the statusline folds `divert`
/// and `compress` rows beside instruction-compression, falls back to a linked
/// sibling session, and renders `💸—` as an explicit empty state — so the
/// segment renders and the claim is false. A decline here zeroes ONE technique's
/// contribution, never the segment, and an operator who reads the wider claim
/// stops looking for the savings they do have.
/// What: captures the warning and rejects any word that asserts the segment is
/// gone, then pins the two facts that replaced it — the scope (this project) and
/// the remedy (a CLAUDE.md section override).
/// Test: itself.
#[test]
fn the_no_fold_warning_claims_no_segment_wide_absence() {
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
    });

    let logged = String::from_utf8(capture.0.lock().expect("capture lock").clone())
        .expect("the captured log must be utf-8");
    for banned in ["stays absent", "absent", "hidden", "no 💸", "never renders"] {
        assert!(
            !logged.contains(banned),
            "the warning must not claim the segment is gone, but says {banned:?}: {logged}"
        );
    }
    assert!(
        logged.contains("this project"),
        "the warning must scope the decline to this project: {logged}"
    );
    assert!(
        logged.contains("CLAUDE.md section override"),
        "the warning must name the override that would fold a section away: {logged}"
    );
}
