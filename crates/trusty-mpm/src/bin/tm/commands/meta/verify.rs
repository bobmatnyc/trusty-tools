//! Deterministic artifact verification for the metaharness demo (#1051, WI-C).
//!
//! Why: the POC's success criterion is checkable WITHOUT a human: the launched
//! `claude` session is told to write a known file, and after the session exits
//! the harness must decide pass/fail purely from disk state. Pulling that
//! decision into a small, side-effect-light module keeps the verdict logic
//! unit-testable against a tempdir (no live `claude`, no tmux) and keeps the
//! launch/poll orchestration in `launch.rs` free of file-checking prose.
//! What: [`DEMO_ARTIFACT`] / [`DEMO_CONTENT`] name the file the demo task writes
//! and the marker its body must contain; [`demo_task`] builds the instruction
//! text handed to the session; [`VerifyOutcome`] is the verdict; and
//! [`verify_artifact`] reads the file and classifies it (missing / wrong content
//! / pass). [`expected_content`] folds a per-run id into the marker so two runs
//! never share an identical body.
//! Test: `verify::tests` cover all three [`VerifyOutcome`] arms plus the task /
//! content composition.

use std::path::Path;

/// Name of the file the demo task instructs the session to create.
///
/// Why: the verifier and the task prose must agree on the exact filename; a
/// single constant keeps them in lockstep (a drift would make every demo fail).
/// What: the literal `"hello_metaharness.txt"` (the #1051 acceptance artifact).
/// Test: `demo_task_names_artifact`, `verify_passes_on_matching_content`.
pub(crate) const DEMO_ARTIFACT: &str = "hello_metaharness.txt";

/// The fixed marker the artifact body must contain for a passing verdict.
///
/// Why: verifying *content* (not mere existence) proves the session actually did
/// the work rather than an empty file appearing by accident. Centralising the
/// marker keeps the task instruction and the verifier from diverging.
/// What: the literal `"metaharness OK"`.
/// Test: `verify_passes_on_matching_content`, `verify_fails_on_wrong_content`.
pub(crate) const DEMO_CONTENT: &str = "metaharness OK";

/// The verdict of checking the demo artifact on disk.
///
/// Why: the launch handler must map a single, exhaustive outcome to a process
/// exit code and a structured report; an enum (rather than a bool + message)
/// makes the missing-vs-wrong-content distinction explicit and testable.
/// What: `Pass` (file present and contains the expected marker), `Missing` (no
/// file at the expected path), or `WrongContent` (file present but the marker is
/// absent), the latter carrying a short, truncated preview for the report.
/// Test: `verify::tests` assert each arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VerifyOutcome {
    /// The artifact exists and contains the expected marker.
    Pass,
    /// No file was found at the expected path.
    Missing,
    /// The file exists but does not contain the expected marker; carries a
    /// truncated preview of what was found for the diagnostic report.
    WrongContent {
        /// A short, truncated preview of the file body that was found.
        found_preview: String,
    },
}

impl VerifyOutcome {
    /// Whether this verdict is a pass.
    ///
    /// Why: the caller decides the process exit code from the verdict; a named
    /// predicate reads better than matching at every call site.
    /// What: returns `true` only for [`VerifyOutcome::Pass`].
    /// Test: `verify_passes_on_matching_content` (true) and the failing arms.
    pub(crate) fn is_pass(&self) -> bool {
        matches!(self, VerifyOutcome::Pass)
    }

    /// A stable, lower-case status token for the structured report.
    ///
    /// Why: the JSON report and tooling that scrapes it need a machine-readable
    /// status string that is decoupled from the human prose.
    /// What: `"pass"`, `"missing"`, or `"wrong-content"`.
    /// Test: `verify_outcome_status_tokens`.
    pub(crate) fn status(&self) -> &'static str {
        match self {
            VerifyOutcome::Pass => "pass",
            VerifyOutcome::Missing => "missing",
            VerifyOutcome::WrongContent { .. } => "wrong-content",
        }
    }
}

/// Compose the expected artifact body for a run identified by `run_id`.
///
/// Why: embedding a per-run id in the body lets a verifier prove THIS run wrote
/// the file (not a leftover from a prior run) while still keeping the stable
/// [`DEMO_CONTENT`] marker the pass check keys off. Factoring it out keeps the
/// task prose and the (future, stricter) verifier consistent.
/// What: returns `"<DEMO_CONTENT> (run <run_id>)"`.
/// Test: `expected_content_embeds_run_id`.
pub(crate) fn expected_content(run_id: &str) -> String {
    format!("{DEMO_CONTENT} (run {run_id})")
}

/// Build the bundled demo task handed to the launched `claude` session.
///
/// Why: `--demo` runs a fixed, checkable task so the run's success is verifiable
/// without operator input (#1051). Isolating the prose keeps it unit-testable
/// and the launch handler free of inline instruction text.
/// What: returns a one-line instruction telling the session to create
/// [`DEMO_ARTIFACT`] in the project root with a body containing the
/// [`expected_content`] marker for `run_id`, then stop.
/// Test: `demo_task_names_artifact`, `demo_task_embeds_expected_content`.
pub(crate) fn demo_task(run_id: &str) -> String {
    let body = expected_content(run_id);
    format!(
        "Create a file named `{DEMO_ARTIFACT}` in the current project directory \
         whose contents are exactly the single line `{body}`. Use your file-writing \
         tools to create it, confirm it exists, then end the session."
    )
}

/// Truncate a file body to a short, single-line preview for the report.
///
/// Why: a `WrongContent` verdict should surface WHAT was found without dumping a
/// large file into the JSON report or log; a bounded preview keeps diagnostics
/// readable. Factoring it out makes the truncation rule itself testable.
/// What: replaces newlines with spaces and truncates to at most 120 characters,
/// appending an ellipsis when truncated.
/// Test: `preview_truncates_long_bodies`, `preview_collapses_newlines`.
fn preview(body: &str) -> String {
    const MAX: usize = 120;
    let one_line: String = body.replace(['\n', '\r'], " ");
    if one_line.chars().count() > MAX {
        let truncated: String = one_line.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        one_line
    }
}

/// Verify the demo artifact exists under `project_dir` with the expected marker.
///
/// Why: this is the deterministic pass/fail gate for the POC (#1051). It is a
/// pure read of disk state so it can be unit-tested against a tempdir with no
/// live `claude` or tmux, which is the CI-runnable coverage the live end-to-end
/// (#1053, `#[ignore]`) cannot provide.
/// What: reads `<project_dir>/<DEMO_ARTIFACT>`; returns
/// [`VerifyOutcome::Missing`] if the file cannot be read, [`VerifyOutcome::Pass`]
/// if its body contains `expected_marker`, else [`VerifyOutcome::WrongContent`]
/// carrying a truncated preview. The check is a `contains` (not an exact match)
/// so trailing newlines / surrounding prose the model may add do not fail a run
/// that genuinely wrote the marker.
/// Test: `verify_passes_on_matching_content`, `verify_reports_missing`,
/// `verify_fails_on_wrong_content`.
pub(crate) fn verify_artifact(project_dir: &Path, expected_marker: &str) -> VerifyOutcome {
    let path = project_dir.join(DEMO_ARTIFACT);
    match std::fs::read_to_string(&path) {
        Ok(body) if body.contains(expected_marker) => VerifyOutcome::Pass,
        Ok(body) => VerifyOutcome::WrongContent {
            found_preview: preview(&body),
        },
        Err(_) => VerifyOutcome::Missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_task_names_artifact() {
        let task = demo_task("abc123");
        assert!(
            task.contains(DEMO_ARTIFACT),
            "task must name the artifact file: {task}"
        );
    }

    #[test]
    fn demo_task_embeds_expected_content() {
        let task = demo_task("abc123");
        assert!(
            task.contains(&expected_content("abc123")),
            "task must embed the expected content marker: {task}"
        );
    }

    #[test]
    fn expected_content_embeds_run_id() {
        let body = expected_content("run-42");
        assert!(body.contains(DEMO_CONTENT), "body keeps the stable marker");
        assert!(body.contains("run-42"), "body embeds the run id");
    }

    #[test]
    fn verify_reports_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // No file written → Missing.
        assert_eq!(
            verify_artifact(tmp.path(), DEMO_CONTENT),
            VerifyOutcome::Missing
        );
    }

    #[test]
    fn verify_passes_on_matching_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let marker = expected_content("run-1");
        std::fs::write(tmp.path().join(DEMO_ARTIFACT), format!("{marker}\n"))
            .expect("write artifact");
        let outcome = verify_artifact(tmp.path(), &marker);
        assert_eq!(outcome, VerifyOutcome::Pass);
        assert!(outcome.is_pass());
        assert_eq!(outcome.status(), "pass");
    }

    #[test]
    fn verify_passes_when_marker_is_substring() {
        // The model may add surrounding prose; a `contains` check still passes
        // as long as the marker is present.
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join(DEMO_ARTIFACT),
            "Done!\nmetaharness OK (run x)\nGoodbye.\n",
        )
        .expect("write artifact");
        assert_eq!(
            verify_artifact(tmp.path(), "metaharness OK (run x)"),
            VerifyOutcome::Pass
        );
    }

    #[test]
    fn verify_fails_on_wrong_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join(DEMO_ARTIFACT), "something else entirely")
            .expect("write artifact");
        let outcome = verify_artifact(tmp.path(), DEMO_CONTENT);
        match &outcome {
            VerifyOutcome::WrongContent { found_preview } => {
                assert!(found_preview.contains("something else"));
            }
            other => panic!("expected WrongContent, got {other:?}"),
        }
        assert!(!outcome.is_pass());
        assert_eq!(outcome.status(), "wrong-content");
    }

    #[test]
    fn verify_outcome_status_tokens() {
        assert_eq!(VerifyOutcome::Pass.status(), "pass");
        assert_eq!(VerifyOutcome::Missing.status(), "missing");
        assert_eq!(
            VerifyOutcome::WrongContent {
                found_preview: "x".into()
            }
            .status(),
            "wrong-content"
        );
    }

    #[test]
    fn preview_truncates_long_bodies() {
        let body = "a".repeat(500);
        let p = preview(&body);
        // 120 chars + the ellipsis.
        assert_eq!(p.chars().count(), 121);
        assert!(p.ends_with('…'));
    }

    #[test]
    fn preview_collapses_newlines() {
        let p = preview("line1\nline2\r\nline3");
        assert!(!p.contains('\n'));
        assert!(!p.contains('\r'));
        assert!(p.contains("line1 line2"));
    }
}
