//! Unit tests for the project-registry data types and pure helpers.
//!
//! Why: `ProjectEntry::last_active`, `extract_github_repo`,
//! `discover_active_projects`, and `is_real_project` are pure and worth
//! exhaustive coverage; the `ProjectRegistry` store methods are exercised
//! indirectly via their callers.
//! What: Tests for timestamp selection, GitHub-repo parsing, active-project
//! discovery (incl. temp-dir filtering), and serde backwards-compat.
//! Test: This module is itself the test coverage.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use super::{
    ProjectEntry, ProjectRegistry, ProjectStatus, RootOverlap, classify_root_overlap,
    discover_active_projects, extract_github_repo, find_project_overlap, registry_overlaps,
};

fn entry_with_times(
    path: &str,
    last_run: Option<DateTime<Utc>>,
    last_connected: Option<DateTime<Utc>>,
) -> ProjectEntry {
    ProjectEntry {
        path: PathBuf::from(path),
        name: path.into(),
        last_run,
        status: ProjectStatus::Active,
        last_connected,
        pm_count: 0,
        is_self: false,
        manually_registered: false,
        git_origin: None,
        open_issues_count: None,
        open_prs_count: None,
    }
}

#[test]
fn last_active_picks_max() {
    let early = Utc::now() - chrono::Duration::days(5);
    let late = Utc::now() - chrono::Duration::days(1);

    // Both set: returns the later one.
    let e = entry_with_times("/a", Some(early), Some(late));
    assert_eq!(e.last_active(), Some(late));

    // Only last_run.
    let e = entry_with_times("/a", Some(early), None);
    assert_eq!(e.last_active(), Some(early));

    // Only last_connected.
    let e = entry_with_times("/a", None, Some(late));
    assert_eq!(e.last_active(), Some(late));

    // Neither.
    let e = entry_with_times("/a", None, None);
    assert_eq!(e.last_active(), None);
}

#[test]
fn extract_github_repo_https_form() {
    assert_eq!(
        extract_github_repo("https://github.com/bobmatnyc/trusty-agents.git"),
        Some("bobmatnyc/trusty-agents".into())
    );
    assert_eq!(
        extract_github_repo("https://github.com/bobmatnyc/trusty-agents"),
        Some("bobmatnyc/trusty-agents".into())
    );
}

#[test]
fn extract_github_repo_ssh_form() {
    assert_eq!(
        extract_github_repo("git@github.com:bobmatnyc/trusty-agents.git"),
        Some("bobmatnyc/trusty-agents".into())
    );
    assert_eq!(
        extract_github_repo("git@github.com:duettoresearch/duetto"),
        Some("duettoresearch/duetto".into())
    );
}

#[test]
fn extract_github_repo_returns_none_for_non_github() {
    assert!(extract_github_repo("https://gitlab.com/o/r.git").is_none());
    assert!(extract_github_repo("git@bitbucket.org:o/r.git").is_none());
    assert!(extract_github_repo("").is_none());
    // github.com prefix but no repo path is invalid.
    assert!(extract_github_repo("https://github.com/").is_none());
}

#[test]
fn discover_active_projects_returns_recent_and_session_owned() {
    let now = Utc::now();
    let recent = entry_with_times("/recent", Some(now - chrono::Duration::days(2)), None);
    let stale = entry_with_times("/stale", Some(now - chrono::Duration::days(60)), None);
    let session_owned = entry_with_times(
        "/session-owned",
        Some(now - chrono::Duration::days(60)),
        None,
    );

    let entries = vec![recent.clone(), stale.clone(), session_owned.clone()];
    let session_paths = vec![PathBuf::from("/session-owned")];
    let window = chrono::Duration::days(14);

    let active = discover_active_projects(&entries, &session_paths, window);
    let paths: Vec<&PathBuf> = active.iter().map(|e| &e.path).collect();

    // recent is included (within 14 days).
    assert!(paths.iter().any(|p| p.to_string_lossy() == "/recent"));
    // session_owned is included (has a TM session despite stale activity).
    assert!(
        paths
            .iter()
            .any(|p| p.to_string_lossy() == "/session-owned")
    );
    // stale is excluded.
    assert!(!paths.iter().any(|p| p.to_string_lossy() == "/stale"));
}

fn make_entry(path: &str) -> ProjectEntry {
    ProjectEntry {
        path: PathBuf::from(path),
        name: path.into(),
        last_run: None,
        status: ProjectStatus::Active,
        last_connected: None,
        pm_count: 0,
        is_self: false,
        manually_registered: false,
        git_origin: None,
        open_issues_count: None,
        open_prs_count: None,
    }
}

#[test]
fn is_real_project_rejects_temp_dirs() {
    // macOS temp dir under /var/folders
    let e = make_entry("/private/var/folders/l1/abc123/T/.tmptcuMXm");
    assert!(
        !e.is_real_project(),
        "macOS /var/folders temp should be excluded"
    );

    // basename starting with .tmp
    let e = make_entry("/private/var/folders/l1/abc123/T/.tmpXe19Vm");
    assert!(
        !e.is_real_project(),
        ".tmp-prefixed basename should be excluded"
    );

    // /tmp prefix
    let e = make_entry("/tmp/myworkdir");
    assert!(!e.is_real_project(), "/tmp/ prefix should be excluded");

    // /private/tmp prefix
    let e = make_entry("/private/tmp/workdir");
    assert!(
        !e.is_real_project(),
        "/private/tmp/ prefix should be excluded"
    );

    // /private/var prefix (covers broader macOS system paths)
    let e = make_entry("/private/var/something/project");
    assert!(
        !e.is_real_project(),
        "/private/var/ prefix should be excluded"
    );
}

#[test]
fn is_real_project_accepts_normal_dirs() {
    // Normal home-directory project
    let e = make_entry("/Users/masa/Projects/trusty-agents");
    assert!(
        e.is_real_project(),
        "normal home project should be accepted"
    );

    // /var/www style server path (not macOS /var/folders)
    let e = make_entry("/var/www/myapp");
    assert!(
        e.is_real_project(),
        "/var/www should be accepted (not /var/folders)"
    );

    // Project whose name happens to contain "tmp" but not as a prefix
    let e = make_entry("/Users/masa/projects/dumptruck");
    assert!(
        e.is_real_project(),
        "name containing tmp (not prefix) should be accepted"
    );
}

#[test]
fn discover_active_projects_excludes_temp_dirs() {
    let now = Utc::now();
    // A temp-dir entry that is recent enough it would normally pass the window.
    let temp_entry = ProjectEntry {
        path: PathBuf::from("/private/var/folders/l1/abc/T/.tmptcuMXm"),
        name: ".tmptcuMXm".into(),
        last_run: Some(now - chrono::Duration::days(1)),
        status: ProjectStatus::Active,
        last_connected: None,
        pm_count: 0,
        is_self: false,
        manually_registered: false,
        git_origin: None,
        open_issues_count: None,
        open_prs_count: None,
    };
    let real_entry = entry_with_times(
        "/Users/masa/Projects/myapp",
        Some(now - chrono::Duration::days(1)),
        None,
    );
    let entries = vec![temp_entry, real_entry];
    let active = discover_active_projects(&entries, &[], chrono::Duration::days(14));
    let paths: Vec<&PathBuf> = active.iter().map(|e| &e.path).collect();
    assert!(
        !paths.iter().any(|p| p.to_string_lossy().contains(".tmp")),
        "temp dir should be filtered out of discover_active_projects"
    );
    assert!(
        paths.iter().any(|p| p.to_string_lossy().contains("myapp")),
        "real project should remain in discover_active_projects"
    );
}

#[test]
fn project_entry_old_json_deserializes_without_new_fields() {
    // Why: existing users have projects.json without the new fields;
    // serde defaults must keep them loadable.
    let json = r#"{
        "path": "/p",
        "name": "p",
        "last_run": null,
        "status": "active"
    }"#;
    let e: ProjectEntry = serde_json::from_str(json).expect("deserialize");
    assert_eq!(e.git_origin, None);
    assert_eq!(e.open_issues_count, None);
    assert_eq!(e.open_prs_count, None);
    assert_eq!(e.pm_count, 0);
    assert!(!e.is_self);
}

// #4289: root-containment guard for project registration.

fn entry_at(path: &Path, name: &str) -> ProjectEntry {
    let mut entry = entry_with_times(&path.to_string_lossy(), None, None);
    entry.name = name.to_string();
    entry
}

/// Create `<tmp>/<relative>` and return it.
fn dir(tmp: &Path, relative: &str) -> PathBuf {
    let path = tmp.join(relative);
    std::fs::create_dir_all(&path).expect("create dir");
    path
}

#[test]
fn same_tree_is_detected_through_a_symlink_and_trailing_slash() {
    // Why: a string comparison of two spellings of one tree is the exact
    // miss #2519 recorded; the guard has to answer over the inode.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = dir(tmp.path(), "project");
    let trailing = PathBuf::from(format!("{}/", root.display()));
    assert_eq!(
        classify_root_overlap(&trailing, &root),
        Some(RootOverlap::SameTree)
    );
    #[cfg(unix)]
    {
        let alias = tmp.path().join("project-alias");
        std::os::unix::fs::symlink(&root, &alias).expect("symlink");
        assert_eq!(
            classify_root_overlap(&alias, &root),
            Some(RootOverlap::SameTree)
        );
    }
    // Both paths gone: component equality still absorbs the trailing slash.
    assert_eq!(
        classify_root_overlap(
            Path::new("/nonexistent/gone/"),
            Path::new("/nonexistent/gone")
        ),
        Some(RootOverlap::SameTree)
    );
}

#[test]
fn candidate_inside_a_registered_root_is_flagged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let outer = dir(tmp.path(), "outer");
    let inner = dir(tmp.path(), "outer/crates/api");
    let found = find_project_overlap(&inner, &[entry_at(&outer, "outer-project")])
        .expect("subdirectory of a registered root overlaps it");
    assert_eq!(found.overlap, RootOverlap::InsideKnownRoot);
    assert_eq!(found.path, outer);
    let refusal = found.refusal();
    assert!(
        refusal.contains("outer-project") && refusal.contains(&outer.display().to_string()),
        "refusal must name the existing project and its path: {refusal}"
    );
}

#[test]
fn candidate_that_encloses_a_registered_root_is_flagged() {
    // Why: the ancestor case is the one a user picking a folder never sees
    // coming — it would swallow every index below it.
    let tmp = tempfile::tempdir().expect("tempdir");
    let outer = dir(tmp.path(), "outer");
    let inner = dir(tmp.path(), "outer/crates/api");
    let found = find_project_overlap(&outer, &[entry_at(&inner, "inner-project")])
        .expect("an ancestor of a registered root overlaps it");
    assert_eq!(found.overlap, RootOverlap::EnclosesKnownRoot);
    assert!(
        found.refusal().contains("inner-project"),
        "refusal must name the enclosed project: {}",
        found.refusal()
    );
}

#[test]
fn siblings_do_not_overlap() {
    // Why: a textual prefix check would call `/srv/app2` a child of `/srv/app`.
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = dir(tmp.path(), "app");
    let app2 = dir(tmp.path(), "app2");
    assert_eq!(classify_root_overlap(&app2, &app), None);
    assert_eq!(classify_root_overlap(&app, &app2), None);
    assert!(find_project_overlap(&app2, &[entry_at(&app, "app")]).is_none());
}

#[test]
fn re_registering_a_known_path_is_not_an_overlap() {
    // Why: `om connect <path>` on a known project is an idempotent refresh,
    // and it must keep working even in a registry that already overlaps.
    let tmp = tempfile::tempdir().expect("tempdir");
    let outer = dir(tmp.path(), "outer");
    let inner = dir(tmp.path(), "outer/nested");
    let entries = [entry_at(&outer, "outer"), entry_at(&inner, "inner")];
    assert!(find_project_overlap(&inner, &entries).is_none());
    assert!(find_project_overlap(&outer, &entries).is_none());
}

#[test]
fn removed_projects_do_not_block_registration() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let outer = dir(tmp.path(), "outer");
    let inner = dir(tmp.path(), "outer/nested");
    let mut gone = entry_at(&outer, "outer");
    gone.status = ProjectStatus::Removed;
    assert!(find_project_overlap(&inner, &[gone]).is_none());
}

#[test]
fn registry_overlaps_reports_each_pair_once() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let outer = dir(tmp.path(), "outer");
    let inner = dir(tmp.path(), "outer/nested");
    let other = dir(tmp.path(), "unrelated");
    let found = registry_overlaps(&[
        entry_at(&inner, "inner"),
        entry_at(&other, "unrelated"),
        entry_at(&outer, "outer"),
    ]);
    assert_eq!(found.len(), 1, "one pair, reported once: {found:?}");
    assert_eq!(found[0].name, "inner");
    assert_eq!(found[0].against_name, "outer");
    assert_eq!(found[0].overlap, RootOverlap::InsideKnownRoot);
}

#[tokio::test]
async fn overlap_report_surfaces_a_pre_existing_overlap() {
    // Why: a registry written before the guard must still load in full — the
    // overlap is reported, never a reason to reject the file.
    let tmp = tempfile::tempdir().expect("tempdir");
    let outer = dir(tmp.path(), "outer");
    let inner = dir(tmp.path(), "outer/nested");
    let registry = ProjectRegistry::with_registry_path(tmp.path().join("projects.json"));
    registry.register(&outer).await.expect("register outer");
    registry.register(&inner).await.expect("register inner");
    assert_eq!(registry.load().await.expect("load").len(), 2);
    let report = registry.overlap_report().await.expect("report");
    assert_eq!(report.len(), 1, "expected one reported pair: {report:?}");
    assert_eq!(report[0].overlap, RootOverlap::InsideKnownRoot);
}

#[tokio::test]
async fn overlap_report_errors_on_an_unreadable_registry() {
    // Fail-open check: the report says "I could not tell" rather than "no
    // overlaps", so its caller logs and continues instead of recording a
    // clean bill of health it never earned.
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("projects.json");
    std::fs::write(&path, "{ not json").expect("write");
    let registry = ProjectRegistry::with_registry_path(path);
    assert!(registry.overlap_report().await.is_err());
}
