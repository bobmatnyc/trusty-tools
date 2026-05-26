//! E2E: project registry.

use crate::harness::TestDaemon;
use serde_json::{Value, json};

/// Registering a project makes it appear in `GET /projects`.
#[tokio::test]
async fn register_and_list_project() {
    let daemon = TestDaemon::spawn().await;
    let client = daemon.client();

    let info: Value = client
        .post(daemon.url("/projects"))
        .json(&json!({ "path": "/work/demo" }))
        .send()
        .await
        .expect("register project")
        .json()
        .await
        .expect("register body");
    assert_eq!(info["name"], "demo");
    assert_eq!(info["path"], "/work/demo");

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
    assert_eq!(projects[0]["path"], "/work/demo");
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
    let daemon = TestDaemon::spawn().await;
    let client = daemon.client();

    client
        .post(daemon.url("/projects"))
        .json(&json!({ "path": "/work/cwd" }))
        .send()
        .await
        .expect("register project");

    let resp = client
        .get(daemon.url("/projects/current"))
        .query(&[("path", "/work/cwd")])
        .send()
        .await
        .expect("current project request");
    assert_eq!(resp.status(), 200);
    let info: Value = resp.json().await.expect("current body");
    assert_eq!(info["path"], "/work/cwd");
    assert_eq!(info["name"], "cwd");
}
