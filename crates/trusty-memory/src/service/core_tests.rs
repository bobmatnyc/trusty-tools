//! Palace and drawer CRUD, and the absent-vs-undeterminable split on open.
//!
//! Why this file exists (#6286): these tests lived in `web::tests::
//! palace_tests`, `palace_crud_tests` and `palace_open_error_tests`, and drove
//! an in-process axum router that ADR-0032 retired. Almost none of what they
//! asserted was about HTTP — every one of them called a `MemoryService` method
//! through a route that did nothing but decode the path and re-encode the
//! result. They now call `MemoryService` directly.
//!
//! **The status codes became `ServiceError` variants, one for one.** 404 is
//! `NotFound`, 409 is `Conflict`, 400 is `BadRequest`, 500 is `Internal`, and
//! 200/204 is `Ok`. Nothing was widened or dropped: #5549's whole point is that
//! `NotFound` and `Internal` are different answers, and that distinction is
//! what these tests still assert.
//!
//! **The names are unchanged, including the ones that say `404` and `500`.**
//! Those numbers are how #5549 and #180 are written up in their own issues, so
//! the name still points a reader at the defect it guards — and keeping them
//! means every `Test:` pointer in `service/core.rs` still resolves.
//!
//! Two tests are NOT here, because what they asserted no longer exists:
//! `memories_alias_routes_to_drawers` pinned that `…/memories` and `…/drawers`
//! reached the same handler, and `unknown_api_returns_404` pinned that an
//! unregistered path 404s. A listener with no paths can be asked neither.
//! `transport::uds::tests`' `rpc_reports_method_not_found_for_an_unknown_method`
//! is the second one's replacement on the surface that does exist.
//!
//! Test: run with `cargo test -p trusty-memory service::core_tests`.

use serde_json::json;

use crate::service::{
    CreateDrawerBody, CreatePalaceBody, ListDrawersQuery, MemoryService, ServiceError,
};
use crate::{ActivitySource, AppState};

/// Build a fresh `AppState` rooted in an ephemeral tempdir.
///
/// Lifted from the retired `web::tests` module, which is where every test in
/// this file used to get it.
fn test_state() -> AppState {
    // Seed the process-wide embedder cell with the mock: under per-test process
    // isolation each test gets a virgin cell and would otherwise reach for the
    // real ONNX model (#4413). Idempotent, so calling it here is free.
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    // #88: bypass the project-slug enforcement gate so a test can name a palace
    // freely without a real project root on disk.
    // SAFETY: every test in this process wants the same idempotent "1".
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }
    let state = AppState::new(root);
    // #911: flip past the warming preflight so writes run.
    state.set_ready();
    state
}

/// A `MemoryService` over a fresh state, plus the state itself.
fn service() -> (MemoryService, AppState) {
    let state = test_state();
    (MemoryService::new(state.clone()), state)
}

/// The body `POST /api/v1/palaces` used to carry.
fn palace_body(name: &str) -> CreatePalaceBody {
    CreatePalaceBody {
        name: name.to_string(),
        description: None,
        cwd: None,
        force: false,
    }
}

/// The body `POST …/drawers` used to carry, with only `content` set.
fn drawer_body(content: &str) -> CreateDrawerBody {
    CreateDrawerBody {
        content: content.to_string(),
        room: None,
        tags: Vec::new(),
        importance: None,
        force: None,
    }
}

/// Attribution for a write with no caller-supplied identity — what an
/// unadorned request produced when the `X-Trusty-Client-*` headers were absent.
fn default_creator() -> crate::attribution::CreatorInfo {
    crate::transport::methods::CallerParams::default().creator()
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// Why: the console header and external tooling read `version` and the palace
/// count off this payload; a field that changed type or vanished would break
/// them silently.
/// Test: itself.
#[tokio::test]
async fn status_endpoint_returns_payload() {
    let (svc, _state) = service();
    let payload = serde_json::to_value(svc.status().await).expect("status serialises");
    assert!(payload["version"].is_string());
    assert_eq!(payload["palace_count"], 0);
}

/// Why: the dashboard's top-row stats render the `total_*` counters directly,
/// so they must be present and zero on an empty data root rather than absent —
/// otherwise the UI has to special-case a missing field.
/// Test: itself.
#[tokio::test]
async fn status_includes_total_counters() {
    let (svc, _state) = service();
    let payload = serde_json::to_value(svc.status().await).expect("status serialises");
    assert_eq!(payload["total_drawers"], 0);
    assert_eq!(payload["total_vectors"], 0);
    assert_eq!(payload["total_kg_triples"], 0);
}

// ---------------------------------------------------------------------------
// Palace CRUD
// ---------------------------------------------------------------------------

/// Why: the round trip is the base case — a palace that is created and then
/// not listed would break every consumer at once.
/// Test: itself.
#[tokio::test]
async fn create_then_list_palace() {
    let (svc, _state) = service();
    svc.create_palace(palace_body("web-test"), ActivitySource::Http)
        .await
        .expect("create");

    let palaces = svc.list_palaces().await.expect("list");
    assert!(palaces.iter().any(|p| p.id == "web-test"));
}

/// Why: the operator TUI's MEMORY tab reads `node_count`, `edge_count`,
/// `community_count` and `is_compacting` straight off the palace list. If any
/// disappears or changes type the counters break silently, so the shape is
/// pinned here.
/// Test: itself.
#[tokio::test]
async fn palace_list_includes_graph_counts() {
    let (svc, _state) = service();
    svc.create_palace(palace_body("graph-counts"), ActivitySource::Http)
        .await
        .expect("create");

    let listed = serde_json::to_value(svc.list_palaces().await.expect("list"))
        .expect("palace list serialises");
    let row = listed
        .as_array()
        .expect("array")
        .iter()
        .find(|p| p["id"] == "graph-counts")
        .expect("created palace must appear in the list")
        .clone();

    assert_eq!(row["node_count"].as_u64(), Some(0));
    assert_eq!(row["edge_count"].as_u64(), Some(0));
    assert_eq!(row["community_count"].as_u64(), Some(0));
    assert_eq!(row["is_compacting"].as_bool(), Some(false));
}

/// Why (#180): the happy path — an empty palace deletes, stops resolving, and
/// its directory goes with it. The on-disk assertion is the one that catches a
/// delete that only forgot the registry entry.
/// Test: itself.
#[tokio::test]
async fn delete_palace_removes_dir_when_empty() {
    let (svc, state) = service();
    svc.create_palace(palace_body("to-delete"), ActivitySource::Http)
        .await
        .expect("create");

    // `force` defaults to false; a fresh palace has no drawers, so the conflict
    // guard does not fire.
    svc.delete_palace("to-delete", false)
        .await
        .expect("an empty palace deletes without force");

    assert!(
        matches!(
            svc.get_palace("to-delete").await,
            Err(ServiceError::NotFound(_))
        ),
        "a deleted palace must stop resolving"
    );

    let palace_dir = state.data_root.join("to-delete");
    assert!(
        !palace_dir.exists(),
        "palace dir should be removed: {}",
        palace_dir.display()
    );
}

/// Why (#180): without `force` a palace that still holds drawers must be
/// refused, or a stray delete drops hours of memory in one call. `Conflict` is
/// the answer because the request is well formed and the STATE says no.
/// Test: itself.
#[tokio::test]
async fn delete_palace_refuses_when_drawers_present() {
    let (svc, _state) = service();
    svc.create_palace(palace_body("keep-me"), ActivitySource::Http)
        .await
        .expect("create");
    svc.create_drawer(
        "keep-me",
        drawer_body("Important fact that should not be deleted accidentally."),
        default_creator(),
        ActivitySource::Http,
    )
    .await
    .expect("seed a drawer so the conflict guard fires");

    assert!(
        matches!(
            svc.delete_palace("keep-me", false).await,
            Err(ServiceError::Conflict(_))
        ),
        "a populated palace must be refused without force"
    );

    // And the palace is still there.
    svc.get_palace("keep-me")
        .await
        .expect("a refused delete must leave the palace resolvable");
}

/// Why (#180): `force` is the explicit destructive opt-in — the conflict guard
/// must yield to it and the palace must vanish with its drawers.
/// Test: itself.
#[tokio::test]
async fn delete_palace_force_removes_populated_palace() {
    let (svc, _state) = service();
    svc.create_palace(palace_body("force-delete"), ActivitySource::Http)
        .await
        .expect("create");
    svc.create_drawer(
        "force-delete",
        drawer_body("Sacrificial drawer for the force-delete path."),
        default_creator(),
        ActivitySource::Http,
    )
    .await
    .expect("seed a drawer");

    svc.delete_palace("force-delete", true)
        .await
        .expect("force must override the conflict guard");

    assert!(
        matches!(
            svc.get_palace("force-delete").await,
            Err(ServiceError::NotFound(_))
        ),
        "a force-deleted palace must stop resolving"
    );
}

/// Why (#180): deleting an id nobody created must be `NotFound`, so an
/// idempotent retry is distinguishable from the drawers-present precondition.
/// Test: itself.
#[tokio::test]
async fn delete_palace_returns_not_found_for_missing_id() {
    let (svc, _state) = service();
    assert!(matches!(
        svc.delete_palace("never-existed", false).await,
        Err(ServiceError::NotFound(_))
    ));
}

/// Why (#180 follow-up): an operator must be able to relabel a palace without
/// re-creating it — which would lose every drawer, vector and triple. Only the
/// display name moves; the id is the directory and is immutable.
/// Test: itself.
#[tokio::test]
async fn update_palace_name_renames_palace() {
    let (svc, _state) = service();
    svc.create_palace(palace_body("rename-me"), ActivitySource::Http)
        .await
        .expect("create");

    let updated = svc
        .update_palace_name_typed("rename-me", "New Display Name")
        .await
        .expect("rename");
    assert_eq!(updated["id"].as_str(), Some("rename-me"));
    assert_eq!(updated["name"].as_str(), Some("New Display Name"));

    // Re-read, so the assertion is about what reached disk rather than what the
    // call happened to return.
    let reread = svc.get_palace("rename-me").await.expect("get");
    assert_eq!(reread.id, "rename-me");
    assert_eq!(reread.name, "New Display Name");
}

/// Why (#180 follow-up): an empty or whitespace-only name would leave the
/// dashboard with a blank label. `BadRequest` says the request was well formed
/// and the VALUE is not.
/// Test: itself.
#[tokio::test]
async fn update_palace_name_rejects_empty_name() {
    let (svc, _state) = service();
    svc.create_palace(palace_body("keep-name"), ActivitySource::Http)
        .await
        .expect("create");

    assert!(matches!(
        svc.update_palace_name_typed("keep-name", "   ").await,
        Err(ServiceError::BadRequest(_))
    ));
}

/// Why (#180 follow-up): renaming an id nobody created must surface, not
/// silently no-op, or a typo in the id looks like a successful rename.
/// Test: itself.
#[tokio::test]
async fn update_palace_name_returns_not_found_for_missing_id() {
    let (svc, _state) = service();
    assert!(matches!(
        svc.update_palace_name_typed("no-such-palace", "irrelevant")
            .await,
        Err(ServiceError::NotFound(_))
    ));
}

/// Why (#5549): hardening `load_palace` to tell absent from undeterminable
/// buys nothing if the caller flattens the distinction again.
/// `update_palace_name_typed` mapped EVERY `PalaceStoreError` through
/// `NotFound`, so a palace whose `palace.json` could not be stat'd was reported
/// as one that does not exist — for a denial or a transient `EIO` that
/// established no such thing.
/// What: creates a palace, strips its directory to mode 000 so stat of the
/// metadata inside is denied, and renames it. Asserts `Internal` and
/// specifically NOT `NotFound`. Panics rather than passing vacuously if the
/// denial does not take hold.
/// Test: itself.
#[cfg(unix)]
#[tokio::test]
async fn update_palace_name_reports_an_unstattable_palace_as_internal() {
    let (svc, state) = service();
    svc.create_palace(palace_body("locked-palace"), ActivitySource::Http)
        .await
        .expect("create");

    let palace_dir = state.data_root.join("locked-palace");
    let _restore = deny_statting_metadata(&palace_dir);

    match svc
        .update_palace_name_typed("locked-palace", "New Display Name")
        .await
    {
        Err(ServiceError::NotFound(m)) => panic!(
            "a palace whose metadata cannot be stat'd was reported as absent — that is the \
             #5549 coercion re-created at the caller: {m}"
        ),
        Err(ServiceError::Internal(_)) => {}
        other => panic!("expected Internal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Drawer CRUD
// ---------------------------------------------------------------------------

/// Why (#5231): deleting a drawer id that was never stored answered success —
/// identical to a real delete — because `PalaceHandle::forget` returned `Ok`
/// either way. `delete_palace` has told the truth since #180; this path now
/// matches.
///
/// The malformed-id half of the original test is gone with the route: a
/// non-UUID used to be rejected by axum's path extractor, and a JSON-RPC
/// params struct takes a `String` the service parses. That parse is covered by
/// `transport::methods::chat`'s `message_mark_read`, which is the other caller
/// that takes a drawer id as text.
/// Test: itself.
#[tokio::test]
async fn delete_drawer_404s_for_an_unknown_drawer_id() {
    let (svc, _state) = service();
    svc.create_palace(palace_body("ghost-drawer"), ActivitySource::Http)
        .await
        .expect("create");

    assert!(
        matches!(
            svc.delete_drawer(
                "ghost-drawer",
                "deadbeef-0000-4000-8000-000000000000",
                ActivitySource::Http,
            )
            .await,
            Err(ServiceError::NotFound(_))
        ),
        "an unknown drawer id must be refused, not silently succeed"
    );
}

/// Why (#3225): `content` runs the signal/noise QUALITY gate by default, and a
/// JSON-shaped payload is a designed rejection target —
/// `non_alphabetic_ratio` counts braces, quotes, colons and digits. This proves
/// the gate is still active for a caller that does not opt out.
/// Test: itself; paired with the `force` test below.
#[tokio::test]
async fn create_drawer_rejects_json_content_without_force() {
    let (svc, _state) = service();
    svc.create_palace(palace_body("force-gate-reject"), ActivitySource::Http)
        .await
        .expect("create");

    let result = svc
        .create_drawer(
            "force-gate-reject",
            drawer_body(r#"{"score":0.42,"id":"a1b2","tier":3}"#),
            default_creator(),
            ActivitySource::Http,
        )
        .await;
    assert!(
        result.is_err(),
        "JSON-shaped content without `force` must still be rejected by the quality gate"
    );
}

/// Why (#3225): `force: true` must reach `RememberOptions::force` and bypass
/// the QUALITY gate for exactly the content shape the test above proves is
/// rejected without it. trusty-agents' client JSON-serialises its payload as
/// `content` for lossless round-tripping and would otherwise fail every write.
/// Test: itself.
#[tokio::test]
async fn create_drawer_force_bypasses_quality_gate_for_json_content() {
    let (svc, _state) = service();
    svc.create_palace(palace_body("force-gate-accept"), ActivitySource::Http)
        .await
        .expect("create");

    let mut body = drawer_body(r#"{"score":0.42,"id":"a1b2","tier":3}"#);
    body.force = Some(true);
    svc.create_drawer(
        "force-gate-accept",
        body,
        default_creator(),
        ActivitySource::Http,
    )
    .await
    .expect("`force: true` must bypass the quality gate for JSON-shaped content");
}

/// Why (#133): PR #106 wired auto-KG-extraction only into the MCP path, so a
/// write through the other surface silently left the palace's graph empty. The
/// route is gone; `MemoryService::create_drawer` is the function that carried
/// the defect and still carries the fix.
/// What: writes a drawer with tags, a room and a `#hashtag`, then reads the
/// graph and asserts the auto-provenance triples landed. The tag `test` is on
/// the extraction deny-list (#278), so the fixture uses `backend`.
/// Test: itself.
#[tokio::test]
async fn http_create_drawer_runs_auto_kg_extraction() {
    let (svc, _state) = service();
    svc.create_palace(palace_body("kgauto-http"), ActivitySource::Http)
        .await
        .expect("create");

    let body = CreateDrawerBody {
        content: "trusty-memory is a Rust crate that ships an MCP server. \
                  It tracks #mcp and #rust topics with care."
            .to_string(),
        room: Some("Backend".to_string()),
        tags: vec!["backend".to_string(), "kg".to_string()],
        importance: Some(0.5),
        force: None,
    };
    svc.create_drawer("kgauto-http", body, default_creator(), ActivitySource::Http)
        .await
        .expect("create_drawer");

    let graph = svc.kg_graph("kgauto-http").await.expect("kg_graph");
    assert!(
        !graph.triples.is_empty(),
        "a drawer written through this path must populate the KG; got an empty graph"
    );
    let auto: Vec<_> = graph
        .triples
        .iter()
        .filter(|t| t.provenance.as_deref() == Some(crate::kg_extract::AUTO_PROVENANCE))
        .collect();
    assert!(
        !auto.is_empty(),
        "expected at least one auto-extracted triple; got: {:?}",
        graph.triples
    );
    assert!(
        auto.iter().any(|t| t.subject == "tag:backend"),
        "expected a `tag:backend` auto-extracted edge, got: {auto:?}"
    );
    assert!(
        auto.iter().any(|t| t.predicate == "mentioned-in"),
        "expected at least one #hashtag mention triple, got: {auto:?}"
    );
}

// ---------------------------------------------------------------------------
// open_handle: absent vs undeterminable (#5549, ADR-0045)
// ---------------------------------------------------------------------------

/// Restore a path's mode on drop, including while unwinding from a failed
/// assertion, so a locked path never outlives the test that locked it.
///
/// The mode is a field because the fixtures lock different kinds of path: a
/// regular `palace.json` (0o600) and the directory holding it (0o700), which
/// would be left untraversable by a file's mode.
#[cfg(unix)]
struct RestoreMode {
    path: std::path::PathBuf,
    mode: u32,
}

#[cfg(unix)]
impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(self.mode));
    }
}

/// Make an existing palace's `palace.json` present and stattable but
/// unreadable — `load_palace`'s `try_exists` probe succeeds and the
/// `std::fs::read` under it is denied, which is `PalaceStoreError::Io`.
#[cfg(unix)]
fn deny_reading_metadata(palace_dir: &std::path::Path) -> RestoreMode {
    use std::os::unix::fs::PermissionsExt;

    let target = palace_dir.join("palace.json");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o000)).unwrap();
    let restore = RestoreMode {
        path: target.clone(),
        mode: 0o600,
    };

    // Root bypasses the mode bits outright and some filesystems ignore them, so
    // confirm the denial actually took hold. A vacuous pass on a fail-open guard
    // is worse than no test at all.
    match std::fs::read(&target) {
        Ok(_) => panic!(
            "cannot exercise #5549: {} is still readable at mode 000. Run this suite as a \
             non-root user on a filesystem that honours POSIX permission bits.",
            target.display()
        ),
        Err(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied,
            "expected the locked palace.json to deny reads, got {e}"
        ),
    }
    restore
}

/// Make an existing palace's `palace.json` undeterminable — the probe for
/// whether it is even there fails.
///
/// Why (#5574): `load_palace` used to guard with `Path::exists()`, which is
/// `fs::metadata(..).is_ok()`, so this shape returned `NotFound` and could not
/// reach the branch under test. #5574 switched that guard to `try_exists()`, so
/// a denied stat now raises `Io` — and that the two changes compose is a claim
/// about a live code path, so it is asserted rather than inferred.
#[cfg(unix)]
fn deny_statting_metadata(palace_dir: &std::path::Path) -> RestoreMode {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(palace_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let restore = RestoreMode {
        path: palace_dir.to_path_buf(),
        mode: 0o700,
    };

    // Same vacuous-pass guard as above, against the probe `load_palace` actually
    // makes rather than the read below it.
    let target = palace_dir.join("palace.json");
    match target.try_exists() {
        Ok(_) => panic!(
            "cannot exercise #5549: {} is still stattable with its directory at mode 000. Run \
             this suite as a non-root user on a filesystem that honours POSIX permission bits.",
            target.display()
        ),
        Err(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied,
            "expected the locked directory to deny statting palace.json, got {e}"
        ),
    }
    restore
}

/// Create `id` and evict its cached handle, returning the on-disk directory.
///
/// Why the eviction: creating the palace leaves a live handle in the registry,
/// which would satisfy every later open without touching disk — and the point
/// of these tests is what happens when disk says no.
#[cfg(unix)]
async fn create_and_evict(svc: &MemoryService, state: &AppState, id: &str) -> std::path::PathBuf {
    svc.create_palace(palace_body(id), ActivitySource::Http)
        .await
        .expect("palace create must succeed");
    state
        .registry
        .remove(&trusty_common::memory_core::PalaceId::new(id));
    state.data_root.join(id)
}

/// Why (#5549, ADR-0045): `MemoryService::open_handle` mapped every open
/// failure to `NotFound`. It is the helper every KG read, the drawer CRUD and
/// per-palace recall funnel through, so the coercion reached far more of the
/// surface than the two `update_palace_name` sites #5574 fixed.
/// What: locks a real palace's metadata against reads and asserts the open
/// reports `Internal`, and specifically NOT `NotFound`. The direction is the
/// point, not merely that it failed.
/// Test: itself.
#[cfg(unix)]
#[tokio::test]
async fn unreadable_palace_is_500_not_404_at_the_service_open_handle() {
    let (svc, state) = service();
    let dir = create_and_evict(&svc, &state, "unreadable-svc").await;
    let _restore = deny_reading_metadata(&dir);

    match svc
        .list_drawers("unreadable-svc", ListDrawersQuery::default())
        .await
    {
        Err(ServiceError::NotFound(m)) => panic!(
            "a palace whose metadata cannot be read was reported as absent — that is the \
             #5549 coercion at MemoryService::open_handle: {m}"
        ),
        Err(ServiceError::Internal(_)) => {}
        other => panic!("expected Internal, got {other:?}"),
    }
}

/// Why (#5549, ADR-0045): the second undeterminable shape, and the one that
/// only became reachable when #5574 merged — `load_palace` used to answer
/// `NotFound` for a `palace.json` it could not stat.
/// What: locks the palace's DIRECTORY so the stat probe itself is denied, and
/// asserts `Internal` rather than `NotFound`.
/// Test: itself.
#[cfg(unix)]
#[tokio::test]
async fn unstattable_palace_is_500_not_404_at_the_service_open_handle() {
    let (svc, state) = service();
    let dir = create_and_evict(&svc, &state, "unstattable-svc").await;
    let _restore = deny_statting_metadata(&dir);

    match svc
        .list_drawers("unstattable-svc", ListDrawersQuery::default())
        .await
    {
        Err(ServiceError::NotFound(m)) => panic!(
            "a palace whose metadata could not even be statted was reported as absent — #5574 \
             made that an Io error at load_palace, and open_handle flattened it back: {m}"
        ),
        Err(ServiceError::Internal(_)) => {}
        other => panic!("expected Internal, got {other:?}"),
    }
}

/// Why (#5549, ADR-0045 decision 3): the fix must not turn a genuine absence
/// into an internal error. `NotFound` keeps its benign meaning — otherwise
/// every typo in a palace name becomes an opaque server fault.
/// What: asks both helpers for an id no palace was ever created under and
/// asserts `NotFound` from each. `transport::api_error::open_handle` is the
/// second helper; it used to be `web::error::open_handle` and serves the same
/// callers the `/api/v1/kg/*` and `/api/v1/messages` routes did. Uses no
/// permission bits, so it runs on any platform and under any uid.
/// Test: itself.
#[tokio::test]
async fn absent_palace_is_still_404_at_both_open_handles() {
    let (svc, state) = service();

    assert!(
        matches!(
            svc.open_handle("never-created"),
            Err(ServiceError::NotFound(_))
        ),
        "an absent palace must still be NotFound at MemoryService::open_handle"
    );

    let via_api = crate::transport::api_error::open_handle(&state, "never-created")
        .err()
        .expect("an absent palace cannot be opened");
    assert_eq!(
        via_api.kind,
        crate::transport::ErrorKind::NotFound,
        "an absent palace must still be NotFound at transport::api_error::open_handle: {}",
        via_api.message
    );
}

// ---------------------------------------------------------------------------
// Dream status
// ---------------------------------------------------------------------------

/// Why: the dashboard's first load hits this before any palace has dreamed, so
/// it must answer a well-shaped payload rather than erroring on the empty case.
/// Test: itself.
#[tokio::test]
async fn dream_status_empty_returns_nulls() {
    let (svc, _state) = service();
    let payload =
        serde_json::to_value(svc.dream_status_aggregate().await).expect("dream status serialises");
    assert!(payload["last_run_at"].is_null());
    assert_eq!(payload["merged"], 0);
    assert_eq!(payload["pruned"], 0);
}

/// Why: `memory.drawers_list` is the folded method that replaced the route, and
/// the params reshape — palace id from path segment to field — is the one thing
/// a service-level test cannot see.
/// Test: itself.
#[tokio::test]
async fn drawers_list_reads_through_the_folded_method() {
    let (svc, state) = service();
    svc.create_palace(palace_body("folded-list"), ActivitySource::Http)
        .await
        .expect("create");

    let listed = crate::transport::methods::palaces::list_drawers(
        &state,
        serde_json::from_value(json!({ "palace_id": "folded-list" })).expect("params decode"),
    )
    .await
    .expect("the folded method must read the same palace the service wrote");
    assert!(listed.is_array(), "drawers_list answers an array: {listed}");
}
