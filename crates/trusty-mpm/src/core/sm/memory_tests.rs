//! Tests for the SM dedicated memory palace + recall/remember wiring (SM-4).
//!
//! Why: prove the §8 contract — idempotent palace creation, scoped
//! remember→recall, strict SM-only scope, restart survival, and that writes
//! never target a non-SM palace. All tests point the engine at a `tempdir` so
//! they never touch the real `~/.trusty-mpm` data, and seed the MOCK embedder so
//! they run without the ONNX model (no `#[ignore]` needed — the lexical/L2 path
//! with a deterministic mock embedding is sufficient).
//! What: a `tempfile::TempDir` per test plus `seed_shared_embedder_with_mock()`
//! to avoid any HuggingFace download.

use super::*;
use tempfile::TempDir;
use trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock;

use crate::core::sm::config::SmMemoryConfig;

/// Build an `SmMemory` rooted at a fresh tempdir with the default SM palace
/// name, seeding the mock embedder first.
///
/// Why: every test needs an isolated store + a deterministic embedder; this
/// keeps each test focused on the behaviour it asserts.
/// What: seeds the mock embedder, returns `(SmMemory, TempDir)` — the caller
/// holds the `TempDir` so it isn't dropped (which would delete the store).
/// Test: used by every test below.
fn sm_memory() -> (SmMemory, TempDir) {
    seed_shared_embedder_with_mock();
    let dir = TempDir::new().expect("tempdir");
    let cfg = SmMemoryConfig::default();
    let mem = SmMemory::open(dir.path(), &cfg).expect("open SM memory");
    (mem, dir)
}

/// Why: idempotency is the headline SM-4 guarantee — ensuring the palace twice
/// (here: a second `SmMemory::open` against the same root) must NOT create a
/// duplicate or wipe the first; exactly one palace must persist.
/// What: opens SM memory twice against the same data root, then asserts exactly
/// one persisted palace named `"session-manager"`.
/// Test: this is the test.
#[test]
fn palace_create_is_idempotent() {
    seed_shared_embedder_with_mock();
    let dir = TempDir::new().expect("tempdir");
    let cfg = SmMemoryConfig::default();

    let first = SmMemory::open(dir.path(), &cfg).expect("first open");
    assert_eq!(first.persisted_palace_count().expect("count"), 1);

    // Second construction against the same root must be a no-op create.
    let second = SmMemory::open(dir.path(), &cfg).expect("second open");
    assert_eq!(
        second.persisted_palace_count().expect("count"),
        1,
        "creating the SM palace twice must yield exactly ONE palace"
    );
    assert_eq!(second.palace_id().as_str(), "session-manager");
}

/// Why: the TOCTOU-race fix in `ensure_palace` must take the OPEN branch (not
/// re-create) when the palace already exists on disk but is absent from THIS
/// instance's registry cache — the exact situation a second concurrent or
/// restarted SM faces after another process has already created the palace.
/// This guards against the create-when-exists fallback wiping or duplicating an
/// existing palace.
/// What: builds a first `SmMemory`, writes a distinctive fact (forcing the
/// palace + content onto disk), drops it, then builds a SECOND `SmMemory` with a
/// FRESH registry against the same root. The second instance must converge on
/// the existing palace (exactly one persisted) AND surface the first instance's
/// already-written fact via recall — proving it opened the existing palace
/// rather than creating a fresh empty one.
/// Test: this is the test.
#[tokio::test]
async fn ensure_palace_falls_back_to_open_when_palace_already_exists() {
    seed_shared_embedder_with_mock();
    let dir = TempDir::new().expect("tempdir");
    let cfg = SmMemoryConfig::default();

    // First instance materialises the palace and writes a marker fact.
    {
        let first = SmMemory::open(dir.path(), &cfg).expect("first open");
        assert_eq!(first.persisted_palace_count().expect("count"), 1);
        first
            .remember("TOCTOU_MARKER existing palace content written by first SM")
            .await
            .expect("remember in first instance");
        // `first` (and its registry cache) drops here — the palace now lives
        // ONLY on disk, exactly as a second process would observe it.
    }

    // Second instance has a brand-new registry, so `ensure_palace` cannot hit
    // the cached fast path — it must take the open-first branch against disk.
    let second = SmMemory::open(dir.path(), &cfg).expect("second open");
    assert_eq!(
        second.persisted_palace_count().expect("count"),
        1,
        "create-when-exists fallback must open the existing palace, not add a second"
    );

    let hits = second
        .recall("what marker did the first SM write")
        .await
        .expect("recall from second instance");
    assert!(
        hits.iter()
            .any(|h| h.drawer.content.contains("TOCTOU_MARKER")),
        "second SM must open the EXISTING palace (seeing prior content), not a fresh one; got {hits:?}"
    );
}

/// Why: the core round-trip — a remembered fact must be retrievable via recall
/// from the SM palace.
/// What: remembers a distinctive sentence, recalls a related query, asserts the
/// remembered content appears among the hits.
/// Test: this is the test.
#[tokio::test]
async fn remember_then_recall_round_trips() {
    let (mem, _dir) = sm_memory();

    mem.remember("The session manager delegates all engineering work to t-mpm sessions")
        .await
        .expect("remember");

    let hits = mem
        .recall("who does the session manager delegate work to")
        .await
        .expect("recall");

    assert!(
        hits.iter()
            .any(|h| h.drawer.content.contains("delegates all engineering work")),
        "recall must surface the remembered SM fact; got {hits:?}"
    );
}

/// Why: deep recall (L0+L1+L3) must also return SM facts, proving the deep path
/// is wired and scoped.
/// What: remembers a fact, runs `recall_deep`, asserts the fact is present.
/// Test: this is the test.
#[tokio::test]
async fn recall_deep_round_trips() {
    let (mem, _dir) = sm_memory();

    mem.remember("Decision: adopt worktree-per-ticket discipline for all SM work")
        .await
        .expect("remember");

    let hits = mem
        .recall_deep("what discipline did we adopt for tickets")
        .await
        .expect("recall_deep");

    assert!(
        hits.iter()
            .any(|h| h.drawer.content.contains("worktree-per-ticket")),
        "recall_deep must surface the remembered SM fact; got {hits:?}"
    );
}

/// Why: `note` stores short curated facts that the normal `remember` token gate
/// would reject — proving the curated-fact path works end to end.
/// What: notes a terse decision, recalls it, asserts it is retrievable.
/// Test: this is the test.
#[tokio::test]
async fn note_stores_short_fact() {
    let (mem, _dir) = sm_memory();

    // Short enough to trip the min-token gate on the plain `remember` path.
    mem.note("Goal: ship SM-4").await.expect("note short fact");

    let hits = mem.recall("what is the goal").await.expect("recall");
    assert!(
        hits.iter().any(|h| h.drawer.content.contains("ship SM-4")),
        "note must store a short curated fact retrievable via recall; got {hits:?}"
    );
}

/// Why: scope isolation — facts written to the SM palace must surface ONLY when
/// recalling the SM palace; a sibling palace under the same data root must not
/// leak its content into SM recall (and vice versa).
/// What: writes a distinctive fact to the SM palace, then builds a SECOND
/// `SmMemory` bound to a different palace name under the SAME root, writes a
/// different fact there, and asserts each recall sees only its own palace's fact.
/// Test: this is the test.
#[tokio::test]
async fn recall_is_scoped_to_sm_palace() {
    seed_shared_embedder_with_mock();
    let dir = TempDir::new().expect("tempdir");

    let sm_cfg = SmMemoryConfig::default(); // "session-manager"
    let sm = SmMemory::open(dir.path(), &sm_cfg).expect("open SM");
    sm.remember("SM_PALACE_SECRET zebra orchestration delegation marker")
        .await
        .expect("remember in SM palace");

    let other_cfg = SmMemoryConfig {
        palace: "some-other-project".to_string(),
        recall_top_k: 6,
    };
    let other = SmMemory::open(dir.path(), &other_cfg).expect("open other");
    other
        .remember("OTHER_PALACE_SECRET giraffe unrelated project marker")
        .await
        .expect("remember in other palace");

    // SM recall must NOT see the other palace's marker.
    let sm_hits = sm.recall("secret marker").await.expect("sm recall");
    assert!(
        sm_hits
            .iter()
            .all(|h| !h.drawer.content.contains("OTHER_PALACE_SECRET")),
        "SM recall must never surface another palace's content; got {sm_hits:?}"
    );

    // The other palace's recall must NOT see the SM marker.
    let other_hits = other.recall("secret marker").await.expect("other recall");
    assert!(
        other_hits
            .iter()
            .all(|h| !h.drawer.content.contains("SM_PALACE_SECRET")),
        "non-SM recall must never surface SM content; got {other_hits:?}"
    );
}

/// Why: restart survival — remembered SM data must persist across a FRESH
/// `SmMemory` construction pointing at the same storage (process restart).
/// What: remembers a fact, drops the first `SmMemory`, builds a new one against
/// the same root, and recalls the fact successfully.
/// Test: this is the test.
#[tokio::test]
async fn data_survives_fresh_construction() {
    seed_shared_embedder_with_mock();
    let dir = TempDir::new().expect("tempdir");
    let cfg = SmMemoryConfig::default();

    {
        let mem = SmMemory::open(dir.path(), &cfg).expect("first open");
        mem.remember("Persistent SM outcome: SM-3 merged to main successfully")
            .await
            .expect("remember");
        // `mem` drops here, simulating a process restart.
    }

    // Fresh construction against the same storage.
    let reopened = SmMemory::open(dir.path(), &cfg).expect("reopen");
    assert_eq!(
        reopened.persisted_palace_count().expect("count"),
        1,
        "reopen must reuse the existing palace, not create a second"
    );

    let hits = reopened
        .recall("which SM ticket merged to main")
        .await
        .expect("recall after restart");
    assert!(
        hits.iter()
            .any(|h| h.drawer.content.contains("SM-3 merged to main")),
        "remembered data must survive a fresh SmMemory construction; got {hits:?}"
    );
}

/// Why: the structural "SM never writes to a non-SM palace" guarantee — every
/// write goes to the bound palace id and to no other namespace, regardless of
/// how many writes occur.
/// What: performs `remember`, `note`, and another `remember`, then asserts the
/// bound `palace_id` is the configured SM name and that exactly ONE palace
/// exists on disk (so no stray palace was created by any write).
/// Test: this is the test.
#[tokio::test]
async fn writes_target_only_the_sm_palace() {
    let (mem, _dir) = sm_memory();

    assert_eq!(
        mem.palace_id().as_str(),
        "session-manager",
        "SM must be bound to the configured palace name"
    );

    mem.remember("first SM write — a goal for the milestone")
        .await
        .expect("remember 1");
    mem.note("terse SM decision").await.expect("note");
    mem.remember("third SM write — an outcome record for the milestone")
        .await
        .expect("remember 2");

    assert_eq!(
        mem.persisted_palace_count().expect("count"),
        1,
        "no write may create or touch a palace other than the bound SM palace"
    );
}

/// Why: graceful degradation — when the palace cannot be ensured (here: a data
/// root that cannot be created because a FILE exists at that path), `open` must
/// return a structured `SmMemoryError::Palace`, not panic.
/// What: creates a regular file, then tries to root `SmMemory` at a path BELOW
/// that file (so `create_dir_all` fails), and asserts the `Palace` error variant.
/// Test: this is the test.
#[test]
fn construct_on_unwritable_root_is_error() {
    seed_shared_embedder_with_mock();
    let dir = TempDir::new().expect("tempdir");
    let file_path = dir.path().join("not-a-dir");
    std::fs::write(&file_path, b"x").expect("write blocking file");

    // Rooting under a path whose parent is a file makes create_dir_all fail.
    let bad_root = file_path.join("child");
    let cfg = SmMemoryConfig::default();

    // `SmMemory` is not `Debug` (it owns a non-`Debug` registry), so match on the
    // error directly rather than via `expect_err`.
    match SmMemory::open(&bad_root, &cfg) {
        Ok(_) => panic!("must fail to create store under a file"),
        Err(err) => assert!(
            matches!(err, SmMemoryError::Palace { .. }),
            "unavailable backend must degrade to SmMemoryError::Palace, got {err:?}"
        ),
    }
}
