//! Project-setup stages: trusty-search index-register + trusty-memory palace-create.
//!
//! Why: beyond patching `.mcp.json`, fully provisioning a project means the
//! current directory is registered as a trusty-search index and has a
//! trusty-memory palace. Both operations must be idempotent — re-running
//! `ensure` must not error or duplicate — and tolerant of a daemon that is not
//! yet running (plain `tctl ensure` is commonly run before the stack is up).
//!
//! What: [`register_index`] issues `POST /indexes` to trusty-search (idempotent:
//! the daemon returns `created:false` for an existing id) and [`create_palace`]
//! issues `POST /api/v1/palaces` to trusty-memory (idempotent: a duplicate name
//! resolves to the same palace dir). When the relevant daemon is not running,
//! the stage is reported as an idempotent no-op ("daemon not running; skipped")
//! rather than a hard failure, so `ensure` stays useful pre-boot. A reachable
//! daemon returning an error IS a hard failure (the project is mis-provisioned).
//!
//! Test: `tests` stand up stub HTTP servers to exercise the created /
//! already-exists / daemon-down / error branches for both stages.

use anyhow::Result;
use serde_json::json;

use super::daemon::{build_client, resolve_base_url, MEMORY_APP, SEARCH_APP};
use super::identity;
use super::report::StageOutcome;

/// Stage name for the trusty-search index registration.
///
/// Why: the stage name is part of the `--json` contract and the human render;
/// a constant keeps it consistent across the outcome and any logging.
/// What: `"index-register"`.
/// Test: asserted in `tests`.
pub const STAGE_INDEX: &str = "index-register";

/// Stage name for the trusty-memory palace creation.
///
/// Why: see [`STAGE_INDEX`].
/// What: `"palace-create"`.
/// Test: asserted in `tests`.
pub const STAGE_PALACE: &str = "palace-create";

/// A failed [`StageOutcome`] helper.
///
/// Why: the failure-construction boilerplate repeats across both stages;
/// factoring it keeps each stage body focused on its happy path.
/// What: an `ok = false`, `changed = false` outcome carrying `detail`.
/// Test: exercised via the stage error-branch tests.
fn fail(stage: &str, detail: impl Into<String>) -> StageOutcome {
    StageOutcome {
        stage: stage.to_owned(),
        ok: false,
        changed: false,
        detail: detail.into(),
    }
}

/// An idempotent no-op [`StageOutcome`] helper (success, nothing changed).
///
/// Why: "already provisioned" and "daemon not running; skipped" are both
/// success-but-unchanged outcomes; a helper keeps the spelling consistent.
/// What: an `ok = true`, `changed = false` outcome carrying `detail`.
/// Test: exercised via the daemon-down and already-exists tests.
fn noop(stage: &str, detail: impl Into<String>) -> StageOutcome {
    StageOutcome {
        stage: stage.to_owned(),
        ok: true,
        changed: false,
        detail: detail.into(),
    }
}

/// Register `project_root` as a trusty-search index (idempotent).
///
/// Why: the project must be a registered search index for hybrid search to work;
/// `ensure` provisions it so the user does not have to run `trusty-search index`
/// separately. The daemon's `POST /indexes` is idempotent (`created:false` for an
/// existing id), so re-running is safe.
/// What: resolves the trusty-search base URL; if the daemon is down, returns an
/// idempotent no-op. Otherwise derives the index id (directory basename), POSTs
/// `{id, root_path}`, and maps the response: `created:true` → changed, an
/// existing index → unchanged no-op, a non-2xx → failure.
/// Test: `tests::register_index_created`, `register_index_already_exists`,
/// `register_index_daemon_down`, `register_index_http_error`.
pub async fn register_index(
    client: &reqwest::Client,
    project_root: &std::path::Path,
) -> Result<StageOutcome> {
    let base = match resolve_base_url(SEARCH_APP)? {
        Some(b) => b,
        None => {
            return Ok(noop(
                STAGE_INDEX,
                "trusty-search daemon not running; skipped (run `tctl start` then re-run)",
            ));
        }
    };
    let Some(id) = identity::index_id_for(project_root) else {
        return Ok(fail(
            STAGE_INDEX,
            format!("cannot derive index id from {}", project_root.display()),
        ));
    };
    let url = format!("{base}/indexes");
    let body = json!({ "id": id, "root_path": project_root });
    let resp = match client.post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => return Ok(fail(STAGE_INDEX, format!("POST {url}: {e}"))),
    };
    let status = resp.status();
    if !status.is_success() {
        return Ok(fail(STAGE_INDEX, format!("POST {url} returned {status}")));
    }
    // The daemon returns `{ "created": bool, .. }`; treat a parse failure as a
    // benign "registered" since the 2xx already confirms success.
    let created = resp
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|v| v.get("created").and_then(|c| c.as_bool()))
        .unwrap_or(true);
    if created {
        Ok(StageOutcome {
            stage: STAGE_INDEX.to_owned(),
            ok: true,
            changed: true,
            detail: format!("registered index '{id}'"),
        })
    } else {
        Ok(noop(
            STAGE_INDEX,
            format!("index '{id}' already registered"),
        ))
    }
}

/// Create the trusty-memory palace for `project_root` (idempotent).
///
/// Why: the project should have a memory palace so memory tools resolve to the
/// right store; `ensure` provisions it. Creating a palace whose name already
/// exists resolves to the same on-disk dir (the registry `create_dir_all` is a
/// no-op), so re-running is safe.
/// What: resolves the trusty-memory base URL; if the daemon is down, returns an
/// idempotent no-op. Otherwise derives the palace name (pin-file value or
/// slugified basename — matching the daemon's `validate_palace_name`), POSTs
/// `{name, cwd}` so the daemon's name-enforcement uses the project path, and maps
/// the response: a 2xx → created (or already-present), a non-2xx → failure.
/// Test: `tests::create_palace_created`, `create_palace_daemon_down`,
/// `create_palace_http_error`.
pub async fn create_palace(
    client: &reqwest::Client,
    project_root: &std::path::Path,
) -> Result<StageOutcome> {
    let base = match resolve_base_url(MEMORY_APP)? {
        Some(b) => b,
        None => {
            return Ok(noop(
                STAGE_PALACE,
                "trusty-memory daemon not running; skipped (run `tctl start` then re-run)",
            ));
        }
    };
    let Some(name) = identity::palace_name_for(project_root) else {
        return Ok(fail(
            STAGE_PALACE,
            format!("cannot derive palace name from {}", project_root.display()),
        ));
    };

    // Idempotency fast-path (optimization only — correctness does NOT depend on
    // it): if the palace already exists, GET succeeds and we no-op without a
    // create. This GET assumes trusty-memory derives the palace id from the name
    // so the id path segment equals the name (per the trusty-memory palace API
    // contract). Should that assumption ever drift, the worst case is a missed
    // fast-path: we fall through to the POST below, which is itself idempotent —
    // trusty-memory resolves a duplicate name to the same palace dir (its
    // registry `create_dir_all` is a no-op), so re-creating an existing palace
    // still succeeds without duplication.
    let get_url = format!("{base}/api/v1/palaces/{name}");
    if let Ok(r) = client.get(&get_url).send().await {
        if r.status().is_success() {
            return Ok(noop(
                STAGE_PALACE,
                format!("palace '{name}' already exists"),
            ));
        }
    }

    let url = format!("{base}/api/v1/palaces");
    let body = json!({ "name": name, "cwd": project_root });
    let resp = match client.post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => return Ok(fail(STAGE_PALACE, format!("POST {url}: {e}"))),
    };
    let status = resp.status();
    if status.is_success() {
        Ok(StageOutcome {
            stage: STAGE_PALACE.to_owned(),
            ok: true,
            changed: true,
            detail: format!("created palace '{name}'"),
        })
    } else {
        Ok(fail(STAGE_PALACE, format!("POST {url} returned {status}")))
    }
}

/// Run both project-setup stages, returning their outcomes in order.
///
/// Why: the caller wants a single entry point that provisions the index then the
/// palace; threading the shared client through here keeps `mod.rs` thin.
/// What: builds the HTTP client, runs [`register_index`] then [`create_palace`],
/// and collects the two [`StageOutcome`]s. A client-build error is surfaced as
/// two failure outcomes so the report still renders.
/// Test: side-effecting (network); the individual stages are unit-tested.
pub async fn run_stages(project_root: &std::path::Path) -> Vec<StageOutcome> {
    let client = match build_client() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("HTTP client init failed: {e}");
            return vec![fail(STAGE_INDEX, msg.clone()), fail(STAGE_PALACE, msg)];
        }
    };
    let mut out = Vec::with_capacity(2);
    out.push(
        register_index(&client, project_root)
            .await
            .unwrap_or_else(|e| fail(STAGE_INDEX, e.to_string())),
    );
    out.push(
        create_palace(&client, project_root)
            .await
            .unwrap_or_else(|e| fail(STAGE_PALACE, e.to_string())),
    );
    out
}

#[cfg(test)]
// These tests serialise on a process-global env-var lock (`ENV_TEST_LOCK`) that
// must stay held across the async daemon call (the `TRUSTY_DATA_DIR_OVERRIDE`
// it guards is read inside that call). Holding a std `MutexGuard` across an
// `.await` is the `await_holding_lock` lint's target; here it is intentional and
// safe (test-only serialisation, no cross-task deadlock), so it is allowed.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use crate::commands::ensure::ENV_TEST_LOCK as ENV_LOCK;
    use trusty_common::DATA_DIR_OVERRIDE_ENV;

    /// Spawn a one-shot TCP server that answers the first request with a fixed
    /// HTTP response, and return its `host:port`.
    ///
    /// Why: standing up a real `reqwest` round-trip against a controlled
    /// response lets the stage logic be tested end-to-end without a live daemon.
    /// What: binds an ephemeral loopback port, accepts one connection, reads the
    /// request, writes `response_body` wrapped in a 200 (or the raw status line
    /// when `raw` is set), and returns the bound address.
    async fn stub_once(status_line: &'static str, body: &'static str) -> String {
        stub_seq(vec![(status_line, body)]).await
    }

    /// Spawn a TCP server that answers a *sequence* of requests, one fixed
    /// response per accepted connection in order.
    ///
    /// Why: the palace-create stage issues two requests (an existence GET then a
    /// create POST); covering the "created" branch needs a stub that 404s the
    /// GET then 200s the POST.
    /// What: binds an ephemeral loopback port, accepts `responses.len()`
    /// connections, and writes each `(status_line, body)` in turn.
    async fn stub_seq(responses: Vec<(&'static str, &'static str)>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            for (status_line, body) in responses {
                if let Ok((mut sock, _)) = listener.accept().await {
                    // Drain the request up to (and including) the end-of-headers
                    // marker before replying. A single fixed-size read can split
                    // a request whose `root_path` body is long, which used to
                    // race the write and flake CI; reading until `\r\n\r\n` (or
                    // EOF) consumes the whole header block deterministically. We
                    // don't need the body — only that the request is fully sent.
                    let mut acc = Vec::with_capacity(2048);
                    let mut chunk = [0u8; 2048];
                    loop {
                        match sock.read(&mut chunk).await {
                            Ok(0) => break,
                            Ok(n) => {
                                acc.extend_from_slice(&chunk[..n]);
                                if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    let resp = format!(
                        "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                } else {
                    break;
                }
            }
        });
        addr
    }

    fn stub_data_dir(app: &str, addr: &str) -> std::path::PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "tctl-ensure-setup-{}-{}-{}",
            app,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        trusty_common::write_daemon_addr(app, addr).unwrap();
        tmp
    }

    /// Why: a fresh registration (`created:true`) must report `changed = true`.
    /// What: stub returns `{"created":true}`; assert the outcome.
    /// Test: This is the test.
    #[tokio::test]
    async fn register_index_created() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let addr = stub_once("HTTP/1.1 200 OK", r#"{"id":"proj","created":true}"#).await;
        let dir = stub_data_dir(SEARCH_APP, &addr);
        let client = build_client().unwrap();
        let out = register_index(&client, std::path::Path::new("/tmp/proj"))
            .await
            .unwrap();
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(out.stage, STAGE_INDEX);
        assert!(out.ok);
        assert!(out.changed);
    }

    /// Why: re-registering an existing index (`created:false`) must be an
    /// idempotent no-op (`ok`, `!changed`).
    /// What: stub returns `{"created":false}`; assert the outcome.
    /// Test: This is the test.
    #[tokio::test]
    async fn register_index_already_exists() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let addr = stub_once("HTTP/1.1 200 OK", r#"{"id":"proj","created":false}"#).await;
        let dir = stub_data_dir(SEARCH_APP, &addr);
        let client = build_client().unwrap();
        let out = register_index(&client, std::path::Path::new("/tmp/proj"))
            .await
            .unwrap();
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.ok);
        assert!(!out.changed);
    }

    /// Why: a non-2xx from a reachable daemon means the project is
    /// mis-provisioned and must be a hard failure.
    /// What: stub returns 500; assert `!ok`.
    /// Test: This is the test.
    #[tokio::test]
    async fn register_index_http_error() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let addr = stub_once("HTTP/1.1 500 Internal Server Error", r#"{"error":"boom"}"#).await;
        let dir = stub_data_dir(SEARCH_APP, &addr);
        let client = build_client().unwrap();
        let out = register_index(&client, std::path::Path::new("/tmp/proj"))
            .await
            .unwrap();
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!out.ok);
    }

    /// Why: when the trusty-search daemon is not running, the stage must be an
    /// idempotent no-op so plain `tctl ensure` still exits 0 pre-boot.
    /// What: a data dir with no `http_addr` → `ok`, `!changed`.
    /// Test: This is the test.
    #[tokio::test]
    async fn register_index_daemon_down() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "tctl-ensure-down-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let client = build_client().unwrap();
        let out = register_index(&client, std::path::Path::new("/tmp/proj"))
            .await
            .unwrap();
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(out.ok);
        assert!(!out.changed);
        assert!(out.detail.contains("not running"));
    }

    /// Why: when the trusty-memory daemon is not running, the palace stage must
    /// be an idempotent no-op so plain `tctl ensure` still exits 0 pre-boot.
    /// What: a data dir with no `http_addr` → `ok`, `!changed`, "not running".
    /// Test: This is the test.
    #[tokio::test]
    async fn create_palace_daemon_down() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "tctl-ensure-pdown-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let client = build_client().unwrap();
        let out = create_palace(&client, std::path::Path::new("/tmp/widget"))
            .await
            .unwrap();
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(out.stage, STAGE_PALACE);
        assert!(out.ok);
        assert!(!out.changed);
        assert!(out.detail.contains("not running"));
    }

    /// Why: a fresh palace (existence GET 404s, create POST 200s) must report
    /// `changed = true`.
    /// What: a two-response stub (404 GET, 200 POST); assert `ok` + `changed`.
    /// Test: This is the test.
    #[tokio::test]
    async fn create_palace_created() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let addr = stub_seq(vec![
            ("HTTP/1.1 404 Not Found", r#"{"error":"not found"}"#),
            ("HTTP/1.1 200 OK", r#"{"id":"widget"}"#),
        ])
        .await;
        let dir = stub_data_dir(MEMORY_APP, &addr);
        let client = build_client().unwrap();
        let out = create_palace(&client, std::path::Path::new("/tmp/widget"))
            .await
            .unwrap();
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(out.stage, STAGE_PALACE);
        assert!(out.ok, "detail: {}", out.detail);
        assert!(out.changed);
    }

    /// Why: an existing palace (existence GET 200s) must be an idempotent no-op
    /// without issuing a create POST.
    /// What: a stub that 200s the existence GET; assert `ok` + `!changed`.
    /// Test: This is the test.
    #[tokio::test]
    async fn create_palace_already_exists() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let addr = stub_once("HTTP/1.1 200 OK", r#"{"id":"widget","name":"widget"}"#).await;
        let dir = stub_data_dir(MEMORY_APP, &addr);
        let client = build_client().unwrap();
        let out = create_palace(&client, std::path::Path::new("/tmp/widget"))
            .await
            .unwrap();
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.ok);
        assert!(!out.changed);
        assert!(out.detail.contains("already exists"));
    }

    /// Why: a reachable trusty-memory returning a non-2xx on create (e.g. name
    /// rejected) must be a hard failure.
    /// What: a two-response stub (404 GET so the create is attempted, 500 POST);
    /// assert `!ok`.
    /// Test: This is the test.
    #[tokio::test]
    async fn create_palace_http_error() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let addr = stub_seq(vec![
            ("HTTP/1.1 404 Not Found", r#"{"error":"not found"}"#),
            ("HTTP/1.1 500 Internal Server Error", r#"{"error":"boom"}"#),
        ])
        .await;
        let dir = stub_data_dir(MEMORY_APP, &addr);
        let client = build_client().unwrap();
        let out = create_palace(&client, std::path::Path::new("/tmp/widget"))
            .await
            .unwrap();
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!out.ok);
    }
}
