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
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = AppState::new(tmp.path().to_path_buf());
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
    res["drawer_id"]
        .as_str()
        .expect("drawer_id in response")
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
    AppState::new(data_root.to_path_buf())
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

    dispatch_tool(
        fx.state(),
        "memory_forget",
        json!({"palace": "forgetful", "drawer_id": id}),
    )
    .await
    .expect("memory_forget");

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
    let state = AppState::new(data_root.to_path_buf());
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

/// Why: `memory_remember` is the slowest tool because it owns the ONNX
/// embedding pass; we want to catch regressions if the warm-path cost
/// exceeds 500 ms.
/// What: Warms the embedder with one priming call, then times a single
/// `memory_remember` round-trip and asserts the elapsed time is below
/// 500 ms.
/// Test: this test (run with `cargo test -- --include-ignored`).
#[tokio::test]
#[ignore = "perf budget — requires warm embedder; run with --include-ignored"]
async fn perf_memory_remember_under_500ms() {
    let fx = Fixture::new();
    create_palace(fx.state(), "perf-remember").await;
    // Warm-up: first call pays the ONNX session-load cost.
    remember(fx.state(), "perf-remember", "warm-up drawer", &[]).await;

    let started = Instant::now();
    remember(
        fx.state(),
        "perf-remember",
        "timed drawer for the perf budget",
        &[],
    )
    .await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "memory_remember took {elapsed:?} (budget: 500ms)"
    );
}

/// Why: `memory_recall` over a moderately-sized palace must stay below
/// 50 ms post-warmup; this gates regressions on the hot retrieval path.
/// What: Seeds 100 drawers, primes the embedder, then times one
/// `memory_recall` call and asserts the budget.
/// Test: this test (run with `cargo test -- --include-ignored`).
#[tokio::test]
#[ignore = "perf budget — 100-drawer seed is slow; run with --include-ignored"]
async fn perf_memory_recall_100_drawers_under_50ms() {
    let fx = Fixture::new();
    create_palace(fx.state(), "perf-recall").await;
    for i in 0..100 {
        remember(
            fx.state(),
            "perf-recall",
            &format!("Seed drawer {i} about unique topic alpha-{i}"),
            &[],
        )
        .await;
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
    assert!(
        elapsed < Duration::from_millis(50),
        "memory_recall took {elapsed:?} (budget: 50ms)"
    );
}

/// Why: `kg_assert` is a single redb write transaction; budget 10 ms.
/// What: Times one `kg_assert` call on a fresh palace.
/// Test: this test (run with `--include-ignored`).
#[tokio::test]
#[ignore = "perf budget — run with --include-ignored"]
async fn perf_kg_assert_under_10ms() {
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
    assert!(
        elapsed < Duration::from_millis(10),
        "kg_assert took {elapsed:?} (budget: 10ms)"
    );
}

/// Why: `kg_query` against a 1000-triple palace must stay below 20 ms.
/// What: Seeds 1000 triples (all for distinct subjects so the query
/// touches a single subject's row), then times one `kg_query` call.
/// Test: this test (run with `--include-ignored`).
#[tokio::test]
#[ignore = "perf budget — 1000-triple seed is slow; run with --include-ignored"]
async fn perf_kg_query_1000_triples_under_20ms() {
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
    assert!(
        elapsed < Duration::from_millis(20),
        "kg_query took {elapsed:?} (budget: 20ms)"
    );
}

/// Why: Cold palace open (palace dir already on disk, no in-process
/// cache) must complete in under 200 ms so daemon start-up scales.
/// What: Creates a palace in one `AppState`, drops it, then times the
/// first `palace_info` against a fresh state pointing at the same data
/// root — that's the cold-open path.
/// Test: this test (run with `--include-ignored`).
#[tokio::test]
#[ignore = "perf budget — run with --include-ignored"]
async fn perf_palace_cold_open_under_200ms() {
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
    assert!(
        elapsed < Duration::from_millis(200),
        "cold palace_info took {elapsed:?} (budget: 200ms)"
    );
}

/// Why: Ten parallel snapshot opens must all succeed and finish within
/// 1 s total. Validates that `try_open_or_snapshot` does not serialise
/// snapshot creation under contention.
/// What: Locks the redb files of a seeded palace, spawns 10
/// `palace_info` tasks against fresh `AppState`s, joins them, asserts
/// all succeeded and total elapsed < 1 s.
/// Test: this test (run with `--include-ignored`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "perf budget — run with --include-ignored"]
async fn perf_ten_concurrent_read_only_opens_under_1s() {
    let fx = Fixture::new();
    create_palace(fx.state(), "perf-concurrent").await;
    remember(fx.state(), "perf-concurrent", "seed", &[]).await;
    let data_root = fx.data_root().to_path_buf();
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
    assert!(
        elapsed < Duration::from_secs(1),
        "10 concurrent snapshot opens took {elapsed:?} (budget: 1s)"
    );
}
