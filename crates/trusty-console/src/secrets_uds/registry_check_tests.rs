//! Tests for the registry check on every bridged call (#9067 PR B1).
//!
//! Why: a bridged call names the project whose secrets it reads or writes.
//! The page offers only projects from the tm registry, so a request naming
//! any other project is refused before the secrets socket is dialled, and a
//! registry the console cannot read refuses everything (fail closed).
//! What: each test writes a registry into a tempdir, sends requests through
//! the real secrets router to a stub socket, and counts what reached it.
//! Test: this file IS the test.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::SecretsBridge;
use super::errors::Refusal;
use super::tests::{
    BRIDGE_REGISTRY, Seen, bridge_listing, family, ok_frame, own_request, registry_listing, send,
    stub, text,
};

/// The project the registry lists in these tests.
const LISTED: &str = "/work/acme-web";

/// Every route that forwards a bridged call.
const ROUTES: [&str; 6] = [
    "/api/secrets/scopes",
    "/api/secrets/list",
    "/api/secrets/set",
    "/api/secrets/delete",
    "/api/secrets/copy",
    "/api/secrets/agents_may_use",
];

/// A valid body for `route` naming `project`.
fn body_for(route: &str, project: &str) -> String {
    let vault = "trusty/acme/web";
    let body = match route {
        "/api/secrets/scopes" => json!({ "project": project }),
        "/api/secrets/list" => json!({ "project": project, "vault": vault }),
        "/api/secrets/set" => {
            json!({ "project": project, "vault": vault, "key": "API_KEY", "value": "v" })
        }
        "/api/secrets/delete" => json!({ "project": project, "vault": vault, "key": "API_KEY" }),
        "/api/secrets/copy" => {
            json!({ "project": project, "from_backend": "keychain", "to_backend": "file" })
        }
        _ => json!({ "project": project, "vault": vault, "key": "API_KEY", "allowed": true }),
    };
    body.to_string()
}

/// A stub answering `scopes` with a project scope and every other method `ok`.
fn answering_stub(dir: &Path) -> (PathBuf, Seen) {
    stub(dir, |request| {
        if request["method"] == "secrets.bridge.scopes" {
            ok_frame(json!({ "scopes": [{ "kind": "project", "vault": "trusty/acme/web" }] }))
        } else {
            ok_frame(json!({ "ok": true }))
        }
    })
}

/// The bridge at `socket`, reading its registry from `registry`.
fn bridge_reading(socket: &Path, registry: Option<PathBuf>) -> Arc<SecretsBridge> {
    let bridge = bridge_listing(socket, Arc::new(family()), &[]);
    Arc::new(
        Arc::try_unwrap(bridge)
            .expect("sole owner")
            .with_registry(registry),
    )
}

/// Send `project` on `route`; the status and body.
async fn call(bridge: &Arc<SecretsBridge>, route: &str, project: &str) -> (StatusCode, String) {
    send(
        bridge,
        own_request(bridge, route, &body_for(route, project)),
    )
    .await
}

/// Why: #9067 PR B1 — a project the registry does not list is refused with
/// the existing bad-body answer on every bridged route, and the secrets
/// socket is never dialled. The answer names neither the project nor the
/// registry file.
/// Test: itself.
#[tokio::test]
async fn an_unlisted_project_is_refused_on_every_route_without_a_dial() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, seen) = answering_stub(tmp.path());
    let bridge = bridge_listing(&socket, Arc::new(family()), &[LISTED]);
    let refused = json!({ "error": text(Refusal::BadBody) }).to_string();
    for route in ROUTES {
        let (status, body) = call(&bridge, route, "/work/other").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{route}: {body}");
        assert_eq!(body, refused, "{route}");
        assert!(!body.contains("/work"), "{route}: {body}");
        assert!(!body.contains(BRIDGE_REGISTRY), "{route}: {body}");
    }
    assert!(seen.lock().expect("lock").is_empty(), "no dial");
}

/// Why: #9067 PR B1, Architect ruling — the match is exact. A trailing
/// slash, a case difference, a prefix of the listed path, a path under it,
/// a non-normal spelling of it, a relative form, or the registry alias is
/// a different project, refused without a dial.
/// Test: itself.
#[tokio::test]
async fn near_miss_project_names_are_refused_without_a_dial() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, seen) = answering_stub(tmp.path());
    let bridge = bridge_listing(&socket, Arc::new(family()), &[LISTED]);
    let near_misses = [
        "/work/acme-web/",
        "/WORK/ACME-WEB",
        "/work/Acme-web",
        "/work/acme",
        "/work/acme-we",
        "/work/acme-web/sub",
        "/work/./acme-web",
        "/work//acme-web",
        "/work/x/../acme-web",
        "work/acme-web",
        " /work/acme-web",
        "/work/acme-web ",
        "listed",
        "",
    ];
    for route in ["/api/secrets/list", "/api/secrets/scopes"] {
        for project in near_misses {
            let (status, body) = call(&bridge, route, project).await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "{route} {project:?}: {body}"
            );
            assert!(body.contains(text(Refusal::BadBody)), "{project:?}: {body}");
        }
    }
    assert!(seen.lock().expect("lock").is_empty(), "no dial");
}

/// Why: #9067 PR B1 — a registry that is missing, malformed, not a list,
/// unreadable, or has no known location answers 503 on every bridged route
/// and dials nothing; it never falls back to allowing the call. The answer
/// quotes neither the file's path nor its content.
/// What: the mode-000 case is skipped when this process can still read the
/// file, as root can.
/// Test: itself.
#[tokio::test]
async fn an_unreadable_registry_refuses_every_route_without_a_dial() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, seen) = answering_stub(tmp.path());
    let malformed = tmp.path().join("malformed.json");
    std::fs::write(&malformed, format!("SECRET-LOOKING-TEXT [\"{LISTED}\"")).expect("write");
    let not_a_list = tmp.path().join("object.json");
    let row = json!({ "alias": "listed", "path": LISTED });
    std::fs::write(&not_a_list, row.to_string()).expect("write");
    let unreadable = registry_listing(tmp.path(), "unreadable.json", &[LISTED]);
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let mut registries = vec![
        Some(tmp.path().join("missing.json")),
        Some(malformed),
        Some(not_a_list),
        None,
    ];
    if std::fs::read(&unreadable).is_err() {
        registries.push(Some(unreadable));
    }
    let refused = json!({ "error": text(Refusal::RegistryUnavailable) }).to_string();
    let tmp_path = tmp.path().to_string_lossy().into_owned();
    for registry in registries {
        let bridge = bridge_reading(&socket, registry.clone());
        for route in ROUTES {
            let (status, body) = call(&bridge, route, LISTED).await;
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "{registry:?} {route}: {body}"
            );
            assert_eq!(body, refused, "{registry:?} {route}");
            assert!(!body.contains(&tmp_path), "{body}");
        }
    }
    assert!(seen.lock().expect("lock").is_empty(), "no dial");
}

/// Why: #9067 PR B1 — a listed project still goes through: each bridged
/// route dials exactly once, carrying the project as sent.
/// Test: itself.
#[tokio::test]
async fn a_listed_project_is_dialled_once_per_route() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, seen) = answering_stub(tmp.path());
    let bridge = bridge_listing(&socket, Arc::new(family()), &["/work/other", LISTED]);
    for route in ROUTES {
        let (status, body) = call(&bridge, route, LISTED).await;
        assert_eq!(status, StatusCode::OK, "{route}: {body}");
    }
    let seen = seen.lock().expect("lock");
    assert_eq!(seen.len(), ROUTES.len(), "exactly one dial per request");
    for request in seen.iter() {
        assert_eq!(request["params"]["project"], Value::from(LISTED));
    }
}

/// Why: #9067 PR B1, Architect ruling — the registry is read on every call,
/// never cached. A project added between two calls is allowed on the
/// second; one removed is refused again; a registry deleted after an
/// allowed call refuses the next one 503.
/// Test: itself.
#[tokio::test]
async fn a_registry_edit_between_calls_is_seen_by_the_next_call() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, seen) = answering_stub(tmp.path());
    let bridge = bridge_listing(&socket, Arc::new(family()), &["/work/other"]);
    let route = "/api/secrets/list";
    let dials = || seen.lock().expect("lock").len();

    assert_eq!(
        call(&bridge, route, LISTED).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(dials(), 0);

    registry_listing(tmp.path(), BRIDGE_REGISTRY, &["/work/other", LISTED]);
    assert_eq!(call(&bridge, route, LISTED).await.0, StatusCode::OK);
    assert_eq!(dials(), 1);

    registry_listing(tmp.path(), BRIDGE_REGISTRY, &["/work/other"]);
    assert_eq!(
        call(&bridge, route, LISTED).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(dials(), 1);

    registry_listing(tmp.path(), BRIDGE_REGISTRY, &[LISTED]);
    assert_eq!(call(&bridge, route, LISTED).await.0, StatusCode::OK);
    std::fs::remove_file(tmp.path().join(BRIDGE_REGISTRY)).expect("remove");
    assert_eq!(
        call(&bridge, route, LISTED).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(dials(), 2);
}
