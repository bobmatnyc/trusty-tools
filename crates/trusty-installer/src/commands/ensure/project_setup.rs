//! Project-setup stages: trusty-search index-register + trusty-memory palace-create.
//!
//! Why: beyond patching `.mcp.json`, fully provisioning a project means the
//! current directory is registered as a trusty-search index and has a
//! trusty-memory palace. Both operations must be idempotent — re-running
//! `ensure` must not error or duplicate — and tolerant of a daemon that is not
//! yet running (plain `tctl ensure` is commonly run before the stack is up).
//!
//! What: [`register_index`] calls `search.index.create` on trusty-search's
//! socket (idempotent: the daemon returns `created:false` for an existing id)
//! and [`create_palace`]
//! calls `palace_create` on trusty-memory's socket (idempotent: a duplicate name
//! resolves to the same palace dir). When the relevant daemon is not running,
//! the stage is reported as an idempotent no-op ("daemon not running; skipped")
//! rather than a hard failure, so `ensure` stays useful pre-boot. A reachable
//! daemon returning an error IS a hard failure (the project is mis-provisioned).
//!
//! Both stages dial Unix sockets: trusty-memory since ADR-0032 (#6286),
//! trusty-search since #9214. Neither reads an `http_addr` file.
//!
//! Test: `tests` stand up stub search and memory sockets to exercise the
//! created / already-exists / daemon-down / error branches for both stages.

use anyhow::Result;
use serde_json::json;

use super::daemon::{
    memory_serving, memory_socket, search_serving, search_socket, MEMORY_CALL_TIMEOUT,
    SEARCH_CALL_TIMEOUT,
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
/// separately. The daemon's `search.index.create` is idempotent (`created:false`
/// for an existing id), so re-running is safe.
/// What: probes trusty-search's `socket`; if nothing is serving it, returns an
/// idempotent no-op. Otherwise derives the index id (directory basename), calls
/// `search.index.create` with `{id, root_path}`, and maps the answer:
/// `created:true` → changed, `created:false` → unchanged no-op, any JSON-RPC
/// error (a conflict included) or failed call → failure carrying the reason.
/// Test: `tests::register_index_dials_the_socket_not_http_addr`,
/// `register_index_already_exists`, `register_index_daemon_errors_fail_with_its_message`,
/// `register_index_dead_or_missing_socket_is_skipped`.
pub async fn register_index(
    socket: &std::path::Path,
    project_root: &std::path::Path,
) -> Result<StageOutcome> {
    // #9214: a bare connect decides "not running"; no http_addr is read.
    if !search_serving(socket).await {
        return Ok(noop(
            STAGE_INDEX,
            "trusty-search daemon not running; skipped (run `tctl start` then re-run)",
        ));
    }
    let Some(id) = identity::index_id_for(project_root) else {
        return Ok(fail(
            STAGE_INDEX,
            format!("cannot derive index id from {}", project_root.display()),
        ));
    };
    let params = json!({ "id": id, "root_path": project_root });
    let method = trusty_common::search_rpc::METHOD_INDEX_CREATE;
    let answer =
        match trusty_common::search_rpc::call_at(socket, method, params, SEARCH_CALL_TIMEOUT).await
        {
            Ok(v) => v,
            // #9214: SearchRpcError's Display carries the daemon's own message.
            Err(e) => return Ok(fail(STAGE_INDEX, format!("{method}: {e:#}"))),
        };
    // The daemon answers `{ "created": bool, .. }`; a result without the field
    // still confirms success, so read it as registered.
    let created = answer
        .get("created")
        .and_then(serde_json::Value::as_bool)
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
/// What: resolves trusty-search's and trusty-memory's sockets, runs
/// [`register_index`] then [`create_palace`], and collects the two
/// [`StageOutcome`]s. Each transport's own init failure fails only the stage it
/// serves — an unresolvable data directory has nothing to do with whether the
/// index registered — so the report still renders either way.
/// Test: side-effecting (network); the individual stages are unit-tested.
pub async fn run_stages(project_root: &std::path::Path) -> Vec<StageOutcome> {
    let mut out = Vec::with_capacity(2);

    out.push(match search_socket() {
        Ok(socket) => register_index(&socket, project_root)
            .await
            .unwrap_or_else(|e| fail(STAGE_INDEX, e.to_string())),
        Err(e) => fail(STAGE_INDEX, format!("{e:#}")),
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
// must stay held across the async daemon call (the `TRUSTY_SEARCH_SOCKET` /
// `TRUSTY_DATA_DIR_OVERRIDE` it guards are read inside that call). Holding a std `MutexGuard` across an
// `.await` is the `await_holding_lock` lint's target; here it is intentional and
// safe (test-only serialisation, no cross-task deadlock), so it is allowed.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use crate::commands::ensure::ENV_TEST_LOCK as ENV_LOCK;
    use crate::commands::test_support::{
        clear_data_dir_override, clear_env, search_stub, set_env, stub_data_dir,
        stub_empty_data_dir, stub_memory_socket, tcp_tripwire,
    };
    use serde_json::Value;
    use std::sync::atomic::Ordering;
    use trusty_common::search_rpc::{CODE_CONFLICT, TRUSTY_SEARCH_SOCKET_ENV};
    use trusty_common::uds::server::RpcError;

    /// Why (#9214): the index stage must dial trusty-search's socket, never the
    /// TCP address a stale `http_addr` names — that listener is going away.
    /// What: a stub socket answering `search.index.create`, reached through
    /// `TRUSTY_SEARCH_SOCKET`, and a counting TCP tripwire behind a planted
    /// `http_addr`; assert `changed`, the exact method literal and
    /// `{id, root_path}` params, and zero TCP connections.
    /// Test: This is the test.
    #[tokio::test]
    async fn register_index_dials_the_socket_not_http_addr() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (seen, daemon) = search_stub(
            "search.index.create",
            Ok(json!({ "id": "proj", "created": true })),
        )
        .await;
        let (addr, hits) = tcp_tripwire().await;
        let dir = stub_data_dir(super::super::daemon::SEARCH_APP, &addr);
        set_env(TRUSTY_SEARCH_SOCKET_ENV, daemon.socket());
        let socket = search_socket().unwrap();
        let out = register_index(&socket, std::path::Path::new("/tmp/proj"))
            .await
            .unwrap();
        clear_env(&[TRUSTY_SEARCH_SOCKET_ENV]);
        clear_data_dir_override(&dir);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "the stale http_addr was dialled"
        );
        assert!(out.ok && out.changed, "detail: {}", out.detail);
        assert_eq!(
            *seen.lock().unwrap_or_else(|e| e.into_inner()),
            vec![(
                "search.index.create".to_string(),
                json!({ "id": "proj", "root_path": "/tmp/proj" })
            )]
        );
    }

    /// Why (#9214): with no `http_addr` and no TCP listener at all, a serving
    /// socket is the whole story — the stage must still register.
    /// What: an empty data dir and a stub socket; assert `ok` + `changed`.
    /// Test: This is the test.
    #[tokio::test]
    async fn register_index_created_over_the_socket_without_http_addr() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_seen, daemon) = search_stub(
            "search.index.create",
            Ok(json!({ "id": "proj", "created": true })),
        )
        .await;
        let dir = stub_empty_data_dir("tctl-ensure-uds");
        set_env(TRUSTY_SEARCH_SOCKET_ENV, daemon.socket());
        let socket = search_socket().unwrap();
        let out = register_index(&socket, std::path::Path::new("/tmp/proj"))
            .await
            .unwrap();
        clear_env(&[TRUSTY_SEARCH_SOCKET_ENV]);
        clear_data_dir_override(&dir);
        assert_eq!(out.stage, STAGE_INDEX);
        assert!(out.ok && out.changed, "detail: {}", out.detail);
    }

    /// Why: re-registering an existing index (`created:false`) must be an
    /// idempotent no-op (`ok`, `!changed`).
    /// What: the stub answers `{"created":false}`; assert the outcome.
    /// Test: This is the test.
    #[tokio::test]
    async fn register_index_already_exists() {
        let (_seen, daemon) = search_stub(
            "search.index.create",
            Ok(json!({ "id": "proj", "created": false })),
        )
        .await;
        let out = register_index(daemon.socket(), std::path::Path::new("/tmp/proj"))
            .await
            .unwrap();
        assert!(out.ok, "detail: {}", out.detail);
        assert!(!out.changed);
        assert!(out.detail.contains("already registered"), "{}", out.detail);
    }

    /// Why (#9214): a reachable daemon that refuses — a `409`-style conflict or
    /// any other error — means the project is mis-provisioned. That must be a
    /// hard failure carrying the daemon's own reason, never a skip.
    /// What: one stub per refusal; assert `!ok` and that the message survives.
    /// Test: This is the test.
    #[tokio::test]
    async fn register_index_daemon_errors_fail_with_its_message() {
        for (refusal, message) in [
            (
                RpcError::new(CODE_CONFLICT, "index 'proj' is registered to /elsewhere"),
                "registered to /elsewhere",
            ),
            (
                RpcError::internal("registry unreadable"),
                "registry unreadable",
            ),
        ] {
            let (_seen, daemon) = search_stub("search.index.create", Err(refusal)).await;
            let out = register_index(daemon.socket(), std::path::Path::new("/tmp/proj"))
                .await
                .unwrap();
            assert!(!out.ok, "a refusal must fail the stage: {}", out.detail);
            assert!(out.detail.contains(message), "{}", out.detail);
        }
    }

    /// Why: when the trusty-search daemon is not running, the stage must be an
    /// idempotent no-op so plain `tctl ensure` still exits 0 pre-boot. Since
    /// #9214 "not running" is a socket nothing serves: a missing path, or a
    /// stale socket file a dead daemon left behind.
    /// What: both shapes → `ok`, `!changed`, "not running".
    /// Test: This is the test.
    #[tokio::test]
    async fn register_index_dead_or_missing_socket_is_skipped() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let stale = tmp.path().join("stale.sock");
        drop(std::os::unix::net::UnixListener::bind(&stale).expect("bind"));
        assert!(stale.exists(), "the stale socket file must remain");
        for socket in [tmp.path().join("absent.sock"), stale] {
            let out = register_index(&socket, std::path::Path::new("/tmp/proj"))
                .await
                .unwrap();
            assert!(out.ok, "{}: {}", socket.display(), out.detail);
            assert!(!out.changed);
            assert!(out.detail.contains("not running"), "{}", out.detail);
        }
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
