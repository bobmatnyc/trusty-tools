//! Tests for the SM goal store: lifecycle + dual persistence (SM-6, DOC-14 §9).
//!
//! Why: prove the acceptance contracts — a created goal has a STABLE id that
//! survives reload; linking a session recomputes progress (1/3 → 33%); the
//! verification gate REJECTS `Done` unless every linked task is `Verified`; goals
//! REBUILD from the palace on startup with the `goals.json` cache matching the
//! palace-derived state; and a palace-unavailable startup falls back to the cache
//! without panicking. The palace is mocked in-memory (a `GoalMemory` impl), so
//! these run without ONNX and need no `#[ignore]`. Each test uses a fresh
//! `tempdir` for the cache.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use tempfile::TempDir;

use super::cache::GoalCache;
use super::error::SmGoalError;
use super::memory::{GOAL_TAG, GoalMemory};
use super::model::{GoalStatus, SessionLink, SessionTaskState};
use super::store::{SessionUpdate, SmGoalStore};

/// A deterministic clock for reproducible timestamps.
fn fixed_clock() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap()
}

/// In-memory mock of the SM palace implementing [`GoalMemory`].
///
/// Why: the store's source of truth is the palace; a deterministic in-memory mock
/// lets us assert dual-persistence behaviour (writes land in the palace; rebuild
/// enumerates them) and inject an "unavailable" failure — all without the heavy
/// ONNX-backed Memory Palace.
/// What: stores tagged JSON entries newest-last; `remember_goal` upserts by the
/// goal's `id` (so re-writing a mutated goal replaces, not duplicates — mirroring
/// how a real rebuild keys by id), `list_goals` returns the current set. A
/// `fail` flag forces both methods to error (the palace-unavailable case).
#[derive(Default)]
struct MockPalace {
    inner: Mutex<MockState>,
}

#[derive(Default)]
struct MockState {
    /// (goal_id, json) entries, in insertion/upsert order.
    entries: Vec<(String, String)>,
    /// When true, every operation reports the palace as unavailable.
    fail: bool,
    /// Count of `remember_goal` calls (asserts dual-write happened).
    writes: usize,
}

impl MockPalace {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn set_fail(&self, fail: bool) {
        self.inner.lock().expect("lock").fail = fail;
    }

    fn write_count(&self) -> usize {
        self.inner.lock().expect("lock").writes
    }

    fn entry_count(&self) -> usize {
        self.inner.lock().expect("lock").entries.len()
    }
}

#[async_trait]
impl GoalMemory for MockPalace {
    async fn remember_goal(&self, json: String, tag: &str) -> Result<(), String> {
        assert_eq!(tag, GOAL_TAG, "store must always tag goals with GOAL_TAG");
        let mut st = self.inner.lock().expect("lock");
        if st.fail {
            return Err("mock palace unavailable".to_string());
        }
        st.writes += 1;
        // Upsert by id so a re-written goal replaces its prior entry, exactly as a
        // real id-keyed rebuild would collapse them.
        let id = extract_id(&json);
        if let Some(slot) = st.entries.iter_mut().find(|(eid, _)| *eid == id) {
            slot.1 = json;
        } else {
            st.entries.push((id, json));
        }
        Ok(())
    }

    async fn list_goals(&self, tag: &str) -> Result<Vec<String>, String> {
        assert_eq!(tag, GOAL_TAG);
        let st = self.inner.lock().expect("lock");
        if st.fail {
            return Err("mock palace unavailable".to_string());
        }
        Ok(st.entries.iter().map(|(_, j)| j.clone()).collect())
    }
}

/// Pull the `id` field out of a serialised goal (test helper).
fn extract_id(json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).expect("valid goal json");
    v.get("id")
        .and_then(|x| x.as_str())
        .expect("goal has id")
        .to_string()
}

/// Build a store with the mock palace + a tempdir cache + a fixed clock.
fn store_with(palace: Arc<MockPalace>, dir: &TempDir) -> SmGoalStore {
    SmGoalStore::with_clock(palace, dir.path(), fixed_clock)
}

/// Why: every created goal must carry a stable `g-…` id, and that id must survive
/// a reload (rebuild from the palace) — the join-key invariant of dual persistence.
/// What: creates a goal, records its id, rebuilds a fresh store from the SAME
/// palace, asserts the same id (and content) is present.
/// Test: this is the test.
#[tokio::test]
async fn create_assigns_stable_id_that_survives_reload() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");

    let id = {
        let mut store = store_with(palace.clone(), &dir);
        let g = store
            .create("ship SM-6", vec!["tests pass".into()])
            .await
            .expect("create");
        assert!(
            g.id.starts_with("g-"),
            "id must be g-prefixed; got {}",
            g.id
        );
        assert_eq!(g.status, GoalStatus::Pending);
        g.id
    };

    // Fresh store over the same palace → rebuild from truth.
    let reloaded = SmGoalStore::load(palace.clone(), dir.path())
        .await
        .expect("reload");
    let g = reloaded
        .get(&id)
        .expect("goal survives reload by stable id");
    assert_eq!(g.description, "ship SM-6");
}

/// Why: the acceptance criterion — linking sessions and verifying 1 of 3 yields
/// ~33% progress, derived from the link states.
/// What: creates a goal, links 3 sessions, verifies one, asserts `progress == 33`
/// and the goal moved to `InProgress`.
/// Test: this is the test.
#[tokio::test]
async fn link_updates_progress() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");
    let mut store = store_with(palace, &dir);

    let g = store
        .create("multi-session goal", vec![])
        .await
        .expect("create");
    let id = g.id.clone();

    for i in 0..3 {
        store
            .link(
                &id,
                SessionLink::launched(format!("s-{i}"), format!("task {i}")),
            )
            .await
            .expect("link");
    }
    assert_eq!(store.get(&id).unwrap().status, GoalStatus::InProgress);
    assert_eq!(store.get(&id).unwrap().progress, 0, "no links verified yet");

    store
        .update(
            &id,
            SessionUpdate {
                session_id: "s-0".into(),
                state: Some(SessionTaskState::Verified),
                evidence: Some("https://example.com/pr/1".into()),
                note: None,
            },
        )
        .await
        .expect("update");

    assert_eq!(
        store.get(&id).unwrap().progress,
        33,
        "1 of 3 verified must derive 33% progress"
    );
}

/// Why: linking an unknown goal must be a clean `NotFound`, not a panic.
/// What: links to a bogus id, asserts the error variant.
/// Test: this is the test.
#[tokio::test]
async fn link_unknown_goal_is_not_found() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");
    let mut store = store_with(palace, &dir);

    match store
        .link("g-nope", SessionLink::launched("s-1", "x"))
        .await
    {
        Err(SmGoalError::NotFound(id)) => assert_eq!(id, "g-nope"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

/// Why: THE verification gate (§3.5) — `Done` must be REJECTED while any linked
/// task is unverified, and the goal must be left unchanged (still `InProgress`).
/// What: creates a goal with 2 links, verifies only one, asserts `close` returns
/// `VerificationGate` with the right counts and does not mutate the status.
/// Test: this is the test.
#[tokio::test]
async fn close_without_all_verified_is_rejected() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");
    let mut store = store_with(palace, &dir);

    let id = store.create("gated goal", vec![]).await.expect("create").id;
    store
        .link(&id, SessionLink::launched("s-0", "a"))
        .await
        .expect("link a");
    store
        .link(&id, SessionLink::launched("s-1", "b"))
        .await
        .expect("link b");
    store
        .update(
            &id,
            SessionUpdate {
                session_id: "s-0".into(),
                state: Some(SessionTaskState::Verified),
                evidence: Some("ev".into()),
                ..Default::default()
            },
        )
        .await
        .expect("verify one");

    match store.close(&id).await {
        Err(SmGoalError::VerificationGate {
            goal_id,
            verified,
            total,
        }) => {
            assert_eq!(goal_id, id);
            assert_eq!(verified, 1);
            assert_eq!(total, 2);
        }
        other => panic!("gate must reject Done with 1/2 verified, got {other:?}"),
    }
    assert_eq!(
        store.get(&id).unwrap().status,
        GoalStatus::InProgress,
        "a rejected close must leave the goal unchanged"
    );
}

/// Why: once every linked task is `Verified`, the gate ALLOWS `Done` — the
/// positive half of §3.5.
/// What: verifies all links, closes, asserts `Done` and `progress == 100`.
/// Test: this is the test.
#[tokio::test]
async fn close_with_all_verified_succeeds() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");
    let mut store = store_with(palace, &dir);

    let id = store
        .create("completable goal", vec![])
        .await
        .expect("create")
        .id;
    for i in 0..2 {
        let sid = format!("s-{i}");
        store
            .link(&id, SessionLink::launched(&sid, "task"))
            .await
            .expect("link");
        store
            .update(
                &id,
                SessionUpdate {
                    session_id: sid,
                    state: Some(SessionTaskState::Verified),
                    evidence: Some("ev".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("verify");
    }

    let done = store
        .close(&id)
        .await
        .expect("close must succeed when all verified");
    assert_eq!(done.status, GoalStatus::Done);
    assert_eq!(done.progress, 100);
}

/// Why: non-`Done` statuses carry NO gate — `Blocked`/`Abandoned` are always
/// allowed (the SM marks a stalled goal blocked regardless of verification).
/// What: sets `Blocked` on an unverified goal, asserts success.
/// Test: this is the test.
#[tokio::test]
async fn set_blocked_has_no_gate() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");
    let mut store = store_with(palace, &dir);

    let id = store.create("stuck goal", vec![]).await.expect("create").id;
    store
        .link(&id, SessionLink::launched("s-0", "a"))
        .await
        .expect("link");
    let g = store
        .set_status(&id, GoalStatus::Blocked)
        .await
        .expect("blocked is ungated");
    assert_eq!(g.status, GoalStatus::Blocked);
}

/// Why: an update must mutate the link's state + evidence, and a supplied note
/// must be appended to the goal.
/// What: links one session, updates state+evidence+note, asserts all three landed.
/// Test: this is the test.
#[tokio::test]
async fn update_sets_state_evidence_and_note() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");
    let mut store = store_with(palace, &dir);

    let id = store.create("g", vec![]).await.expect("create").id;
    store
        .link(&id, SessionLink::launched("s-0", "a"))
        .await
        .expect("link");
    let g = store
        .update(
            &id,
            SessionUpdate {
                session_id: "s-0".into(),
                state: Some(SessionTaskState::Running),
                evidence: Some("partial output".into()),
                note: Some("operator asked to continue".into()),
            },
        )
        .await
        .expect("update");

    assert_eq!(g.sessions[0].state, SessionTaskState::Running);
    assert_eq!(g.sessions[0].evidence.as_deref(), Some("partial output"));
    assert_eq!(g.notes, vec!["operator asked to continue".to_string()]);
}

/// Why: updating an unlinked session id must be a clean `NotFound`.
/// What: updates a session not linked to the goal, asserts the error.
/// Test: this is the test.
#[tokio::test]
async fn update_unknown_session_is_not_found() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");
    let mut store = store_with(palace, &dir);

    let id = store.create("g", vec![]).await.expect("create").id;
    match store
        .update(
            &id,
            SessionUpdate {
                session_id: "s-missing".into(),
                ..Default::default()
            },
        )
        .await
    {
        Err(SmGoalError::NotFound(s)) => assert_eq!(s, "s-missing"),
        other => panic!("expected NotFound for unlinked session, got {other:?}"),
    }
}

/// Why: every mutation must DUAL-write — the palace (truth) AND the `goals.json`
/// cache — so both stores stay consistent (§9.4).
/// What: creates + links a goal, asserts the palace recorded writes and the cache
/// file on disk now holds the same goal.
/// Test: this is the test.
#[tokio::test]
async fn mutations_dual_persist_palace_and_cache() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");
    let mut store = store_with(palace.clone(), &dir);

    let id = store.create("dual goal", vec![]).await.expect("create").id;
    store
        .link(&id, SessionLink::launched("s-0", "a"))
        .await
        .expect("link");

    assert!(
        palace.write_count() >= 2,
        "create + link must each write the palace"
    );
    assert_eq!(
        palace.entry_count(),
        1,
        "the same goal must upsert, not duplicate"
    );

    // Cache on disk must reflect the goal.
    let cached = GoalCache::new(dir.path()).load().expect("load cache");
    assert_eq!(cached.len(), 1);
    assert_eq!(cached[0].id, id);
    assert_eq!(cached[0].sessions.len(), 1);
}

/// Why: the rebuild contract (§9.4) — on startup the store rebuilds its map from
/// the PALACE (source of truth), and the re-derived `goals.json` cache must match
/// that palace-derived state exactly.
/// What: writes several goals via one store, drops it, rebuilds a fresh store from
/// the same palace, then asserts the rebuilt set equals what the cache holds.
/// Test: this is the test.
#[tokio::test]
async fn rebuild_from_palace_matches_cache() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");

    {
        let mut store = store_with(palace.clone(), &dir);
        let a = store
            .create("goal A", vec!["x".into()])
            .await
            .expect("create A")
            .id;
        store.create("goal B", vec![]).await.expect("create B");
        store
            .link(&a, SessionLink::launched("s-0", "t"))
            .await
            .expect("link A");
        // store drops — palace + cache persist independently.
    }

    // Wipe the cache file to PROVE the rebuild comes from the palace, then reload.
    std::fs::remove_file(GoalCache::new(dir.path()).path()).expect("rm cache");

    let reloaded = SmGoalStore::load(palace.clone(), dir.path())
        .await
        .expect("reload");
    let rebuilt = reloaded.all();
    assert_eq!(rebuilt.len(), 2, "both goals rebuild from the palace");

    // The reload re-wrote the cache from palace truth; it must match the map.
    let cached = GoalCache::new(dir.path())
        .load()
        .expect("load rebuilt cache");
    assert_eq!(
        cached, rebuilt,
        "rebuilt cache must equal palace-derived state"
    );
}

/// Why: graceful degradation (§9.4) — when the palace is unavailable on startup,
/// the store must fall back to the last-written `goals.json` cache and NOT panic.
/// What: populates the palace + cache via a working store, then reloads with the
/// palace forced into failure; asserts the goals still load (from cache).
/// Test: this is the test.
#[tokio::test]
async fn palace_unavailable_falls_back_to_cache() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");

    let id = {
        let mut store = store_with(palace.clone(), &dir);
        store
            .create("cached goal", vec![])
            .await
            .expect("create")
            .id
    };

    // Palace now reports unavailable; startup must use the cache instead.
    palace.set_fail(true);
    let reloaded = SmGoalStore::load(palace.clone(), dir.path())
        .await
        .expect("reload must not panic when palace is down");
    assert!(
        reloaded.get(&id).is_some(),
        "goal must rebuild from the cache when the palace is unavailable"
    );
}

/// Why: a palace WRITE failure mid-mutation must propagate as a structured
/// `Palace` error (the truth-first dual-write fails closed), not silently succeed.
/// What: forces the palace to fail, then attempts a create; asserts the `Palace`
/// error variant.
/// Test: this is the test.
#[tokio::test]
async fn palace_write_failure_propagates() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");
    let mut store = store_with(palace.clone(), &dir);

    palace.set_fail(true);
    match store.create("doomed", vec![]).await {
        Err(SmGoalError::Palace { message }) => assert!(message.contains("unavailable")),
        other => panic!("palace write failure must surface as Palace error, got {other:?}"),
    }
}

/// Why: goal ids are the palace/cache upsert key, so a too-narrow id space risks
/// silent overwrites via birthday collisions. The id must carry 64 bits of entropy
/// (16 hex chars) behind the `g-` prefix, not the old 32-bit (8-char) prefix.
/// What: creates a goal and asserts its id is `g-` + 16 lowercase hex chars.
/// Test: this is the test.
#[tokio::test]
async fn goal_id_has_64bit_width() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");
    let mut store = store_with(palace, &dir);

    let id = store.create("widen me", vec![]).await.expect("create").id;
    let hex = id.strip_prefix("g-").expect("id must be g-prefixed");
    assert_eq!(
        hex.len(),
        16,
        "id must carry 16 hex chars (64-bit) for collision resistance; got {id}"
    );
    assert!(
        hex.chars().all(|c| c.is_ascii_hexdigit()),
        "id body must be hex; got {id}"
    );
}

/// Why: `create` must be ATOMIC w.r.t. the in-memory map — if the palace write
/// fails, the freshly-inserted goal must NOT linger as a phantom visible to
/// `all()`/`get()` (it was never durably written). The store must be clean.
/// What: forces the palace to fail, attempts a create, asserts the `Palace` error
/// AND that `all()` is empty and the would-be goal is absent.
/// Test: this is the test.
#[tokio::test]
async fn failed_create_leaves_no_phantom_goal() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");
    let mut store = store_with(palace.clone(), &dir);

    palace.set_fail(true);
    match store.create("phantom", vec![]).await {
        Err(SmGoalError::Palace { .. }) => {}
        other => panic!("expected Palace error on failed create, got {other:?}"),
    }

    assert!(
        store.all().is_empty(),
        "a failed create must leave NO phantom goal in the in-memory map"
    );
    assert_eq!(
        palace.entry_count(),
        0,
        "nothing was durably written, so the palace must hold nothing"
    );
}

/// Why: every existing-goal mutator (`link`/`update`/`note`/`set_status`) must be
/// ATOMIC w.r.t. the map — if the palace write fails mid-mutation, the goal
/// observed afterward must be byte-identical to before the call (no half-applied
/// mutation lingering). This proves the snapshot/restore rollback.
/// What: creates + links a goal (succeeds), snapshots it, then forces the palace to
/// fail and drives each mutator in turn; after each failure asserts the `Palace`
/// error AND that `get()` returns the UNCHANGED prior goal.
/// Test: this is the test.
#[tokio::test]
async fn failed_mutation_leaves_existing_goal_unchanged() {
    let palace = MockPalace::new();
    let dir = TempDir::new().expect("tempdir");
    let mut store = store_with(palace.clone(), &dir);

    let id = store.create("durable", vec![]).await.expect("create").id;
    store
        .link(&id, SessionLink::launched("s-0", "task"))
        .await
        .expect("link");

    // Snapshot the goal in its committed state.
    let before = store.get(&id).expect("present").clone();

    // From now on every palace write fails — each mutator must roll its map change
    // back so the goal stays byte-identical to `before`.
    palace.set_fail(true);

    let assert_unchanged = |store: &SmGoalStore, ctx: &str| {
        let after = store.get(&id).expect("goal must still be present");
        assert_eq!(
            after, &before,
            "after a failed {ctx} the goal must be UNCHANGED"
        );
    };

    // link
    match store
        .link(&id, SessionLink::launched("s-1", "second"))
        .await
    {
        Err(SmGoalError::Palace { .. }) => {}
        other => panic!("failed link must surface Palace error, got {other:?}"),
    }
    assert_unchanged(&store, "link");

    // update (existing session s-0)
    match store
        .update(
            &id,
            SessionUpdate {
                session_id: "s-0".into(),
                state: Some(SessionTaskState::Verified),
                evidence: Some("ev".into()),
                note: Some("note".into()),
            },
        )
        .await
    {
        Err(SmGoalError::Palace { .. }) => {}
        other => panic!("failed update must surface Palace error, got {other:?}"),
    }
    assert_unchanged(&store, "update");

    // note
    match store.note(&id, "blocker noted").await {
        Err(SmGoalError::Palace { .. }) => {}
        other => panic!("failed note must surface Palace error, got {other:?}"),
    }
    assert_unchanged(&store, "note");

    // set_status (Blocked is ungated, so it reaches the persist)
    match store.set_status(&id, GoalStatus::Blocked).await {
        Err(SmGoalError::Palace { .. }) => {}
        other => panic!("failed set_status must surface Palace error, got {other:?}"),
    }
    assert_unchanged(&store, "set_status");
}
