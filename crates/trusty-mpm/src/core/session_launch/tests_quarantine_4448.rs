//! #4448 call-site coverage for the shadowing-agent quarantine.
//!
//! Why a separate file rather than a few asserts inside an existing test: the
//! quarantine is a MOVE authorised on every session launch, and #4526's review
//! found its primary call site had zero coverage — deleting the call entirely
//! left the whole `trusty-mpm` suite green. These tests exist to fail when that
//! call site is deleted, reordered against retraction, or re-aimed at the
//! operator's own `~/.claude/agents`.
//!
//! Every fixture is a `TempDir` with `$HOME` overridden. Nothing here points the
//! sweep at a real deployed directory.

use super::tests::EnvVarGuard;
use super::*;
use tempfile::tempdir;

/// A composed trusty-mpm agent — the only shape the sweep's schema gate accepts.
/// The base preamble is the positive half of gate 4; without it the file reads
/// as hand-authored and is (correctly) refused.
fn tm_shadow(name: &str) -> String {
    format!(
        "---\nname: {name}\nrole: engineer\ndescription: 'Stale copy an older binary wrote.'\n\
         model: sonnet\n---\n\n{BASE_MARKER}\n\nComposed preamble.\n\n# {name}\n\nSTALE.\n"
    )
}

/// The composition root a real deployed agent carries — the REAL constant the
/// sweep gates on, not a local copy, so the pin below cannot drift from it.
use trusty_agents_common::agents::agent_schema::COMPOSED_BASE_MARKER as BASE_MARKER;

/// Seed a minimal bundled source so the deploy half of `prepare_session` has
/// something to do. The ROSTER the quarantine keys on additionally includes
/// every agent compiled into this binary (`core::bundle::ALL`), so
/// `rust-engineer` is a bundled name here regardless of this fixture.
fn seed_source(fw: &crate::core::paths::FrameworkPaths) {
    std::fs::create_dir_all(&fw.agents).unwrap();
    std::fs::write(
        fw.agents.join("base-engineer.md"),
        "---\nname: base-engineer\nrole: base-engineer\n---\n\n# Base Eng\n\nBASE.\n",
    )
    .unwrap();
    std::fs::write(
        fw.agents.join("rust-engineer.md"),
        "---\nname: rust-engineer\nrole: engineer\nextends: base-engineer\n---\n\n# Rust\n\nLEAF.\n",
    )
    .unwrap();
}

/// A managed-workspace `fw` plus a project directory, ready for
/// `prepare_session`.
fn managed_fixture(
    tmp_home: &std::path::Path,
    project: &std::path::Path,
) -> crate::core::paths::FrameworkPaths {
    let mut fw = crate::core::paths::FrameworkPaths::under(tmp_home);
    fw.trusty_mpm_root = None;
    seed_source(&fw);
    let _ = project;
    fw
}

/// Stage a file in the project's own agent tier.
fn stage_project_agent(project: &std::path::Path, file_name: &str, content: &str) -> PathBuf {
    let tier = project.join(".claude").join("agents");
    std::fs::create_dir_all(&tier).unwrap();
    let path = tier.join(file_name);
    std::fs::write(&path, content).unwrap();
    path
}

fn git(project: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(project)
        .args(args)
        .output()
        .expect("git must be available");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// THE APPROVAL PATH AT THE CALL SITE. Deleting the `quarantine_workspace_shadows`
/// call from `prepare_session_inner` must fail this.
#[test]
#[serial_test::serial]
fn prepare_session_quarantines_a_shadowing_workspace_agent() {
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = managed_fixture(tmp_home.path(), project);

    let content = tm_shadow("rust-engineer");
    let staged = stage_project_agent(project, "rust-engineer.md", &content);

    prepare_session(&fw, project).expect("prep succeeds");

    assert!(
        !staged.exists(),
        "the shadowing project-tier copy must not survive a session launch"
    );
    let disabled = staged.with_file_name("rust-engineer.md.disabled");
    assert!(disabled.exists(), "an inert sibling must remain");
    assert_eq!(
        std::fs::read_to_string(&disabled).unwrap(),
        content,
        "the moved file must be byte-identical"
    );

    // A verified backup and a receipt, outside the agent tier.
    let backups = quarantine_shadows::backup_root(project);
    assert!(backups.is_dir(), "a backup root must exist at {backups:?}");
    let receipts: Vec<PathBuf> = walk(&backups)
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "RECEIPT.md"))
        .collect();
    assert_eq!(receipts.len(), 1, "exactly one receipt: {receipts:?}");
    let receipt = std::fs::read_to_string(&receipts[0]).unwrap();
    assert!(receipt.contains("rust-engineer"), "{receipt}");
    assert!(receipt.contains("## Moved (1)"), "{receipt}");

    let backed_up: Vec<PathBuf> = walk(&backups)
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "rust-engineer.md"))
        .collect();
    assert_eq!(backed_up.len(), 1, "exactly one backup: {backed_up:?}");
    assert_eq!(std::fs::read_to_string(&backed_up[0]).unwrap(), content);
}

/// THE GUARD, mirroring `prepare_session_never_retracts_the_operator_home_agents_tier`.
///
/// `prepare_session` is called with a HOME-TIER `FrameworkPaths::default()` on
/// two production paths — non-git `tm session start` and the TUI `/connect` —
/// where `fw.claude_agents_dir()` IS the operator's `~/.claude/agents`. Aiming
/// the sweep at that field would move files out of a Claude Code install
/// trusty-mpm does not own, and it is strictly MORE dangerous there than
/// retraction: retraction can only delete ledger-tracked files (that tier has
/// none), while this sweep moves UNTRACKED ones — which is exactly what that
/// directory is full of. The fixture is deliberately an UNTRACKED file, since a
/// manifest-tracked one is invisible to the quarantine by construction and the
/// test would prove nothing.
#[test]
#[serial_test::serial]
fn prepare_session_never_quarantines_the_operator_home_agents_tier() {
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = managed_fixture(tmp_home.path(), project);

    // An untracked, trusty-mpm-schema copy on a bundled name, sitting in the
    // operator's own home tier — every gate would pass if it were aimed there.
    let home_agents = fw.claude_agents_dir();
    std::fs::create_dir_all(&home_agents).unwrap();
    let content = tm_shadow("rust-engineer");
    let home_copy = home_agents.join("rust-engineer.md");
    std::fs::write(&home_copy, &content).unwrap();

    prepare_session(&fw, project).expect("prep succeeds");

    assert!(
        home_copy.exists(),
        "the operator's home-tier file must never be quarantined"
    );
    assert_eq!(
        std::fs::read_to_string(&home_copy).unwrap(),
        content,
        "it must survive byte-identical"
    );
    assert!(
        !home_agents.join("rust-engineer.md.disabled").exists(),
        "no inert sibling may appear in the operator's home tier"
    );
    assert!(
        !quarantine_shadows::backup_root(tmp_home.path()).exists(),
        "no quarantine backup root may appear under the operator's home"
    );
}

/// GATE 3, end to end. The repository's claim is what stands in for the closed
/// `Origin::Project` (#4443), and #4526 would have renamed this file.
#[test]
#[serial_test::serial]
fn prepare_session_never_quarantines_a_git_tracked_workspace_agent() {
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = managed_fixture(tmp_home.path(), project);

    git(project, &["init", "-q"]);
    let content = tm_shadow("rust-engineer");
    let staged = stage_project_agent(project, "rust-engineer.md", &content);
    // `--force`: a project may gitignore `.claude/agents/`, and an EXPLICIT
    // track still claims the file.
    git(
        project,
        &["add", "--force", ".claude/agents/rust-engineer.md"],
    );

    prepare_session(&fw, project).expect("prep succeeds");

    assert!(staged.exists(), "a git-tracked project agent must survive");
    assert_eq!(std::fs::read_to_string(&staged).unwrap(), content);
    assert!(
        !staged.with_file_name("rust-engineer.md.disabled").exists(),
        "no inert sibling may be created for a tracked file"
    );
}

/// GATE 4, end to end. claude-mpm ships `rust-engineer.md` under a different
/// schema; it is another live project's file.
#[test]
#[serial_test::serial]
fn prepare_session_never_quarantines_a_claude_mpm_workspace_agent() {
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = managed_fixture(tmp_home.path(), project);

    let content = "---\nname: rust-engineer\ndescription: \"Use this agent when…\"\n\
                   model: sonnet\neffort: balanced\nagent_type: engineer\nversion: \"1.0.0\"\n\
                   ---\n# Rust Engineer\n\n**Inherits from**: BASE_AGENT.md\n";
    let staged = stage_project_agent(project, "rust-engineer.md", content);

    prepare_session(&fw, project).expect("prep succeeds");

    assert!(staged.exists(), "a claude-mpm file must never be moved");
    assert_eq!(std::fs::read_to_string(&staged).unwrap(), content);
}

/// A clean project gains nothing — no backup root, no receipt. This runs on
/// EVERY launch, so a sweep that littered would be a visible regression.
#[test]
#[serial_test::serial]
fn prepare_session_leaves_a_clean_project_clean() {
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = managed_fixture(tmp_home.path(), project);

    let custom = stage_project_agent(project, "acme.md", &tm_shadow("acme-house-agent"));

    prepare_session(&fw, project).expect("prep succeeds");

    assert!(custom.exists(), "a project's own agent must survive");
    assert!(
        !quarantine_shadows::backup_root(project).exists(),
        "a clean project must gain no quarantine directory"
    );
}

/// #6649 deliverable 1: hand-place a bundled-named agent, launch, and the
/// operator gets a line. Deleting `launch_asset_notices` from
/// `prepare_session_inner` must fail this.
#[test]
#[serial_test::serial]
fn prepare_session_reports_a_quarantined_agent_as_a_launch_notice() {
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = managed_fixture(tmp_home.path(), project);

    stage_project_agent(project, "rust-engineer.md", &tm_shadow("rust-engineer"));

    let report = prepare_session(&fw, project).expect("prep succeeds");

    let agents_line = report
        .asset_notices
        .iter()
        .find(|n| n.starts_with("agents quarantined"))
        .unwrap_or_else(|| {
            panic!(
                "a quarantined agent must reach the operator: {:?}",
                report.asset_notices
            )
        });
    assert!(
        agents_line.contains("rust-engineer"),
        "the line must NAME what moved: {agents_line}"
    );
}

/// #6649 deliverable 1, the other half: a clean project adds nothing to the
/// terminal. This runs on every launch, so silence is the contract — and
/// deliverable 4's negative case rides on the same fixture, since `acme.md`
/// declares a name no roster carries.
#[test]
#[serial_test::serial]
fn prepare_session_on_a_clean_project_reports_no_asset_notice() {
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = managed_fixture(tmp_home.path(), project);

    stage_project_agent(project, "acme.md", &tm_shadow("acme-house-agent"));

    let report = prepare_session(&fw, project).expect("prep succeeds");

    assert!(
        report.asset_notices.is_empty(),
        "a clean launch must say nothing: {:?}",
        report.asset_notices
    );
}

/// #6649 deliverable 3, at the launch line: a same-stem duplicate is counted.
#[test]
#[serial_test::serial]
fn prepare_session_reports_a_same_stem_duplicate() {
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = managed_fixture(tmp_home.path(), project);

    // An operator-authored directory beside a file of the same stem. Neither is
    // a bundled name, so nothing quarantines or sweeps it — only the duplicate
    // check can see this.
    let tier = project.join(".claude").join("agents");
    std::fs::create_dir_all(tier.join("acme-house-agent")).unwrap();
    stage_project_agent(project, "acme-house-agent.md", "---\nname: a\n---\n");

    let report = prepare_session(&fw, project).expect("prep succeeds");

    let line = report
        .asset_notices
        .iter()
        .find(|n| n.starts_with("duplicates"))
        .unwrap_or_else(|| panic!("expected a duplicates line: {:?}", report.asset_notices));
    assert!(line.contains("acme-house-agent"), "{line}");
}

/// The sync-assets path authorises the same move and must be covered too —
/// deleting its call site alone must fail something.
#[test]
fn sync_assets_quarantines_a_shadowing_workspace_agent() {
    let tmp = tempdir().unwrap();
    let mut fw = crate::core::paths::FrameworkPaths::under(tmp.path().join("home"));
    fw.trusty_mpm_root = None;
    seed_source(&fw);

    let project = tmp.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();
    let mut ws_fw = fw.clone();
    ws_fw.claude_agents = project.join(".claude").join("agents");
    ws_fw.claude_skills = project.join(".claude").join("skills");

    let content = tm_shadow("rust-engineer");
    let staged = stage_project_agent(&project, "rust-engineer.md", &content);

    let report = super::sync_assets::sync_session_assets(&ws_fw, &project).unwrap();

    assert!(!staged.exists(), "the shadowing copy must be moved");
    assert!(staged.with_file_name("rust-engineer.md.disabled").exists());
    let summary = report
        .quarantine_summary
        .expect("sync-assets must report the move");
    assert!(summary.contains("quarantined 1"), "{summary}");
    assert!(
        summary.contains("nothing was deleted"),
        "the summary must state the safety property: {summary}"
    );
}

/// A clean sync-assets run says nothing about the quarantine.
#[test]
fn sync_assets_is_silent_when_nothing_shadows() {
    let tmp = tempdir().unwrap();
    let mut fw = crate::core::paths::FrameworkPaths::under(tmp.path().join("home"));
    fw.trusty_mpm_root = None;
    seed_source(&fw);

    let project = tmp.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();
    let mut ws_fw = fw.clone();
    ws_fw.claude_agents = project.join(".claude").join("agents");
    ws_fw.claude_skills = project.join(".claude").join("skills");

    let report = super::sync_assets::sync_session_assets(&ws_fw, &project).unwrap();
    assert_eq!(report.quarantine_summary, None);
}

/// The backup root is OUTSIDE the agent tier. A backup nested inside the tier
/// it was taken from is one recursive loader away from being an agent again.
#[test]
fn the_backup_root_is_not_inside_the_agent_tier() {
    let project = std::path::Path::new("/tmp/some-project");
    assert!(
        !quarantine_shadows::backup_root(project)
            .starts_with(quarantine_shadows::workspace_tier(project))
    );
}

/// Every regular file under `root`, recursively. Test-only.
fn walk(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// THE ASSET PIN for `agent_schema`'s `COMPOSED_BASE_MARKER`. That constant
/// lives in `trusty-agents-common`, but the file it describes is shipped HERE,
/// so this is where the two can actually be compared. If the heading is ever
/// edited, the sweep stops recognising its own artifacts and silently becomes a
/// no-op — this fails first.
#[test]
fn the_bundled_base_agent_carries_the_composition_marker() {
    let base = trusty_agents_common::agent_assets::BASE_AGENT;
    let heading = base
        .lines()
        .find(|l| l.starts_with("# "))
        .expect("BASE-AGENT.md must open with a level-1 heading");
    // EQUALITY, not `contains`: a shortened marker still matches every composed
    // file, so `contains` would tolerate silently loosening the predicate.
    assert_eq!(
        heading, BASE_MARKER,
        "crates/trusty-mpm/src/assets/agents/BASE-AGENT.md's heading and \
         trusty-agents-common's COMPOSED_BASE_MARKER have drifted. They must be \
         byte-identical, or the #4448 quarantine either refuses every file it exists \
         to move (heading changed) or matches too loosely (constant shortened)."
    );
}

/// Every bundled agent's `extends:` chain must root at `base-agent`, which is
/// what makes the marker universal rather than incidental. A new agent that
/// extends nothing would deploy without the preamble and become unsweepable.
#[test]
fn every_bundled_agent_roots_at_base_agent() {
    let dir =
        std::path::Path::new(trusty_agents_common::agent_assets::AGENT_ASSETS_DIR).to_path_buf();
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("the bundled agent source directory must exist")
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".md"))
        .collect();
    assert!(
        entries.len() > 20,
        "roster looks truncated: {}",
        entries.len()
    );

    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let body = std::fs::read_to_string(entry.path()).expect("read agent");
        let declares_extends = body.lines().any(|l| l.starts_with("extends:"));
        let is_root = name == "BASE-AGENT.md";
        assert!(
            declares_extends || is_root,
            "`{name}` declares no `extends:` and is not BASE-AGENT.md, so its composed output \
             would carry no base preamble and the #4448 quarantine could never recognise it"
        );
    }
}

/// The never-deletes pin, extended to the module that AIMS the sweep. #4448
/// review noted `trusty-agents-common`'s own pin covers the four modules that
/// perform the move but not this one, which chooses the target. It has no
/// filesystem calls today; this is what keeps it that way.
#[test]
fn the_wiring_module_never_deletes() {
    let source = include_str!("quarantine_shadows.rs");
    for needle in [
        concat!("remove_", "file"),
        concat!("remove_", "dir"),
        concat!("set_", "len"),
    ] {
        let calls: Vec<&str> = source
            .lines()
            .filter(|line| {
                let t = line.trim_start();
                !t.starts_with("//") && line.contains(needle)
            })
            .collect();
        assert!(
            calls.is_empty(),
            "quarantine_shadows.rs must never call `{needle}` — it aims the sweep, \
             it does not destroy. Offending lines: {calls:?}"
        );
    }
}
