//! Black-box proof that a real `tcode` run materializes the agent roster to
//! `<project>/.trusty-code/agents/` with a manifest (#2074, epic #2892).
//!
//! Why: `agents::deploy`'s unit tests call `ensure_roster_deployed` directly, so
//! they prove the deployer adapter works but not that it is WIRED into a path a
//! user drives. The acceptance bar #2074 names is the filesystem after a real
//! run, read with `std::fs::read_dir` rather than through `agents.list` — a
//! catalog listing would report the same names from the in-memory embed and
//! could not tell a materialized roster from an unmaterialized one.
//!
//! What: [`run_task_materializes_the_roster_to_disk`] runs the real binary
//! against a clean tempdir with no `.trusty-code/`, `.claude/`, or `.open-mpm/`
//! and asserts every roster file plus the manifest exist afterwards.
//! [`hand_edited_agent_file_survives_a_second_run`] hand-edits one deployed file
//! and proves a second run leaves it byte-identical.
//! [`compat_root_is_never_shadowed`] proves a project whose `.claude/agents/`
//! currently wins gets nothing written, so its catalog keeps driving.
//! [`paths_show_reports_the_roster_manifest`] proves the diagnostic surface
//! reports the ledger.
//! Test: this file IS the test.

mod support;

use std::path::Path;

/// Every `.md` filename directly inside `dir`, sorted. Empty when absent.
fn md_files(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.ends_with(".md"))
        .collect();
    names.sort();
    names
}

/// Run `tcode run-task engineer "say hi" --project <dir> --json` offline.
fn run_task(project: &Path) -> std::process::Output {
    let output = support::tcode_command()
        .args([
            "run-task",
            "engineer",
            "say hi",
            "--project",
            &project.display().to_string(),
            "--json",
        ])
        .env("TCODE_MOCK_LLM", "echo")
        .output()
        .expect("spawn tcode run-task");
    assert!(
        output.status.success(),
        "tcode run-task must exit 0, got {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn run_task_materializes_the_roster_to_disk() {
    let project = tempfile::tempdir().expect("project tempdir");
    let agents_dir = project.path().join(".trusty-code").join("agents");
    assert!(
        md_files(&agents_dir).is_empty(),
        "the fixture must start with no deployed roster"
    );

    run_task(project.path());

    let on_disk = md_files(&agents_dir);
    assert!(
        on_disk.len() >= 30,
        "the whole roster must land on disk, got {} file(s): {on_disk:?}",
        on_disk.len()
    );
    for expected in ["engineer.md", "pm.md", "rust-engineer.md", "code-critic.md"] {
        assert!(
            on_disk.contains(&expected.to_string()),
            "'{expected}' must be materialized: {on_disk:?}"
        );
    }
    assert!(
        !on_disk
            .iter()
            .any(|n| n.to_lowercase().starts_with("base-")),
        "composition bases must never be deployed: {on_disk:?}"
    );
    assert!(
        agents_dir.join(".trusty-mpm-manifest.json").is_file(),
        "a manifest must exist beside the deployed files"
    );
}

#[test]
fn hand_edited_agent_file_survives_a_second_run() {
    let project = tempfile::tempdir().expect("project tempdir");
    run_task(project.path());

    let edited = project
        .path()
        .join(".trusty-code")
        .join("agents")
        .join("rust-engineer.md");
    let hand_edit = "---\nname: rust-engineer\nmodel: marker/hand-edited\n---\n\nMine now.\n";
    std::fs::write(&edited, hand_edit).expect("hand-edit a deployed agent");

    run_task(project.path());

    assert_eq!(
        std::fs::read_to_string(&edited).expect("read back"),
        hand_edit,
        "a hand-edited deployed agent must survive a second run byte-identical"
    );
}

#[test]
fn compat_root_is_never_shadowed() {
    let project = support::project_with_agents();
    run_task(project.path());

    assert!(
        md_files(&project.path().join(".trusty-code").join("agents")).is_empty(),
        "a project whose .claude/agents/ wins must have nothing written into \
         .trusty-code/agents/, or its own catalog would be silently demoted"
    );
}

#[test]
fn paths_show_reports_the_roster_manifest() {
    let project = tempfile::tempdir().expect("project tempdir");

    let before = support::tcode_command()
        .args([
            "paths",
            "show",
            "--project",
            &project.path().display().to_string(),
            "--json",
        ])
        .output()
        .expect("spawn tcode paths show");
    assert!(before.status.success(), "paths show must exit 0");
    let doc: serde_json::Value =
        serde_json::from_slice(&before.stdout).expect("paths show --json must emit one document");
    assert_eq!(doc["roster_manifest"]["status"], "absent");

    run_task(project.path());

    let after = support::tcode_command()
        .args([
            "paths",
            "show",
            "--project",
            &project.path().display().to_string(),
            "--json",
        ])
        .output()
        .expect("spawn tcode paths show");
    let doc: serde_json::Value =
        serde_json::from_slice(&after.stdout).expect("paths show --json must emit one document");
    assert_eq!(doc["roster_manifest"]["status"], "present");
    assert!(
        doc["roster_manifest"]["managed"].as_u64().unwrap_or(0) >= 30,
        "the ledger must track the deployed roster: {doc}"
    );
}
