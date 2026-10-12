//! #9282 (PR 2): the `palace_backup` MCP tool and the `palace backup` /
//! `palace restore` CLI.
//!
//! Why: a backup is proven by what a restore of it holds, and by the refusals
//! that keep a restore from writing over a live or foreign palace. Only the
//! public surfaces — the tool through the dispatcher, the CLI through the
//! built binary — can show either.
//! What: the tool tests drive `transport::dispatch` / `dispatch_tool` against
//! an `AppState`; the CLI tests seed a palace on disk, run the binary with
//! `TRUSTY_DATA_DIR_OVERRIDE`, and reopen what it wrote. Its own test binary
//! because it seeds the process-wide mock embedder.
//! Test: this *is* the test file.

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use serde_json::{json, Value};
use trusty_common::memory_core::palace::{Drawer, Palace, PalaceId, RoomType};
use trusty_common::memory_core::retrieval::{seed_shared_embedder_with_mock, PalaceHandle};
use trusty_common::memory_core::store::chat_sessions::ChatSessionStore;
use trusty_common::memory_core::store::kg_redb::{BatchWriteOp, KgStoreRedb};
use trusty_common::memory_core::store::rooms::resolve_or_create_room_sync;
use trusty_common::memory_core::store::PalaceStore;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::transport::{dispatch, JsonRpcRequest};
use trusty_memory::AppState;

// ---------------------------------------------------------------- MCP tool

struct Fixture {
    tmp: tempfile::TempDir,
    state: AppState,
}

impl Fixture {
    fn new() -> Self {
        seed_shared_embedder_with_mock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = AppState::new(tmp.path().to_path_buf());
        state.set_ready();
        Self { tmp, state }
    }

    fn palace(&self, id: &str) -> Arc<PalaceHandle> {
        let root = &self.state.data_root;
        self.state
            .registry
            .create_palace(
                root,
                Palace {
                    id: PalaceId::new(id),
                    name: id.to_string(),
                    description: None,
                    created_at: chrono::Utc::now(),
                    data_dir: root.join(id),
                },
            )
            .expect("create palace")
    }

    async fn tool(&self, name: &str, args: Value) -> anyhow::Result<Value> {
        dispatch_tool(&self.state, name, args).await
    }
}

/// Raw-method call, the path `trusty-memory palace backup` takes.
async fn raw(state: &AppState, method: &str, params: Value) -> Result<Value, String> {
    let req = JsonRpcRequest {
        jsonrpc: Some("2.0".to_string()),
        id: Some(json!(1)),
        method: method.to_string(),
        params: Some(params),
    };
    let resp = dispatch(state, req).await;
    match (resp.result, resp.error) {
        (Some(v), _) => Ok(v),
        (None, Some(e)) => Err(format!("{} ({})", e.message, e.code)),
        (None, None) => Err("neither result nor error".to_string()),
    }
}

fn read_manifest(dir: &Path) -> Value {
    let bytes = std::fs::read(dir.join("manifest.json")).expect("read manifest.json");
    serde_json::from_slice(&bytes).expect("parse manifest.json")
}

/// Why (#9282): the tool's answer is what an operator records as "the
/// backup"; its counts must be the palace's, not a plausible template.
/// What: seeds three notes and two extra rooms through the tools, calls
/// `palace_backup` by raw method name, and compares the returned counts, the
/// on-disk manifest and the file list with `palace_info`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn palace_backup_manifest_counts_equal_the_palace_counts() {
    let fx = Fixture::new();
    fx.palace("bk-round");
    for label in ["Backend", "Planning"] {
        fx.tool("room_create", json!({"palace": "bk-round", "label": label}))
            .await
            .expect("room_create");
    }
    for i in 0..3 {
        fx.tool(
            "memory_remember",
            json!({
                "palace": "bk-round",
                "text": format!("backup round trip note {i} records a separate durable fact"),
                "room": "Backend",
                "force": true,
            }),
        )
        .await
        .expect("remember");
    }
    let info = fx
        .tool("palace_info", json!({"palace": "bk-round"}))
        .await
        .expect("palace_info");

    let out = raw(&fx.state, "palace_backup", json!({"palace": "bk-round"}))
        .await
        .expect("palace_backup");
    assert_eq!(out["palace"], "bk-round", "{out}");
    assert_eq!(out["drawer_count"], info["drawer_count"], "{out} vs {info}");
    assert_eq!(out["room_count"], info["room_count"], "{out} vs {info}");
    assert_eq!(out["vector_index_included"], false, "{out}");
    let dir = PathBuf::from(out["dir"].as_str().expect("dir"));
    assert!(
        dir.starts_with(fx.tmp.path().join(".trusty-memory/backups/manual/bk-round")),
        "default destination: {}",
        dir.display()
    );
    let manifest = read_manifest(&dir);
    assert_eq!(manifest["drawer_count"], info["drawer_count"], "{manifest}");
    assert_eq!(manifest["room_count"], info["room_count"], "{manifest}");
    let names: Vec<&str> = out["files"]
        .as_array()
        .expect("files")
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    assert!(names.contains(&"kg.redb"), "{names:?}");
    assert!(names.contains(&"palace.json"), "{names:?}");
    assert!(
        !names.iter().any(|n| n.starts_with("index.usearch")),
        "the vector index is never backed up: {names:?}"
    );
}

/// Seed `n` drawers straight into the KG store, so the copy takes a while.
fn seed_drawers(handle: &PalaceHandle, n: usize) {
    let store = handle.kg.store();
    let room = resolve_or_create_room_sync(&store, &RoomType::Custom("Bulk".into())).expect("room");
    let ops: Vec<BatchWriteOp> = (0..n)
        .map(|i| BatchWriteOp::UpsertDrawer(Drawer::new(room, format!("bulk drawer {i}"))))
        .collect();
    for chunk in ops.chunks(500) {
        store.apply_batch(chunk).expect("seed drawers");
    }
}

/// Why (#9282 review, requirement 1): redb allows one handle per file per
/// process. A backup that opened `chat_sessions.redb` itself would fail while
/// the daemon's cached store holds it, and while its own handle was held any
/// chat call that opened the store would fail.
/// What: writes a chat session through the tool (so the daemon caches the
/// store), then runs the backup while 16 chat calls hit the same palace. The
/// backup must hold the new session and every chat call must succeed.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn palace_backup_lends_the_cached_chat_store() {
    let fx = Arc::new(Fixture::new());
    let handle = fx.palace("bk-chat");
    seed_drawers(&handle, 3000);
    fx.tool(
        "chat_session_create",
        json!({"palace": "bk-chat", "title": "written just before the backup"}),
    )
    .await
    .expect("chat_session_create");

    let backup = {
        let fx = fx.clone();
        tokio::spawn(
            async move { raw(&fx.state, "palace_backup", json!({"palace": "bk-chat"})).await },
        )
    };
    let mut chats = Vec::new();
    for _ in 0..16 {
        let fx = fx.clone();
        chats.push(tokio::spawn(async move {
            fx.tool("chat_session_list", json!({"palace": "bk-chat"}))
                .await
        }));
    }
    for c in chats {
        c.await
            .expect("join")
            .expect("a chat call during the backup must succeed");
    }
    let out = backup.await.expect("join").expect("palace_backup");
    assert_eq!(out["chat_tables"]["chat_sessions"], 1, "{out}");
    assert_eq!(out["drawer_count"], 3000, "{out}");
    let dir = PathBuf::from(out["dir"].as_str().expect("dir"));
    assert!(dir.join("chat_sessions.redb").is_file());
}

/// Why (#9282): every refusal must be an error, never a partial backup.
/// What: a relative `dest`, an existing `dest`, a non-string `dest` and a
/// palace that does not exist each fail, and none leaves a directory behind.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn palace_backup_error_arms_fail_and_write_nothing() {
    let fx = Fixture::new();
    fx.palace("bk-err");
    let root = fx.tmp.path();

    let rel = raw(
        &fx.state,
        "palace_backup",
        json!({"palace": "bk-err", "dest": "rel/dir"}),
    )
    .await;
    let err = rel.expect_err("a relative dest is refused");
    assert!(err.contains("absolute"), "{err}");
    assert!(!root.join("rel").exists());

    let typed = raw(
        &fx.state,
        "palace_backup",
        json!({"palace": "bk-err", "dest": 7}),
    )
    .await;
    assert!(typed
        .expect_err("a non-string dest is refused")
        .contains("dest"));

    // Outside the registry dir (`root`), which is refused for its own reason.
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let existing = elsewhere.path().join("already-here");
    std::fs::create_dir(&existing).expect("mkdir");
    let exists = raw(
        &fx.state,
        "palace_backup",
        json!({"palace": "bk-err", "dest": existing}),
    )
    .await;
    assert!(
        exists
            .expect_err("an existing dest is refused")
            .contains("already exists"),
        "the snapshot's own refusal must propagate"
    );
    assert_eq!(std::fs::read_dir(&existing).expect("ls").count(), 0);

    let missing = raw(
        &fx.state,
        "palace_backup",
        json!({"palace": "no-such-palace"}),
    )
    .await;
    missing.expect_err("a missing palace is an error");
    assert!(
        !root.join("no-such-palace").exists(),
        "a failed backup must not create the palace dir"
    );
    assert!(!root
        .join(".trusty-memory/backups/manual/no-such-palace")
        .exists());
}

/// Every path under `dir`, relative and sorted.
fn tree(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).expect("list") {
            let p = e.expect("entry").path();
            if p.is_dir() && !p.is_symlink() {
                todo.push(p.clone());
            }
            out.push(p.strip_prefix(dir).expect("prefix").to_path_buf());
        }
    }
    out.sort();
    out
}

/// Why (#9282, Opus critic HIGH): in the flat layout the data root IS the
/// palace registry dir. A backup written there holds a `palace.json`, so the
/// palace lister takes it for a palace, and after a restart the live handle
/// can point at the backup.
/// What: `dest` = `<registry>/bk`, named directly and through a symlink to
/// the registry. Each call is refused with the registry error, and the tree
/// under the registry is unchanged: no `bk`, no `bk.tmp` staging dir.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn palace_backup_refuses_a_dest_in_the_registry_dir() {
    let fx = Fixture::new();
    fx.palace("bk-reg");
    let registry = fx.tmp.path().to_path_buf();
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let link = elsewhere.path().join("registry-link");
    std::os::unix::fs::symlink(&registry, &link).expect("symlink");
    let before = tree(&registry);
    for dest in [registry.join("bk"), link.join("bk-via-link")] {
        let res = raw(
            &fx.state,
            "palace_backup",
            json!({"palace": "bk-reg", "dest": dest}),
        )
        .await;
        let err = res.expect_err("a dest in the registry dir must be refused");
        assert!(
            err.contains("palace registry directory"),
            "{}: {err}",
            dest.display()
        );
        assert_eq!(
            tree(&registry),
            before,
            "{} wrote something",
            dest.display()
        );
    }
}

/// Why (#9282, delta critic BLOCKER): in the flat layout a backup under
/// `<data root>/palaces` creates that directory, and the next start takes it
/// as the palace registry, so every real palace vanishes from the daemon.
/// What: `dest` = `<root>/palaces/bk` and `<root>/palaces/sub/bk`. Each call
/// is refused with the registry-switch error, and the tree under the data
/// root is unchanged: no `palaces/`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn palace_backup_refuses_a_flat_layout_dest_under_palaces() {
    let fx = Fixture::new();
    fx.palace("bk-switch");
    let root = fx.tmp.path().to_path_buf();
    let before = tree(&root);
    for dest in [
        root.join("palaces").join("bk"),
        root.join("palaces").join("sub").join("bk"),
    ] {
        let res = raw(
            &fx.state,
            "palace_backup",
            json!({"palace": "bk-switch", "dest": dest}),
        )
        .await;
        let err = res.expect_err("a dest under <root>/palaces must be refused");
        assert!(
            err.contains("switch its palace registry"),
            "{}: {err}",
            dest.display()
        );
        assert_eq!(tree(&root), before, "{} wrote something", dest.display());
    }
    assert!(!root.join("palaces").exists(), "palaces/ was created");
}

// --------------------------------------------------------------------- CLI

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trusty-memory"))
}

/// Run the binary with `override_dir` as `TRUSTY_DATA_DIR_OVERRIDE`.
fn run(override_dir: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .env("TRUSTY_DATA_DIR_OVERRIDE", override_dir)
        .env("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1")
        .env("RUST_LOG", "warn")
        .output()
        .expect("spawn trusty-memory")
}

fn text(out: &Output) -> String {
    format!(
        "status={:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The trusty-memory data dir under an override.
fn data_dir(override_dir: &Path) -> PathBuf {
    override_dir.join("trusty-memory")
}

/// Seed `<override>/trusty-memory/palaces/<id>` with rooms, drawers and a
/// chat session, then close every handle. Returns (drawers, rooms).
fn seed_on_disk(override_dir: &Path, id: &str) -> (usize, usize) {
    seed_shared_embedder_with_mock();
    let data_dir = data_dir(override_dir).join("palaces").join(id);
    let palace = Palace {
        id: PalaceId::new(id),
        name: id.into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: data_dir.clone(),
    };
    PalaceStore::save_palace(&palace).expect("palace.json");
    let handle = PalaceHandle::open(&palace).expect("open palace");
    let store = handle.kg.store();
    let rooms: Vec<_> = ["alpha", "beta"]
        .iter()
        .map(|r| resolve_or_create_room_sync(&store, &RoomType::Custom((*r).into())).expect("room"))
        .collect();
    for i in 0..40 {
        store
            .upsert_drawer(&Drawer::new(rooms[i % 2], format!("cli seeded drawer {i}")))
            .expect("drawer");
    }
    let counts = (
        store.load_drawer_ids().expect("ids").len(),
        store.list_rooms().expect("rooms").len(),
    );
    drop(store);
    drop(handle);
    let chat = ChatSessionStore::open(&data_dir.join("chat_sessions.db")).expect("chat");
    chat.create_session(Some("cli".into())).expect("session");
    counts
}

/// (drawers, rooms) of the palace `id` found under a data dir's registry.
fn counts_under(data_dir: &Path, id: &str) -> (usize, usize) {
    let root = trusty_common::palace_alias::palace_registry_dir_from(data_dir.to_path_buf());
    let kg = KgStoreRedb::open(&root.join(id).join("kg.redb")).expect("open restored kg");
    (
        kg.load_drawer_ids().expect("ids").len(),
        kg.list_rooms().expect("rooms").len(),
    )
}

/// Write a backup of `id` from `src` into `dest` through the binary.
fn cli_backup(src: &Path, id: &str, dest: &Path) {
    let out = run(
        src,
        &[
            "palace",
            "backup",
            id,
            "--dest",
            dest.to_str().expect("utf8"),
        ],
    );
    assert!(out.status.success(), "backup: {}", text(&out));
    assert!(dest.join("manifest.json").is_file(), "{}", text(&out));
}

/// Why (#9282 AC2): a backup is only proven by restoring it somewhere empty
/// and finding the same palace there.
/// What: seeds a palace, runs `palace backup` with no daemon (the direct
/// path), restores into a second, empty override root, reopens it, and
/// compares drawer and room counts. The restore must print the re-embed step.
/// Test: itself.
#[test]
fn cli_backup_then_restore_into_a_fresh_root_keeps_counts() {
    let src = tempfile::tempdir().expect("src");
    let dst = tempfile::tempdir().expect("dst");
    let scratch = tempfile::tempdir().expect("scratch");
    let (drawers, rooms) = seed_on_disk(src.path(), "cli-rt");
    let snap = scratch.path().join("snap");
    cli_backup(src.path(), "cli-rt", &snap);

    let out = run(
        dst.path(),
        &["palace", "restore", snap.to_str().expect("utf8")],
    );
    assert!(out.status.success(), "restore: {}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&format!("drawers {drawers}")), "{stdout}");
    assert!(stdout.contains("palace_reembed"), "the next step: {stdout}");
    assert_eq!(
        counts_under(&data_dir(dst.path()), "cli-rt"),
        (drawers, rooms)
    );
}

/// Why (#9282 review, requirement 2): restoring a backup of palace A under
/// the id B would silently hand B's callers A's memories.
/// What: `--palace other` against a backup of `cli-mm` exits non-zero with
/// the mismatch error and writes nothing under the fresh root.
/// Test: itself.
#[test]
fn cli_restore_refuses_a_palace_id_mismatch_and_writes_nothing() {
    let src = tempfile::tempdir().expect("src");
    let dst = tempfile::tempdir().expect("dst");
    let scratch = tempfile::tempdir().expect("scratch");
    seed_on_disk(src.path(), "cli-mm");
    let snap = scratch.path().join("snap");
    cli_backup(src.path(), "cli-mm", &snap);

    let out = run(
        dst.path(),
        &[
            "palace",
            "restore",
            snap.to_str().expect("utf8"),
            "--palace",
            "other",
        ],
    );
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("is a backup of palace cli-mm"),
        "{}",
        text(&out)
    );
    let dd = data_dir(dst.path());
    assert!(!dd.join("other").exists() && !dd.join("cli-mm").exists());
}

/// Why (#9282 review, requirement 3): a daemon serving the root could load a
/// half-written palace, or hold a stale view of one; restore is offline only.
/// What: binds a listener on the root's daemon socket, then restores. The
/// restore must exit non-zero naming the daemon and write nothing.
/// Test: itself.
#[test]
fn cli_restore_refuses_while_a_daemon_serves_the_root() {
    let src = tempfile::tempdir().expect("src");
    let dst = tempfile::tempdir().expect("dst");
    let scratch = tempfile::tempdir().expect("scratch");
    seed_on_disk(src.path(), "cli-live");
    let snap = scratch.path().join("snap");
    cli_backup(src.path(), "cli-live", &snap);

    let dd = data_dir(dst.path());
    std::fs::create_dir_all(&dd).expect("mkdir data dir");
    let _listener = UnixListener::bind(dd.join("trusty-memory.sock")).expect("bind fake daemon");
    let out = run(
        dst.path(),
        &["palace", "restore", snap.to_str().expect("utf8")],
    );
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("daemon is serving"), "{}", text(&out));
    assert!(!dd.join("cli-live").exists(), "nothing may be written");
}

/// Why (#9282): with a daemon up, `palace backup` must go through it; if that
/// call fails it must fail, not fall back to opening the palace directly.
/// What: a fake daemon accepts and drops every connection. The backup exits
/// non-zero and leaves no backup directory.
/// Test: itself.
#[test]
fn cli_backup_with_a_failing_daemon_errors_without_falling_back() {
    let src = tempfile::tempdir().expect("src");
    let scratch = tempfile::tempdir().expect("scratch");
    seed_on_disk(src.path(), "cli-fd");
    let listener =
        UnixListener::bind(data_dir(src.path()).join("trusty-memory.sock")).expect("bind");
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            drop(conn);
        }
    });
    let snap = scratch.path().join("snap");
    let out = run(
        src.path(),
        &[
            "palace",
            "backup",
            "cli-fd",
            "--dest",
            snap.to_str().expect("utf8"),
        ],
    );
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("on the trusty-memory daemon"),
        "the error must be the daemon call's: {}",
        text(&out)
    );
    assert!(!snap.exists(), "no direct-open fallback: {}", text(&out));
}

/// Why (#9282, Opus critic HIGH): the offline path must refuse a backup into
/// the palace registry dir as the daemon does; here the legacy layout,
/// where the registry is `<data root>/palaces`.
/// What: with no daemon, `palace backup --dest <data root>/palaces/bk` exits
/// non-zero with the registry error, and nothing under the registry dir
/// changes: no `bk`, no `bk.tmp` staging dir, no other new entry.
/// Test: itself.
#[test]
fn cli_backup_refuses_a_dest_in_the_registry_dir_and_writes_nothing() {
    let src = tempfile::tempdir().expect("src");
    seed_on_disk(src.path(), "cli-reg");
    let dd = data_dir(src.path());
    let registry = dd.join("palaces");
    let dest = registry.join("bk");
    let before = tree(&registry);
    let out = run(
        src.path(),
        &[
            "palace",
            "backup",
            "cli-reg",
            "--dest",
            dest.to_str().expect("utf8"),
        ],
    );
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("palace registry directory"),
        "{}",
        text(&out)
    );
    assert_eq!(
        tree(&registry),
        before,
        "nothing may be written: {}",
        text(&out)
    );
    assert!(!dest.exists() && !registry.join("bk.tmp").exists());
}
