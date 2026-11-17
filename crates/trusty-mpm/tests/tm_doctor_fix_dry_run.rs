//! `tm doctor --fix --dry-run` and `tm validate --repair --dry-run` write
//! nothing, and `--only` scopes the plan (#10425).
//!
//! Why: the repair modules' own dry-run tests prove each repair in isolation;
//! only the built binary proves the CLI wires `--dry-run` and `--only` through
//! and that the whole pass, report included, leaves the disk as it found it.
//! What: seeds a scratch HOME and project with a tm hook entry in
//! `.claude/settings.json` (a `hooks_contamination` finding), warms the scratch
//! HOME with a plain `tm doctor` (whose own first-run markers are not the
//! repair's), snapshots every path, mode and byte under both roots, runs the
//! dry run, and asserts the snapshot is unchanged and the plan was printed.
//! Test: `cargo test -p trusty-mpm --test integration tm_doctor_fix_dry_run::`.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::common;

/// Relative path to `(mode, bytes)`; a directory carries no bytes.
type Snapshot = BTreeMap<PathBuf, (u32, Option<Vec<u8>>)>;

/// Every entry under `root`: relative path to `(mode, bytes)`; a dir has none.
fn snapshot(root: &Path) -> Snapshot {
    let mut out = Snapshot::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read snapshot dir") {
            let path = entry.expect("dir entry").path();
            let meta = std::fs::symlink_metadata(&path).expect("metadata");
            let rel = path.strip_prefix(root).expect("under root").to_path_buf();
            let mode = meta.permissions().mode();
            if meta.is_dir() {
                out.insert(rel, (mode, None));
                stack.push(path);
            } else {
                out.insert(rel, (mode, Some(std::fs::read(&path).unwrap_or_default())));
            }
        }
    }
    out
}

/// A scratch HOME plus a project holding one tm hook entry.
struct FakeRoot {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl FakeRoot {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("scratch home");
        let project = tempfile::tempdir().expect("scratch project");
        let claude = project.path().join(".claude");
        std::fs::create_dir_all(&claude).expect("project .claude");
        let settings = serde_json::json!({
            "hooks": { "PreToolUse": [
                { "hooks": [{ "type": "command", "command": "tm hook" }] }
            ]}
        });
        std::fs::write(claude.join("settings.json"), settings.to_string()).expect("settings");
        Self { home, project }
    }

    /// Run `tm <args>` confined to this root; the search socket points into it.
    fn tm(&self, args: &[&str]) -> Output {
        common::tm_command_in(self.home.path())
            .env_remove("TRUSTY_DATA_DIR")
            .env(
                "TRUSTY_SEARCH_SOCKET",
                self.home.path().join("no-search.sock"),
            )
            .args(args)
            .current_dir(self.project.path())
            .output()
            .expect("spawn tm")
    }

    fn snapshot(&self) -> (Snapshot, Snapshot) {
        (snapshot(self.home.path()), snapshot(self.project.path()))
    }
}

/// Assert `before == after`, naming the first path that differs.
fn assert_unchanged(before: &Snapshot, after: &Snapshot, stdout: &str) {
    let changed: Vec<&PathBuf> = before
        .keys()
        .chain(after.keys())
        .filter(|k| before.get(*k) != after.get(*k))
        .collect();
    assert!(
        changed.is_empty(),
        "the dry run changed {changed:?}\nstdout:\n{stdout}"
    );
}

/// The regression #10425 exists for: a dry run with a planned repair, scoped
/// to one check, writes nothing and prints only that check's plan.
#[test]
fn tm_doctor_fix_dry_run_only_changes_nothing_and_plans_one_check() {
    let root = FakeRoot::new();
    root.tm(&["doctor"]);
    let (home0, proj0) = root.snapshot();

    let out = root.tm(&[
        "doctor",
        "--fix",
        "--dry-run",
        "--only",
        "hooks_contamination",
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "exit {:?}\nstderr:\n{stderr}",
        out.status
    );

    let (home1, proj1) = root.snapshot();
    assert_unchanged(&home0, &home1, &stdout);
    assert_unchanged(&proj0, &proj1, &stdout);

    let fix = stdout
        .split("\nfix (")
        .nth(1)
        .unwrap_or_else(|| panic!("no fix section.\nstdout:\n{stdout}"));
    assert!(fix.starts_with("dry run"), "{fix}");
    assert!(fix.contains("~ [hooks_contamination]"), "{fix}");
    assert!(
        fix.contains("tm doctor --fix --only hooks_contamination --yes"),
        "the apply hint must keep the scope: {fix}"
    );
    let tags: Vec<&str> = fix
        .lines()
        .filter_map(|l| l.split_once('[').and_then(|(_, r)| r.split_once(']')))
        .map(|(tag, _)| tag)
        .collect();
    assert!(
        tags.iter().all(|t| *t == "hooks_contamination"),
        "--only ran other repairs: {tags:?}"
    );
}

/// The unscoped dry run — every repair — also writes nothing under the root.
#[test]
fn tm_doctor_fix_dry_run_changes_nothing() {
    let root = FakeRoot::new();
    root.tm(&["doctor"]);
    let (home0, proj0) = root.snapshot();

    let out = root.tm(&["doctor", "--fix", "--dry-run"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (home1, proj1) = root.snapshot();
    assert_unchanged(&home0, &home1, &stdout);
    assert_unchanged(&proj0, &proj1, &stdout);
    assert!(stdout.contains("~ [hooks_contamination]"), "{stdout}");
}

/// An unknown `--only` id is refused before anything runs.
#[test]
fn tm_doctor_fix_only_rejects_an_unknown_check() {
    let root = FakeRoot::new();
    let out = root.tm(&["doctor", "--fix", "--only", "no_such_check"]);
    assert!(!out.status.success(), "an unknown id must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no_such_check"), "{stderr}");
}

/// `tm validate --repair --dry-run` prints the planned repair and writes nothing.
#[test]
fn tm_validate_repair_dry_run_changes_nothing() {
    let root = FakeRoot::new();
    root.tm(&["doctor"]);
    let (home0, proj0) = root.snapshot();

    let out = root.tm(&["validate", "--repair", "--dry-run"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        !out.status.success(),
        "an incomplete workspace still exits 1"
    );

    let (home1, proj1) = root.snapshot();
    assert_unchanged(&home0, &home1, &stdout);
    assert_unchanged(&proj0, &proj1, &stdout);
    assert!(
        stdout.contains("repair dry run — nothing will be written"),
        "{stdout}"
    );
    assert!(
        stdout.contains("would re-run the session preparation pipeline"),
        "{stdout}"
    );
}
