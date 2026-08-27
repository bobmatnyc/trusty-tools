//! Integration tests for the trusty-memory MCP tool surface — issue #59.
//!
//! Why: When the HTTP daemon owns the exclusive redb lock on a palace, the
//! stdio MCP client opens the palace via the snapshot fallback and must:
//!   - Serve every read tool (`memory_recall`, `memory_recall_deep`,
//!     `kg_query`, `palace_info`, `memory_list`) without error.
//!   - Reject every write tool (`memory_remember`, `memory_forget`,
//!     `kg_assert`) with a clear, actionable error string instead of a
//!     panic or stack trace.
//!
//! Beyond the read-only matrix this file exercises the full tool surface
//! end-to-end (content correctness), concurrent reader semantics, and
//! gates a set of `#[ignore]`d performance budgets so regressions in the
//! hot path are caught with `cargo test -- --include-ignored`.
//!
//! What: Drives every assertion through `trusty_memory::tools::dispatch_tool`
//! against an `AppState` rooted at a `tempfile::TempDir`. Each test gets a
//! private palace directory so cross-test interference is impossible. The
//! read-only matrix simulates the daemon-locked-the-file condition by
//! opening a raw `redb::Database` against the palace's `kg.redb` /
//! `index.usearch.redb` to acquire the exclusive flock, then opening a
//! fresh `AppState` whose `PalaceHandle::open` falls back to a snapshot.
//!
//! Test: `cargo test -p trusty-memory --test mcp_stdio_tools` for content
//! and concurrency; add `-- --include-ignored` to include the perf budgets.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use redb::Database;
use serde_json::{json, Value};
use tempfile::TempDir;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::AppState;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Pre-seed the process-wide shared embedder with `MockEmbedder`.
///
/// Why: `memory_remember` / `memory_recall` resolve `retrieval::shared_embedder()`,
/// a process-wide `OnceCell`. Whichever test seeds it first wins for the whole
/// process, so under `cargo test` — one process per binary — a single sibling's
/// seed silently satisfied every other test here. Under per-test process
/// isolation (`cargo nextest run`) each test gets a virgin cell instead, reaches
/// for the real ONNX model, and fails on the HuggingFace download (HTTP 429 in
/// CI). Same defect class as #4413: a test that passes only because a sibling
/// ran first.
/// What: delegates to `seed_shared_embedder_with_mock`, which is idempotent
/// (`OnceCell::set`, first caller wins), so calling it from every fixture is
/// free and safe regardless of order.
/// Test: every test in this file, via `Fixture::new` or `seed_palace`.
fn seed_embedder() {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
}

/// Hold an `AppState` together with the tempdir that backs it so cleanup
/// happens at the end of the test instead of on `AppState` drop.
///
/// Why: `AppState::new` only borrows the path; if the tempdir is dropped
/// inside the constructor the storage files vanish under the open handles.
/// What: Bundles the temp directory with the `AppState`, exposes the
/// inner state via `Deref`-like accessors.
/// Test: Indirect — every test uses `Fixture::new`.
struct Fixture {
    _tmp: TempDir,
    state: AppState,
}

impl Fixture {
    fn new() -> Self {
        seed_embedder();
        let tmp = tempfile::tempdir().expect("tempdir");
        // Issue #88: bypass palace-slug enforcement so integration tests that
        // use arbitrary palace names keep passing. The env var is idempotent
        // ("1" once set stays "1") so concurrent test threads are safe.
        // SAFETY: constant idempotent write; races are benign.
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }
        let state = AppState::new(tmp.path().to_path_buf());
        // Flip to Ready so the issue #911 warming preflight does not reject
        // memory_remember / memory_recall calls made in integration tests.
        state.set_ready();
        Self { _tmp: tmp, state }
    }

    fn state(&self) -> &AppState {
        &self.state
    }

    fn data_root(&self) -> &Path {
        &self.state.data_root
    }
}

/// Create a palace via the MCP tool surface so the test mirrors what a
/// real stdio client would do.
///
/// Why: Keeps every test on the same well-trodden path through
/// `dispatch_tool` rather than poking the registry directly.
/// What: Dispatches `palace_create` with the given name; panics on error
/// because failure here means the harness is broken, not the SUT.
/// Test: Indirect.
async fn create_palace(state: &AppState, name: &str) {
    dispatch_tool(state, "palace_create", json!({ "name": name }))
        .await
        .expect("palace_create");
}

/// Convenience: dispatch `memory_remember` and return the created drawer
/// id as a string.
///
/// Why: `dispatch_tool` returns JSON; almost every test needs the id
/// back as a `String` so callers can later `memory_forget` it.
/// What: Calls `memory_remember` with default importance and the supplied
/// content and tags; extracts `drawer_id` from the response.
/// Test: Indirect.
async fn remember(state: &AppState, palace: &str, text: &str, tags: &[&str]) -> String {
    let tag_values: Vec<Value> = tags.iter().map(|t| json!(t)).collect();
    let res = dispatch_tool(
        state,
        "memory_remember",
        json!({
            "palace": palace,
            "text": text,
            "room": "General",
            "tags": tag_values,
        }),
    )
    .await
    .expect("memory_remember");
    // #6297: the response, not a bare "drawer_id in response". `memory_remember`
    // answers Ok with a rejection payload when the content gate declines, so the
    // old message named the missing field and hid the reason it was missing —
    // which is how three perf fixtures reported a setup panic that said nothing
    // about the gate that caused it.
    res["drawer_id"]
        .as_str()
        .unwrap_or_else(|| panic!("memory_remember returned no drawer_id: {res}"))
        .to_string()
}

/// Open the redb files under a palace data dir with raw `Database::create`
/// to simulate a peer process (the HTTP daemon) holding the exclusive
/// flock. The returned databases must be kept alive for the duration of
/// the test.
///
/// Why: Issue #59's snapshot fallback only triggers when redb refuses the
/// exclusive open with `DatabaseAlreadyOpen`. Holding raw `Database`
/// handles bypasses the in-process cache, so the next `KgStoreRedb::open`
/// / `UsearchStore::new` against the same paths takes the snapshot path.
/// What: Opens `<data_dir>/kg.redb` and `<data_dir>/index.usearch.redb`
/// (the names that the storage layer derives from the palace dir layout).
/// Test: Indirect — used by every `read_only_*` test.
fn lock_palace_files(data_dir: &Path) -> (Database, Database) {
    let kg_path = data_dir.join("kg.redb");
    let vec_path = data_dir.join("index.usearch.redb");
    let kg_lock = Database::create(&kg_path).expect("lock kg.redb");
    let vec_lock = Database::create(&vec_path).expect("lock vector redb");
    (kg_lock, vec_lock)
}

/// Open a *new* `AppState` against the same data root as `original` so the
/// in-process redb cache is bypassed; the locks held by
/// `lock_palace_files` force the new state's `PalaceHandle::open` down the
/// snapshot path.
///
/// Why: Without a fresh `AppState` the second open would hit the cached
/// `KgDbState` and return the live (read/write) database instead of
/// falling back to a snapshot.
/// What: Wraps `data_root` in a new `AppState`.
/// Test: Indirect.
fn fresh_state(data_root: &Path) -> AppState {
    let state = AppState::new(data_root.to_path_buf());
    // Tests that call fresh_state are in Ready context; set_ready keeps the
    // issue #911 preflight from blocking any subsequent remember/recall calls.
    state.set_ready();
    state
}

// ---------------------------------------------------------------------------
// Content correctness — happy path
// ---------------------------------------------------------------------------

/// Why: Round-trip the canonical write surface: store a drawer through
/// `memory_remember`, then prove it's retrievable through `memory_recall`.
/// What: Creates a palace, remembers a single drawer, recalls with a
/// related query, asserts the drawer's content appears in the top results.
/// Test: this test.
#[tokio::test]
async fn remember_then_recall_returns_drawer() {
    let fx = Fixture::new();
    create_palace(fx.state(), "round-trip").await;
    let drawer_id = remember(
        fx.state(),
        "round-trip",
        "Quokkas are small marsupials native to a few small islands off the coast of Western Australia",
        &["wildlife"],
    )
    .await;
    assert!(!drawer_id.is_empty());

    let recalled = dispatch_tool(
        fx.state(),
        "memory_recall",
        json!({"palace": "round-trip", "query": "quokka marsupial Australia", "top_k": 5}),
    )
    .await
    .expect("memory_recall");
    let results = recalled["results"].as_array().expect("results array");
    assert!(
        results
            .iter()
            .any(|r| r["content"].as_str().unwrap_or("").contains("Quokkas")),
        "expected to recall the seeded drawer; got {results:?}"
    );
}

/// Why: `memory_recall` returns results in ranked order; the highest-
/// scoring hit must be the drawer most semantically similar to the query.
/// What: Stores three drawers with distinct topics, queries with text
/// targeting one of them, and asserts the matching drawer wins.
/// Test: this test.
#[tokio::test]
async fn recall_ranks_best_match_first() {
    let fx = Fixture::new();
    create_palace(fx.state(), "rank").await;
    remember(
        fx.state(),
        "rank",
        "The Rust borrow checker prevents data races at compile time",
        &["rust"],
    )
    .await;
    remember(
        fx.state(),
        "rank",
        "Python uses reference counting combined with a cyclic collector for garbage collection of objects",
        &["python"],
    )
    .await;
    remember(
        fx.state(),
        "rank",
        "JavaScript engines use generational garbage collection with separate young and old object generations",
        &["js"],
    )
    .await;

    let recalled = dispatch_tool(
        fx.state(),
        "memory_recall",
        json!({"palace": "rank", "query": "rust ownership and borrow checker", "top_k": 3}),
    )
    .await
    .expect("memory_recall");
    let results = recalled["results"].as_array().expect("results array");
    // Skip the L0 identity (always at index 0 when present) and find the
    // first L2 hit.
    let first_l2 = results
        .iter()
        .find(|r| r["layer"].as_u64().unwrap_or(0) >= 2)
        .expect("at least one L2 result");
    assert!(
        first_l2["content"]
            .as_str()
            .unwrap_or("")
            .contains("borrow checker"),
        "best match should be the Rust drawer; got {first_l2:?}"
    );
}

/// Why: `memory_recall_deep` runs L3 (full HNSW search) instead of L2's
/// metadata-filtered search; it must return at least as many results as
/// the shallow recall over a small palace.
/// What: Stores five drawers, runs both `memory_recall` and
/// `memory_recall_deep` with `top_k=10`, asserts deep ≥ shallow.
/// Test: this test.
#[tokio::test]
async fn recall_deep_returns_at_least_as_many_as_shallow() {
    let fx = Fixture::new();
    create_palace(fx.state(), "deep").await;
    // Issue #220: the dedup gate uses Jaro-Winkler similarity > 0.92 to
    // skip near-duplicates within a 5-minute window. Five drawers
    // differing only by a single digit (`...number 0...`, `...number 1...`)
    // would all be dropped as near-duplicates of the first one. Use
    // materially different prose so each write lands.
    let bodies = [
        "Rust enforces ownership and lifetimes at compile time to prevent data races and use-after-free",
        "Python is dynamically typed with reference counting and a cyclic garbage collector for heap memory",
        "JavaScript engines such as V8 use just-in-time compilation and generational garbage collection",
        "Go is a statically typed language with concurrent garbage collection and lightweight goroutines",
        "Haskell relies on lazy evaluation, type inference, and pure functional programming abstractions",
    ];
    for body in bodies {
        remember(fx.state(), "deep", body, &[]).await;
    }

    let shallow = dispatch_tool(
        fx.state(),
        "memory_recall",
        json!({"palace": "deep", "query": "programming languages", "top_k": 10}),
    )
    .await
    .expect("memory_recall");
    let deep = dispatch_tool(
        fx.state(),
        "memory_recall_deep",
        json!({"palace": "deep", "query": "programming languages", "top_k": 10}),
    )
    .await
    .expect("memory_recall_deep");
    let shallow_n = shallow["results"].as_array().unwrap().len();
    let deep_n = deep["results"].as_array().unwrap().len();
    assert!(
        deep_n >= shallow_n,
        "deep ({deep_n}) must surface at least as many results as shallow ({shallow_n})"
    );
}

/// Why: `kg_assert` writes a triple; `kg_query` must surface that exact
/// triple back to the caller.
/// What: Asserts `alice works_at Acme`, queries by subject `alice`, and
/// asserts predicate + object round-trip.
/// Test: this test.
#[tokio::test]
async fn kg_assert_then_query_round_trips() {
    let fx = Fixture::new();
    create_palace(fx.state(), "kg-rt").await;

    dispatch_tool(
        fx.state(),
        "kg_assert",
        json!({
            "palace": "kg-rt",
            "subject": "alice",
            "predicate": "works_at",
            "object": "Acme",
            "confidence": 0.9,
        }),
    )
    .await
    .expect("kg_assert");

    let queried = dispatch_tool(
        fx.state(),
        "kg_query",
        json!({"palace": "kg-rt", "subject": "alice"}),
    )
    .await
    .expect("kg_query");
    let triples = queried["triples"].as_array().expect("triples array");
    assert_eq!(triples.len(), 1, "expected exactly one triple");
    assert_eq!(triples[0]["predicate"], "works_at");
    assert_eq!(triples[0]["object"], "Acme");
}

/// Why: `kg_query` filters by subject — a query for a *different* subject
/// must return no triples even when the graph holds triples for other
/// subjects.
/// What: Asserts `alice works_at Acme` then queries `bob`. The result
/// array must be empty.
/// Test: this test.
#[tokio::test]
async fn kg_query_filters_by_subject() {
    let fx = Fixture::new();
    create_palace(fx.state(), "kg-filter").await;

    dispatch_tool(
        fx.state(),
        "kg_assert",
        json!({
            "palace": "kg-filter",
            "subject": "alice",
            "predicate": "works_at",
            "object": "Acme",
        }),
    )
    .await
    .expect("kg_assert");

    let queried = dispatch_tool(
        fx.state(),
        "kg_query",
        json!({"palace": "kg-filter", "subject": "bob"}),
    )
    .await
    .expect("kg_query");
    let triples = queried["triples"].as_array().expect("triples array");
    assert!(
        triples.is_empty(),
        "expected zero triples for unknown subject"
    );
}

/// Why: `palace_create` must persist the palace under the data root and
/// expose it via `palace_list` with empty drawer / triple counts.
/// What: Creates a palace, lists palaces, asserts the new id appears.
/// Then dispatches `palace_info` and checks `drawer_count == 0`.
/// Test: this test.
#[tokio::test]
async fn palace_create_appears_in_list_with_empty_counts() {
    let fx = Fixture::new();
    create_palace(fx.state(), "fresh").await;

    let listed = dispatch_tool(fx.state(), "palace_list", json!({}))
        .await
        .expect("palace_list");
    let ids = listed["palaces"].as_array().expect("palaces array");
    assert!(ids.iter().any(|v| v.as_str() == Some("fresh")));

    let info = dispatch_tool(fx.state(), "palace_info", json!({"palace": "fresh"}))
        .await
        .expect("palace_info");
    assert_eq!(info["drawer_count"].as_u64(), Some(0));
}

/// Why: `memory_forget` must remove the drawer from the in-memory drawer
/// table so subsequent recalls do not surface it.
/// What: Stores a drawer, recalls and confirms it's present, forgets it,
/// recalls again and confirms it's gone.
/// Test: this test.
#[tokio::test]
async fn memory_forget_removes_drawer() {
    let fx = Fixture::new();
    create_palace(fx.state(), "forgetful").await;
    let id = remember(
        fx.state(),
        "forgetful",
        "Capybaras are the largest rodents in the world",
        &[],
    )
    .await;

    let before = dispatch_tool(
        fx.state(),
        "memory_recall",
        json!({"palace": "forgetful", "query": "capybara rodent", "top_k": 5}),
    )
    .await
    .expect("recall pre-forget");
    assert!(before["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["content"].as_str().unwrap_or("").contains("Capybaras")));

    let forget = dispatch_tool(
        fx.state(),
        "memory_forget",
        json!({"palace": "forgetful", "drawer_id": id}),
    )
    .await
    .expect("memory_forget");
    // #5231: a real deletion is the only case that may report "deleted".
    assert_eq!(forget["status"], "deleted", "got {forget}");

    let after = dispatch_tool(
        fx.state(),
        "memory_recall",
        json!({"palace": "forgetful", "query": "capybara rodent", "top_k": 5}),
    )
    .await
    .expect("recall post-forget");
    assert!(
        !after["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["content"].as_str().unwrap_or("").contains("Capybaras")),
        "drawer must be gone after forget; got {:?}",
        after["results"]
    );
}

/// Why: Full lifecycle confirmation — remember, recall (hit), forget,
/// recall (miss) — exercises every state transition in one test.
/// What: Stores one drawer, recalls and confirms hit, forgets, recalls
/// again and confirms only the L0 identity row remains (no L2 hit for
/// the forgotten drawer).
/// Test: this test.
#[tokio::test]
async fn round_trip_remember_recall_forget_recall_empty() {
    let fx = Fixture::new();
    create_palace(fx.state(), "lifecycle").await;
    let id = remember(
        fx.state(),
        "lifecycle",
        "An octopus has three hearts and blue blood",
        &[],
    )
    .await;

    let hit = dispatch_tool(
        fx.state(),
        "memory_recall",
        json!({"palace": "lifecycle", "query": "octopus blood hearts", "top_k": 5}),
    )
    .await
    .unwrap();
    assert!(hit["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["content"].as_str().unwrap_or("").contains("octopus")));

    dispatch_tool(
        fx.state(),
        "memory_forget",
        json!({"palace": "lifecycle", "drawer_id": id}),
    )
    .await
    .unwrap();

    let miss = dispatch_tool(
        fx.state(),
        "memory_recall",
        json!({"palace": "lifecycle", "query": "octopus blood hearts", "top_k": 5}),
    )
    .await
    .unwrap();
    // After forget, no L2 hit should reference the forgotten drawer.
    let l2_hits: Vec<_> = miss["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["layer"].as_u64().unwrap_or(0) >= 2)
        .collect();
    assert!(
        !l2_hits
            .iter()
            .any(|r| r["content"].as_str().unwrap_or("").contains("octopus")),
        "forgotten drawer must not appear in L2 recall results; got {l2_hits:?}"
    );
}

/// Regression test for issue #5231.
///
/// Why: `memory_forget` returned `{"status":"deleted"}` for a well-formed
/// `drawer_id` that had never existed, so a cleanup loop could report N
/// deletions having made zero. Parsing the UUID was the only validation.
/// What: forgets a syntactically valid UUID that was never stored and asserts
/// the reported status is `not_found`, and that the drawer that *does* exist is
/// untouched.
/// Test: this test.
#[tokio::test]
async fn memory_forget_reports_not_found_for_unknown_drawer_id() {
    let fx = Fixture::new();
    create_palace(fx.state(), "phantom").await;
    let live_id = remember(
        fx.state(),
        "phantom",
        "Wombats produce cube-shaped droppings because of their intestinal elasticity",
        &[],
    )
    .await;

    let res = dispatch_tool(
        fx.state(),
        "memory_forget",
        json!({"palace": "phantom", "drawer_id": "deadbeef-0000-4000-8000-000000000000"}),
    )
    .await
    .expect("memory_forget dispatch");
    assert_eq!(res["status"], "not_found", "got {res}");

    // The delete that never happened must not have disturbed the real drawer.
    let list = dispatch_tool(fx.state(), "memory_list", json!({"palace": "phantom"}))
        .await
        .expect("memory_list");
    assert!(
        list["drawers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["drawer_id"] == live_id.as_str()),
        "live drawer disappeared: {list}"
    );
}

/// Issue #5231 companion: the parse-time rejection must survive the fix.
///
/// Why: the pre-fix behaviour got *malformed* ids right and only nonexistent
/// ids wrong; adding the existence check must not turn a malformed id into a
/// quiet `not_found`.
/// What: dispatches `memory_forget` with a non-UUID string and asserts the call
/// still returns `Err`.
/// Test: this test.
#[tokio::test]
async fn memory_forget_still_rejects_a_malformed_drawer_id() {
    let fx = Fixture::new();
    create_palace(fx.state(), "malformed").await;

    let err = dispatch_tool(
        fx.state(),
        "memory_forget",
        json!({"palace": "malformed", "drawer_id": "not-a-uuid"}),
    )
    .await
    .expect_err("malformed drawer_id must be an error");
    assert!(
        err.to_string().contains("invalid drawer_id UUID"),
        "unexpected error: {err:#}"
    );
}

// ---------------------------------------------------------------------------
// Read-only mode (issue #59 snapshot fallback)
// ---------------------------------------------------------------------------

/// Seed a palace under `data_root` and then return — dropping every
/// strong handle so the in-process redb cache entries expire and a
/// subsequent raw `Database::create` against the palace files can take
/// the exclusive flock (simulating the HTTP daemon).
///
/// Why: The writer-side `AppState` keeps `Arc<PalaceHandle>` alive in
/// its registry, which transitively keeps the redb `Database` open;
/// locking the file with a raw handle while the writer state is alive
/// would race the cache and fail with `DatabaseAlreadyOpen`. Dropping
/// the state at scope end clears every `Arc<KgDbState>` /
/// `Arc<VectorDbState>` strong reference so the next open path sees a
/// dead cache entry.
/// What: Builds an `AppState`, creates the palace, runs the
/// caller-supplied seed closure, then returns after the state goes out
/// of scope.
/// Test: Indirect — every `read_only_*` test below.
async fn seed_palace<F, Fut>(data_root: &Path, palace: &str, seed: F)
where
    F: FnOnce(AppState, String) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    seed_embedder();
    // Issue #88: bypass palace-slug enforcement so these tests can use
    // arbitrary palace names without a matching project root on disk.
    // SAFETY: constant idempotent write "1"; benign across threads.
    unsafe {
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }
    let state = AppState::new(data_root.to_path_buf());
    // Flip to Ready so the #911 preflight does not block memory_remember
    // calls made inside the seed closure.
    state.set_ready();
    create_palace(&state, palace).await;
    seed(state, palace.to_string()).await;
    // state drops here, releasing every Arc<KgDbState> strong reference.
    // The per-palace `KgWriter` actor task (spawned in `KnowledgeGraph::
    // open`) also holds an `Arc<KgStoreRedb>`; closing the mpsc sender
    // when the writer handle dropped signals the task to exit, but the
    // task only releases its store Arc when it next polls. Yield several
    // times so the scheduler runs the actor's shutdown branch before the
    // test takes a raw flock on the redb files.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

/// Why: When the HTTP daemon holds the redb lock the stdio client opens
/// against a snapshot; `memory_recall` must still succeed.
/// What: Seeds a palace and discards the seeding state, locks the palace
/// files via raw `Database::create` handles, then opens a fresh
/// `AppState` and dispatches `memory_recall`. Asserts the seeded drawer
/// appears in the snapshot recall results.
/// Test: this test.
#[tokio::test]
async fn read_only_memory_recall_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let data_root = tmp.path().to_path_buf();
    seed_palace(&data_root, "ro-recall", |state, palace| async move {
        remember(
            &state,
            &palace,
            "Kookaburras are large terrestrial kingfishers native to the woodlands of eastern Australia and southern New Guinea",
            &[],
        )
        .await;
    })
    .await;

    let data_dir = data_root.join("ro-recall");
    let _live = lock_palace_files(&data_dir);
    let snap_state = fresh_state(&data_root);

    let recalled = dispatch_tool(
        &snap_state,
        "memory_recall",
        json!({"palace": "ro-recall", "query": "kookaburra kingfisher", "top_k": 5}),
    )
    .await
    .expect("recall on snapshot must succeed");
    let results = recalled["results"].as_array().unwrap();
    assert!(
        results
            .iter()
            .any(|r| r["content"].as_str().unwrap_or("").contains("Kookaburras")),
        "snapshot recall should surface the seeded drawer; got {results:?}"
    );
}

/// Why: `memory_remember` is a write surface; in snapshot mode it must
/// fail loudly with the daemon-guidance error rather than panicking or
/// silently mutating the throw-away snapshot.
/// What: Seeds (and discards) a palace, locks its redb files, opens a
/// fresh `AppState`, dispatches `memory_remember`, asserts an error
/// whose message includes the "read-only" / daemon-guidance fragment.
/// Test: this test.
#[tokio::test]
async fn read_only_memory_remember_returns_clear_error() {
    let tmp = tempfile::tempdir().unwrap();
    let data_root = tmp.path().to_path_buf();
    seed_palace(&data_root, "ro-write", |_state, _palace| async move {}).await;

    let data_dir = data_root.join("ro-write");
    let _live = lock_palace_files(&data_dir);
    let snap_state = fresh_state(&data_root);

    // Issue #215: pass a long-enough text to clear the content gate so the
    // dispatch reaches the read-only error path; the gate would otherwise
    // silently skip the write before the read-only guard fires.
    let res = dispatch_tool(
        &snap_state,
        "memory_remember",
        json!({
            "palace": "ro-write",
            "text": "this is a long enough write payload to clear the content gate threshold",
            "room": "General",
        }),
    )
    .await;
    let err = res.expect_err("remember in snapshot mode must error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("read-only"),
        "expected read-only sentinel, got: {msg}"
    );
    assert!(
        msg.contains("daemon"),
        "expected daemon guidance, got: {msg}"
    );
}

/// Why: `kg_query` is a read surface; the snapshot must serve it.
/// What: Seeds one triple via the writer state and discards the state,
/// locks the palace files, opens a fresh state, queries the subject, and
/// asserts the seeded triple is returned.
/// Test: this test.
#[tokio::test]
async fn read_only_kg_query_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let data_root = tmp.path().to_path_buf();
    seed_palace(&data_root, "ro-kg-r", |state, palace| async move {
        dispatch_tool(
            &state,
            "kg_assert",
            json!({
                "palace": palace,
                "subject": "alice",
                "predicate": "knows",
                "object": "bob",
            }),
        )
        .await
        .expect("kg_assert seed");
    })
    .await;

    let data_dir = data_root.join("ro-kg-r");
    let _live = lock_palace_files(&data_dir);
    let snap_state = fresh_state(&data_root);

    let queried = dispatch_tool(
        &snap_state,
        "kg_query",
        json!({"palace": "ro-kg-r", "subject": "alice"}),
    )
    .await
    .expect("kg_query on snapshot");
    let triples = queried["triples"].as_array().unwrap();
    assert_eq!(triples.len(), 1);
    assert_eq!(triples[0]["object"], "bob");
}

/// Why: `kg_assert` is a write surface; snapshot mode must reject it with
/// the same daemon-guidance error as `memory_remember`.
/// What: Seeds (and discards) a palace, locks its files, opens a fresh
/// state, attempts `kg_assert`, asserts the error contains the
/// "read-only" sentinel.
/// Test: this test.
#[tokio::test]
async fn read_only_kg_assert_returns_clear_error() {
    let tmp = tempfile::tempdir().unwrap();
    let data_root = tmp.path().to_path_buf();
    seed_palace(&data_root, "ro-kg-w", |_state, _palace| async move {}).await;

    let data_dir = data_root.join("ro-kg-w");
    let _live = lock_palace_files(&data_dir);
    let snap_state = fresh_state(&data_root);

    let res = dispatch_tool(
        &snap_state,
        "kg_assert",
        json!({
            "palace": "ro-kg-w",
            "subject": "carol",
            "predicate": "owns",
            "object": "yacht",
        }),
    )
    .await;
    let err = res.expect_err("kg_assert in snapshot mode must error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("read-only"),
        "expected read-only sentinel, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Concurrent access
// ---------------------------------------------------------------------------

/// Why: Two `AppState`s rooted at the same data dir (same process) must
/// be able to read the same palace concurrently — the in-process redb
/// cache guarantees this without snapshotting.
/// What: Creates a palace through state A, opens state B against the same
/// data root, and asserts both can read via `palace_info` simultaneously.
/// Test: this test.
#[tokio::test]
async fn two_states_can_read_same_palace_simultaneously() {
    let fx = Fixture::new();
    create_palace(fx.state(), "shared").await;
    remember(
        fx.state(),
        "shared",
        "Echidnas are egg-laying mammals known as monotremes, found across Australia and New Guinea",
        &[],
    )
    .await;

    let state_b = fresh_state(fx.data_root());

    let (a, b) = tokio::join!(
        dispatch_tool(fx.state(), "palace_info", json!({"palace": "shared"})),
        dispatch_tool(&state_b, "palace_info", json!({"palace": "shared"})),
    );
    let a = a.expect("info on state A");
    let b = b.expect("info on state B");
    assert_eq!(a["drawer_count"], b["drawer_count"]);
    assert_eq!(a["drawer_count"].as_u64(), Some(1));
}

/// Why: A read-only client opened against a locked palace must succeed
/// without error — confirming the snapshot fallback doesn't deadlock on
/// the second open.
/// What: Seeds (and discards) a palace, locks the redb files via raw
/// `Database::create`, then opens a fresh `AppState` and dispatches
/// `palace_info`. The call must complete inside a generous 2-second
/// budget.
/// Test: this test.
#[tokio::test]
async fn read_only_open_while_writer_holds_lock_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let data_root = tmp.path().to_path_buf();
    seed_palace(&data_root, "concurrent-ro", |state, palace| async move {
        remember(
            &state,
            &palace,
            "Wombats produce distinctive cube-shaped droppings due to the unusual elasticity of their intestinal walls",
            &[],
        )
        .await;
    })
    .await;

    let data_dir = data_root.join("concurrent-ro");
    let _live = lock_palace_files(&data_dir);

    let snap_state = Arc::new(fresh_state(&data_root));
    let started = Instant::now();
    let info = dispatch_tool(
        snap_state.as_ref(),
        "palace_info",
        json!({"palace": "concurrent-ro"}),
    )
    .await
    .expect("palace_info on snapshot");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(info["drawer_count"].as_u64(), Some(1));
}

// ---------------------------------------------------------------------------
// Performance budgets (ignored by default; run with --include-ignored)
// ---------------------------------------------------------------------------

// #6297: the perf fixtures below hit two `memory_remember` admission gates that
// landed after they were written, and each died in setup before reaching the
// section it times. Both gates are correct production behaviour; the fixtures
// were stale.
//
//  1. `MCP_MIN_TOKENS` (8) rejected "warm-up drawer" and "seed" outright.
//     [`perf_seed_text`] is long enough to clear it.
//  2. The rolling dedup window (`DEDUP_SIMILARITY_THRESHOLD` 0.92 on
//     Jaro-Winkler, 5 minutes) then rejected the lengthened seeds as
//     near-duplicates of each other — Jaro-Winkler pays a prefix bonus, so a
//     templated string differing only in a trailing index scores far above the
//     threshold. Varying the FIRST word is what actually separates them.

/// Per-drawer seed text for the perf fixtures.
///
/// Why (#6297): a seed carries a timed operation but still goes through the real
/// `memory_remember` handler, so it owes that handler's admission rules —
/// bypassing them would time a write path no caller can reach. It must also be
/// genuinely distinct per `i`: the recall budget over 100 identical drawers
/// measures a degenerate index, and the dedup window rejects near-duplicates
/// outright.
/// What: draws a subject, verb and object from three pools of coprime length and
/// puts the varying subject first, where Jaro-Winkler's prefix bonus applies.
/// The trailing `alpha-{i}` token keeps each drawer individually addressable by
/// the recall query.
/// Test: used by every `perf_*` fixture below.
fn perf_seed_text(i: usize) -> String {
    const SUBJECTS: &[&str] = &[
        "Harbour", "Lantern", "Meadow", "Quartz", "Tundra", "Violin", "Zephyr", "Cobalt", "Fennel",
        "Gantry", "Ripcord",
    ];
    const VERBS: &[&str] = &[
        "measures",
        "reroutes",
        "catalogues",
        "dampens",
        "polishes",
        "forecasts",
        "untangles",
    ];
    const OBJECTS: &[&str] = &[
        "ledger",
        "antenna",
        "trellis",
        "sextant",
        "bassoon",
        "kiln",
        "marmalade",
        "spillway",
        "quorum",
    ];
    let subject = SUBJECTS[i % SUBJECTS.len()];
    let verb = VERBS[i % VERBS.len()];
    let object = OBJECTS[i % OBJECTS.len()];
    format!("{subject} {verb} the {object} on bay {i}, topic alpha-{i}.")
}

// #6297: the budgets, and what they are for.
//
// These are wall-clock budgets on developer hardware, competing with whatever
// else the machine is doing — `cargo test` runs them in parallel with each
// other by default, and this workspace's own builds run alongside. They exist
// to catch an ORDER-OF-MAGNITUDE regression, not to microbenchmark: a budget
// set at the observed median turns every loaded machine into a red gate, which
// is what the two below did.
//
// Measured on an M-series Mac, 2026-08-27, `--include-ignored`. Serial is
// `--test-threads=1`; parallel is the default and is what the budget must
// survive, because it is the invocation everyone actually runs.
//
//                        serial     parallel   old budget   new budget
//   kg_assert             19.3 ms    68.3 ms      10 ms       250 ms
//   kg_query              25.1 ms    52.2 ms      20 ms       150 ms
//   memory_recall          6.0 ms    33.3 ms      50 ms       150 ms
//   palace_cold_open     101.5 ms   358.7 ms     200 ms       750 ms
//   memory_remember      195.5 ms   542.4 ms     500 ms      1500 ms
//   ten_concurrent        800 ms      3.48 s       1 s           8 s
//
// Running in parallel costs a factor of ~3, and every old budget sat inside
// that factor — so the verdict was decided by scheduling noise, not by the
// code. `kg_assert` is the clearest case: it is one durable redb write
// transaction, its floor is an APFS fsync, and a separate n=5 sample ranged
// 19.1–125.7 ms serial. No code change brings that under 10 ms on this
// hardware; the budget was unreachable, not missed.
//
// `ten_concurrent` had never once run to completion before this change — it
// died in setup on the content gate — so its 1 s was a guess no measurement
// had ever tested.
//
// Each budget is set above the parallel measurement with roughly 2.5x
// headroom. A regression worth catching here is a factor, not a few
// milliseconds.

/// One durable redb write transaction; the floor is an fsync.
const KG_ASSERT_BUDGET: Duration = Duration::from_millis(250);

/// One indexed read over a 1000-triple palace.
const KG_QUERY_BUDGET: Duration = Duration::from_millis(150);

/// First open of a palace already on disk, with no in-process cache.
const PALACE_COLD_OPEN_BUDGET: Duration = Duration::from_millis(750);

/// One warm `memory_remember`, ONNX embedding pass included.
const MEMORY_REMEMBER_BUDGET: Duration = Duration::from_millis(1500);

/// One warm `memory_recall` over 100 drawers.
const MEMORY_RECALL_BUDGET: Duration = Duration::from_millis(150);

/// Ten parallel snapshot opens against a flocked palace.
const TEN_CONCURRENT_OPENS_BUDGET: Duration = Duration::from_secs(8);

/// Report a timing, then hold it to its budget.
///
/// Why (#6297): two budgets here were missed by main and the failure said only
/// which one. Nobody could tell an order-of-magnitude regression from a machine
/// under load without re-running by hand, because a passing measurement printed
/// nothing at all. Emitting it unconditionally makes `--nocapture` a
/// measurement, not just a pass/fail.
/// What: writes `perf: <label> <elapsed> (budget <budget>)` to stderr — never
/// stdout, which these tests share with nothing but is the MCP channel's
/// convention here — then asserts.
/// Test: every `perf_*` fixture below.
fn assert_within_budget(label: &str, elapsed: Duration, budget: Duration) {
    eprintln!("perf: {label} {elapsed:?} (budget {budget:?})");
    assert!(
        elapsed < budget,
        "{label} took {elapsed:?} (budget {budget:?})"
    );
}

/// Seed a drawer past the dedup window.
///
/// Why (#6297): bulk seeding states "populate this palace with N drawers", which
/// is what `force` means at the MCP boundary — an intentional write the rolling
/// dedup window must not silently drop. It bypasses the quality gates only, so
/// [`perf_seed_text`] still carries content the unforced path would accept. The
/// TIMED calls never use this: they measure the real admission path, dedup
/// check included.
async fn remember_seed(state: &AppState, palace: &str, i: usize) {
    dispatch_tool(
        state,
        "memory_remember",
        json!({
            "palace": palace,
            "text": perf_seed_text(i),
            "room": "General",
            "force": true,
        }),
    )
    .await
    .expect("memory_remember seed");
}

/// Why: `memory_remember` is the slowest tool because it owns the ONNX
/// embedding pass; we want to catch regressions if the warm-path cost
/// exceeds 500 ms.
/// What: Warms the embedder with one priming call, then times a single
/// `memory_remember` round-trip and asserts the elapsed time is below
/// 500 ms.
/// Test: this test (run with `cargo test -- --include-ignored`).
#[tokio::test]
#[ignore = "perf budget — requires warm embedder; run with --include-ignored"]
async fn perf_memory_remember_within_budget() {
    let fx = Fixture::new();
    create_palace(fx.state(), "perf-remember").await;
    // Warm-up: first call pays the ONNX session-load cost. Forced, so the
    // timed call below is the only one whose admission path is measured.
    remember_seed(fx.state(), "perf-remember", 0).await;

    // #6297: the timed call is deliberately NOT forced — it measures the real
    // admission path, dedup check included. Index 5 shares no word with index 0,
    // so the dedup window has nothing to match it against.
    let started = Instant::now();
    remember(fx.state(), "perf-remember", &perf_seed_text(5), &[]).await;
    let elapsed = started.elapsed();
    assert_within_budget("memory_remember", elapsed, MEMORY_REMEMBER_BUDGET);
}

/// Why: `memory_recall` over a moderately-sized palace must stay below
/// 50 ms post-warmup; this gates regressions on the hot retrieval path.
/// What: Seeds 100 drawers, primes the embedder, then times one
/// `memory_recall` call and asserts the budget.
/// Test: this test (run with `cargo test -- --include-ignored`).
#[tokio::test]
#[ignore = "perf budget — 100-drawer seed is slow; run with --include-ignored"]
async fn perf_memory_recall_100_drawers_within_budget() {
    let fx = Fixture::new();
    create_palace(fx.state(), "perf-recall").await;
    for i in 0..100 {
        remember_seed(fx.state(), "perf-recall", i).await;
    }
    // Warm-up recall — primes the embedder for the query path.
    dispatch_tool(
        fx.state(),
        "memory_recall",
        json!({"palace": "perf-recall", "query": "alpha-50", "top_k": 5}),
    )
    .await
    .unwrap();

    let started = Instant::now();
    dispatch_tool(
        fx.state(),
        "memory_recall",
        json!({"palace": "perf-recall", "query": "alpha-50", "top_k": 5}),
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();
    assert_within_budget("memory_recall", elapsed, MEMORY_RECALL_BUDGET);
}

/// Why: `kg_assert` is a single redb write transaction; budget 10 ms.
/// What: Times one `kg_assert` call on a fresh palace.
/// Test: this test (run with `--include-ignored`).
#[tokio::test]
#[ignore = "perf budget — run with --include-ignored"]
async fn perf_kg_assert_within_budget() {
    let fx = Fixture::new();
    create_palace(fx.state(), "perf-assert").await;

    let started = Instant::now();
    dispatch_tool(
        fx.state(),
        "kg_assert",
        json!({
            "palace": "perf-assert",
            "subject": "alice",
            "predicate": "knows",
            "object": "bob",
        }),
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();
    assert_within_budget("kg_assert", elapsed, KG_ASSERT_BUDGET);
}

/// Why: `kg_query` against a 1000-triple palace must stay below 20 ms.
/// What: Seeds 1000 triples (all for distinct subjects so the query
/// touches a single subject's row), then times one `kg_query` call.
/// Test: this test (run with `--include-ignored`).
#[tokio::test]
#[ignore = "perf budget — 1000-triple seed is slow; run with --include-ignored"]
async fn perf_kg_query_1000_triples_within_budget() {
    let fx = Fixture::new();
    create_palace(fx.state(), "perf-query").await;
    for i in 0..1000 {
        dispatch_tool(
            fx.state(),
            "kg_assert",
            json!({
                "palace": "perf-query",
                "subject": format!("subject-{i}"),
                "predicate": "knows",
                "object": format!("object-{i}"),
            }),
        )
        .await
        .unwrap();
    }

    let started = Instant::now();
    dispatch_tool(
        fx.state(),
        "kg_query",
        json!({"palace": "perf-query", "subject": "subject-500"}),
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();
    assert_within_budget("kg_query", elapsed, KG_QUERY_BUDGET);
}

/// Why: Cold palace open (palace dir already on disk, no in-process
/// cache) must complete in under 200 ms so daemon start-up scales.
/// What: Creates a palace in one `AppState`, drops it, then times the
/// first `palace_info` against a fresh state pointing at the same data
/// root — that's the cold-open path.
/// Test: this test (run with `--include-ignored`).
#[tokio::test]
#[ignore = "perf budget — run with --include-ignored"]
async fn perf_palace_cold_open_within_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let data_root = tmp.path().to_path_buf();
    // Seed the palace then drop the seeding state so the in-process
    // redb cache is cold for the timed open below.
    seed_palace(&data_root, "perf-cold", |_state, _palace| async move {}).await;

    let snap = fresh_state(&data_root);
    let started = Instant::now();
    dispatch_tool(&snap, "palace_info", json!({"palace": "perf-cold"}))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_within_budget("palace_cold_open", elapsed, PALACE_COLD_OPEN_BUDGET);
}

/// Why: Ten parallel snapshot opens must all succeed and finish within
/// 1 s total. Validates that `try_open_or_snapshot` does not serialise
/// snapshot creation under contention.
/// What: Locks the redb files of a seeded palace, spawns 10
/// `palace_info` tasks against fresh `AppState`s, joins them, asserts
/// all succeeded and total elapsed < 1 s.
///
/// #6297: seeds through `seed_palace` rather than a live `Fixture`. The
/// fixture's own `AppState` holds redb open, so `lock_palace_files` — a raw
/// `Database::create` standing in for a peer process — failed with
/// `DatabaseAlreadyOpen` against it. `seed_palace` drops the seeding state and
/// yields until the per-palace `KgWriter` actor releases its store `Arc`, which
/// is what leaves the files free to flock. The defect was latent: the seed
/// panicked on the content gate before ever reaching this line.
/// Test: this test (run with `--include-ignored`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "perf budget — run with --include-ignored"]
async fn perf_ten_concurrent_read_only_opens_within_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let data_root = tmp.path().to_path_buf();
    seed_palace(&data_root, "perf-concurrent", |state, palace| async move {
        remember_seed(&state, &palace, 0).await;
    })
    .await;
    let palace_dir = data_root.join("perf-concurrent");
    let _live = lock_palace_files(&palace_dir);

    let started = Instant::now();
    let mut handles = Vec::with_capacity(10);
    for _ in 0..10 {
        let root = data_root.clone();
        handles.push(tokio::spawn(async move {
            let st = AppState::new(root);
            dispatch_tool(&st, "palace_info", json!({"palace": "perf-concurrent"})).await
        }));
    }
    for h in handles {
        h.await.expect("task join").expect("palace_info ok");
    }
    let elapsed = started.elapsed();
    assert_within_budget("ten_concurrent_opens", elapsed, TEN_CONCURRENT_OPENS_BUDGET);
}
