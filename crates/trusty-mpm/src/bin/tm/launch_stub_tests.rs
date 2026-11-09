//! A stub daemon for the `tm` bin's launch tests (#10101 D3c).
//!
//! Why: since D3c a CLI launch (`tm launch`, `tm connect`, the in-place
//! `tm session start`) refuses an unreachable daemon, so every test that
//! drives one past the name check needs a daemon that answers. One stub
//! serves them all.
//! What: [`spawn`] answers `GET /health`, the managed list, the legacy
//! registry, registration (`POST /sessions`, `POST /api/v1/sessions/connect`)
//! under the posted name with [`STUB_ID`], `DELETE /sessions/{id}`,
//! `PATCH /sessions/{id}/pid` and the managed resume route, recording what it
//! saw in [`Seen`]. [`spawn_empty`] holds no records.
//! Test: used by `start_tests.rs` and `tests_behavior_b_tests.rs`.

use std::sync::{Arc, Mutex};

/// The id every registration answers with.
pub(crate) const STUB_ID: &str = "22222222-2222-2222-2222-222222222222";

/// What one stub daemon saw.
#[derive(Default)]
pub(crate) struct Seen {
    /// Bodies of every registration POST.
    pub(crate) posts: Vec<serde_json::Value>,
    /// Ids of every `DELETE /sessions/{id}`.
    pub(crate) deletes: Vec<String>,
    /// Ids of every managed resume.
    pub(crate) resumes: Vec<String>,
}

/// A resume reply: HTTP status and body.
pub(crate) type ResumeReply = (u16, serde_json::Value);

/// A stub holding no records; its resume route answers 404.
pub(crate) async fn spawn_empty() -> (Arc<Mutex<Seen>>, String) {
    spawn(Vec::new(), Vec::new(), (404, serde_json::json!({}))).await
}

/// A stub daemon with `managed` and `legacy` records whose resume route
/// answers `resume`. Returns what it saw and its URL.
pub(crate) async fn spawn(
    managed: Vec<serde_json::Value>,
    legacy: Vec<serde_json::Value>,
    resume: ResumeReply,
) -> (Arc<Mutex<Seen>>, String) {
    use axum::{
        Json, Router,
        extract::Path,
        http::StatusCode,
        routing::{delete, get, patch, post},
    };
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let (on_post, on_delete, on_resume) = (seen.clone(), seen.clone(), seen.clone());
    let managed = serde_json::json!({ "sessions": managed });
    let legacy = serde_json::json!({ "sessions": legacy });
    let register = move |Json(body): Json<serde_json::Value>| {
        let seen = on_post.clone();
        async move {
            let name = body["name"].as_str().unwrap_or("tm-stub").to_owned();
            seen.lock().expect("seen").posts.push(body);
            Json(serde_json::json!({ "id": STUB_ID, "name": name }))
        }
    };
    let router = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route(
            "/api/v1/sessions/managed",
            get(move || {
                let m = managed.clone();
                async move { Json(m) }
            }),
        )
        .route(
            "/sessions",
            get(move || {
                let l = legacy.clone();
                async move { Json(l) }
            })
            .post(register.clone()),
        )
        .route("/api/v1/sessions/connect", post(register))
        .route(
            "/sessions/{id}",
            delete(move |Path(id): Path<String>| {
                let seen = on_delete.clone();
                async move {
                    seen.lock().expect("seen").deletes.push(id.clone());
                    Json(serde_json::json!({ "removed": id }))
                }
            }),
        )
        .route(
            "/sessions/{id}/pid",
            patch(|| async { Json(serde_json::json!({})) }),
        )
        .route(
            "/api/v1/sessions/managed/{id}/resume",
            post(move |Path(id): Path<String>| {
                let seen = on_resume.clone();
                let (code, body) = resume.clone();
                async move {
                    seen.lock().expect("seen").resumes.push(id);
                    (StatusCode::from_u16(code).expect("status"), Json(body))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });
    (seen, format!("http://{addr}"))
}
