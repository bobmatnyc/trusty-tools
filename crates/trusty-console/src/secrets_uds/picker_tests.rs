//! Tests for the scope and project picker routes (#9067, DOC-74 §15.6).
//!
//! Why: the picker routes sit behind the same guard as the credential
//! routes, so each refusal is proved on them too, and the scope list must
//! never carry a machine scope.
//! What: `scopes` against a stub socket, `projects` against a registry file
//! in a tempdir; both through the real router.
//! Test: this file IS the test.

use std::path::Path;
use std::sync::Arc;

use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::CSRF_HEADER;
use super::errors::Refusal;
use super::tests::{
    bridge_at, error_frame, family, ok_frame, ok_stub, own_request, send, stub, text,
};
use super::verdict::{AcceptedPeer, PeerLookup};

const SCOPES_BODY: &str = r#"{"project":"/p"}"#;

/// What a real `secrets.scopes` answers for the `acme/web` checkout.
fn scopes_result() -> Value {
    json!({ "scopes": [
        { "kind": "project", "vault": "trusty/acme/web" },
        { "kind": "owner", "vault": "trusty/acme" },
    ]})
}

/// Write `rows` as the registry file under `dir`; return its path.
fn registry_at(dir: &Path, rows: &Value) -> std::path::PathBuf {
    let file = dir.join("project-paths.json");
    std::fs::write(&file, rows.to_string()).expect("write registry");
    file
}

/// Why: #9067 — the scope picker is one bridged call, `secrets.bridge.scopes`,
/// carrying the console's verdict like every other bridged call, and the
/// answer is relayed as the server sent it.
/// Red until the route exists.
/// Test: itself.
#[tokio::test]
async fn scopes_route_dials_secrets_bridge_scopes_with_an_assertion() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, seen) = stub(tmp.path(), |_| ok_frame(scopes_result()));
    let bridge = bridge_at(&socket, Arc::new(family()));
    let mut request = own_request(&bridge, "/api/secrets/scopes", SCOPES_BODY);
    request
        .extensions_mut()
        .insert(ConnectInfo(PeerLookup::ready(AcceptedPeer::Unfound)));
    let (status, body) = send(&bridge, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answer: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(answer, scopes_result());
    let seen = seen.lock().expect("lock");
    assert_eq!(seen.len(), 1, "exactly one call");
    assert_eq!(seen[0]["method"], "secrets.bridge.scopes");
    assert_eq!(seen[0]["params"]["project"], "/p");
    assert_eq!(
        seen[0]["params"]["bridge"],
        json!({ "agent_parent": true, "source": "unmappable" })
    );
}

/// Why: #9067 — the picker shows project and owner scopes only. A result
/// naming any other kind (a machine scope) is refused, not relayed, so a
/// server that grew one could not put it on the page.
/// Red until the route exists and checks the kinds.
/// Test: itself.
#[tokio::test]
async fn scopes_route_never_relays_a_machine_scope() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, seen) = stub(tmp.path(), |_| {
        ok_frame(json!({ "scopes": [
            { "kind": "project", "vault": "trusty/acme/web" },
            { "kind": "machine", "vault": "trusty/machine" },
        ]}))
    });
    let bridge = bridge_at(&socket, Arc::new(family()));
    let (status, body) = send(
        &bridge,
        own_request(&bridge, "/api/secrets/scopes", SCOPES_BODY),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body.contains(text(Refusal::UnknownUpstream)), "{body}");
    assert!(!body.contains("machine"), "{body}");
    assert_eq!(seen.lock().expect("lock").len(), 1);
}

/// Why: #9067 — a server refusal on the picker passes only through the
/// closed kind table, as on every bridged route.
/// Test: itself.
#[tokio::test]
async fn scopes_route_relays_only_the_upstream_kind() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (socket, _seen) = stub(tmp.path(), |_| error_frame(Some("bridge_refused")));
    let bridge = bridge_at(&socket, Arc::new(family()));
    let (status, body) = send(
        &bridge,
        own_request(&bridge, "/api/secrets/scopes", SCOPES_BODY),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("bridge_refused"), "{body}");
    assert!(!body.contains("UPSTREAM-MESSAGE-TEXT"), "{body}");
}

/// Why: #9067 — the picker routes take the guard every secrets route takes:
/// a foreign or absent Origin, a foreign Host, a missing or wrong CSRF
/// header and any method but POST are refused, and none reaches the socket
/// or reads the registry.
/// Test: itself.
#[tokio::test]
async fn picker_routes_refuse_bad_origin_host_csrf_and_method() {
    let (tmp, socket, seen) = ok_stub();
    let registry = registry_at(tmp.path(), &json!([{ "alias": "x", "path": "/" }]));
    let bridge = bridge_at(&socket, Arc::new(family()));
    let bridge = Arc::new(
        Arc::try_unwrap(bridge)
            .expect("sole owner")
            .with_registry(Some(registry)),
    );
    for path in ["/api/secrets/scopes", "/api/secrets/projects"] {
        let mut no_origin = own_request(&bridge, path, SCOPES_BODY);
        no_origin.headers_mut().remove("origin");
        let mut foreign_origin = own_request(&bridge, path, SCOPES_BODY);
        foreign_origin
            .headers_mut()
            .insert("origin", "http://evil.test".parse().expect("header"));
        let mut foreign_host = own_request(&bridge, path, SCOPES_BODY);
        foreign_host
            .headers_mut()
            .insert("host", "evil.test:7788".parse().expect("header"));
        foreign_host
            .headers_mut()
            .insert("origin", "http://evil.test:7788".parse().expect("header"));
        let mut no_csrf = own_request(&bridge, path, SCOPES_BODY);
        no_csrf.headers_mut().remove(CSRF_HEADER);
        let mut wrong_csrf = own_request(&bridge, path, SCOPES_BODY);
        wrong_csrf
            .headers_mut()
            .insert(CSRF_HEADER, "0".repeat(64).parse().expect("header"));
        let mut get = own_request(&bridge, path, SCOPES_BODY);
        *get.method_mut() = "GET".parse().expect("method");
        let mut put = own_request(&bridge, path, SCOPES_BODY);
        *put.method_mut() = "PUT".parse().expect("method");
        let cases = [
            (
                "no origin",
                no_origin,
                StatusCode::FORBIDDEN,
                Refusal::Origin,
            ),
            (
                "foreign origin",
                foreign_origin,
                StatusCode::FORBIDDEN,
                Refusal::Origin,
            ),
            (
                "foreign host",
                foreign_host,
                StatusCode::FORBIDDEN,
                Refusal::Origin,
            ),
            ("no csrf", no_csrf, StatusCode::FORBIDDEN, Refusal::Csrf),
            (
                "wrong csrf",
                wrong_csrf,
                StatusCode::FORBIDDEN,
                Refusal::Csrf,
            ),
            (
                "GET",
                get,
                StatusCode::METHOD_NOT_ALLOWED,
                Refusal::MethodNotAllowed,
            ),
            (
                "PUT",
                put,
                StatusCode::METHOD_NOT_ALLOWED,
                Refusal::MethodNotAllowed,
            ),
        ];
        for (case, request, status, refusal) in cases {
            let (got, body) = send(&bridge, request).await;
            assert_eq!(got, status, "{path} {case}: {body}");
            assert!(body.contains(text(refusal)), "{path} {case}: {body}");
            assert!(!body.contains("\"x\""), "{path} {case}: registry leaked");
        }
    }
    assert!(
        seen.lock().expect("lock").is_empty(),
        "nothing reached the socket"
    );
}

/// Why: #9067, Architect ruling — the page picks a project from the tm
/// project registry, never a typed path. The route answers each registered
/// checkout that is an existing absolute directory, by alias and path, and
/// never dials the socket.
/// Red until the route exists.
/// Test: itself.
#[tokio::test]
async fn projects_route_lists_the_registry_and_never_dials() {
    let (tmp, socket, seen) = ok_stub();
    let web = tmp.path().join("web");
    let api = tmp.path().join("api");
    std::fs::create_dir(&web).expect("mkdir");
    std::fs::create_dir(&api).expect("mkdir");
    let registry = registry_at(
        tmp.path(),
        &json!([
            { "alias": "web", "path": web, "extra": 1 },
            { "alias": "gone", "path": tmp.path().join("gone") },
            { "alias": "relative", "path": "some/where" },
            { "alias": "api", "path": api },
        ]),
    );
    let bridge = Arc::new(
        Arc::try_unwrap(bridge_at(&socket, Arc::new(family())))
            .expect("sole owner")
            .with_registry(Some(registry)),
    );
    let (status, body) = send(&bridge, own_request(&bridge, "/api/secrets/projects", "")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answer: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        answer,
        json!({ "projects": [
            { "name": "web", "path": web },
            { "name": "api", "path": api },
        ]})
    );
    assert!(
        seen.lock().expect("lock").is_empty(),
        "the socket is not dialled"
    );
}

/// Why: #9067 — a registry that is absent or empty is a picker with no
/// projects, not an error; one that does not parse is refused with fixed
/// text that quotes nothing from the file.
/// Red until the route exists.
/// Test: itself.
#[tokio::test]
async fn projects_route_answers_an_empty_list_for_an_absent_or_empty_registry() {
    let (tmp, socket, _seen) = ok_stub();
    let empty = registry_at(tmp.path(), &json!([]));
    let absent = tmp.path().join("absent.json");
    for registry in [empty, absent] {
        let bridge = Arc::new(
            Arc::try_unwrap(bridge_at(&socket, Arc::new(family())))
                .expect("sole owner")
                .with_registry(Some(registry.clone())),
        );
        let (status, body) = send(&bridge, own_request(&bridge, "/api/secrets/projects", "")).await;
        assert_eq!(status, StatusCode::OK, "{}: {body}", registry.display());
        assert_eq!(body, r#"{"projects":[]}"#, "{}", registry.display());
    }
    let malformed = tmp.path().join("malformed.json");
    std::fs::write(&malformed, "SECRET-LOOKING-TEXT {").expect("write");
    for registry in [Some(malformed), None] {
        let bridge = Arc::new(
            Arc::try_unwrap(bridge_at(&socket, Arc::new(family())))
                .expect("sole owner")
                .with_registry(registry.clone()),
        );
        let (status, body) = send(&bridge, own_request(&bridge, "/api/secrets/projects", "")).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{registry:?}: {body}"
        );
        assert!(body.contains(text(Refusal::RegistryUnavailable)), "{body}");
        assert!(!body.contains("SECRET-LOOKING-TEXT"), "{body}");
    }
}
