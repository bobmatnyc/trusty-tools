//! #5551: Error-arm regression tests for the relocation presence probes.
//!
//! Why: `helpers.rs` gated every destructive relocation on
//! `try_exists(...).unwrap_or(false)`, which answers "the probe failed" as
//! "the destination is absent" — so a transient `EIO`/`ETIMEDOUT`/`ESTALE`
//! unblocked a rename over a real output followed by deletion of the source,
//! logged as a success. ADR-0045 requires absent and undeterminable to be
//! distinguished before a destructive filesystem operation.
//! What: Two techniques, both asserting the DIRECTION of the outcome — the
//! destination stays byte-identical, the source still exists, and the caller
//! sees an error rather than a clean summary.
//!
//! 1. A real-filesystem fixture: the destination is a symlink into a mode-000
//!    directory, so `stat` is denied `EACCES` while `rename` onto it still
//!    succeeds (`rename(2)` does not dereference its target). That reaches the
//!    destructive branch with no injected probe at all, and is how the three
//!    plain-file destinations are covered.
//! 2. An injected `ExistsProbe` that fails for one chosen path. This is needed
//!    for the `out_stubs` DIRECTORY site: its fallback runs `copy_dir_all`,
//!    whose own `create_dir_all` hits the same `EACCES` wall, so a symlink
//!    fixture there fails safe even pre-fix and proves nothing. The seam also
//!    keeps the other sites' error arms deterministic under `EIO`/`ESTALE`,
//!    which no fixture can produce on demand.
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

// ── Real-filesystem fixtures: no seam, no injected probe ───────────────────
//
// #5551: the three plain-file destinations are renamed onto directly, and
// `rename(2)` does not dereference the destination. So a destination symlink
// pointing into a mode-000 directory denies `stat` while leaving the rename
// path intact — exactly the pre-fix trigger, produced by the real filesystem.
// These tests exercise the same sites as the injected-probe tests above and
// hold against the fix with no production seam involved.

#[cfg(unix)]
mod real_fs {
    use super::*;
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    /// Restore a directory's mode on drop, including while unwinding.
    ///
    /// Without this, an assertion failure leaves a mode-000 directory that
    /// `TempDir` cannot delete.
    struct RestoreMode(PathBuf);

    impl Drop for RestoreMode {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, Permissions::from_mode(0o700));
        }
    }

    /// A destination whose presence probe is denied while a rename onto it
    /// still succeeds.
    ///
    /// Field order is drop order: `_restore` must run before `_tmp`, or
    /// `TempDir` cannot delete a tree that still holds a mode-000 directory.
    struct DeniedDest {
        _restore: RestoreMode,
        _tmp: TempDir,
        locked: PathBuf,
        dest: PathBuf,
    }

    impl DeniedDest {
        /// What the destination resolves to now, read with the denial lifted.
        ///
        /// Pre-fix this returns the stray's bytes, because `rename` replaced
        /// the symlink; post-fix it still returns the real output.
        fn resolved(&self) -> Vec<u8> {
            self.unlock();
            let bytes = std::fs::read(&self.dest).expect("destination must still be readable");
            self.relock();
            bytes
        }

        fn unlock(&self) {
            std::fs::set_permissions(&self.locked, Permissions::from_mode(0o700)).unwrap();
        }

        fn relock(&self) {
            std::fs::set_permissions(&self.locked, Permissions::from_mode(0o000)).unwrap();
        }
    }

    /// Point `dest` at a real file inside a directory the process may not
    /// search, so probing `dest` is denied but renaming onto it is not.
    ///
    /// Panics rather than passes when the denial does not take hold — as root,
    /// or on a filesystem that ignores POSIX mode bits, every test here would
    /// otherwise pass vacuously, which on a fail-open guard is worse than no
    /// test at all.
    fn deny_presence_of(tmp: TempDir, dest: PathBuf, content: &[u8]) -> DeniedDest {
        let locked = tmp.path().join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        let real = locked.join("real-output");
        std::fs::write(&real, content).unwrap();
        std::os::unix::fs::symlink(&real, &dest).unwrap();
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            content,
            "fixture is wrong — {} must resolve to the real output before the denial",
            dest.display()
        );

        std::fs::set_permissions(&locked, Permissions::from_mode(0o000)).unwrap();
        let restore = RestoreMode(locked.clone());

        match std::fs::metadata(&dest) {
            Ok(_) => panic!(
                "cannot exercise #5551: stat of {} still succeeds through a mode-000 parent. \
                 Run this suite as a non-root user on a filesystem that honours POSIX \
                 permission bits.",
                dest.display()
            ),
            Err(e) => assert_eq!(
                e.kind(),
                std::io::ErrorKind::PermissionDenied,
                "expected the locked parent to deny the probe, got {e}"
            ),
        }
        assert!(
            dest.parent().is_some_and(|p| std::fs::metadata(p).is_ok()),
            "fixture is wrong — the destination's own directory must stay writable, \
             or the pre-fix rename would fail for the wrong reason"
        );

        DeniedDest {
            _restore: restore,
            _tmp: tmp,
            locked,
            dest,
        }
    }

    /// #5551 (helpers.rs:116), real filesystem: the code-target destination
    /// cannot be stat-ed, so pre-fix the project-root stray is renamed over it
    /// and deleted. Uses no injected probe.
    #[tokio::test]
    async fn reconcile_against_leaves_an_unstattable_destination_intact() {
        let tmp = TempDir::new().unwrap();
        let project_root = canonical_dir(tmp.path(), "project").await;
        let code_target = canonical_dir(tmp.path(), "code").await;
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
        let stray = project_root.join("src/app.py");
        tokio::fs::create_dir_all(stray.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&stray, b"STRAY").await.unwrap();

        let fx = deny_presence_of(tmp, dest, b"REAL OUTPUT");
        let result = reconcile_code_outputs_against_from(
            &project_root,
            &assignments_dir,
            &code_target,
            &FsExistsProbe,
        )
        .await;

        assert_eq!(
            fx.resolved(),
            b"REAL OUTPUT",
            "the destination must still resolve to the real output"
        );
        assert!(stray.exists(), "the source must not be deleted");
        assert!(result.is_err(), "an unstattable destination must error");
    }

    /// #5551 (helpers.rs:210), real filesystem: the `out_dir` twin.
    #[tokio::test]
    async fn reconcile_from_leaves_an_unstattable_destination_intact() {
        let tmp = TempDir::new().unwrap();
        let project_root = canonical_dir(tmp.path(), "project").await;
        let out_dir = canonical_dir(tmp.path(), "out").await;
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
        let stray = project_root.join("src/app.py");
        tokio::fs::create_dir_all(stray.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&stray, b"STRAY").await.unwrap();

        let fx = deny_presence_of(tmp, dest, b"REAL OUTPUT");
        let result = reconcile_code_outputs_from(&project_root, &out_dir, &FsExistsProbe).await;

        assert_eq!(fx.resolved(), b"REAL OUTPUT");
        assert!(stray.exists(), "the source must not be deleted");
        assert!(result.is_err(), "an unstattable destination must error");
    }

    /// #5551 (helpers.rs:304), real filesystem: the destination is the plan
    /// manifest the code phase and QA read.
    #[tokio::test]
    async fn relocate_plan_leaves_an_unstattable_manifest_intact() {
        let tmp = TempDir::new().unwrap();
        let project_root = canonical_dir(tmp.path(), "project").await;
        let out_dir = canonical_dir(tmp.path(), "out").await;

        let root_asg = project_root.join("assignments.json");
        tokio::fs::write(&root_asg, b"STRAY").await.unwrap();

        let fx = deny_presence_of(tmp, out_dir.join("assignments.json"), b"AUTHORITATIVE");
        let result = relocate_plan_outputs_from(&project_root, &out_dir, &FsExistsProbe).await;

        assert_eq!(
            fx.resolved(),
            b"AUTHORITATIVE",
            "the plan manifest must not be overwritten"
        );
        assert!(root_asg.exists(), "the stray must not be deleted");
        assert!(result.is_err(), "an unstattable manifest must error");
    }
}
