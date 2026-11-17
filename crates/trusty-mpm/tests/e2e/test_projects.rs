//! E2E: project registry.

use super::harness::TestDaemon;
use serde_json::{Value, json};

/// A git repo at `<tempdir>/<name>` with an `origin` remote, or `None` when git
/// is not on this runner.
///
/// Why (#10586): `POST /projects` refuses a directory with no `origin` remote,
/// because the persistent registry record it writes needs a `repo_url`.
fn repo_with_origin(name: &str) -> Option<(tempfile::TempDir, String)> {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join(name);
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    let git_ok = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if !git_ok(&["init", "-q", "."])
        || !git_ok(&["remote", "add", "origin", "https://example.invalid/e2e.git"])
    {
        return None;
    }
    let path = repo.to_string_lossy().into_owned();
    Some((dir, path))
}

/// Registering a project makes it appear in `GET /projects` and in the
/// persistent registry `tm project list` reads (#10586).
#[tokio::test]
async fn register_and_list_project() {
    let Some((_dir, path)) = repo_with_origin("demo") else {
        return; // no git on this runner
    };
    let daemon = TestDaemon::spawn().await;
    let client = daemon.client();

    let info: Value = client
        .post(daemon.url("/projects"))
        .json(&json!({ "path": path }))
        .send()
        .await
        .expect("register project")
        .json()
        .await
        .expect("register body");
    assert_eq!(info["name"], "demo");
    assert_eq!(info["path"], path.as_str());

    let listed: Value = client
        .get(daemon.url("/projects"))
        .send()
        .await
        .expect("list projects")
        .json()
        .await
        .expect("list body");
    let projects = listed["projects"].as_array().expect("projects array");
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["path"], path.as_str());

    let registry: Value = client
        .get(daemon.url("/api/v1/projects"))
        .send()
        .await
        .expect("list registry")
        .json()
        .await
        .expect("registry body");
    let names: Vec<&str> = registry["projects"]
        .as_array()
        .expect("registry projects array")
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert!(
        names.contains(&"demo"),
        "registry B must hold it: {registry}"
    );
}

/// `GET /projects/current` for an unregistered path returns `404`.
#[tokio::test]
async fn current_project_not_found() {
    let daemon = TestDaemon::spawn().await;
    let resp = daemon
        .client()
        .get(daemon.url("/projects/current"))
        .query(&[("path", "/work/missing")])
        .send()
        .await
        .expect("current project request");
    assert_eq!(resp.status(), 404);
}

/// `GET /projects/current` for a registered path returns the project.
#[tokio::test]
async fn current_project_found() {
    let Some((_dir, path)) = repo_with_origin("cwd") else {
        return; // no git on this runner
    };
    let daemon = TestDaemon::spawn().await;
    let client = daemon.client();

    let resp = client
        .post(daemon.url("/projects"))
        .json(&json!({ "path": path }))
        .send()
        .await
        .expect("register project");
    assert!(resp.status().is_success(), "register: {}", resp.status());

    let resp = client
        .get(daemon.url("/projects/current"))
        .query(&[("path", path.as_str())])
        .send()
        .await
        .expect("current project request");
    assert_eq!(resp.status(), 200);
    let info: Value = resp.json().await.expect("current body");
    assert_eq!(info["path"], path.as_str());
    assert_eq!(info["name"], "cwd");
}
