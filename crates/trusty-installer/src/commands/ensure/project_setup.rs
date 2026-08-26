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
//! calls `palace_create` on trusty-memory's socket (idempotent: a duplicate name
//! resolves to the same palace dir). When the relevant daemon is not running,
//! the stage is reported as an idempotent no-op ("daemon not running; skipped")
//! rather than a hard failure, so `ensure` stays useful pre-boot. A reachable
//! daemon returning an error IS a hard failure (the project is mis-provisioned).
//!
//! The two stages no longer share a transport: trusty-search is still loopback
//! HTTP, and trusty-memory is the Unix socket ADR-0032 moved it onto. See
//! [`super::daemon`] for why the shared resolver could not serve both.
//!
//! Test: `tests` stand up a stub HTTP server and a stub memory socket to
//! exercise the created / already-exists / daemon-down / error branches for
//! both stages.

use anyhow::Result;
use serde_json::json;

use super::daemon::{
    build_client, memory_serving, memory_socket, resolve_base_url, MEMORY_CALL_TIMEOUT, SEARCH_APP,
};
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
///
/// **This stage silently did nothing between #6286 pass A and this fix.**
/// `resolve_base_url(MEMORY_APP)` reads an `http_addr` file ADR-0032 stopped
/// writing, so it answered `None` on every machine and the stage reported
/// "daemon not running; skipped" whether or not one was — a green outcome for a
/// project that was never provisioned.
///
/// What: probes trusty-memory's socket; if nothing is serving it, returns an
/// idempotent no-op. Otherwise derives the palace name (pin-file value or
/// slugified basename — matching the daemon's `validate_palace_name`), asks
/// `memory.palace_get`, and on not-found calls `palace_create` with
/// `{name, cwd}` so the daemon's name-enforcement uses the project path.
/// Test: `tests::create_palace_created`, `create_palace_daemon_down`,
/// `create_palace_already_exists`, `create_palace_rpc_error`.
pub async fn create_palace(
    socket: &std::path::Path,
    project_root: &std::path::Path,
) -> Result<StageOutcome> {
    if !memory_serving(socket).await {
        return Ok(noop(
            STAGE_PALACE,
            "trusty-memory daemon not running; skipped (run `tctl start` then re-run)",
        ));
    }
    let Some(name) = identity::palace_name_for(project_root) else {
        return Ok(fail(
            STAGE_PALACE,
            format!("cannot derive palace name from {}", project_root.display()),
        ));
    };

    // Idempotency fast-path (optimization only — correctness does NOT depend on
    // it): an existing palace answers `memory.palace_get` and we no-op without a
    // create. This assumes trusty-memory derives the palace id from the name, so
    // `palace_id` equals the name (per the trusty-memory palace contract).
    // Should that ever drift, the worst case is a missed fast-path: we fall
    // through to the create below, which is itself idempotent — trusty-memory
    // resolves a duplicate name to the same palace dir (its registry
    // `create_dir_all` is a no-op).
    //
    // Only a NOT-FOUND refusal falls through. Any other error is the daemon
    // saying something went wrong, and creating on top of that would turn a
    // reportable failure into a second one.
    match call_memory(socket, "memory.palace_get", json!({ "palace_id": name })).await {
        Ok(_) => {
            return Ok(noop(
                STAGE_PALACE,
                format!("palace '{name}' already exists"),
            ));
        }
        Err(e) if !is_not_found(&e) => {
            return Ok(fail(STAGE_PALACE, format!("memory.palace_get: {e:#}")));
        }
        Err(_) => {}
    }

    match call_memory(
        socket,
        "palace_create",
        json!({ "name": name, "cwd": project_root }),
    )
    .await
    {
        Ok(_) => Ok(StageOutcome {
            stage: STAGE_PALACE.to_owned(),
            ok: true,
            changed: true,
            detail: format!("created palace '{name}'"),
        }),
        Err(e) => Ok(fail(STAGE_PALACE, format!("palace_create: {e:#}"))),
    }
}

/// One bounded trusty-memory call through the shared client.
///
/// Why: both arms of [`create_palace`] want the same budget, and the shared
/// client is the workspace's one way to reach this daemon.
async fn call_memory(
    socket: &std::path::Path,
    method: &str,
    params: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    trusty_common::memory_rpc::call_memory_tool_at_with_timeout(
        socket,
        method,
        params,
        MEMORY_CALL_TIMEOUT,
    )
    .await
}

/// Did the daemon say the palace does not exist, rather than fail?
///
/// The REST predecessor read this off a 404; the typed error carries the same
/// distinction over the socket (#6286).
fn is_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<trusty_common::memory_rpc::MemoryRpcError>()
        .is_some_and(trusty_common::memory_rpc::MemoryRpcError::is_not_found)
}

/// Run both project-setup stages, returning their outcomes in order.
///
/// Why: the caller wants a single entry point that provisions the index then the
/// palace; resolving each stage's transport here keeps `mod.rs` thin.
/// What: builds the HTTP client for trusty-search and resolves trusty-memory's
/// socket, runs [`register_index`] then [`create_palace`], and collects the two
/// [`StageOutcome`]s. Each transport's own init failure fails only the stage it
/// serves — an unresolvable data directory has nothing to do with whether the
/// index registered — so the report still renders either way.
/// Test: side-effecting (network); the individual stages are unit-tested.
pub async fn run_stages(project_root: &std::path::Path) -> Vec<StageOutcome> {
    let mut out = Vec::with_capacity(2);

    out.push(match build_client() {
        Ok(client) => register_index(&client, project_root)
            .await
            .unwrap_or_else(|e| fail(STAGE_INDEX, e.to_string())),
        Err(e) => fail(STAGE_INDEX, format!("HTTP client init failed: {e}")),
    });

    out.push(match memory_socket() {
        Ok(socket) => create_palace(&socket, project_root)
            .await
            .unwrap_or_else(|e| fail(STAGE_PALACE, e.to_string())),
        Err(e) => fail(STAGE_PALACE, format!("{e:#}")),
    });

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
    // #4246: the stub-server + stubbed-data-dir vehicle these tests grew is now
    // shared with `probe_http`/`verify_tail` — one copy, in `test_support`.
    use crate::commands::test_support::{
        clear_data_dir_override, stub_data_dir, stub_empty_data_dir, stub_memory_socket, stub_once,
    };
    use serde_json::Value;
    use trusty_common::uds::server::RpcError;

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
        clear_data_dir_override(&dir);
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
        clear_data_dir_override(&dir);
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
        clear_data_dir_override(&dir);
        assert!(!out.ok);
    }

    /// Why: when the trusty-search daemon is not running, the stage must be an
    /// idempotent no-op so plain `tctl ensure` still exits 0 pre-boot.
    /// What: a data dir with no `http_addr` → `ok`, `!changed`.
    /// Test: This is the test.
    #[tokio::test]
    async fn register_index_daemon_down() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = stub_empty_data_dir("tctl-ensure-down");
        let client = build_client().unwrap();
        let out = register_index(&client, std::path::Path::new("/tmp/proj"))
            .await
            .unwrap();
        clear_data_dir_override(&tmp);
        assert!(out.ok);
        assert!(!out.changed);
        assert!(out.detail.contains("not running"));
    }

    /// Why: when the trusty-memory daemon is not running, the palace stage must
    /// be an idempotent no-op so plain `tctl ensure` still exits 0 pre-boot.
    /// What: a socket path nothing has ever bound → `ok`, `!changed`, "not
    /// running".
    /// Test: This is the test.
    #[tokio::test]
    async fn create_palace_daemon_down() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out = create_palace(
            &tmp.path().join("absent.sock"),
            std::path::Path::new("/tmp/widget"),
        )
        .await
        .unwrap();
        assert_eq!(out.stage, STAGE_PALACE);
        assert!(out.ok);
        assert!(!out.changed);
        assert!(out.detail.contains("not running"), "{}", out.detail);
    }

    /// Why: a fresh palace (`memory.palace_get` refuses not-found,
    /// `palace_create` succeeds) must report `changed = true`.
    /// What: a stub socket that refuses the get with the daemon's own not-found
    /// code and accepts the create; assert `ok` + `changed`, and that the create
    /// carried both `name` and `cwd` — the daemon's name-enforcement reads the
    /// second.
    /// Test: This is the test.
    #[tokio::test]
    async fn create_palace_created() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, Value)>::new()));
        let recorder = std::sync::Arc::clone(&seen);
        let daemon = stub_memory_socket(move |method: &str, params: Value| {
            recorder
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((method.to_string(), params));
            let method = method.to_string();
            Box::pin(async move {
                if method == "memory.palace_get" {
                    Err(RpcError::new(
                        trusty_common::memory_rpc::CODE_NOT_FOUND,
                        "palace not found: widget",
                    ))
                } else {
                    Ok(json!({ "id": "widget" }))
                }
            })
        })
        .await;

        let out = create_palace(daemon.socket(), std::path::Path::new("/tmp/widget"))
            .await
            .unwrap();
        assert_eq!(out.stage, STAGE_PALACE);
        assert!(out.ok, "detail: {}", out.detail);
        assert!(out.changed);

        let calls = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(
            calls.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>(),
            vec!["memory.palace_get", "palace_create"],
            "the fast-path get must precede the create"
        );
        assert_eq!(calls[1].1["name"], "widget");
        assert_eq!(
            calls[1].1["cwd"], "/tmp/widget",
            "the daemon's name-enforcement reads cwd; omitting it would fail a \
             real project whose slug is pinned"
        );
    }

    /// Why: an existing palace must be an idempotent no-op without issuing a
    /// create — and, since #6286, WITHOUT the fast-path being the only thing
    /// standing between a green report and a project that was never
    /// provisioned.
    /// What: a stub that answers `memory.palace_get`; assert `ok` + `!changed`
    /// and that nothing else was called.
    /// Test: This is the test.
    #[tokio::test]
    async fn create_palace_already_exists() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let recorder = std::sync::Arc::clone(&seen);
        let daemon = stub_memory_socket(move |method: &str, _params: Value| {
            recorder
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(method.to_string());
            Box::pin(async move { Ok(json!({ "id": "widget", "name": "widget" })) })
        })
        .await;

        let out = create_palace(daemon.socket(), std::path::Path::new("/tmp/widget"))
            .await
            .unwrap();
        assert!(out.ok);
        assert!(!out.changed);
        assert!(out.detail.contains("already exists"));
        assert_eq!(
            *seen.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["memory.palace_get".to_string()],
            "an existing palace must not be re-created"
        );
    }

    /// Why: a reachable trusty-memory that refuses the create (e.g. the name is
    /// rejected) means the project is mis-provisioned, and that must be a hard
    /// failure carrying the daemon's own message rather than a silent skip.
    /// What: a stub that refuses the get as not-found and the create with an
    /// internal error; assert `!ok` and that the message survives.
    /// Test: This is the test.
    #[tokio::test]
    async fn create_palace_rpc_error() {
        let daemon = stub_memory_socket(|method: &str, _params: Value| {
            let method = method.to_string();
            Box::pin(async move {
                if method == "memory.palace_get" {
                    Err(RpcError::new(
                        trusty_common::memory_rpc::CODE_NOT_FOUND,
                        "palace not found",
                    ))
                } else {
                    Err(RpcError::internal("palace name rejected"))
                }
            })
        })
        .await;

        let out = create_palace(daemon.socket(), std::path::Path::new("/tmp/widget"))
            .await
            .unwrap();
        assert!(!out.ok);
        assert!(
            out.detail.contains("palace name rejected"),
            "the daemon's own reason must survive: {}",
            out.detail
        );
    }

    /// Why (#6286 review, finding 3): a `memory.palace_get` failure that is NOT
    /// not-found means the daemon is in trouble, and creating on top of it turns
    /// one reportable failure into a second. The REST predecessor had the same
    /// hazard and the same fix — only a 404 fell through to the POST.
    /// What: a stub that refuses the get with an internal error; assert the
    /// stage fails naming the get, and that no create was attempted.
    /// Test: This is the test.
    #[tokio::test]
    async fn create_palace_does_not_create_over_a_non_not_found_failure() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let recorder = std::sync::Arc::clone(&seen);
        let daemon = stub_memory_socket(move |method: &str, _params: Value| {
            recorder
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(method.to_string());
            Box::pin(async move { Err(RpcError::internal("registry unreadable")) })
        })
        .await;

        let out = create_palace(daemon.socket(), std::path::Path::new("/tmp/widget"))
            .await
            .unwrap();
        assert!(!out.ok, "{}", out.detail);
        assert!(out.detail.contains("memory.palace_get"), "{}", out.detail);
        assert_eq!(
            *seen.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["memory.palace_get".to_string()],
            "a failing probe must not be followed by a create"
        );
    }
}
