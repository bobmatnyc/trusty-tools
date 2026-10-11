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

/// Read `created_at` out of a palace's `palace.json`.
///
/// Why: `created_at` is the field `create_palace` destroys when it is reached
/// for a palace that already exists — it stamps `Utc::now()` on every save.
/// Reading it directly makes the assertion name the thing that is lost.
/// What: parses the metadata file and returns the `created_at` string.
/// Test: used by
/// `ensure_palace_never_rewrites_metadata_of_a_present_but_unopenable_palace`.
fn created_at_of(metadata: &std::path::Path) -> String {
    let raw = std::fs::read_to_string(metadata).expect("read palace.json");
    let json: serde_json::Value = serde_json::from_str(&raw).expect("parse palace.json");
    json["created_at"]
        .as_str()
        .expect("palace.json carries created_at")
        .to_string()
}

/// Materialise a palace on disk that exists but cannot be opened, WITHOUT this
/// process ever opening it.
///
/// Why: two traps make the obvious fixture (create a palace, then corrupt it)
/// silently vacuous. The store keeps a process-wide cache of open redb
/// databases keyed by path, so a palace this process already opened is served
/// from that cache and never touches the filesystem — the corruption is never
/// read. And `PalaceHandle` opens `<dir>/kg.db` through
/// `KnowledgeGraph::open_with_intent`, which rewrites the extension, so the
/// bytes that actually matter live at `kg.redb`. Hand-writing the metadata
/// avoids the first trap; naming `kg.redb` avoids the second.
/// What: writes a valid `palace.json` carrying `created_at`, then puts a
/// DIRECTORY where the KG store file belongs. Opening a directory as a database
/// file fails at the OS layer on every platform and every redb version, so the
/// failure is deterministic — no lock, no race, no format dependence. Returns
/// the palace directory.
/// Test: used by
/// `ensure_palace_never_rewrites_metadata_of_a_present_but_unopenable_palace`.
fn seed_unopenable_palace_on_disk(
    data_root: &std::path::Path,
    palace_id: &str,
) -> std::path::PathBuf {
    let palace_dir = data_root.join(palace_id);
    std::fs::create_dir_all(&palace_dir).expect("create palace dir");
    let metadata = serde_json::json!({
        "id": palace_id,
        "name": palace_id,
        "description": serde_json::Value::Null,
        "created_at": "2020-01-02T03:04:05Z",
        "data_dir": palace_dir,
        "schema_version": 1,
    });
    std::fs::write(
        palace_dir.join("palace.json"),
        serde_json::to_vec_pretty(&metadata).expect("serialise palace.json"),
    )
    .expect("write palace.json");
    std::fs::create_dir(palace_dir.join("kg.redb")).expect("directory where the KG store belongs");
    palace_dir
}

/// Why (issue #4911): `ensure_palace` treated EVERY `open_palace` failure as
/// "the palace does not exist" and fell through to `create_palace`, which
/// rewrites `palace.json` unconditionally via `PalaceStore::save_palace` — so a
/// palace that merely could not be READ lost its `created_at` and only then
/// re-failed against the same unreadable store. Making a read-only open of an
/// incompatible-format store refuse (rather than silently recreate it empty)
/// turned that latent path into a live one, which is how a fix for data
/// destruction reintroduced data destruction one layer out.
/// What: seeds a present-but-unopenable SM palace on disk (see
/// `seed_unopenable_palace_on_disk`), records its `created_at`, then constructs
/// an `SmMemory` against that root. Asserts the construction fails AND that
/// `palace.json` is byte-identical. The byte-identity assertion is what proves
/// `create_palace` was never reached: reaching it rewrites the file with a fresh
/// `created_at`, so this test cannot pass while that call happens.
/// Test: this is the test.
#[test]
fn ensure_palace_never_rewrites_metadata_of_a_present_but_unopenable_palace() {
    seed_shared_embedder_with_mock();
    let dir = TempDir::new().expect("tempdir");
    let cfg = SmMemoryConfig::default();

    let palace_dir = seed_unopenable_palace_on_disk(dir.path(), &cfg.palace);
    let metadata = palace_dir.join("palace.json");
    let created_at_before = created_at_of(&metadata);
    let metadata_before = std::fs::read(&metadata).expect("read palace.json");

    // `SmMemory` is not `Debug`, so `expect_err` is unavailable here.
    let err = match SmMemory::open(dir.path(), &cfg) {
        Ok(_) => {
            panic!("opening a present-but-unopenable palace must fail, not silently recreate it")
        }
        Err(e) => e,
    };

    assert_eq!(
        created_at_before,
        created_at_of(&metadata),
        "created_at was rewritten, so `ensure_palace` fell through to \
         `create_palace` for a palace that exists but could not be read; the \
         open error was: {err}"
    );
    assert_eq!(
        metadata_before,
        std::fs::read(&metadata).expect("read palace.json"),
        "palace.json must be byte-identical — any rewrite means `create_palace` ran"
    );
    assert!(
        palace_dir.join("kg.redb").is_dir(),
        "the unopenable store must be left exactly as it is, not recovered destructively"
    );
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
            .any(|h| h.drawer.content().contains("TOCTOU_MARKER")),
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
        hits.iter().any(|h| h
            .drawer
            .content()
            .contains("delegates all engineering work")),
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
            .any(|h| h.drawer.content().contains("worktree-per-ticket")),
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
        hits.iter()
            .any(|h| h.drawer.content().contains("ship SM-4")),
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
            .all(|h| !h.drawer.content().contains("OTHER_PALACE_SECRET")),
        "SM recall must never surface another palace's content; got {sm_hits:?}"
    );

    // The other palace's recall must NOT see the SM marker.
    let other_hits = other.recall("secret marker").await.expect("other recall");
    assert!(
        other_hits
            .iter()
            .all(|h| !h.drawer.content().contains("SM_PALACE_SECRET")),
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
            .any(|h| h.drawer.content().contains("SM-3 merged to main")),
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

/// #9274 (PR3 critic HIGH 2): the daemon opens the SM palace as writer, so an
/// existing format-0 SM palace migrates at open. Under read-only intent it is
/// refused with `FormatNeedsMigration`.
/// Test: this is the test.
#[test]
fn an_existing_format_0_sm_palace_migrates_at_open() {
    use trusty_common::memory_core::store::format_v0_fixture::write_format_0_palace;
    use trusty_common::memory_core::store::palace_format::{
        MIGRATION_RECORD, PALACE_FORMAT_SUPPORTED,
    };

    seed_shared_embedder_with_mock();
    let dir = TempDir::new().expect("tempdir");
    let cfg = SmMemoryConfig::default();
    let palace_dir = dir.path().join(&cfg.palace);
    write_format_0_palace(&palace_dir, &cfg.palace).expect("format-0 palace");

    // #9274: the daemon's open, the one that migrates.
    let mem = SmMemory::open_with_intent(dir.path(), &cfg, OpenIntent::Writer)
        .expect("a format-0 SM palace opens");
    assert_eq!(mem.palace_id().as_str(), cfg.palace);
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(palace_dir.join("palace.json")).expect("read"))
            .expect("palace.json");
    assert_eq!(json["format_version"], PALACE_FORMAT_SUPPORTED, "{json}");
    assert!(
        !palace_dir.join(MIGRATION_RECORD).exists(),
        "migration left its record"
    );
}

/// The env var naming the data root the writer child process holds (#9274).
const HOLD_ROOT_ENV: &str = "TM_9274_HOLD_SM_PALACE_ROOT";

/// Kills the writer child process if the test fails before releasing it.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// #9274: `tm sm serve --stdio` opens the SM palace read-only, so it still
/// opens and recalls while the daemon holds the palace as writer. A writer
/// open there failed after the handoff retry and lost recall.
///
/// What: a child process (this test binary, running
/// `hold_the_sm_palace_as_writer_child`) opens the palace as writer, writes a
/// fact and keeps the palace open. This process then opens the palace the
/// way `tm sm serve` does and recalls the fact, twice: once through the
/// handle `PalaceRegistry::open` hydrates (always read-only), and once after
/// dropping that handle, so the palace reopens with the registry's own
/// intent. The writer is a separate process because this process's store
/// cache would otherwise serve the open.
/// Test: this is the test.
#[tokio::test]
async fn a_read_only_open_recalls_while_a_writer_holds_the_palace() {
    use std::time::{Duration, Instant};

    seed_shared_embedder_with_mock();
    let dir = TempDir::new().expect("tempdir");
    let cfg = SmMemoryConfig::default();
    let child = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "core::sm::memory::tests::hold_the_sm_palace_as_writer_child",
            "--ignored",
            "--nocapture",
        ])
        .env(HOLD_ROOT_ENV, dir.path())
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn the writer");
    let mut writer = ChildGuard(child);

    let held = dir.path().join("held");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !held.exists() {
        if let Some(status) = writer.0.try_wait().expect("poll the writer") {
            panic!("the writer exited before it held the palace: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "the writer never held the palace"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let reader = SmMemory::open(dir.path(), &cfg).expect("sm serve opens a held palace");
    let hits = reader
        .recall("which marker did the daemon write")
        .await
        .expect("recall from a held palace");
    assert!(
        hits.iter()
            .any(|h| h.drawer.content().contains("HELD_PALACE_MARKER")),
        "a read-only open of a held palace must recall its content"
    );

    // `PalaceRegistry::open` hydrates an existing palace read-only before any
    // intent applies; drop that handle so the next recall reopens the palace
    // with the intent this registry carries.
    reader.registry.remove(reader.palace_id());
    let hits = reader
        .recall("which marker did the daemon write")
        .await
        .expect("recall after a reopen of a held palace");
    assert!(
        hits.iter()
            .any(|h| h.drawer.content().contains("HELD_PALACE_MARKER")),
        "a reopen of a held palace must recall its content"
    );
    std::fs::write(dir.path().join("release"), b"").expect("release the writer");
}

/// Child half of `a_read_only_open_recalls_while_a_writer_holds_the_palace`:
/// the daemon's writer open of the SM palace, held until the parent releases it.
///
/// What: returns at once unless `HOLD_ROOT_ENV` names a data root. Otherwise
/// opens the palace there with writer intent, writes the marker fact, signals
/// `held`, and keeps the palace open until `release` appears (at most 120 s).
/// Test: run by `a_read_only_open_recalls_while_a_writer_holds_the_palace`.
#[test]
#[ignore = "child process of a_read_only_open_recalls_while_a_writer_holds_the_palace"]
fn hold_the_sm_palace_as_writer_child() {
    use std::time::{Duration, Instant};

    let Some(root) = std::env::var_os(HOLD_ROOT_ENV).map(std::path::PathBuf::from) else {
        return;
    };
    seed_shared_embedder_with_mock();
    let cfg = SmMemoryConfig::default();
    let writer = SmMemory {
        registry: PalaceRegistry::open(&root)
            .expect("registry")
            .with_writer_intent(),
        data_root: root.clone(),
        palace_id: PalaceId::new(cfg.palace.clone()),
        recall_top_k: cfg.recall_top_k as usize,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime
        .block_on(writer.remember("HELD_PALACE_MARKER written by the daemon"))
        .expect("remember as writer");
    std::fs::write(root.join("held"), b"").expect("signal held");

    let deadline = Instant::now() + Duration::from_secs(120);
    while !root.join("release").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(writer);
}

/// The env var naming the data root the #9659 daemon child opens as writer.
const DAEMON_ROOT_ENV: &str = "TM_9659_DAEMON_SM_PALACE_ROOT";

/// The fact the #9659 daemon child remembers.
const DAEMON_MARKER: &str = "DAEMON_9659_MARKER written by the daemon";

/// The open intent `tm sm serve --stdio` takes on the SM palace (#9659).
fn sm_serve_intent() -> OpenIntent {
    crate::daemon::state::PalaceAccess::ReadOnly.open_intent()
}

/// Run `open_the_sm_palace_as_the_daemon_child` against `root` to completion.
///
/// What: the daemon is a separate process, as in production, so this
/// process's store cache cannot serve its open. Fails with the child's output
/// when any of its assertions fails.
fn run_the_daemon_child(root: &std::path::Path) {
    let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "core::sm::memory::tests::open_the_sm_palace_as_the_daemon_child",
            "--ignored",
            "--nocapture",
        ])
        .env(DAEMON_ROOT_ENV, root)
        .output()
        .expect("run the daemon child");
    assert!(
        out.status.success(),
        "the daemon's writer open failed beside tm sm serve ({}):\n{}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Child half of the #9659 tests: the daemon's writer open of the SM palace.
///
/// What: returns at once unless `DAEMON_ROOT_ENV` names a data root. Otherwise
/// opens the SM palace there with writer intent and asserts the open is Ok,
/// the handle holds the live palace (not a read-only snapshot), the palace is
/// at the supported format, and a `remember` lands.
/// Test: run by `sm_serve_first_then_daemon_gets_the_writer_lock` and its
/// siblings.
#[test]
#[ignore = "child process of the #9659 sm-serve-first tests"]
fn open_the_sm_palace_as_the_daemon_child() {
    use trusty_common::memory_core::store::palace_format::PALACE_FORMAT_SUPPORTED;

    let Some(root) = std::env::var_os(DAEMON_ROOT_ENV).map(std::path::PathBuf::from) else {
        return;
    };
    seed_shared_embedder_with_mock();
    let cfg = SmMemoryConfig::default();
    let daemon = match SmMemory::open_with_intent(&root, &cfg, OpenIntent::Writer) {
        Ok(mem) => mem,
        Err(e) => panic!("the daemon's writer open of the SM palace failed: {e}"),
    };
    let handle = daemon
        .ensure_palace()
        .expect("the daemon's SM palace handle");
    assert!(
        !handle.kg.is_read_only(),
        "the daemon holds a read-only snapshot of the SM palace, not the live file"
    );
    let json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join(&cfg.palace).join("palace.json")).expect("read palace.json"),
    )
    .expect("palace.json");
    assert_eq!(json["format_version"], PALACE_FORMAT_SUPPORTED, "{json}");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime
        .block_on(daemon.remember(DAEMON_MARKER))
        .expect("remember as the daemon");
}

/// #9659: `tm sm serve` started before the daemon must leave the daemon the
/// writer. With a format-0 SM palace and `tm sm serve`'s open held for the
/// whole test, the daemon's writer open migrates the palace and `remember`
/// lands. `tm sm serve` cannot read a format-0 palace; its open may refuse.
/// Test: this is the test.
#[test]
fn sm_serve_first_then_daemon_gets_the_writer_lock() {
    use trusty_common::memory_core::store::format_v0_fixture::write_format_0_palace;

    seed_shared_embedder_with_mock();
    let dir = TempDir::new().expect("tempdir");
    let cfg = SmMemoryConfig::default();
    write_format_0_palace(&dir.path().join(&cfg.palace), &cfg.palace).expect("format-0 palace");

    let _serve = SmMemory::open_with_intent(dir.path(), &cfg, sm_serve_intent());
    run_the_daemon_child(dir.path());
}

/// #9659: the same order with a current-format SM palace. `tm sm serve` opens
/// it and holds it; the daemon's writer open must still hold the live file,
/// never a read-only snapshot.
/// Test: this is the test.
#[test]
fn sm_serve_first_then_daemon_gets_the_writer_lock_on_a_current_palace() {
    seed_shared_embedder_with_mock();
    let dir = TempDir::new().expect("tempdir");
    let cfg = SmMemoryConfig::default();
    // A daemon run creates the current-format palace, then exits.
    run_the_daemon_child(dir.path());

    let serve = SmMemory::open_with_intent(dir.path(), &cfg, sm_serve_intent());
    assert!(serve.is_ok(), "tm sm serve opens a current-format palace");
    run_the_daemon_child(dir.path());
}

/// #9659: with no daemon running, `tm sm serve` still recalls a fact the
/// daemon wrote earlier, and it reads a snapshot copy: the live `kg.redb` and
/// vector store stay unlocked while its open is held.
/// Test: this is the test.
#[test]
fn daemon_not_running_sm_serve_still_recalls() {
    seed_shared_embedder_with_mock();
    let dir = TempDir::new().expect("tempdir");
    let cfg = SmMemoryConfig::default();
    run_the_daemon_child(dir.path());

    let serve = match SmMemory::open_with_intent(dir.path(), &cfg, sm_serve_intent()) {
        Ok(mem) => mem,
        Err(e) => panic!("tm sm serve could not open the SM palace: {e}"),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let hits = runtime
        .block_on(serve.recall("which marker did the daemon write"))
        .expect("recall with no daemon running");
    assert!(
        hits.iter()
            .any(|h| h.drawer.content().contains("DAEMON_9659_MARKER")),
        "tm sm serve must recall the daemon's fact with no daemon running"
    );

    let palace_dir = dir.path().join(&cfg.palace);
    for file in ["kg.redb", "index.usearch.redb"] {
        let live = trusty_common::redb_cache::create_palace_db(&palace_dir.join(file));
        assert!(
            live.is_ok(),
            "tm sm serve holds the redb lock on the live {file}: {:?}",
            live.err()
        );
    }
    drop(serve);
}
