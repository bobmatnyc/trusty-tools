//! #5551: Error-arm regression tests for the relocation presence probes.
//!
//! Why: `helpers.rs` gated every destructive relocation on
//! `try_exists(...).unwrap_or(false)`, which answers "the probe failed" as
//! "the destination is absent" — so a transient `EIO`/`ETIMEDOUT`/`ESTALE`
//! unblocked a rename over a real output followed by deletion of the source,
//! logged as a success. ADR-0045 requires absent and undeterminable to be
//! distinguished before a destructive filesystem operation.
//! What: Injects an `ExistsProbe` that fails for one chosen path and defers
//! every other path to the real filesystem, then asserts the DIRECTION of the
//! outcome — the destination stays byte-identical, the source still exists,
//! and the caller sees an error rather than a clean summary.
//! Test: This file IS the test body.

use super::*;

use crate::workflow::engine::helpers::{
    ExistsProbe, FsExistsProbe, reconcile_code_outputs_against_from, reconcile_code_outputs_from,
    relocate_plan_outputs_from,
};
use std::path::{Path, PathBuf};

/// Probe that answers one path with a transient I/O error and every other
/// path from the real filesystem.
struct FailingProbe {
    fail_on: PathBuf,
}

impl FailingProbe {
    fn new(fail_on: impl Into<PathBuf>) -> Self {
        Self {
            fail_on: fail_on.into(),
        }
    }
}

#[async_trait]
impl ExistsProbe for FailingProbe {
    async fn try_exists(&self, path: &Path) -> std::io::Result<bool> {
        if path == self.fail_on {
            // Not NotFound: an unanswerable probe, the shape a network mount
            // produces mid-hiccup.
            return Err(std::io::Error::other("simulated transient I/O failure"));
        }
        tokio::fs::try_exists(path).await
    }
}

/// One declared file in a single wave.
fn assignments_json(rel_path: &str) -> String {
    format!(
        r#"{{"error_convention":"exceptions","waves":[{{"wave":1,"files":[{{"path":"{rel_path}","stub":null,"purpose":"test","depends_on":[],"max_lines":100}}]}}]}}"#
    )
}

/// Create `tmp/<name>` and return its CANONICAL path.
///
/// `safe_join` canonicalizes its base before joining, so a destination derived
/// from a non-canonical temp dir (macOS `/var` → `/private/var`) would never
/// equal the path the probe is asked about.
async fn canonical_dir(tmp: &Path, name: &str) -> PathBuf {
    let p = tmp.join(name);
    tokio::fs::create_dir_all(&p).await.unwrap();
    p.canonicalize().unwrap()
}

/// Simulated project root + a second directory, both created and canonical.
async fn two_dirs(tmp: &Path, a: &str, b: &str) -> (PathBuf, PathBuf) {
    (canonical_dir(tmp, a).await, canonical_dir(tmp, b).await)
}

// ── HIGH sites: the probe gates a rename/copy plus a source delete ─────────

/// #5551 (helpers.rs:116): an undeterminable probe on the code-target
/// destination must not be read as "absent" — the stray at the project root
/// would otherwise be renamed over a real generated output and then deleted.
#[tokio::test]
async fn reconcile_against_aborts_when_destination_probe_is_undeterminable() {
    let tmp = tempfile::tempdir().unwrap();
    let (project_root, code_target) = two_dirs(tmp.path(), "project", "code").await;
    let assignments_dir = canonical_dir(tmp.path(), "artifacts").await;
    tokio::fs::write(
        assignments_dir.join("assignments.json"),
        assignments_json("src/app.py"),
    )
    .await
    .unwrap();

    let dest = code_target.join("src/app.py");
    tokio::fs::create_dir_all(dest.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&dest, b"REAL OUTPUT").await.unwrap();

    let stray = project_root.join("src/app.py");
    tokio::fs::create_dir_all(stray.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&stray, b"STRAY").await.unwrap();

    let probe = FailingProbe::new(&dest);
    let result =
        reconcile_code_outputs_against_from(&project_root, &assignments_dir, &code_target, &probe)
            .await;

    assert_eq!(
        tokio::fs::read(&dest).await.unwrap(),
        b"REAL OUTPUT",
        "destination must be byte-identical after an undeterminable probe"
    );
    assert!(stray.exists(), "the source must not be deleted");
    assert_eq!(tokio::fs::read(&stray).await.unwrap(), b"STRAY");
    assert!(
        result.is_err(),
        "an undeterminable destination probe must surface as an error"
    );
}

/// #5551 (helpers.rs:210): same shape against `out_dir`.
#[tokio::test]
async fn reconcile_from_aborts_when_destination_probe_is_undeterminable() {
    let tmp = tempfile::tempdir().unwrap();
    let (project_root, out_dir) = two_dirs(tmp.path(), "project", "out").await;
    tokio::fs::write(
        out_dir.join("assignments.json"),
        assignments_json("src/app.py"),
    )
    .await
    .unwrap();

    let dest = out_dir.join("src/app.py");
    tokio::fs::create_dir_all(dest.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&dest, b"REAL OUTPUT").await.unwrap();

    let stray = project_root.join("src/app.py");
    tokio::fs::create_dir_all(stray.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&stray, b"STRAY").await.unwrap();

    let probe = FailingProbe::new(&dest);
    let result = reconcile_code_outputs_from(&project_root, &out_dir, &probe).await;

    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"REAL OUTPUT");
    assert!(stray.exists(), "the source must not be deleted");
    assert!(result.is_err(), "undeterminable destination must error");
}

/// #5551 (helpers.rs:304): the destination here is `assignments.json`, the
/// plan manifest that drives which files the code phase writes and QA reads.
#[tokio::test]
async fn relocate_plan_aborts_when_out_assignments_probe_is_undeterminable() {
    let tmp = tempfile::tempdir().unwrap();
    let (project_root, out_dir) = two_dirs(tmp.path(), "project", "out").await;

    let out_asg = out_dir.join("assignments.json");
    tokio::fs::write(&out_asg, b"AUTHORITATIVE").await.unwrap();
    let root_asg = project_root.join("assignments.json");
    tokio::fs::write(&root_asg, b"STRAY").await.unwrap();

    let probe = FailingProbe::new(&out_asg);
    let result = relocate_plan_outputs_from(&project_root, &out_dir, &probe).await;

    assert_eq!(
        tokio::fs::read(&out_asg).await.unwrap(),
        b"AUTHORITATIVE",
        "the plan manifest must not be overwritten"
    );
    assert!(root_asg.exists(), "the stray must not be deleted");
    assert!(result.is_err(), "undeterminable destination must error");
}

/// #5551 (helpers.rs:359): the `out_stubs` half of the `&&`. A coerced `Err`
/// makes the condition true, `rename` fails `ENOTEMPTY`, the `copy_dir_all`
/// fallback merge-overwrites, and `remove_dir_all` then deletes the source
/// tree outright.
#[tokio::test]
async fn relocate_stubs_aborts_when_out_stubs_probe_is_undeterminable() {
    let tmp = tempfile::tempdir().unwrap();
    let (project_root, out_dir) = two_dirs(tmp.path(), "project", "out").await;

    // No assignments.json in out_dir, a recent one at the root: the manifest
    // relocation runs and the stubs branch is reached.
    tokio::fs::write(project_root.join("assignments.json"), b"{}")
        .await
        .unwrap();

    let root_stubs = project_root.join("stubs");
    tokio::fs::create_dir_all(&root_stubs).await.unwrap();
    tokio::fs::write(root_stubs.join("main.py"), b"STRAY STUB")
        .await
        .unwrap();

    let out_stubs = out_dir.join("stubs");
    tokio::fs::create_dir_all(&out_stubs).await.unwrap();
    tokio::fs::write(out_stubs.join("main.py"), b"REAL STUB")
        .await
        .unwrap();

    let probe = FailingProbe::new(&out_stubs);
    let result = relocate_plan_outputs_from(&project_root, &out_dir, &probe).await;

    assert_eq!(
        tokio::fs::read(out_stubs.join("main.py")).await.unwrap(),
        b"REAL STUB",
        "existing stubs must not be merge-overwritten"
    );
    assert!(
        root_stubs.join("main.py").is_file(),
        "the source stubs tree must not be recursively deleted"
    );
    assert!(result.is_err(), "undeterminable destination must error");
}

// ── MEDIUM sites: the coercion picks SKIP, and the skip goes unaccounted ───

/// #5551 (helpers.rs:120): a failed probe on the stray must not be recorded as
/// a clean skip — the file goes unaccounted in `moved` and `skipped_too_old`.
#[tokio::test]
async fn reconcile_against_reports_unresolved_when_stray_probe_is_undeterminable() {
    let tmp = tempfile::tempdir().unwrap();
    let (project_root, code_target) = two_dirs(tmp.path(), "project", "code").await;
    let assignments_dir = canonical_dir(tmp.path(), "artifacts").await;
    tokio::fs::write(
        assignments_dir.join("assignments.json"),
        assignments_json("src/app.py"),
    )
    .await
    .unwrap();

    let stray = project_root.join("src/app.py");
    tokio::fs::create_dir_all(stray.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&stray, b"STRAY").await.unwrap();

    let probe = FailingProbe::new(&stray);
    let result =
        reconcile_code_outputs_against_from(&project_root, &assignments_dir, &code_target, &probe)
            .await;

    assert!(
        result.is_err(),
        "an unaccounted-for file must not read as a clean pass"
    );
    assert!(
        stray.exists(),
        "nothing was relocated, so nothing is deleted"
    );
    assert!(!code_target.join("src/app.py").exists());
}

/// #5551 (helpers.rs:217): the `out_dir` twin of the case above.
#[tokio::test]
async fn reconcile_from_reports_unresolved_when_stray_probe_is_undeterminable() {
    let tmp = tempfile::tempdir().unwrap();
    let (project_root, out_dir) = two_dirs(tmp.path(), "project", "out").await;
    tokio::fs::write(
        out_dir.join("assignments.json"),
        assignments_json("src/app.py"),
    )
    .await
    .unwrap();

    let stray = project_root.join("src/app.py");
    tokio::fs::create_dir_all(stray.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&stray, b"STRAY").await.unwrap();

    let probe = FailingProbe::new(&stray);
    let result = reconcile_code_outputs_from(&project_root, &out_dir, &probe).await;

    assert!(result.is_err(), "unaccounted file must not read as clean");
    assert!(stray.exists());
    assert!(!out_dir.join("src/app.py").exists());
}

/// #5551 (helpers.rs:310): a failed probe on the root `assignments.json` must
/// not be reported as "nothing misrouted".
#[tokio::test]
async fn relocate_plan_reports_unresolved_when_root_probe_is_undeterminable() {
    let tmp = tempfile::tempdir().unwrap();
    let (project_root, out_dir) = two_dirs(tmp.path(), "project", "out").await;

    let root_asg = project_root.join("assignments.json");
    tokio::fs::write(&root_asg, b"STRAY").await.unwrap();

    let probe = FailingProbe::new(&root_asg);
    let result = relocate_plan_outputs_from(&project_root, &out_dir, &probe).await;

    assert!(
        result.is_err(),
        "an undeterminable source probe must not read as 'nothing misrouted'"
    );
    assert!(root_asg.exists());
    assert!(!out_dir.join("assignments.json").exists());
}

/// #5551 (helpers.rs:358): the `root_stubs` half of the `&&`.
#[tokio::test]
async fn relocate_stubs_reports_unresolved_when_root_stubs_probe_is_undeterminable() {
    let tmp = tempfile::tempdir().unwrap();
    let (project_root, out_dir) = two_dirs(tmp.path(), "project", "out").await;

    tokio::fs::write(project_root.join("assignments.json"), b"{}")
        .await
        .unwrap();
    let root_stubs = project_root.join("stubs");
    tokio::fs::create_dir_all(&root_stubs).await.unwrap();
    tokio::fs::write(root_stubs.join("main.py"), b"STRAY STUB")
        .await
        .unwrap();

    let probe = FailingProbe::new(&root_stubs);
    let result = relocate_plan_outputs_from(&project_root, &out_dir, &probe).await;

    assert!(
        result.is_err(),
        "an undeterminable stubs probe must not be a silent skip"
    );
    assert!(root_stubs.join("main.py").is_file());
}

// ── NotFound stays benign ──────────────────────────────────────────────────

/// #5551: a genuine "does not exist" is a definite answer and must keep
/// returning `Ok(false)`, so the fix cannot turn every missing destination
/// into a hard failure.
#[tokio::test]
async fn fs_probe_reports_missing_path_as_absent_not_error() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nope").join("still-nope.json");
    let answer = FsExistsProbe.try_exists(&missing).await;
    assert!(
        matches!(answer, Ok(false)),
        "a missing path must answer Ok(false), got {answer:?}"
    );
}
