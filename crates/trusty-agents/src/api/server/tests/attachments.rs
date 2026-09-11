//! Chat-attachment routes, driven through the real router (#7370).
//!
//! Why: every assertion here is about what reached — or did NOT reach — the
//! filesystem. A guard that answers `4xx` while leaving a partial file behind
//! passes a status-code-only test and fails the product, so each refusal is
//! checked on both halves.
//!
//! The attachments root is injected on `AppState`, the pattern `attendance.rs`
//! established (#4703): without it these tests would write into the
//! developer's real `~/trusty-agents` and have nothing local to assert on.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;

use super::super::routes::build_router;
use super::super::state::AppState;

/// An assistant instance id. Nothing here consults the agent roster (see the
/// `attachments` module doc), so this need not be a real roster entry.
const AGENT: &str = "attachments-probe-7370";

fn session() -> String {
    crate::ctrl::pm_task::session_id_for(AGENT)
}

fn state_at(root: &std::path::Path) -> AppState {
    AppState {
        attachments_root: Some(root.to_path_buf()),
        ..Default::default()
    }
}

/// `<root>/<agent>/attachments/<session>` — where the store must land files.
fn session_dir(root: &std::path::Path) -> std::path::PathBuf {
    root.join(AGENT).join("attachments").join(session())
}

/// A `multipart/form-data` body carrying exactly one file part.
fn multipart_body(file_name: &str, content_type: &str, bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(b"--BOUND7370\r\n");
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n--BOUND7370--\r\n");
    body
}

fn upload_request(agent: &str, file_name: &str, content_type: &str, bytes: &[u8]) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/api/agents/{agent}/sessions/{}/attachments",
            session()
        ))
        .header(
            header::CONTENT_TYPE,
            "multipart/form-data; boundary=BOUND7370",
        )
        .body(Body::from(multipart_body(file_name, content_type, bytes)))
        .expect("request")
}

async fn json_of(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

/// Upload one file, then read back its row and its bytes.
async fn upload_one(
    root: &std::path::Path,
    file_name: &str,
    content_type: &str,
    bytes: &[u8],
) -> serde_json::Value {
    let response = build_router(state_at(root))
        .oneshot(upload_request(AGENT, file_name, content_type, bytes))
        .await
        .expect("upload");
    assert_eq!(response.status(), StatusCode::CREATED);
    json_of(response).await
}

#[tokio::test]
async fn upload_stores_the_file_and_returns_its_row() {
    let dir = tempfile::TempDir::new().expect("temp");
    let row = upload_one(dir.path(), "data.csv", "text/csv", b"a,b\n1,2\n").await;

    let stored = session_dir(dir.path()).join("data.csv");
    assert!(stored.is_file(), "nothing at {}", stored.display());
    assert_eq!(std::fs::read(&stored).unwrap(), b"a,b\n1,2\n");
    assert_eq!(row["file_name"], "data.csv");
    assert_eq!(row["media_type"], "text/csv");
    assert_eq!(row["size"], 8);
    // The recorded digest is re-derived from the bytes AT THAT PATH, so this
    // asserts the row describes the file that actually landed.
    let on_disk = format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(std::fs::read(&stored).unwrap())
    );
    assert_eq!(row["sha256"].as_str().unwrap(), on_disk);
    // The wire body never carries the absolute server path.
    assert!(row.get("stored_path").is_none(), "{row}");
    assert!(
        row["url"]
            .as_str()
            .unwrap()
            .ends_with(row["id"].as_str().unwrap()),
        "{row}"
    );
}

#[tokio::test]
async fn upload_refuses_a_traversal_file_name() {
    let dir = tempfile::TempDir::new().expect("temp");
    let response = build_router(state_at(dir.path()))
        .oneshot(upload_request(AGENT, "../escape.txt", "text/plain", b"x"))
        .await
        .expect("upload");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        !dir.path().join(AGENT).join("attachments").exists(),
        "a refused upload created part of the tree"
    );
    assert!(
        !dir.path().join("escape.txt").exists(),
        "a refused upload escaped the attachments tree"
    );
}

#[tokio::test]
async fn upload_refuses_a_traversal_agent_name() {
    let dir = tempfile::TempDir::new().expect("temp");
    let response = build_router(state_at(dir.path()))
        .oneshot(upload_request("%2E%2E", "notes.txt", "text/plain", b"x"))
        .await
        .expect("upload");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn upload_refuses_an_oversize_file() {
    let dir = tempfile::TempDir::new().expect("temp");
    let oversize = vec![b'x'; crate::attachments::MAX_ATTACHMENT_BYTES as usize + 1];
    let response = build_router(state_at(dir.path()))
        .oneshot(upload_request(AGENT, "big.txt", "text/plain", &oversize))
        .await
        .expect("upload");

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        !session_dir(dir.path()).join("big.txt").exists(),
        "an oversize upload left a partial file"
    );
}

#[tokio::test]
async fn list_returns_the_session_manifest() {
    let dir = tempfile::TempDir::new().expect("temp");
    let first = upload_one(dir.path(), "a.txt", "text/plain", b"a").await;
    let second = upload_one(dir.path(), "b.png", "image/png", b"\x89PNG").await;

    let response = build_router(state_at(dir.path()))
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/agents/{AGENT}/sessions/{}/attachments",
                    session()
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("list");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_of(response).await;
    let rows = body["attachments"].as_array().expect("array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], first["id"]);
    assert_eq!(rows[1]["id"], second["id"]);
    assert_eq!(rows[1]["media_type"], "image/png");
}

#[tokio::test]
async fn download_returns_the_bytes_and_headers() {
    let dir = tempfile::TempDir::new().expect("temp");
    let row = upload_one(dir.path(), "data.csv", "text/csv", b"a,b\n1,2\n").await;

    let response = build_router(state_at(dir.path()))
        .oneshot(
            Request::builder()
                .uri(row["url"].as_str().unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("download");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "text/csv");
    assert_eq!(
        response.headers()[header::CONTENT_DISPOSITION],
        "inline; filename=\"data.csv\""
    );
    assert_eq!(
        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    assert_eq!(&bytes[..], b"a,b\n1,2\n");
}

/// An uploaded HTML file must not render in the API origin.
#[tokio::test]
async fn download_forces_scriptable_content_to_download() {
    let dir = tempfile::TempDir::new().expect("temp");
    let row = upload_one(dir.path(), "x.html", "text/html", b"<script>1</script>").await;

    let response = build_router(state_at(dir.path()))
        .oneshot(
            Request::builder()
                .uri(row["url"].as_str().unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("download");

    assert!(
        response.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment;"),
        "scriptable content was served inline"
    );
}

#[tokio::test]
async fn download_rejects_an_unknown_id() {
    let dir = tempfile::TempDir::new().expect("temp");
    upload_one(dir.path(), "a.txt", "text/plain", b"a").await;

    for id in ["0".repeat(32), "not-an-id".to_string()] {
        let response = build_router(state_at(dir.path()))
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/agents/{AGENT}/sessions/{}/attachments/{id}",
                        session()
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("download");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "id {id}");
    }
}

/// The validate-before-persist gate: a send naming an attachment that is not in
/// the manifest is refused with the task store untouched.
///
/// Why this is the regression: the prior attempt on this issue persisted the
/// user turn first and validated afterwards, so a rejected send polluted chat
/// history and duplicated on retry. Everything `submit_task` writes — the
/// `running` placeholder asserted here, and `spawn_persist_turn`'s chat-history
/// append downstream of it — happens after this gate, so an empty task store
/// proves nothing was recorded.
#[tokio::test]
async fn send_with_an_unknown_attachment_is_refused() {
    let dir = tempfile::TempDir::new().expect("temp");
    let app = build_router(state_at(dir.path()));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/task")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "task": "what is in this file?",
                        "agent": AGENT,
                        "attachments": ["0".repeat(32)],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("submit");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let listed = app
        .oneshot(
            Request::builder()
                .uri("/api/tasks")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("list tasks");
    let body = json_of(listed).await;
    assert_eq!(
        body.as_array().map(Vec::len).unwrap_or_default(),
        0,
        "a refused send recorded a task: {body}"
    );
}

/// The success arm of the same gate — an id that IS in the manifest is
/// accepted, so the refusal above is about the id and not about the field.
#[tokio::test]
async fn send_with_a_known_attachment_is_accepted() {
    let dir = tempfile::TempDir::new().expect("temp");
    let row = upload_one(dir.path(), "data.csv", "text/csv", b"a,b\n1,2\n").await;

    let response = build_router(state_at(dir.path()))
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/task")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "task": "what is in this file?",
                        "agent": AGENT,
                        "attachments": [row["id"].as_str().unwrap()],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("submit");

    assert_eq!(response.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn send_refuses_too_many_attachments() {
    let dir = tempfile::TempDir::new().expect("temp");
    let ids: Vec<String> = (0..9).map(|_| "0".repeat(32)).collect();

    let response = build_router(state_at(dir.path()))
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/task")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({ "task": "hi", "agent": AGENT, "attachments": ids })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("submit");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
