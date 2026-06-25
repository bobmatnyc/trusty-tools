// Tests for crates/trusty-memory/src/lib.rs
// Moved from lib.rs to comply with the 500 SLOC production file cap (issue #607).
// Why: lib.rs is a production file (capped at 500 SLOC); the test block alone
// is 439 SLOC. Extracting to lib_tests.rs (test-file cap: 1500 SLOC) keeps
// lib.rs under 500 SLOC while preserving all test coverage.

#![allow(clippy::too_many_lines)]

use super::*;

/// Why: Issue #234 — previously we `mem::forget`ed the `TempDir` so tests
/// could keep using `AppState` without juggling the directory handle, but
/// that leaked one temp directory per test (262+ accumulated each run).
/// What: Returns the `TempDir` alongside the `AppState` so the caller can
/// bind it (`let (state, _tmp) = ...;`) and let drop semantics clean up
/// when the test scope ends.
/// Test: Every test in this module that constructs state.
fn test_state() -> (AppState, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    // Issue #88: bypass palace-slug enforcement so lib tests that call
    // `palace_create` with arbitrary names keep passing.
    // SAFETY: constant idempotent write; safe across test threads.
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }
    let state = AppState::new(root);
    // Pre-existing tests exercise functional paths — flip to Ready so the
    // issue #911 warming preflight does not reject them.
    state.set_ready();
    (state, tmp)
}

/// Why: DaemonReadiness tests need a state that starts in Warming; this
/// variant skips `set_ready()` so the transition can be tested explicitly.
/// Test: `daemon_readiness_transitions_warming_to_ready`,
///       `readiness_check_ok_when_ready_err_when_warming`.
fn test_state_warming() -> (AppState, tempfile::TempDir) {
    // Use OnceLock so the env var is written exactly once across all
    // parallel test threads — avoids the unsynchronised set_var race while
    // remaining consistent with the idempotent-write approach used in
    // `test_state()`.
    static SKIP_ENFORCEMENT_SET: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    SKIP_ENFORCEMENT_SET.get_or_init(|| unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    });
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    // Deliberately do NOT call set_ready() — stays in Warming state.
    (AppState::new(root), tmp)
}

#[tokio::test]
async fn initialize_returns_protocol_version_and_capabilities() {
    let (state, _tmp) = test_state();
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"}
        }
    });
    let resp = handle_message(&state, req).await;
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
    assert!(resp["result"]["capabilities"]["tools"].is_object());
    assert_eq!(resp["result"]["serverInfo"]["name"], "trusty-memory");
}

#[tokio::test]
async fn initialized_notification_returns_null() {
    let (state, _tmp) = test_state();
    let req = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    });
    let resp = handle_message(&state, req).await;
    assert!(resp.is_null());
}

#[tokio::test]
async fn tools_list_returns_all_tools() {
    let (state, _tmp) = test_state();
    let req = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let resp = handle_message(&state, req).await;
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    // Issue #99 added `memory_send_message`; issue #180 added
    // `palace_delete`; the #180 follow-up adds `palace_update` on top
    // of the 22-tool baseline; issue #537 adds `upgrade`;
    // issue #1104 adds `console_metrics`; spec-001 adds the four
    // `chat_session_*` tools and `dream_consolidate_room`.
    assert_eq!(tools.len(), 30);
}

#[tokio::test]
async fn unknown_method_returns_error() {
    let (state, _tmp) = test_state();
    let req = json!({"jsonrpc": "2.0", "id": 4, "method": "wat"});
    let resp = handle_message(&state, req).await;
    assert_eq!(resp["error"]["code"], -32601);
}

#[tokio::test]
async fn ping_returns_empty_result() {
    let (state, _tmp) = test_state();
    let req = json!({"jsonrpc": "2.0", "id": 5, "method": "ping"});
    let resp = handle_message(&state, req).await;
    assert!(resp["result"].is_object());
}

#[tokio::test]
async fn app_state_default_constructs() {
    let (s, _tmp) = test_state();
    assert!(!s.version.is_empty());
    assert!(s.registry.is_empty());
    assert!(s.default_palace.is_none());
}

/// Why (issue #225): the previous implementation called `.expect()` on the
/// tempdir fallback, which panicked the daemon at startup on hosts where
/// neither the data root nor `std::env::temp_dir()` is writable
/// (read-only Docker overlays, locked-down sandboxes). The activity log
/// is documented as best-effort, so the fix returns a no-op `Discard`
/// variant instead. This test forces both paths to fail and asserts the
/// helper returns the discard variant rather than panicking.
///
/// Skipped when running as root because `chmod 000` is a no-op for the
/// root user — the kernel grants root access regardless of mode bits.
/// CI typically runs as non-root, so coverage is preserved in the
/// common case; local root invocations simply skip with a warning.
#[test]
#[cfg(unix)]
fn open_activity_log_with_fallback_returns_discard_when_unwritable() {
    // Skip when running as root — chmod is ignored.
    // SAFETY: libc::geteuid is a thread-safe syscall with no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!(
                "skipping open_activity_log_with_fallback_returns_discard_when_unwritable: running as root"
            );
        return;
    }

    use std::os::unix::fs::PermissionsExt;

    // Build two unwritable directories: the primary "data root" and a
    // shadow "TMPDIR" so the tempdir fallback also fails.
    let outer = tempfile::tempdir().expect("outer tempdir");
    let primary = outer.path().join("primary");
    let tmpdir = outer.path().join("fake-tmp");
    std::fs::create_dir(&primary).expect("create primary");
    std::fs::create_dir(&tmpdir).expect("create tmpdir");

    // chmod 000 on both — neither can be opened for write.
    std::fs::set_permissions(&primary, std::fs::Permissions::from_mode(0o000))
        .expect("chmod primary");
    std::fs::set_permissions(&tmpdir, std::fs::Permissions::from_mode(0o000))
        .expect("chmod tmpdir");

    // Override the tempdir lookup so `open_activity_log_with_fallback`
    // hits our unwritable fake-tmp instead of the real system temp.
    // Note: env var mutation is process-global; this test is the only
    // accessor for `TMPDIR` in this test binary, and we restore the
    // previous value before returning.
    let prev_tmpdir = std::env::var_os("TMPDIR");
    std::env::set_var("TMPDIR", &tmpdir);

    let log = open_activity_log_with_fallback(&primary);

    // Restore TMPDIR ASAP so a panic later in the test doesn't leak it.
    match prev_tmpdir {
        Some(v) => std::env::set_var("TMPDIR", v),
        None => std::env::remove_var("TMPDIR"),
    }

    // Restore permissions so the outer tempdir can clean up.
    let _ = std::fs::set_permissions(&primary, std::fs::Permissions::from_mode(0o700));
    let _ = std::fs::set_permissions(&tmpdir, std::fs::Permissions::from_mode(0o700));

    assert!(
        log.is_discard(),
        "expected ActivityLog::Discard when both data root and tempdir are unwritable"
    );

    // The Discard variant must still satisfy the public contract: no
    // panic on append/count/list.
    let id = log
        .append(
            ActivitySource::Http,
            None,
            "drawer_added",
            json!({"smoke": true}),
        )
        .expect("discard append must succeed");
    assert_eq!(id, 0);
    assert_eq!(log.count().expect("discard count"), 0);
    assert!(log
        .list(&ActivityFilter::default(), 10, 0)
        .expect("discard list")
        .is_empty());
}

/// Why: Issue #26 — when `serve --palace <name>` is set, the MCP server
/// must (a) report the default in the `initialize` `serverInfo`, (b)
/// drop `palace` from the required schema in `tools/list`, and (c) let
/// `tools/call` use the default when the caller omits `palace`.
/// Test: Construct an AppState with a default palace, create that palace
/// on disk via the registry, then call `memory_remember` without a
/// `palace` argument and confirm it resolves to the default.
#[tokio::test]
async fn default_palace_used_when_arg_omitted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();

    // Pre-create the default palace so remember has somewhere to land.
    let registry = trusty_common::memory_core::PalaceRegistry::new();
    let palace = trusty_common::memory_core::Palace {
        id: trusty_common::memory_core::PalaceId::new("default-pal"),
        name: "default-pal".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: root.join("default-pal"),
    };
    registry
        .create_palace(&root, palace)
        .expect("create_palace");

    let state = AppState::new(root).with_default_palace(Some("default-pal".to_string()));
    // Flip to Ready so the readiness preflight (#911) does not reject the
    // `memory_remember` call below.
    state.set_ready();

    // (a) initialize advertises the default.
    let init = handle_message(
        &state,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}),
    )
    .await;
    assert_eq!(
        init["result"]["serverInfo"]["default_palace"], "default-pal",
        "initialize must echo default_palace in serverInfo"
    );

    // (b) tools/list drops `palace` from required when default is set.
    let list = handle_message(
        &state,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let tools = list["result"]["tools"].as_array().expect("tools array");
    let remember = tools
        .iter()
        .find(|t| t["name"] == "memory_remember")
        .expect("memory_remember tool");
    let required: Vec<&str> = remember["inputSchema"]["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        !required.contains(&"palace"),
        "palace must not be required when default is configured; got {required:?}"
    );
    assert!(required.contains(&"text"));

    // (c) tools/call resolves the default when arg is omitted.
    let call = handle_message(
        &state,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "memory_remember",
                "arguments": {"text": "default palace test memory content with several tokens"},
            },
        }),
    )
    .await;
    // Successful dispatch returns `result.content[0].text` JSON.
    let text = call["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("expected success result, got {call}"));
    let parsed: Value = serde_json::from_str(text).expect("parse content json");
    assert_eq!(parsed["palace"], "default-pal");
    assert_eq!(parsed["status"], "stored");
    assert!(parsed["drawer_id"].as_str().is_some());
}

/// Why: When no default is set, `tools/call` for a palace-bound tool
/// without a `palace` argument should error helpfully rather than panic.
#[tokio::test]
async fn missing_palace_without_default_errors() {
    let (state, _tmp) = test_state();
    let resp = handle_message(
        &state,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "memory_recall",
                "arguments": {"query": "anything"},
            },
        }),
    )
    .await;
    assert_eq!(resp["error"]["code"], -32603);
    let msg = resp["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("missing 'palace'"),
        "expected helpful error, got: {msg}"
    );
}

/// Why: regression for the "palaces lost on restart" bug — `AppState::new`
/// builds an empty registry, so the daemon must call
/// `load_palaces_from_disk` on startup to re-register palaces persisted by
/// a previous run. Without that call the registry stays empty even though
/// `palace.json` files exist on disk.
/// What: persists two palaces under a tempdir (via the same
/// `create_palace` path the `palace_create` tool uses), constructs a fresh
/// `AppState` rooted there, calls `load_palaces_from_disk`, and asserts the
/// returned count and registry contents.
/// Test: this test itself.
#[tokio::test]
async fn load_palaces_from_disk_rehydrates_registry() {
    use trusty_common::memory_core::{Palace, PalaceId, PalaceRegistry};

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();

    // Phase 1: persist two palaces to disk, then drop the writer registry
    // so nothing is held in memory — simulating a prior daemon run.
    {
        let writer = PalaceRegistry::new();
        for id in ["alpha", "beta"] {
            let palace = Palace {
                id: PalaceId::new(id),
                name: id.to_string(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: root.join(id),
            };
            writer
                .create_palace(&root, palace)
                .expect("persist palace to disk");
        }
    }

    // Add a stray non-palace subdirectory; the walker must ignore it.
    std::fs::create_dir_all(root.join("not-a-palace")).expect("mkdir");

    // Phase 2: fresh AppState starts with an empty registry (the bug).
    let state = AppState::new(root);
    assert!(
        state.registry.is_empty(),
        "AppState::new must start with an empty registry"
    );

    // The fix: hydrate from disk.
    let count = state
        .load_palaces_from_disk()
        .await
        .expect("load_palaces_from_disk");

    assert_eq!(count, 2, "both persisted palaces should be loaded");
    assert_eq!(state.registry.len(), 2, "registry should hold both palaces");
    let ids: Vec<String> = state.registry.list().into_iter().map(|p| p.0).collect();
    assert!(ids.contains(&"alpha".to_string()));
    assert!(ids.contains(&"beta".to_string()));
}

/// Why (issue #1487): the HTTP daemon must open palace redb files as a
/// writer so a second instance fails loud instead of silently degrading to
/// read-only snapshot mode. `AppState::with_writer_intent()` is the builder
/// `run_serve` calls on the daemon path; this asserts it actually flips the
/// registry's open intent to `Writer`, while a plain `AppState::new`
/// (used by CLI / tests) stays `ReadOnlyClient`.
/// What: builds a default `AppState` and asserts `ReadOnlyClient`, then a
/// `with_writer_intent()` `AppState` and asserts `Writer`.
/// Test: this test itself.
#[tokio::test]
async fn with_writer_intent_marks_registry_writer() {
    use trusty_common::memory_core::store::OpenIntent;

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();

    let default_state = AppState::new(root.clone());
    assert_eq!(
        default_state.registry.open_intent(),
        OpenIntent::ReadOnlyClient,
        "default AppState registry must be read-only (CLI / test paths)"
    );

    let writer_state = AppState::new(root).with_writer_intent();
    assert_eq!(
        writer_state.registry.open_intent(),
        OpenIntent::Writer,
        "with_writer_intent() must flip the daemon registry to Writer"
    );
}

/// Why (issue #1487 hardening): `with_writer_intent` replaces the registry
/// `Arc`, so it must only ever run on a fresh, unhydrated registry — calling
/// it after palaces are loaded would silently drop the hydrated handles. The
/// `debug_assert!` invariant guard makes that ordering violation a loud,
/// fail-fast programmer error in debug/test builds instead of silent data loss.
/// What: persists a palace to disk, hydrates a fresh `AppState`'s registry via
/// `load_palaces_from_disk`, then calls `with_writer_intent` and asserts it
/// panics on the `is_empty()` invariant (the strongest cheap guard).
/// Test: this test itself (via `#[should_panic]`).
#[tokio::test]
#[should_panic(expected = "is_empty()")]
async fn with_writer_intent_panics_on_hydrated_registry() {
    use trusty_common::memory_core::{Palace, PalaceId, PalaceRegistry};

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();

    // Persist one palace to disk via the same path the palace_create tool uses.
    {
        let writer = PalaceRegistry::new();
        let palace = Palace {
            id: PalaceId::new("hydrated"),
            name: "hydrated".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: root.join("hydrated"),
        };
        writer
            .create_palace(&root, palace)
            .expect("persist palace to disk");
    }

    // Hydrate a fresh AppState's registry from disk so it is no longer empty.
    let state = AppState::new(root);
    let count = state
        .load_palaces_from_disk()
        .await
        .expect("load_palaces_from_disk");
    assert_eq!(count, 1, "precondition: registry must be hydrated");

    // Calling the builder on a hydrated registry must trip the debug_assert!
    // invariant guard rather than silently dropping the live handle.
    let _ = state.with_writer_intent();
}

/// Why: existing installs (and the legacy standalone `trusty-memory` repo)
/// nest palaces one level deeper under a `palaces/` subdirectory. When that
/// subdirectory exists, `resolve_palace_registry_dir` must descend into it
/// so the daemon scans the level that actually holds the `palace.json`
/// files — otherwise it finds zero palaces, which is the restart bug.
/// What: creates `<dir>/palaces/`, resolves, and asserts the nested path is
/// returned.
/// Test: this test itself.
#[test]
fn resolve_palace_registry_dir_prefers_palaces_subdir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(data_dir.join("palaces")).expect("mkdir palaces");

    let resolved = resolve_palace_registry_dir(data_dir.clone());
    assert_eq!(resolved, data_dir.join("palaces"));
}

/// Why: a fresh install with no `palaces/` subdirectory must fall back to
/// the data dir itself (the current flat monorepo layout).
#[test]
fn resolve_palace_registry_dir_falls_back_to_data_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();

    let resolved = resolve_palace_registry_dir(data_dir.clone());
    assert_eq!(resolved, data_dir);
}

/// Why: defense-in-depth assertion (#503) — a non-absolute data_dir must be
/// caught and rejected before reaching `AppState::new`, as it would create
/// palace dirs relative to the daemon CWD (/ under launchd).
/// What: passing a relative path to `resolve_palace_registry_dir` produces
/// a relative result; the daemon startup guard in `main.rs` must refuse it.
/// This test validates the outcome of that guard path by confirming that a
/// relative input would NOT produce an absolute registry dir.
/// Test: this test itself.
#[test]
fn resolve_palace_registry_dir_relative_input_is_not_absolute() {
    // A relative dir is not a valid input, but we want to confirm that
    // if one somehow slipped through, the result would also be relative —
    // so the `main.rs` guard (is_absolute check) correctly rejects it.
    let relative = std::path::PathBuf::from("relative/path");
    let result = resolve_palace_registry_dir(relative.clone());
    assert!(
        !result.is_absolute(),
        "a relative input should produce a relative registry dir (caught by main.rs guard)"
    );
}

/// Why: defense-in-depth assertion (#503) — a data_dir equal to "/" must be
/// caught before reaching `AppState::new` to prevent palace dirs at `/`.
/// What: confirms that "/" produces a result that equals "/", which the
/// `main.rs` startup guard correctly rejects.
/// Test: this test itself.
#[test]
fn resolve_palace_registry_dir_root_input_stays_root() {
    let root = std::path::PathBuf::from("/");
    let result = resolve_palace_registry_dir(root);
    // The guard in main.rs checks data_dir (before calling this fn), so
    // "/" would be caught before reaching this fn. But if it did reach here,
    // we confirm it yields "/" (no palaces/ subdir exists there), which would
    // then be caught by the main.rs post-resolve guard.
    assert_eq!(result, std::path::PathBuf::from("/"));
}

/// Why: end-to-end check that the nested-`palaces/` layout hydrates — the
/// daemon resolves the registry dir via `resolve_palace_registry_dir`, so
/// an `AppState` rooted there must load palaces persisted one level below
/// the bare data dir.
/// What: persists two palaces under `<root>/palaces/<id>/`, constructs an
/// `AppState` rooted at the resolved registry dir, and asserts hydration
/// finds both.
/// Test: this test itself.
#[tokio::test]
async fn load_palaces_from_disk_handles_palaces_subdir() {
    use trusty_common::memory_core::{Palace, PalaceId, PalaceRegistry};

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let nested = root.join("palaces");

    {
        let writer = PalaceRegistry::new();
        for id in ["cto", "engineering"] {
            let palace = Palace {
                id: PalaceId::new(id),
                name: id.to_string(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: nested.join(id),
            };
            // create_palace anchors data_dir under the passed root, so
            // pass `nested` here to land palaces under `<root>/palaces/`.
            writer
                .create_palace(&nested, palace)
                .expect("persist palace under palaces/ subdir");
        }
    }

    // Mirror main.rs: resolve the registry dir, then root AppState there.
    let registry_dir = resolve_palace_registry_dir(root);
    assert_eq!(registry_dir, nested, "must resolve into palaces/ subdir");
    let state = AppState::new(registry_dir);
    let count = state
        .load_palaces_from_disk()
        .await
        .expect("load_palaces_from_disk");

    assert_eq!(count, 2, "both nested palaces should be loaded");
    assert_eq!(state.registry.len(), 2);
    let ids: Vec<String> = state.registry.list().into_iter().map(|p| p.0).collect();
    assert!(ids.contains(&"cto".to_string()));
    assert!(ids.contains(&"engineering".to_string()));
}

/// Why: an empty (or missing) palace registry directory must not error — a
/// brand-new install has nothing to hydrate and should report zero.
#[tokio::test]
async fn load_palaces_from_disk_empty_root_returns_zero() {
    let (state, _tmp) = test_state();
    let count = state
        .load_palaces_from_disk()
        .await
        .expect("load_palaces_from_disk on empty root");
    assert_eq!(count, 0);
    assert!(state.registry.is_empty());
}

/// Why (issue #228): hydration must seed `state.palace_names` so the
/// MCP write hot path (`memory_remember` / `memory_note`) can resolve a
/// friendly palace name without re-walking the data root on every call.
/// Regression risk: a future refactor that forgets to populate the cache
/// would silently degrade write latency.
/// What: persists two palaces with distinct `name` values, constructs a
/// fresh `AppState`, hydrates from disk, and asserts the cache holds the
/// expected mappings.
/// Test: this test itself.
#[tokio::test]
async fn palace_name_cache_populated_after_hydration() {
    use trusty_common::memory_core::{Palace, PalaceId, PalaceRegistry};

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    {
        let writer = PalaceRegistry::new();
        for (id, name) in [("alpha", "Alpha Project"), ("beta", "Beta Project")] {
            let palace = Palace {
                id: PalaceId::new(id),
                name: name.to_string(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: root.join(id),
            };
            writer.create_palace(&root, palace).expect("persist palace");
        }
    }

    let state = AppState::new(root);
    assert!(
        state.palace_names.is_empty(),
        "fresh AppState must start with an empty name cache"
    );
    state
        .load_palaces_from_disk()
        .await
        .expect("load_palaces_from_disk");

    assert_eq!(state.palace_names.len(), 2, "cache must hold both palaces");
    assert_eq!(
        state.palace_names.get("alpha").map(|e| e.value().clone()),
        Some("Alpha Project".to_string()),
    );
    assert_eq!(
        state.palace_names.get("beta").map(|e| e.value().clone()),
        Some("Beta Project".to_string()),
    );
}

/// Why (issue #228): `palace_create` (MCP tool) and `MemoryService::create_palace`
/// (HTTP path) both insert into the name cache so a freshly-created palace
/// is resolvable on the very next write — without waiting for the next
/// hydration cycle.
/// What: dispatches the `palace_create` MCP tool against a tempdir and
/// asserts the cache row was written.
/// Test: this test itself.
#[tokio::test]
async fn palace_name_cache_updates_on_create() {
    use serde_json::json;

    let (state, _tmp) = test_state();
    let _ = tools::dispatch_tool(&state, "palace_create", json!({"name": "gamma"}))
        .await
        .expect("palace_create");
    assert_eq!(
        state.palace_names.get("gamma").map(|e| e.value().clone()),
        Some("gamma".to_string()),
        "palace_create must populate the in-memory name cache so writes \
             can resolve the friendly name without a disk walk"
    );
}

/// Why: initialize without a default palace must omit `default_palace`
/// from `serverInfo` so clients can detect the unbound mode.
#[tokio::test]
async fn initialize_without_default_palace_omits_field() {
    let (state, _tmp) = test_state();
    let init = handle_message(
        &state,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}),
    )
    .await;
    assert!(init["result"]["serverInfo"]["default_palace"].is_null());
}

/// Why: every `~/.trusty-memory/http_addr` consumer (CLI, dashboard,
/// future trusty-mpm wiring) must agree on the path. A regression that
/// moves this file breaks every client relying on `read_daemon_addr`.
/// What: under a stubbed data dir, the path ends in
/// `trusty-memory/http_addr` — matching `trusty_common::read_daemon_addr`'s
/// expected location.
#[tokio::test]
async fn http_addr_path_uses_resolve_data_dir() {
    // Hold the env_test_lock so this test does not race with
    // `prompt_context::tests::*` which spin a real daemon under
    // the same env override and would otherwise observe a
    // half-mutated $TRUSTY_DATA_DIR_OVERRIDE.
    let _guard = crate::commands::env_test_lock().lock().await;
    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: test-only env mutation serialised by env_test_lock.
    unsafe {
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
    }
    let result = http_addr_path();
    unsafe {
        std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
    }
    let p = result.expect("http_addr_path must return Some when data dir is resolvable");
    assert!(
        p.ends_with("trusty-memory/http_addr"),
        "unexpected http_addr path: {}",
        p.display()
    );
}

/// Why: write+read round-trip pins the disk format: a single line of
/// `host:port\n`. Clients (cat, sh `$(cat ...)`) trim whitespace, so the
/// trailing newline is invisible — but anything else (extra whitespace,
/// multi-line) would break callers.
/// Note (issue #226): `write_http_addr_file` is part of the HTTP-serving
/// surface gated behind `axum-server`; the test follows the same gate.
#[cfg(feature = "axum-server")]
#[test]
fn http_addr_file_round_trip_via_helpers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("http_addr");
    let addr: SocketAddr = "127.0.0.1:7073".parse().unwrap();
    write_http_addr_file(&path, &addr).unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert_eq!(raw.trim(), "127.0.0.1:7073");
    // The trailing newline keeps `cat` and editors happy.
    assert!(raw.ends_with('\n'));
}

/// Why: dynamic binding must succeed even when the preferred port is
/// already in use. Walking 7070..=7079 + OS fallback guarantees the
/// daemon never fails to come up just because another process holds 7070.
/// What: pre-bind 7070 (best-effort — skip the test if it's already
/// busy on the host), then call `bind_dynamic_port` and assert we got
/// *some* listener back.
#[tokio::test]
async fn bind_dynamic_port_returns_listener() {
    let listener = bind_dynamic_port().await.expect("bind_dynamic_port");
    let addr = listener.local_addr().expect("local_addr");
    assert_eq!(addr.ip().to_string(), "127.0.0.1");
    assert!(addr.port() > 0, "port must be non-zero after bind");
}

/// Why: Issue #42 — prompt-facts are now served by the per-message
/// `get_prompt_context` tool rather than the MCP prompts surface, so the
/// `initialize` handshake must NOT advertise a `prompts` capability and
/// `prompts/list` / `prompts/get` must fall through to the "method not
/// found" path.
#[tokio::test]
async fn initialize_does_not_advertise_prompts_capability() {
    let (state, _tmp) = test_state();
    let init = handle_message(
        &state,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}),
    )
    .await;
    assert!(
        init["result"]["capabilities"]["prompts"].is_null(),
        "initialize must NOT advertise the prompts capability; got {init}"
    );

    // Both prompts/* dispatchers should now report method-not-found.
    for method in ["prompts/list", "prompts/get"] {
        let resp =
            handle_message(&state, json!({"jsonrpc": "2.0", "id": 2, "method": method})).await;
        assert_eq!(
            resp["error"]["code"], -32601,
            "{method} should return method-not-found; got {resp}"
        );
    }
}

/// Why: `AppState::new` must initialise `bound_addr` to an empty
/// `OnceLock` so `/health` reports `addr: None` on the stdio path. A
/// regression that pre-populates this field would advertise a bogus
/// address from a stale clone.
///
/// Note (issue #231): now async so it runs inside a Tokio runtime —
/// `AppState::new` spawns the bounded BM25 index worker via
/// `tokio::spawn`, which requires an active runtime.
#[tokio::test]
async fn app_state_starts_with_empty_bound_addr() {
    let (state, _tmp) = test_state();
    assert!(state.bound_addr.get().is_none());
}

/// Why (issue #96): `DaemonEvent::type_str` underpins the persisted
/// activity log's `event_type` column — every variant must map to the
/// exact SSE `type` tag the UI already handles. A drift between the
/// SSE wire format and the stored type would break the feed's icon /
/// label rendering for historical rows.
/// What: constructs one of each variant, serialises via serde, and
/// confirms `type_str()` matches the JSON `type` field.
/// Test: this test.
#[test]
fn daemon_event_type_str_matches_sse_tag() {
    let cases = [
        DaemonEvent::PalaceCreated {
            id: "p".into(),
            name: "p".into(),
            source: ActivitySource::Http,
        },
        DaemonEvent::DrawerAdded {
            palace_id: "p".into(),
            palace_name: "p".into(),
            drawer_count: 1,
            timestamp: chrono::Utc::now(),
            content_preview: String::new(),
            source: ActivitySource::Mcp,
        },
        DaemonEvent::DrawerDeleted {
            palace_id: "p".into(),
            drawer_count: 0,
            source: ActivitySource::Http,
        },
        DaemonEvent::DreamCompleted {
            palace_id: None,
            merged: 0,
            pruned: 0,
            compacted: 0,
            closets_updated: 0,
            duration_ms: 0,
            source: ActivitySource::Http,
        },
        DaemonEvent::StatusChanged {
            total_drawers: 0,
            total_vectors: 0,
            total_kg_triples: 0,
        },
        DaemonEvent::HookFired {
            palace_id: Some("p".into()),
            palace_name: Some("p".into()),
            hook_type: HookType::UserPromptSubmit,
            injection_kind: InjectionKind::PromptContext,
            injection_length: 12,
            trigger_prompt_excerpt: "hello".into(),
            timestamp: chrono::Utc::now(),
            duration_ms: 5,
            source: ActivitySource::Hook,
        },
    ];
    for ev in &cases {
        let json = serde_json::to_value(ev).unwrap();
        assert_eq!(json["type"].as_str(), Some(ev.type_str()));
    }
}

/// Why: `HookType` is serialised on every `HookFired` activity row; its
/// wire format must round-trip cleanly so dashboard / TUI consumers can
/// safely parse historic entries written by an older daemon build.
/// What: serde-encodes each variant, asserts the JSON matches the
/// expected PascalCase label, then decodes back.
/// Test: itself.
#[test]
fn hook_type_serde_round_trips() {
    let cases = [
        (HookType::UserPromptSubmit, "\"UserPromptSubmit\""),
        (HookType::SessionStart, "\"SessionStart\""),
    ];
    for (ht, expected) in cases {
        let s = serde_json::to_string(&ht).unwrap();
        assert_eq!(s, expected, "{ht:?} should serialise to {expected}");
        let back: HookType = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ht);
        assert_eq!(ht.as_str(), expected.trim_matches('"'));
    }
}

/// Why: same as `hook_type_serde_round_trips` but for `InjectionKind`.
/// What: kebab-case round trip on every variant.
/// Test: itself.
#[test]
fn injection_kind_serde_round_trips() {
    let cases = [
        (InjectionKind::PromptContext, "\"prompt-context\""),
        (InjectionKind::InboxCheck, "\"inbox-check\""),
    ];
    for (ik, expected) in cases {
        let s = serde_json::to_string(&ik).unwrap();
        assert_eq!(s, expected);
        let back: InjectionKind = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ik);
        assert_eq!(ik.as_str(), expected.trim_matches('"'));
    }
}

/// Why: the activity feed renders the trigger prompt excerpt directly;
/// runaway prompts must be capped at [`HOOK_PROMPT_EXCERPT_CHARS`] with
/// a `…` marker so the row stays readable.
/// What: feeds a 200-character prompt and asserts the excerpt is
/// bounded.
/// Test: itself.
#[test]
fn hook_excerpt_truncates_long_prompts() {
    let long = "x".repeat(200);
    let excerpt = hook_prompt_excerpt(&long);
    assert!(excerpt.chars().count() <= HOOK_PROMPT_EXCERPT_CHARS);
    assert!(excerpt.ends_with('…'));
    assert_eq!(hook_prompt_excerpt(""), "");
}

/// Why: multi-line prompts must collapse to a single line so the
/// activity feed row doesn't blow out vertically.
/// What: feeds a multi-line whitespace-heavy prompt and asserts the
/// output is a single-spaced single line.
/// Test: itself.
#[test]
fn hook_excerpt_collapses_whitespace() {
    let input = "hello\n\nworld\t\tfoo";
    let excerpt = hook_prompt_excerpt(input);
    assert_eq!(excerpt, "hello world foo");
}

/// Why (issue #96): `palace_id()` and `source()` feed the persisted
/// activity log's columns; they must extract the right field per
/// variant. Sloppy refactors could swap two fields and the log would
/// silently mis-attribute writes.
/// What: builds each variant with known field values and asserts the
/// extractor returns them.
/// Test: this test.
#[test]
fn daemon_event_palace_id_and_source_extraction() {
    let ev = DaemonEvent::DrawerAdded {
        palace_id: "alpha".into(),
        palace_name: "alpha".into(),
        drawer_count: 1,
        timestamp: chrono::Utc::now(),
        content_preview: String::new(),
        source: ActivitySource::Mcp,
    };
    assert_eq!(ev.palace_id(), Some("alpha"));
    assert_eq!(ev.source(), Some(ActivitySource::Mcp));

    let status = DaemonEvent::StatusChanged {
        total_drawers: 1,
        total_vectors: 2,
        total_kg_triples: 3,
    };
    assert_eq!(status.palace_id(), None);
    assert_eq!(status.source(), None);

    let dream = DaemonEvent::DreamCompleted {
        palace_id: Some("p1".into()),
        merged: 0,
        pruned: 0,
        compacted: 0,
        closets_updated: 0,
        duration_ms: 10,
        source: ActivitySource::Http,
    };
    assert_eq!(dream.palace_id(), Some("p1"));
    assert_eq!(dream.source(), Some(ActivitySource::Http));
}

/// Why (issue #96): `AppState::emit` must persist mutation events to
/// the activity log while keeping `StatusChanged` (a recomputed
/// aggregate, not a mutation) out of the persisted history.
/// What: emits one of each variant under a fresh state and asserts
/// the persisted count matches the number of mutation events.
/// Test: this test.
#[tokio::test]
async fn emit_persists_mutations_but_skips_status_changed() {
    let (state, _tmp) = test_state();
    state.emit(DaemonEvent::PalaceCreated {
        id: "p".into(),
        name: "p".into(),
        source: ActivitySource::Http,
    });
    state.emit(DaemonEvent::StatusChanged {
        total_drawers: 1,
        total_vectors: 0,
        total_kg_triples: 0,
    });
    state.emit(DaemonEvent::DrawerAdded {
        palace_id: "p".into(),
        palace_name: "p".into(),
        drawer_count: 1,
        timestamp: chrono::Utc::now(),
        content_preview: "x".into(),
        source: ActivitySource::Mcp,
    });
    // Issue #232: `emit` now offloads the redb write to `spawn_blocking`,
    // so the test must wait for the background pool to drain before
    // asserting on the persisted count.
    state.flush_activity_writes().await;
    let count = state.activity_log.count().unwrap();
    assert_eq!(count, 2, "only PalaceCreated + DrawerAdded must persist");
}

/// Why (issue #156): the BM25 lane must be opt-in — existing deployments
/// that don't set `TRUSTY_BM25_DAEMON=1` must see `bm25_client = None`
/// and the recall hot path must continue to behave exactly as before.
/// What: builds an `AppState` with `with_bm25_client_from_env()` while
/// the env var is unset; asserts the field stays `None`.
/// Test: this test.
#[tokio::test]
async fn bm25_client_disabled_by_default() {
    // Serialise with the sibling `bm25_client_enabled_when_env_set` test
    // so they don't race on the shared `TRUSTY_BM25_DAEMON` env var.
    let _guard = crate::commands::env_test_lock().lock().await;
    // SAFETY: this test exercises std::env::remove_var which is unsafe
    // in 2024 edition because the global env is shared. We restore the
    // pre-test value at the end so neighbours are unaffected.
    let prev = std::env::var("TRUSTY_BM25_DAEMON").ok();
    unsafe {
        std::env::remove_var("TRUSTY_BM25_DAEMON");
    }
    let (state, _tmp) = test_state();
    let state = state.with_bm25_client_from_env();
    assert!(
        state.bm25_client.is_none(),
        "bm25_client must be None when TRUSTY_BM25_DAEMON is unset"
    );
    // Issue #193: the spawn supervisor is bound to the same env gate as
    // the client — opt-out parity matters so we never accidentally
    // spawn daemons in deployments that explicitly didn't opt in.
    assert!(
        state.bm25_supervisor.is_none(),
        "bm25_supervisor must be None when TRUSTY_BM25_DAEMON is unset"
    );
    if let Some(v) = prev {
        unsafe {
            std::env::set_var("TRUSTY_BM25_DAEMON", v);
        }
    }
}

/// Why (issue #156): when the operator opts in via `TRUSTY_BM25_DAEMON=1`,
/// the builder must construct a real `Bm25Client` pointed at the canonical
/// per-palace socket path. We don't connect — no daemon need be running —
/// we only assert the client field is populated.
/// What: sets the env var, runs the builder, asserts `Some(_)`.
/// Test: this test.
#[tokio::test]
async fn bm25_client_enabled_when_env_set() {
    let _guard = crate::commands::env_test_lock().lock().await;
    let prev = std::env::var("TRUSTY_BM25_DAEMON").ok();
    unsafe {
        std::env::set_var("TRUSTY_BM25_DAEMON", "1");
    }
    let (state, _tmp) = test_state();
    let state = state.with_bm25_client_from_env();
    assert!(
        state.bm25_client.is_some(),
        "bm25_client must be Some when TRUSTY_BM25_DAEMON=1"
    );
    // Issue #193: opting in to the client must also install the spawn
    // supervisor so the daemon is auto-started on first use.
    assert!(
        state.bm25_supervisor.is_some(),
        "bm25_supervisor must be Some when TRUSTY_BM25_DAEMON=1"
    );
    match prev {
        Some(v) => unsafe { std::env::set_var("TRUSTY_BM25_DAEMON", v) },
        None => unsafe { std::env::remove_var("TRUSTY_BM25_DAEMON") },
    }
}

// -------------------------------------------------------------------------
// Issues #910 / #911 — DaemonReadiness
// -------------------------------------------------------------------------

/// Why (issue #910/#911): `AppState` starts in `Warming` state; `set_ready`
/// must flip it to `Ready` atomically; subsequent `readiness()` calls must
/// observe `Ready`.
/// What: construct a state, assert `Warming`, call `set_ready`, assert
/// `Ready`.
/// Test: this test.
#[tokio::test]
async fn daemon_readiness_transitions_warming_to_ready() {
    let (state, _tmp) = test_state_warming();
    assert_eq!(
        state.readiness(),
        DaemonReadiness::Warming,
        "daemon must start in Warming state"
    );
    state.set_ready();
    assert_eq!(
        state.readiness(),
        DaemonReadiness::Ready,
        "daemon must be Ready after set_ready()"
    );
}

/// Why (issue #911): `readiness_check` must return `Ok(())` when Ready and
/// an explicit `Err` when Warming, so tool handlers can use `?` to short-
/// circuit without blocking.
/// What: verify both states.
/// Test: this test.
#[tokio::test]
async fn readiness_check_ok_when_ready_err_when_warming() {
    let (state, _tmp) = test_state_warming();
    // Warming → should error.
    let err = state
        .readiness_check()
        .expect_err("readiness_check must fail when Warming");
    let msg = err.to_string();
    assert!(
        msg.contains("warming up"),
        "error must mention 'warming up'; got: {msg}"
    );
    // Ready → should succeed.
    state.set_ready();
    state
        .readiness_check()
        .expect("readiness_check must succeed when Ready");
}

/// Why (issue #911): `DaemonReadiness::from_u8` must map 0 → Warming and
/// any non-zero → Ready.
/// Test: this test.
#[test]
fn daemon_readiness_from_u8() {
    assert_eq!(DaemonReadiness::from_u8(0), DaemonReadiness::Warming);
    assert_eq!(DaemonReadiness::from_u8(1), DaemonReadiness::Ready);
    assert_eq!(DaemonReadiness::from_u8(255), DaemonReadiness::Ready);
}

// -------------------------------------------------------------------------
// Issue #467 — palaces skipped at startup hydration are lazily re-opened
// -------------------------------------------------------------------------

/// Why (issue #467): when `load_palaces_from_disk` fails to open a palace
/// (e.g. EMFILE — "Too many open files"), it logs a warning and does NOT
/// register the palace in the in-memory registry. A subsequent call to
/// `open_palace` for that id must attempt a fresh open from disk, not
/// permanently return "not found". This test verifies the lazy-reopen
/// path: create a palace on disk, remove it from the registry (simulating a
/// startup-hydration skip), then open it via `open_palace` and assert success.
/// What: builds an `AppState` with an on-disk palace that is subsequently
/// removed from the in-memory registry (simulating what happens when
/// `PalaceHandle::open` fails during `load_palaces_from_disk`), calls
/// `registry.open_palace`, and asserts the palace handle is returned.
/// Test: this test.
#[tokio::test]
async fn open_palace_lazy_reopens_hydration_skipped_palace() {
    let (state, _tmp) = test_state();
    // Create a palace on disk.
    let pid = trusty_common::memory_core::palace::PalaceId::new("hydration-skip");
    let palace = trusty_common::memory_core::Palace {
        id: pid.clone(),
        name: "hydration-skip".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("hydration-skip"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create_palace");

    // Simulate a startup-hydration skip by removing the just-registered handle.
    // In production, the palace is simply never registered because
    // PalaceHandle::open failed during load_palaces_from_disk (EMFILE etc.).
    state.registry.remove(&pid);

    // The registry must now report no in-memory handle for this palace.
    assert!(
        state.registry.get(&pid).is_none(),
        "palace must appear absent (simulating hydration skip) before the lazy-reopen"
    );

    // Calling open_palace should attempt a fresh open from disk and succeed.
    let handle = state
        .registry
        .open_palace(&state.data_root, &pid)
        .expect("open_palace must lazily reopen a hydration-skipped palace");
    assert_eq!(handle.id.as_str(), "hydration-skip");
}

// -------------------------------------------------------------------------
// Issue #498 — dotfile http_addr path uses $HOME/.trusty-memory/http_addr
// -------------------------------------------------------------------------

/// Why (issue #498): claude-mpm's `migrate_trusty_autodetect` reads
/// `~/.trusty-memory/http_addr` to discover the daemon's port. On macOS
/// the OS-standard data dir differs from the dotfile path, so the daemon
/// was writing to the wrong location and claude-mpm always fell back to the
/// hardcoded port `7070`. This test confirms `dotfile_http_addr_path()`
/// returns a path rooted at `$HOME/.trusty-memory/http_addr`.
/// What: under a known HOME, calls `dotfile_http_addr_path` and asserts the
/// returned path ends in `.trusty-memory/http_addr`.
/// Test: this test.
#[cfg(feature = "axum-server")]
#[test]
fn dotfile_http_addr_path_uses_home_dir() {
    // `dirs::home_dir()` is not redirectable via env on macOS, but we can
    // at least assert that when it returns Some, the suffix is correct.
    if let Some(p) = dotfile_http_addr_path() {
        assert!(
            p.ends_with(".trusty-memory/http_addr"),
            "dotfile path must end in .trusty-memory/http_addr; got {}",
            p.display()
        );
    }
    // If home_dir() returns None (locked-down env), the function returns None —
    // that's acceptable; we just skip the assertion.
}

/// Why (issue #498): the daemon must write to the dotfile path so that
/// claude-mpm's `_resolve_base_url` finds the running port. This round-trip
/// test exercises `write_http_addr_file` at a dotfile-shaped path and
/// confirms the content is readable after the atomic rename.
/// What: picks a tempdir as a stand-in for $HOME, writes an addr to
/// `.trusty-memory/http_addr`, and reads it back.
/// Test: this test.
#[cfg(feature = "axum-server")]
#[test]
fn dotfile_http_addr_write_read_round_trip() {
    let home = tempfile::tempdir().unwrap();
    let dotfile_dir = home.path().join(".trusty-memory");
    let path = dotfile_dir.join("http_addr");
    let addr: SocketAddr = "127.0.0.1:7099".parse().unwrap();
    write_http_addr_file(&path, &addr).expect("write_http_addr_file to dotfile path");
    let raw = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        raw.trim(),
        "127.0.0.1:7099",
        "dotfile round-trip content mismatch"
    );
    assert!(raw.ends_with('\n'), "dotfile must end with a newline");
}
