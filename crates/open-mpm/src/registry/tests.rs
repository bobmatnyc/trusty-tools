//! Unit tests for the project-registry data types and pure helpers.
//!
//! Why: `ProjectEntry::last_active`, `extract_github_repo`,
//! `discover_active_projects`, and `is_real_project` are pure and worth
//! exhaustive coverage; the `ProjectRegistry` store methods are exercised
//! indirectly via their callers.
//! What: Tests for timestamp selection, GitHub-repo parsing, active-project
//! discovery (incl. temp-dir filtering), and serde backwards-compat.
//! Test: This module is itself the test coverage.

use std::path::PathBuf;

use chrono::{DateTime, Utc};

use super::{ProjectEntry, ProjectStatus, discover_active_projects, extract_github_repo};

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
        extract_github_repo("https://github.com/bobmatnyc/open-mpm.git"),
        Some("bobmatnyc/open-mpm".into())
    );
    assert_eq!(
        extract_github_repo("https://github.com/bobmatnyc/open-mpm"),
        Some("bobmatnyc/open-mpm".into())
    );
}

#[test]
fn extract_github_repo_ssh_form() {
    assert_eq!(
        extract_github_repo("git@github.com:bobmatnyc/open-mpm.git"),
        Some("bobmatnyc/open-mpm".into())
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
    let e = make_entry("/Users/masa/Projects/open-mpm");
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
