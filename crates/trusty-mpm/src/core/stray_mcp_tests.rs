//! Tests for stray-`.mcp.json` detection and quarantine.
//!
//! Why: the repair renames operator-visible config out of the way, so the
//! tests that carry the weight are the REFUSALS — an unattributed file, one
//! edited after tm wrote it, an unreadable ledger, a symlink, and the
//! workspace's own file must each survive a run with `--yes`. A happy-path-only
//! suite here would prove nothing about the property that matters.
//!
//! Every test scopes its assertions to paths inside its own tempdir: the scan
//! set always includes the real temp roots, and a developer machine may
//! genuinely have a `/tmp/.mcp.json` (that is what motivated this work), so a
//! test asserting on the whole result set would be machine-dependent.
//! Test: this file.

use super::*;

use crate::core::mcp_provenance::{ledger_path, record_write};

/// A framework root, a home, and a workspace two levels under it.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    workspace: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(".trusty-mpm");
    let home = tmp.path().join("home");
    let workspace = home.join("projects").join("thing");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    Fixture {
        _tmp: tmp,
        root,
        home,
        workspace,
    }
}

/// Write a `.mcp.json` at `dir` and return its path and content.
fn write_mcp(dir: &Path, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(MCP_JSON);
    std::fs::write(&path, body).unwrap();
    path
}

const SAMPLE: &str = r#"{"mcpServers":{"apex":{"type":"http","url":"https://x"},"trusty-search":{"type":"stdio","command":"trusty-search"}}}"#;

/// Steps whose path lies inside `root` — the machine's real temp roots are
/// always scanned and must never make a test flaky.
fn scoped(steps: Vec<RepairStep>, root: &Path) -> Vec<RepairStep> {
    steps
        .into_iter()
        .filter(|s| s.path.starts_with(root))
        .collect()
}

// ---------------------------------------------------------------- scan bound

#[test]
fn scan_dirs_walks_ancestors_up_to_home() {
    let home = PathBuf::from("/u/me");
    let ws = home.join("projects").join("thing");
    let dirs = scan_dirs(Some(&ws), &home);
    assert!(dirs.contains(&home.join("projects")));
    assert!(dirs.contains(&home));
    assert!(
        !dirs.contains(&PathBuf::from("/u")),
        "the walk must stop at home — above it is other users' business: {dirs:?}"
    );
}

#[test]
fn scan_dirs_excludes_the_workspace_itself() {
    let home = PathBuf::from("/u/me");
    let ws = home.join("thing");
    assert!(
        !scan_dirs(Some(&ws), &home).contains(&ws),
        "the project's own managed .mcp.json is not a stray"
    );
}

#[test]
fn scan_dirs_never_reaches_the_filesystem_root() {
    // A `.mcp.json` at `/` is system configuration, and tm never puts a
    // workspace there.
    let home = PathBuf::from("/nowhere");
    let dirs = scan_dirs(Some(Path::new("/a/b")), &home);
    assert!(
        !dirs.contains(&PathBuf::from("/")),
        "the filesystem root must never be scanned: {dirs:?}"
    );
}

#[test]
fn scan_dirs_stops_at_the_depth_cap_outside_home() {
    // A workspace outside home has no home ceiling, so the cap is what keeps
    // the scan bounded and away from system directories.
    let deep = PathBuf::from("/a/b/c/d/e/f/g/h/i/j/k/l");
    let dirs = scan_dirs(Some(&deep), Path::new("/nowhere"));
    let ancestors = dirs.iter().filter(|d| d.starts_with("/a")).count();
    assert!(
        ancestors <= MAX_ANCESTOR_DEPTH,
        "the walk must respect the depth cap, walked {ancestors}"
    );
}

#[test]
fn scan_dirs_includes_the_temp_roots() {
    // The temp roots are not ancestors of any workspace — they are ancestors
    // of agent scratchpad cwds, which is exactly why a file there reaches so
    // many sessions and why it must be scanned explicitly.
    let dirs = scan_dirs(None, Path::new("/u/me"));
    assert!(dirs.contains(&std::env::temp_dir()));
    assert!(dirs.contains(&PathBuf::from("/tmp")));
}

#[test]
fn scan_dirs_has_no_duplicates() {
    let dirs = scan_dirs(Some(Path::new("/tmp/ws")), Path::new("/tmp"));
    let mut sorted = dirs.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), dirs.len(), "duplicate scan dirs: {dirs:?}");
}

// --------------------------------------------------------------------- scan

#[test]
fn scan_finds_a_stray_above_the_workspace() {
    let f = fixture();
    let stray = write_mcp(&f.home.join("projects"), SAMPLE);
    let found = scan(Some(&f.workspace), &f.home, &LedgerLoad::Missing);
    assert!(found.iter().any(|s| s.path == stray));
}

#[test]
fn scan_ignores_the_workspaces_own_file() {
    let f = fixture();
    let own = write_mcp(&f.workspace, SAMPLE);
    let found = scan(Some(&f.workspace), &f.home, &LedgerLoad::Missing);
    assert!(!found.iter().any(|s| s.path == own));
}

#[test]
fn scan_lists_the_declared_servers() {
    let f = fixture();
    write_mcp(&f.home.join("projects"), SAMPLE);
    let found = scan(Some(&f.workspace), &f.home, &LedgerLoad::Missing);
    let stray = found
        .iter()
        .find(|s| s.path.starts_with(&f.home))
        .expect("stray");
    assert_eq!(stray.servers, vec!["apex", "trusty-search"]);
}

#[test]
fn declared_servers_tolerates_garbage() {
    let f = fixture();
    write_mcp(&f.home.join("projects"), "not json at all");
    let found = scan(Some(&f.workspace), &f.home, &LedgerLoad::Missing);
    let stray = found
        .iter()
        .find(|s| s.path.starts_with(&f.home))
        .expect("stray");
    assert!(
        stray.servers.is_empty(),
        "an unparseable file is a display gap, not a panic"
    );
}

#[cfg(unix)]
#[test]
fn scan_reports_a_symlink_as_unknown() {
    // Following it would mean acting on a file somewhere else entirely.
    let f = fixture();
    let real = write_mcp(&f.home.join("real"), SAMPLE);
    let link_dir = f.home.join("projects");
    std::fs::create_dir_all(&link_dir).unwrap();
    std::os::unix::fs::symlink(&real, link_dir.join(MCP_JSON)).unwrap();

    let found = scan(Some(&f.workspace), &f.home, &LedgerLoad::Missing);
    let link = found
        .iter()
        .find(|s| s.path == link_dir.join(MCP_JSON))
        .expect("the symlink must still be reported");
    assert!(matches!(link.provenance, Provenance::Unknown(_)));
}

// ------------------------------------------------------------- sweep repair

#[test]
fn sweep_quarantines_a_tm_written_stray() {
    let f = fixture();
    let stray = write_mcp(&f.home.join("projects"), SAMPLE);
    record_write(&f.root, &stray, SAMPLE).unwrap();

    let steps = scoped(
        quarantine_strays(&f.root, Some(&f.workspace), &f.home, RepairMode::Apply),
        f.home.parent().unwrap(),
    );
    assert_eq!(steps.len(), 1, "{steps:?}");
    let StepStatus::Applied { backup } = &steps[0].status else {
        panic!("a ledger-proven stray must be quarantined, got {steps:?}");
    };
    assert!(
        !stray.exists(),
        "the .mcp.json must no longer be discovered"
    );
    let dest = backup
        .as_ref()
        .expect("the rename destination is the backup");
    assert!(dest.exists(), "and its bytes must survive at the new name");
    assert_eq!(
        std::fs::read_to_string(dest).unwrap(),
        SAMPLE,
        "quarantine must not alter a single byte — nothing here deletes"
    );
}

#[test]
fn sweep_releases_the_ledger_claim_after_quarantine() {
    let f = fixture();
    let stray = write_mcp(&f.home.join("projects"), SAMPLE);
    record_write(&f.root, &stray, SAMPLE).unwrap();
    quarantine_strays(&f.root, Some(&f.workspace), &f.home, RepairMode::Apply);

    let text = std::fs::read_to_string(ledger_path(&f.root)).unwrap();
    assert!(
        !text.contains(&stray.display().to_string()),
        "a claim must not outlive the file it describes: {text}"
    );
}

#[test]
fn sweep_refuses_an_unattributed_stray() {
    // The case that covers every file already on disk, including the observed
    // /private/tmp one. Content is NOT provenance: this file declares
    // trusty-search and is still refused.
    let f = fixture();
    let stray = write_mcp(&f.home.join("projects"), SAMPLE);

    let steps = scoped(
        quarantine_strays(&f.root, Some(&f.workspace), &f.home, RepairMode::Apply),
        f.home.parent().unwrap(),
    );
    assert_eq!(steps.len(), 1, "{steps:?}");
    let StepStatus::Refused(why) = &steps[0].status else {
        panic!("an unattributed stray must be refused, got {steps:?}");
    };
    assert!(why.contains("no record"), "the refusal must say why: {why}");
    assert!(stray.exists(), "and the file must be untouched");
    assert_eq!(std::fs::read_to_string(&stray).unwrap(), SAMPLE);
}

#[test]
fn sweep_refuses_a_stray_edited_after_tm_wrote_it() {
    // tm wrote it, then somebody changed it — the current bytes are theirs.
    let f = fixture();
    let stray = write_mcp(&f.home.join("projects"), SAMPLE);
    record_write(&f.root, &stray, SAMPLE).unwrap();
    let edited = r#"{"mcpServers":{"my-own-server":{"type":"stdio","command":"mine"}}}"#;
    std::fs::write(&stray, edited).unwrap();

    let steps = scoped(
        quarantine_strays(&f.root, Some(&f.workspace), &f.home, RepairMode::Apply),
        f.home.parent().unwrap(),
    );
    let StepStatus::Refused(why) = &steps[0].status else {
        panic!("an edited stray must be refused, got {steps:?}");
    };
    assert!(why.contains("changed afterwards"), "reason: {why}");
    assert_eq!(
        std::fs::read_to_string(&stray).unwrap(),
        edited,
        "the operator's edit must survive"
    );
}

#[test]
fn sweep_refuses_when_the_ledger_is_unreadable() {
    // Failure path: with attribution unavailable, everything is refused —
    // a corrupt ledger must never read as "tm wrote none of these".
    let f = fixture();
    let stray = write_mcp(&f.home.join("projects"), SAMPLE);
    std::fs::write(ledger_path(&f.root), "{ not json").unwrap();

    let steps = scoped(
        quarantine_strays(&f.root, Some(&f.workspace), &f.home, RepairMode::Apply),
        f.home.parent().unwrap(),
    );
    let StepStatus::Refused(why) = &steps[0].status else {
        panic!("an unreadable ledger must refuse, got {steps:?}");
    };
    assert!(why.contains("unreadable"), "reason: {why}");
    assert!(stray.exists());
}

#[cfg(unix)]
#[test]
fn sweep_refuses_a_symlink() {
    let f = fixture();
    let real = write_mcp(&f.home.join("real"), SAMPLE);
    let link_dir = f.home.join("projects");
    std::fs::create_dir_all(&link_dir).unwrap();
    std::os::unix::fs::symlink(&real, link_dir.join(MCP_JSON)).unwrap();

    let steps = scoped(
        quarantine_strays(&f.root, Some(&f.workspace), &f.home, RepairMode::Apply),
        f.home.parent().unwrap(),
    );
    let link_step = steps
        .iter()
        .find(|s| s.path == link_dir.join(MCP_JSON))
        .expect("the symlink must produce a step");
    assert!(matches!(link_step.status, StepStatus::Refused(_)));
    assert!(real.exists(), "the symlink target must be untouched");
}

#[test]
fn sweep_dry_run_writes_nothing() {
    let f = fixture();
    let stray = write_mcp(&f.home.join("projects"), SAMPLE);
    record_write(&f.root, &stray, SAMPLE).unwrap();

    let steps = scoped(
        quarantine_strays(&f.root, Some(&f.workspace), &f.home, RepairMode::DryRun),
        f.home.parent().unwrap(),
    );
    assert!(matches!(steps[0].status, StepStatus::Planned));
    assert!(stray.exists(), "a dry run must not move the file");
    assert!(
        steps[0].what.contains("quarantined-"),
        "the preview must name the destination: {}",
        steps[0].what
    );
}

#[test]
fn sweep_never_touches_the_workspaces_own_file() {
    // Even with a ledger record proving tm wrote it — that file is live
    // managed config, and removing it breaks the current project.
    let f = fixture();
    let own = write_mcp(&f.workspace, SAMPLE);
    record_write(&f.root, &own, SAMPLE).unwrap();

    quarantine_strays(&f.root, Some(&f.workspace), &f.home, RepairMode::Apply);
    assert!(own.exists(), "the workspace's own .mcp.json must survive");
}

// ---------------------------------------------------------- explicit repair

#[test]
fn explicit_quarantines_an_unattributed_file() {
    // Naming the path IS the attribution — the operator supplies what the
    // ledger cannot for a pre-ledger file.
    let f = fixture();
    let stray = write_mcp(&f.home.join("projects"), SAMPLE);

    let step = quarantine_explicit(&f.root, Some(&f.workspace), &stray, RepairMode::Apply);
    let StepStatus::Applied { backup } = &step.status else {
        panic!("an explicitly named file must be quarantined, got {step:?}");
    };
    assert!(!stray.exists());
    assert_eq!(
        std::fs::read_to_string(backup.as_ref().unwrap()).unwrap(),
        SAMPLE
    );
}

#[test]
fn explicit_refuses_a_non_mcp_path() {
    // This command must never become a general-purpose file renamer.
    let f = fixture();
    let other = f.home.join("important.txt");
    std::fs::create_dir_all(&f.home).unwrap();
    std::fs::write(&other, "keep me").unwrap();

    let step = quarantine_explicit(&f.root, Some(&f.workspace), &other, RepairMode::Apply);
    assert!(matches!(step.status, StepStatus::Refused(_)), "{step:?}");
    assert!(other.exists(), "an unrelated file must never be renamed");
}

#[test]
fn explicit_refuses_the_workspaces_own_file() {
    let f = fixture();
    let own = write_mcp(&f.workspace, SAMPLE);
    let step = quarantine_explicit(&f.root, Some(&f.workspace), &own, RepairMode::Apply);
    let StepStatus::Refused(why) = &step.status else {
        panic!("the workspace's own file must be refused, got {step:?}");
    };
    assert!(why.contains("workspace's own"), "reason: {why}");
    assert!(own.exists());
}

#[cfg(unix)]
#[test]
fn explicit_refuses_a_symlink() {
    let f = fixture();
    let real = write_mcp(&f.home.join("real"), SAMPLE);
    let link_dir = f.home.join("link");
    std::fs::create_dir_all(&link_dir).unwrap();
    let link = link_dir.join(MCP_JSON);
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let step = quarantine_explicit(&f.root, Some(&f.workspace), &link, RepairMode::Apply);
    assert!(matches!(step.status, StepStatus::Refused(_)), "{step:?}");
    assert!(link.exists() && real.exists());
}

#[test]
fn explicit_refuses_a_missing_file() {
    let f = fixture();
    let step = quarantine_explicit(
        &f.root,
        Some(&f.workspace),
        &f.home.join("nope").join(MCP_JSON),
        RepairMode::Apply,
    );
    assert!(matches!(step.status, StepStatus::Refused(_)), "{step:?}");
}

#[test]
fn explicit_refuses_a_directory_named_mcp_json() {
    let f = fixture();
    let dir = f.home.join("odd").join(MCP_JSON);
    std::fs::create_dir_all(&dir).unwrap();
    let step = quarantine_explicit(&f.root, Some(&f.workspace), &dir, RepairMode::Apply);
    assert!(matches!(step.status, StepStatus::Refused(_)), "{step:?}");
    assert!(dir.is_dir());
}

#[test]
fn explicit_dry_run_writes_nothing() {
    let f = fixture();
    let stray = write_mcp(&f.home.join("projects"), SAMPLE);
    let step = quarantine_explicit(&f.root, Some(&f.workspace), &stray, RepairMode::DryRun);
    assert!(matches!(step.status, StepStatus::Planned));
    assert!(stray.exists());
}

#[test]
fn quarantine_destination_does_not_collide() {
    // Two runs in the same second must not have the second overwrite the
    // first's quarantined copy — that would destroy the very bytes the
    // rename-instead-of-delete design exists to preserve.
    let f = fixture();
    let dir = f.home.join("projects");
    let first = write_mcp(&dir, "first");
    quarantine_explicit(&f.root, Some(&f.workspace), &first, RepairMode::Apply);
    let second = write_mcp(&dir, "second");
    quarantine_explicit(&f.root, Some(&f.workspace), &second, RepairMode::Apply);

    let mut bodies: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("quarantined-"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    bodies.sort();
    assert_eq!(bodies, vec!["first", "second"], "both must survive");
}

// ------------------------------------------- path-comparison regression (#5371 critic CRITICAL)

#[cfg(unix)]
#[test]
fn explicit_refuses_the_workspaces_own_file_via_a_symlinked_workspace_spelling() {
    // The workspace guard compared paths lexically. A caller holding a
    // SYMLINKED spelling of the workspace (`/tmp/...` while the file resolves
    // under `/private/tmp/...`, the exact aliasing this PR already fixed for
    // the ledger key and the scan-dir dedupe) slipped past it, and the
    // project's own LIVE `.mcp.json` was renamed away.
    let f = fixture();
    let real_ws = f.home.join("real-ws");
    let own = write_mcp(&real_ws, SAMPLE);
    let linked_ws = f.home.join("linked-ws");
    std::os::unix::fs::symlink(&real_ws, &linked_ws).unwrap();

    // Caller passes the symlinked spelling; the target is the real spelling.
    let step = quarantine_explicit(&f.root, Some(&linked_ws), &own, RepairMode::Apply);

    assert!(
        matches!(step.status, StepStatus::Refused(_)),
        "the workspace's own .mcp.json must be refused however the workspace is \
         spelled, got {step:?}"
    );
    assert!(
        own.exists(),
        "the project's live .mcp.json must still be there"
    );
}

#[cfg(unix)]
#[test]
fn scan_dirs_stops_at_home_through_a_symlinked_spelling() {
    // Same bug class as the guard above: the home CEILING was also a lexical
    // comparison, so a symlinked home spelling never matched and the walk ran
    // on to the depth cap, scanning directories above home that are none of
    // tm's business.
    let tmp = tempfile::tempdir().unwrap();
    let real_home = tmp.path().join("real-home");
    let ws = real_home.join("a").join("b");
    std::fs::create_dir_all(&ws).unwrap();
    let linked_home = tmp.path().join("linked-home");
    std::os::unix::fs::symlink(&real_home, &linked_home).unwrap();

    let dirs = scan_dirs(Some(&ws), &linked_home);
    assert!(
        !dirs.iter().any(|d| d == tmp.path()),
        "the walk must stop at home however home is spelled — it reached above it: {dirs:?}"
    );
}

// ------------------------------------------------ TOCTOU regression (#5371 critic HIGH)

#[test]
fn sweep_refuses_a_file_edited_between_scan_and_rename() {
    // Drives a mutation into the exact window the critic named: the sweep
    // classified during `scan`, and the rename happened later. This calls the
    // post-scan half directly with a file whose bytes changed after that
    // classification — the state a concurrent session launch or an operator's
    // editor save produces. The stale verdict must not be trusted.
    let f = fixture();
    let stray = write_mcp(&f.home.join("projects"), SAMPLE);
    record_write(&f.root, &stray, SAMPLE).unwrap();

    // Scan-time verdict: tm wrote it, bytes unchanged.
    assert_eq!(
        crate::core::mcp_provenance::classify(&mcp_provenance::load(&f.root), &stray),
        Provenance::TmWritten
    );

    // ...the window: somebody edits the file.
    let edited = r#"{"mcpServers":{"my-own-server":{"type":"stdio","command":"mine"}}}"#;
    std::fs::write(&stray, edited).unwrap();

    let step = apply_verified(&f.root, &stray, RepairMode::Apply);
    let StepStatus::Refused(why) = &step.status else {
        panic!("a file edited inside the window must be refused, got {step:?}");
    };
    assert!(why.contains("changed between"), "reason: {why}");
    assert_eq!(
        std::fs::read_to_string(&stray).unwrap(),
        edited,
        "the edit made inside the window must survive"
    );
}

#[test]
fn sweep_still_quarantines_when_nothing_changed_in_the_window() {
    // The re-verification must not turn every quarantine into a refusal —
    // proves the guard is discriminating, not just always-refusing.
    let f = fixture();
    let stray = write_mcp(&f.home.join("projects"), SAMPLE);
    record_write(&f.root, &stray, SAMPLE).unwrap();

    let step = apply_verified(&f.root, &stray, RepairMode::Apply);
    assert!(
        matches!(step.status, StepStatus::Applied { .. }),
        "an unchanged tm-written file must still be quarantined, got {step:?}"
    );
    assert!(!stray.exists());
}
