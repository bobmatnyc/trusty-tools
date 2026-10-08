//! ADR-0071 acceptance criteria for `memory_recall_all` (#9299).
//!
//! Why: the default scope must search resident palaces only, open none, leave
//! idle clocks alone, and report what it did not search on every surface.
//! `scope: "all"` must keep the old ranking. Each test here failed against
//! `origin/main` cb47439155 except two preservation tests that passed there:
//! `scope_all_top5_matches_the_golden` pins the old answer, and
//! `both_scopes_demote_a_superseded_drawer` pins #9421's demotion.
//! What: drives the MCP dispatch, the chat tool and `MemoryService` with the
//! mock embedder seeded, so the vector lane runs. Palaces that must be cold are
//! created on a throwaway runtime and registry, so no handle or redb lock
//! outlives the setup.
//! Test: this IS the test module.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use serde_json::{json, Value};
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::memory_core::{Drawer, PalaceRegistry};
use uuid::Uuid;

use super::core::MemoryService;
use crate::tools::dispatch_tool;
use crate::AppState;

/// A ready state rooted at `root`, with the mock embedder seeded.
async fn ready_state(root: &Path) -> AppState {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    // #88: bypass the project-slug enforcement gate, under the crate's env
    // lock (#5937).
    let guard = crate::commands::env_test_lock().lock().await;
    // SAFETY: every test in this process wants the same idempotent "1", and
    // the lock excludes every test that reads it under the lock.
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }
    drop(guard);
    let state = AppState::new(root.to_path_buf());
    state.set_ready();
    state
}

/// Create palaces on disk, none left open. `true` stores one drawer.
///
/// What: runs on its own thread and runtime and drops the registry before
/// returning, so every KG writer task is gone and no redb lock remains.
fn create_cold(root: &Path, specs: &[(&str, bool)]) {
    let root = root.to_path_buf();
    let specs: Vec<(String, bool)> = specs.iter().map(|(n, d)| (n.to_string(), *d)).collect();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let writer = PalaceRegistry::new();
            for (name, with_drawer) in &specs {
                let palace = Palace {
                    id: PalaceId::new(name.as_str()),
                    name: name.clone(),
                    description: None,
                    created_at: Utc::now(),
                    data_dir: root.join(name),
                };
                let handle = writer.create_palace(&root, palace).expect("create");
                if *with_drawer {
                    let drawer = Drawer::new(Uuid::new_v4(), "a stored rust build fact");
                    handle.kg.store().upsert_drawer(&drawer).expect("upsert");
                }
            }
        });
    })
    .join()
    .expect("setup thread");
}

/// Make `name` resident in `state` and drop the caller's `Arc`.
fn make_resident(state: &AppState, name: &str) {
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(name))
        .unwrap_or_else(|e| panic!("open {name}: {e:#}"));
    drop(handle);
}

/// Create `name` in `state`'s registry and remember each text in it.
async fn resident_with(state: &AppState, name: &str, texts: &[&str]) {
    let palace = Palace {
        id: PalaceId::new(name),
        name: name.to_string(),
        description: None,
        created_at: Utc::now(),
        data_dir: state.data_root.join(name),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create palace");
    for text in texts {
        let args = json!({ "palace": name, "text": text, "force": true });
        dispatch_tool(state, "memory_remember", args)
            .await
            .expect("memory_remember");
    }
}

/// Run `memory_recall_all` over MCP.
async fn mcp(state: &AppState, args: Value) -> Value {
    dispatch_tool(state, "memory_recall_all", args)
        .await
        .expect("memory_recall_all")
}

/// Run the chat `memory_recall_all` tool.
async fn chat(state: &AppState, args: Value) -> Value {
    crate::chat::tools::execute_tool("memory_recall_all", &args.to_string(), state).await
}

/// Assert the ADR-0071 D4 invariant and the `coverage` rule on `out`.
fn assert_d4(out: &Value) {
    let n = |k: &str| {
        out[k]
            .as_u64()
            .unwrap_or_else(|| panic!("`{k}` missing: {out}"))
    };
    let not_searched = n("palaces_not_searched");
    assert_eq!(
        n("palaces_total"),
        n("palaces_searched") + n("palaces_skipped") + not_searched,
        "D4 invariant: {out}"
    );
    let by_reason: u64 = out["not_searched_by_reason"]
        .as_object()
        .unwrap_or_else(|| panic!("`not_searched_by_reason` missing: {out}"))
        .values()
        .filter_map(Value::as_u64)
        .sum();
    assert_eq!(by_reason, not_searched, "reasons sum: {out}");
    let expected = if not_searched == 0 {
        "complete"
    } else {
        "partial"
    };
    assert_eq!(out["coverage"], expected, "{out}");
}

/// The `content` of each hit, in rank order.
fn contents(out: &Value) -> Vec<String> {
    out["results"]
        .as_array()
        .unwrap_or_else(|| panic!("results: {out}"))
        .iter()
        .map(|r| r["content"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// ADR-0071 AC1: a default-scope call opens no palace and leaves the registry
/// id set unchanged.
///
/// Why: on main each call cold-opened every non-resident, non-empty palace and
/// released it again, so the id set matched but the opens still happened. A
/// sentinel `unopenable` record proves no open: a successful open clears it
/// (`PalaceRegistry::register_arc`).
/// What: one resident palace, two cold non-empty ones, one cold empty one.
#[tokio::test]
async fn default_scope_opens_no_palace_and_keeps_the_resident_set() {
    let tmp = tempfile::tempdir().expect("tempdir");
    create_cold(
        tmp.path(),
        &[
            ("res-a", true),
            ("cold-b", true),
            ("cold-c", true),
            ("empty-d", false),
        ],
    );
    let state = ready_state(tmp.path()).await;
    make_resident(&state, "res-a");
    let sentinel = "sentinel: cleared by any successful open";
    for cold in ["cold-b", "cold-c"] {
        state
            .registry
            .record_unopenable(PalaceId::new(cold), sentinel.to_string());
    }
    let ids = |s: &AppState| {
        let mut v = s.registry.list();
        v.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        v
    };
    let before = ids(&state);

    let out = mcp(&state, json!({"q": "rust build fact", "top_k": 5})).await;

    for cold in ["cold-b", "cold-c"] {
        assert_eq!(
            state
                .registry
                .unopenable_reason(&PalaceId::new(cold))
                .as_deref(),
            Some(sentinel),
            "default scope opened {cold}: {out}"
        );
    }
    assert_eq!(ids(&state), before, "the resident set changed");
    assert_d4(&out);
    assert_eq!(out["scope"], "resident", "{out}");
    assert_eq!(out["palaces_total"], 4, "{out}");
    assert_eq!(out["palaces_searched"], 1, "{out}");
    assert_eq!(out["palaces_skipped"], 1, "{out}");
    assert_eq!(out["not_searched_by_reason"], json!({"not_resident": 2}));
    for hit in out["results"].as_array().into_iter().flatten() {
        assert_eq!(hit["palace_id"], "res-a", "{out}");
    }
}

/// ADR-0071 AC2: recall_all does not reset a searched palace's idle clock.
///
/// Why (ruling 3): on main the search path called `PalaceHandle::touch`, so a
/// recall_all every few minutes kept the whole resident set alive.
/// What: a resident palace that has never been touched is searched by a
/// default-scope call, then `evict_idle` must still drop it. The same holds
/// for `scope: "all"` after the palace is made resident again.
#[tokio::test]
async fn default_scope_does_not_reset_the_idle_clock() {
    let tmp = tempfile::tempdir().expect("tempdir");
    create_cold(tmp.path(), &[("idle-a", true)]);
    let state = ready_state(tmp.path()).await;
    let ttl = Duration::from_secs(300);

    for scope in [json!(null), json!("all")] {
        make_resident(&state, "idle-a");
        let out = mcp(&state, json!({"q": "rust build fact", "scope": scope})).await;
        assert_eq!(out["palaces_searched"], 1, "scope {scope}: {out}");
        assert_eq!(
            state.registry.evict_idle(ttl),
            1,
            "scope {scope}: the searched palace's idle clock was reset"
        );
        assert!(state.registry.peek(&PalaceId::new("idle-a")).is_none());
    }
}

/// ADR-0071 AC3: a palace that fails to open is reported, on every surface.
///
/// Why: on main `open_palaces_blocking` dropped it with a log line and
/// `palaces_searched` still counted it, so a partial answer looked complete.
/// The chat and service surfaces returned a bare array with no counts at all.
/// What: `bad-b` has a directory where `kg.redb` belongs, so its open fails at
/// the OS layer. `scope: "all"` on MCP and chat (which runs
/// `MemoryService::recall_all_scoped`) must name it under `open_failed`; the
/// service default scope must carry the D4 fields too.
#[tokio::test]
async fn open_failure_is_reported_on_every_surface() {
    let tmp = tempfile::tempdir().expect("tempdir");
    create_cold(tmp.path(), &[("ok-a", true)]);
    let mut record: Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join("ok-a/palace.json")).expect("read palace.json"),
    )
    .expect("parse palace.json");
    record["id"] = json!("bad-b");
    record["name"] = json!("bad-b");
    let bad = tmp.path().join("bad-b");
    std::fs::create_dir_all(&bad).expect("mkdir");
    std::fs::write(bad.join("palace.json"), record.to_string()).expect("write");
    std::fs::create_dir(bad.join("kg.redb")).expect("kg.redb as a directory");
    let state = ready_state(tmp.path()).await;

    let args = json!({"q": "rust build fact", "scope": "all"});
    for (surface, out) in [
        ("mcp", mcp(&state, args.clone()).await),
        ("chat", chat(&state, args.clone()).await),
    ] {
        assert_d4(&out);
        assert_eq!(out["open_failed"], json!(["bad-b"]), "{surface}: {out}");
        assert_eq!(out["palaces_searched"], 1, "{surface}: {out}");
        assert_eq!(out["coverage"], "partial", "{surface}: {out}");
        // #9299 Fail-Open Check: the failed arm advances no residency.
        assert!(
            state.registry.list().is_empty(),
            "{surface}: a palace stayed resident"
        );
    }
    let out = MemoryService::new(state.clone())
        .recall_all("rust build fact", 5, false)
        .await;
    assert_d4(&out);
    assert_eq!(out["palaces_searched"], 0, "service default: {out}");
}

/// An unknown `scope` is an error, never a silent fallback to a default.
///
/// Why (Fail-Open Check): on main the argument was ignored, so a typo such as
/// `"al"` searched a different set than the caller asked for and said nothing.
#[tokio::test]
async fn recall_all_rejects_an_unknown_scope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    create_cold(tmp.path(), &[("any-a", true)]);
    let state = ready_state(tmp.path()).await;

    for scope in [json!("al"), json!(42)] {
        let args = json!({"q": "x", "scope": scope});
        let mcp_out = dispatch_tool(&state, "memory_recall_all", args.clone()).await;
        assert!(mcp_out.is_err(), "MCP accepted scope {scope}: {mcp_out:?}");
        let chat_out = chat(&state, args).await;
        assert!(
            chat_out["error"].is_string(),
            "chat accepted scope {scope}: {chat_out}"
        );
    }
}

/// `top_k: 0` searches nothing and says so.
///
/// Why (Fail-Open Check): the streamed walk returns early for a zero window,
/// so counting its palaces as searched would claim work that never ran.
#[tokio::test]
async fn top_k_zero_reports_every_palace_unsearched() {
    let tmp = tempfile::tempdir().expect("tempdir");
    create_cold(tmp.path(), &[("zero-a", true)]);
    let state = ready_state(tmp.path()).await;
    make_resident(&state, "zero-a");

    for scope in ["resident", "all"] {
        let out = mcp(&state, json!({"q": "x", "top_k": 0, "scope": scope})).await;
        assert_d4(&out);
        assert_eq!(out["palaces_searched"], 0, "{scope}: {out}");
        assert_eq!(out["not_searched_by_reason"], json!({"top_k_zero": 1}));
    }
}

/// The fixed fixture behind the golden test: palace, then drawer texts.
const GOLDEN_FIXTURE: &[(&str, &[&str])] = &[
    (
        "gold-a",
        &[
            "rust build cache lives in the shared target directory",
            "the cafeteria opens at noon",
            "sccache wraps rustc for cache hits",
        ],
    ),
    (
        "gold-b",
        &[
            "cargo build reuses the incremental cache",
            "quarterly planning starts in march",
            "a cold build compiles every dependency",
        ],
    ),
    (
        "gold-c",
        &[
            "the build cache for rust is keyed by the lockfile",
            "gardening notes: water the ferns",
            "release builds use thin lto",
        ],
    ),
    (
        "gold-d",
        &[
            "docker layer cache speeds image builds",
            "rust analyzer indexes the workspace",
            "cache invalidation is hard",
        ],
    ),
];

/// The fixed query behind the golden test.
const GOLDEN_QUERY: &str = "rust build cache";

/// `scope: "all"` top 5 for [`GOLDEN_QUERY`] over [`GOLDEN_FIXTURE`]. The
/// golden test passes unchanged on `origin/main` bd0148dc70, where every
/// recall_all searched every palace, so this is the pre-change answer.
const GOLDEN_TOP5: [&str; 5] = [
    "rust build cache lives in the shared target directory",
    "rust analyzer indexes the workspace",
    "a cold build compiles every dependency",
    "sccache wraps rustc for cache hits",
    "the cafeteria opens at noon",
];

/// Seed [`GOLDEN_FIXTURE`] through MCP, then leave only `gold-a` and `gold-b`
/// resident.
async fn golden_state(root: &Path) -> AppState {
    let state = ready_state(root).await;
    for (palace, texts) in GOLDEN_FIXTURE {
        resident_with(&state, palace, texts).await;
    }
    for cold in ["gold-c", "gold-d"] {
        state.registry.remove(&PalaceId::new(cold));
    }
    state
}

/// ADR-0071 AC4 (#9141 AC3 restated, ruling 5): `scope: "all"` returns the
/// same top 5 as before the change for a fixed query.
///
/// Why: a golden preservation test. It passes on `origin/main`, which ignored
/// `scope` and always searched everything, so it pins the old answer.
/// What: four palaces of three drawers each, remembered with the mock
/// embedder; two are dropped from the registry so the walk must open them.
#[tokio::test]
async fn scope_all_top5_matches_the_golden() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = golden_state(tmp.path()).await;
    let all5 = mcp(
        &state,
        json!({"q": GOLDEN_QUERY, "top_k": 5, "scope": "all"}),
    )
    .await;
    assert_eq!(contents(&all5), GOLDEN_TOP5, "scope all drifted: {all5}");
}

/// Ruling 5, second half: the default top 5 equals the `scope: "all"` ranking
/// restricted to resident palaces.
///
/// Why: the default must be the same ranking over fewer palaces, not a
/// different ranking. On main the default searched every palace, so its top 5
/// held hits from `gold-c` and `gold-d`.
/// What: ranks the whole fixture with `scope: "all"` at a wide `top_k`, keeps
/// the `gold-a` and `gold-b` hits in order, and compares the first five with a
/// default-scope call.
#[tokio::test]
async fn default_top5_is_the_resident_subset_of_scope_all() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = golden_state(tmp.path()).await;
    let wide = mcp(
        &state,
        json!({"q": GOLDEN_QUERY, "top_k": 50, "scope": "all"}),
    )
    .await;
    let restricted: Vec<String> = wide["results"]
        .as_array()
        .unwrap_or_else(|| panic!("results: {wide}"))
        .iter()
        .filter(|r| matches!(r["palace_id"].as_str(), Some("gold-a" | "gold-b")))
        .take(5)
        .map(|r| r["content"].as_str().unwrap_or_default().to_string())
        .collect();
    let default5 = mcp(&state, json!({"q": GOLDEN_QUERY, "top_k": 5})).await;
    assert_eq!(contents(&default5), restricted, "default: {default5}");
}

/// Write one drawer through `memory_remember`; returns its id.
async fn remember_id(state: &AppState, palace: &str, text: &str) -> Uuid {
    let args = json!({ "palace": palace, "text": text, "force": true });
    let out = dispatch_tool(state, "memory_remember", args)
        .await
        .expect("memory_remember");
    out["drawer_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(|| panic!("drawer_id: {out}"))
}

/// Rank of drawer `id` in `out["results"]`, or `None`.
fn rank_of(out: &Value, id: Uuid) -> Option<usize> {
    let id = id.to_string();
    out["results"]
        .as_array()
        .unwrap_or_else(|| panic!("results: {out}"))
        .iter()
        .position(|r| r["drawer_id"].as_str() == Some(id.as_str()))
}

/// #9299 x #9421: both recall_all scopes demote a drawer superseded through a
/// `superseded_by` edge.
///
/// Why: the merge moved #9421's per-batch edge reads into the
/// `recall_all_scoped` search closure. The resident default and the
/// `scope: "all"` walk over a palace that is not resident must both read the
/// edge and rank the old drawer below its replacement.
/// What: the old drawer repeats the query's wording, so similarity alone puts
/// it first. Recalled once while its palace is resident (default scope), then
/// again after the palace leaves the registry (`scope: "all"`, which opens it).
#[tokio::test]
async fn both_scopes_demote_a_superseded_drawer() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = ready_state(tmp.path()).await;
    resident_with(&state, "sup-a", &[]).await;
    let query = "release train deploy window";
    let old = remember_id(
        &state,
        "sup-a",
        "the release train deploy window opens on Thursdays at noon",
    )
    .await;
    let new = remember_id(
        &state,
        "sup-a",
        "Deploys moved: changes now ship Monday mornings after the train",
    )
    .await;
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new("sup-a"))
        .expect("open sup-a");
    trusty_common::memory_core::share::assert_superseded_by(&handle.kg, old, new, "test:9299")
        .await
        .expect("superseded_by edge");
    drop(handle);

    let resident = mcp(&state, json!({"q": query, "top_k": 5})).await;
    state.registry.remove(&PalaceId::new("sup-a"));
    let all = mcp(&state, json!({"q": query, "top_k": 5, "scope": "all"})).await;

    for (scope, out) in [("resident", &resident), ("all", &all)] {
        assert_eq!(out["scope"], scope, "{out}");
        assert_eq!(out["palaces_searched"], 1, "{scope}: {out}");
        let (o, n) = (rank_of(out, old), rank_of(out, new));
        assert!(n.is_some(), "{scope}: replacement recalled: {out}");
        assert!(
            o.is_none() || n < o,
            "{scope}: the superseded drawer must rank below its replacement: {out}"
        );
    }
}

/// Embedder whose first call returns a wrong-dimension vector (#9299).
///
/// Why: the palace's own vector search then errors, which is a real recall
/// failure after a successful open. Later calls embed normally.
#[derive(Default)]
struct FailFirstEmbedder {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl trusty_common::memory_core::embed::Embedder for FailFirstEmbedder {
    async fn embed_batch(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            return Ok(texts.iter().map(|_| vec![1.0; 8]).collect());
        }
        trusty_common::embedder::MockEmbedder::new(384)
            .embed_batch(texts)
            .await
    }

    fn dimension(&self) -> usize {
        384
    }
}

/// A palace whose recall errors after it opened is not counted as searched.
///
/// Why (#9299, Fail-Open Check): `recall_across_palaces` logs and skips such
/// a palace, and both scopes counted every palace handed to the search, so
/// coverage read `complete` while that palace's hits were missing.
/// What: two resident palaces, a search through the real
/// `recall_across_palaces_reporting` with an embedder that breaks the first
/// palace's vector search, in each scope. Asserts one palace searched, the
/// other under `search_failed`, and `coverage: "partial"`.
/// Test: this test.
#[tokio::test]
async fn a_palace_whose_recall_fails_is_not_counted_as_searched() {
    use super::recall_stream::{recall_all_scoped, RecallAllScope};
    use std::sync::Arc;
    use trusty_common::memory_core::embed::Embedder;
    use trusty_common::memory_core::retrieval::recall_across_palaces_reporting;

    for scope in [RecallAllScope::Resident, RecallAllScope::All] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = ready_state(tmp.path()).await;
        resident_with(&state, "sf-a", &["a stored rust build fact about alpha"]).await;
        resident_with(&state, "sf-b", &["a stored rust build fact about beta"]).await;
        let embedder: Arc<dyn Embedder + Send + Sync> = Arc::new(FailFirstEmbedder::default());

        let outcome = recall_all_scoped(&state, scope, "unit", 10, |handles| {
            let embedder = embedder.clone();
            async move {
                recall_across_palaces_reporting(&handles, &embedder, "rust build fact", 10, false)
                    .await
            }
        })
        .await
        .expect("recall_all_scoped");

        let cov = &outcome.coverage;
        assert_eq!(cov.search_failed.len(), 1, "{scope:?}: {cov:?}");
        assert_eq!(cov.palaces_searched, 1, "{scope:?}: {cov:?}");
        let failed = cov.search_failed[0].clone();
        assert!(
            outcome.results.iter().all(|r| r.palace_id != failed),
            "{scope:?}: no hit can come from the failed palace"
        );
        let mut out = serde_json::Map::new();
        cov.insert_into(scope, &mut out);
        assert_eq!(out["coverage"], "partial", "{scope:?}");
        assert_eq!(out["palaces_not_searched"], 1, "{scope:?}");
        assert_eq!(
            out["not_searched_by_reason"],
            json!({ "search_failed": 1 }),
            "{scope:?}"
        );
        assert_eq!(out["search_failed"], json!([failed]), "{scope:?}");
    }
}
