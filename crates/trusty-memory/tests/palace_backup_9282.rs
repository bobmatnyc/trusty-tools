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
use trusty_memory::engine::palace::{Drawer, Palace, PalaceId, RoomType};
use trusty_memory::engine::retrieval::{seed_shared_embedder_with_mock, PalaceHandle};
use trusty_memory::engine::store::chat_sessions::ChatSessionStore;
use trusty_memory::engine::store::kg_redb::{BatchWriteOp, KgStoreRedb};
use trusty_memory::engine::store::rooms::resolve_or_create_room_sync;
use trusty_memory::engine::store::PalaceStore;
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
    assert_eq!(
        dir.parent(),
        Some(flat_manual(fx.tmp.path()).join("bk-round").as_path()),
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

/// The flat-layout backup root of an `AppState` data root, canonical.
fn flat_manual(data_root: &Path) -> PathBuf {
    data_root
        .canonicalize()
        .expect("canonical data root")
        .join(".trusty-memory/backups/manual")
}

/// Why (#9282): every refusal must be an error, never a partial backup.
/// What: an absolute `dest`, a `dest` with a NUL byte, an existing `dest`
/// under the backup root, a non-string `dest` and a palace that does not
/// exist each fail, and none leaves a directory behind.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn palace_backup_error_arms_fail_and_write_nothing() {
    let fx = Fixture::new();
    fx.palace("bk-err");
    let root = fx.tmp.path();

    let abs = raw(
        &fx.state,
        "palace_backup",
        json!({"palace": "bk-err", "dest": root.join("abs/dir")}),
    )
    .await;
    let err = abs.expect_err("an absolute dest is refused");
    assert!(err.contains("absolute path"), "{err}");
    assert!(!root.join("abs").exists());

    let nul = raw(
        &fx.state,
        "palace_backup",
        json!({"palace": "bk-err", "dest": format!("a{}b", char::from(0u8))}),
    )
    .await;
    let err = nul.expect_err("a NUL byte is refused");
    assert!(err.contains("NUL byte"), "{err}");

    let typed = raw(
        &fx.state,
        "palace_backup",
        json!({"palace": "bk-err", "dest": 7}),
    )
    .await;
    assert!(typed
        .expect_err("a non-string dest is refused")
        .contains("dest"));

    let existing = flat_manual(root).join("already-here");
    std::fs::create_dir_all(&existing).expect("mkdir");
    let exists = raw(
        &fx.state,
        "palace_backup",
        json!({"palace": "bk-err", "dest": "already-here"}),
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
    assert!(!flat_manual(root).join("no-such-palace").exists());
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

/// Why (#9282, option C ruling A): a `dest` is a name under the backup root;
/// any absolute path is refused before a write, so a backup can never land
/// in the registry dir (listed as a palace) or create `<root>/palaces`.
/// What: absolute `<registry>/bk`, the same through a symlink, `<root>/palaces/bk`,
/// `<root>/palaceſ/bk`, and a path inside the backup root. Each is refused
/// as an absolute name, and the data root and the link's dir are unchanged.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn palace_backup_refuses_a_dest_in_the_registry_dir() {
    let fx = Fixture::new();
    fx.palace("bk-reg");
    let registry = fx.tmp.path().to_path_buf();
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let link = elsewhere.path().join("registry-link");
    std::os::unix::fs::symlink(&registry, &link).expect("symlink");
    let (before, before_else) = (tree(&registry), tree(elsewhere.path()));
    for dest in [
        registry.join("bk"),
        link.join("bk-via-link"),
        registry.join("palaces").join("bk"),
        registry.join("palace\u{17f}").join("bk"),
        flat_manual(&registry).join("inside"),
    ] {
        let res = raw(
            &fx.state,
            "palace_backup",
            json!({"palace": "bk-reg", "dest": dest}),
        )
        .await;
        let err = res.expect_err("an absolute dest must be refused");
        assert!(err.contains("absolute path"), "{}: {err}", dest.display());
        assert_eq!(tree(&registry), before, "{} wrote", dest.display());
        assert_eq!(
            tree(elsewhere.path()),
            before_else,
            "{} wrote",
            dest.display()
        );
    }
    assert!(!registry.join("palaces").exists(), "palaces/ was created");
}

/// Why (#9282, option C): a symlinked segment below the backup root would
/// carry the backup anywhere the link points.
/// What: `<backup root>/out -> elsewhere`, `dest` = `out/bk`. Refused as an
/// unsafe component; `elsewhere` stays empty and the data root unchanged.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn palace_backup_refuses_a_symlinked_dest_component() {
    let fx = Fixture::new();
    fx.palace("bk-link");
    let root = fx.tmp.path().to_path_buf();
    let manual = flat_manual(&root);
    std::fs::create_dir_all(&manual).expect("root");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    std::os::unix::fs::symlink(elsewhere.path(), manual.join("out")).expect("symlink");
    // The tool opens the daemon's chat store before the snapshot checks the
    // name's components; any chat call creates that palace file, so open it
    // first and measure only what the backup writes.
    fx.tool("chat_session_list", json!({"palace": "bk-link"}))
        .await
        .expect("chat_session_list");
    let before = tree(&root);
    let res = raw(
        &fx.state,
        "palace_backup",
        json!({"palace": "bk-link", "dest": "out/bk"}),
    )
    .await;
    let err = res.expect_err("a symlinked component must be refused");
    assert!(err.contains("is unsafe"), "{err}");
    assert_eq!(tree(&root), before);
    assert_eq!(std::fs::read_dir(elsewhere.path()).expect("ls").count(), 0);
}

/// Why (#9282, `palaceſ` hole): on APFS `palaceſ` case-folds to `palaces`.
/// Under the backup root that is harmless: no `<root>/palaces` appears, so
/// the next start keeps the same registry and lists the same palaces.
/// What: relative `palaceſ/bk` and `team/weekly/bk` through the tool land at
/// `<backup root>/<name>`; `palace_registry_dir_from` still picks the root.
/// Prints whether this filesystem folded the name.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn palace_backup_long_s_and_nested_names_land_under_the_backup_root() {
    let fx = Fixture::new();
    fx.palace("bk-long-s");
    let root = fx.tmp.path().to_path_buf();
    let manual = flat_manual(&root);
    for name in ["palace\u{17f}/bk", "team/weekly/bk"] {
        let out = raw(
            &fx.state,
            "palace_backup",
            json!({"palace": "bk-long-s", "dest": name}),
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        let dir = PathBuf::from(out["dir"].as_str().expect("dir"));
        assert_eq!(dir, manual.join(name), "{name}");
        assert!(dir.join("manifest.json").is_file(), "{name}");
    }
    assert!(!root.join("palaces").exists(), "<root>/palaces was created");
    assert_eq!(
        trusty_common::palace_alias::palace_registry_dir_from(root.clone()),
        root,
        "the next start would switch the registry"
    );
    eprintln!(
        "fold probe: palace\u{17f} aliases palaces on this filesystem: {}",
        manual.join("palaces").exists()
    );
}

/// The missing first components of the `..` rows (#9282, round-3 delta
/// critic CRITICAL): `palaces/../l/bk` and its case variant.
const DOTDOT_ROWS: [&str; 2] = ["palaces", "Palaces"];

/// The `..` destinations for one row: relative, then absolute under `root`.
fn dotdot_dests(root: &Path, missing: &str) -> [PathBuf; 2] {
    let rel = Path::new(missing).join("..").join("l").join("bk");
    [rel.clone(), root.join(rel)]
}

/// What one `..` row got wrong, empty when it was refused and wrote nothing.
/// `outcome` is the call's error text, or `Ok` when the backup went through.
fn dotdot_row_problems(
    root: &Path,
    before: &[PathBuf],
    dest: &Path,
    outcome: Result<String, String>,
) -> Vec<String> {
    let mut out = Vec::new();
    let want = if dest.is_absolute() {
        "absolute path"
    } else {
        "'..' segment"
    };
    match outcome {
        Ok(ok) => out.push(format!("{}: not refused: {ok}", dest.display())),
        Err(e) if !e.contains(want) => {
            out.push(format!("{}: wrong error: {e}", dest.display()));
        }
        Err(_) => {}
    }
    if tree(root) != before {
        out.push(format!("{} wrote something", dest.display()));
    }
    for made in ["palaces", "Palaces", "l"] {
        if root.join(made).exists() {
            out.push(format!("{}: {made} was created", dest.display()));
        }
    }
    out
}

/// Why (#9282, round-3 delta critic CRITICAL): `palaces/../l/bk` once passed
/// the guard as `l/bk`, and the create then made `<root>/palaces`: in the
/// flat layout the next start switches the registry to it.
/// What: each row, relative and absolute, in a fresh flat data root, through
/// the tool. Each is refused as a name, and the data root is unchanged: no
/// `palaces`, `Palaces`, `l`. Every row is checked before the assert.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn palace_backup_refuses_dotdot_after_a_missing_component() {
    let mut problems = Vec::new();
    for missing in DOTDOT_ROWS {
        let fx = Fixture::new();
        fx.palace("bk-dotdot");
        let root = fx.tmp.path().to_path_buf();
        let before = tree(&root);
        for dest in dotdot_dests(&root, missing) {
            let res = raw(
                &fx.state,
                "palace_backup",
                json!({"palace": "bk-dotdot", "dest": dest}),
            )
            .await;
            let outcome = res.map(|v| v.to_string());
            problems.extend(dotdot_row_problems(&root, &before, &dest, outcome));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

// --------------------------------------------------------------------- CLI

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trusty-memory"))
}

/// Run the binary with `override_dir` as `TRUSTY_DATA_DIR_OVERRIDE`, from
/// that dir, so nothing a test passes can land in the crate directory.
fn run(override_dir: &Path, args: &[&str]) -> Output {
    run_in(override_dir, override_dir, args)
}

/// [`run`] from the working directory `cwd`.
fn run_in(override_dir: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
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

/// The directory a successful `palace backup` reports ("written to <dir>").
fn written_dir(out: &Output) -> PathBuf {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().next().unwrap_or_default();
    let (_, dir) = line
        .split_once(" written to ")
        .unwrap_or_else(|| panic!("no backup dir in: {}", text(out)));
    PathBuf::from(dir)
}

/// The legacy-layout backup root of a data dir, canonical.
fn legacy_manual(data_dir: &Path) -> PathBuf {
    data_dir
        .canonicalize()
        .expect("canonical")
        .join("backups/manual")
}

/// Back up `id` from `src` through the binary to the default directory,
/// and return that directory.
fn cli_backup(src: &Path, id: &str) -> PathBuf {
    let out = run(src, &["palace", "backup", id]);
    assert!(out.status.success(), "backup: {}", text(&out));
    let dir = written_dir(&out);
    assert_eq!(
        dir.parent(),
        Some(legacy_manual(&data_dir(src)).join(id).as_path()),
        "default dir: {}",
        text(&out)
    );
    assert!(dir.join("manifest.json").is_file(), "{}", text(&out));
    dir
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
    let (drawers, rooms) = seed_on_disk(src.path(), "cli-rt");
    let snap = cli_backup(src.path(), "cli-rt");

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
    seed_on_disk(src.path(), "cli-mm");
    let snap = cli_backup(src.path(), "cli-mm");

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
    seed_on_disk(src.path(), "cli-live");
    let snap = cli_backup(src.path(), "cli-live");

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
    seed_on_disk(src.path(), "cli-fd");
    let listener =
        UnixListener::bind(data_dir(src.path()).join("trusty-memory.sock")).expect("bind");
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            drop(conn);
        }
    });
    let snap = legacy_manual(&data_dir(src.path())).join("snap");
    let out = run(
        src.path(),
        &["palace", "backup", "cli-fd", "--dest", "snap"],
    );
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("on the trusty-memory daemon"),
        "the error must be the daemon call's: {}",
        text(&out)
    );
    assert!(!snap.exists(), "no direct-open fallback: {}", text(&out));
}

/// Seed a flat-layout palace at `<override>/trusty-memory/<id>` (no
/// `palaces/` dir, so the data dir is the registry) and close its handle.
fn seed_flat_on_disk(override_dir: &Path, id: &str) {
    seed_shared_embedder_with_mock();
    let palace = Palace {
        id: PalaceId::new(id),
        name: id.into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: data_dir(override_dir).join(id),
    };
    PalaceStore::save_palace(&palace).expect("palace.json");
    drop(PalaceHandle::open(&palace).expect("open palace"));
}

/// `palace backup <id> --dest <dest>` with no daemon, from `cwd`.
fn cli_backup_dest(src: &Path, cwd: &Path, id: &str, dest: &Path) -> Output {
    let dest = dest.to_str().expect("utf8");
    run_in(src, cwd, &["palace", "backup", id, "--dest", dest])
}

/// Why (#9282, option C rulings A and F): the CLI sends the name as given;
/// an absolute path or a symlinked component is refused with exit 1 before
/// anything is written, on the direct path as on the daemon's.
/// What: a flat-layout data dir, no daemon. Rows: absolute `<root>/bk` (the
/// registry dir), `<root>/palaceſ/bk`, a path inside the backup root, and
/// relative `out/bk` with `<backup root>/out -> elsewhere`. Each exits 1, and
/// the data dir and `elsewhere` are unchanged.
/// Test: itself.
#[test]
fn cli_backup_refuses_a_dest_in_the_registry_dir_and_writes_nothing() {
    let src = tempfile::tempdir().expect("src");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    seed_flat_on_disk(src.path(), "cli-reg");
    let root = data_dir(src.path());
    let manual = flat_manual(&root);
    std::fs::create_dir_all(&manual).expect("backup root");
    std::os::unix::fs::symlink(elsewhere.path(), manual.join("out")).expect("symlink");
    let (before, before_else) = (tree(&root), tree(elsewhere.path()));
    for (dest, want) in [
        (root.join("bk"), "absolute path"),
        (root.join("palace\u{17f}").join("bk"), "absolute path"),
        (manual.join("inside"), "absolute path"),
        (PathBuf::from("out/bk"), "is unsafe"),
    ] {
        let out = cli_backup_dest(src.path(), src.path(), "cli-reg", &dest);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}: {}",
            dest.display(),
            text(&out)
        );
        assert!(
            text(&out).contains(want),
            "{}: {}",
            dest.display(),
            text(&out)
        );
        assert_eq!(
            tree(&root),
            before,
            "{} wrote: {}",
            dest.display(),
            text(&out)
        );
        assert_eq!(tree(elsewhere.path()), before_else, "{}", dest.display());
    }
    assert!(!root.join("palaces").exists());
}

/// Why (#9282, round-3 delta critic CRITICAL): the CLI's direct path runs
/// the same name rules; `palaces/../l/bk` must not create `<root>/palaces`.
/// What: each row, relative and absolute, in a fresh flat-layout data dir,
/// no daemon. Each exits 1 naming the refused segment or the absolute path,
/// and the data dir is unchanged: no `palaces`, `Palaces`, `l`. Every row is
/// checked before the assert.
/// Test: itself.
#[test]
fn cli_backup_refuses_dotdot_after_a_missing_component_and_writes_nothing() {
    let mut problems = Vec::new();
    for missing in DOTDOT_ROWS {
        let src = tempfile::tempdir().expect("src");
        seed_flat_on_disk(src.path(), "cli-dotdot");
        let root = data_dir(src.path());
        let before = tree(&root);
        for dest in dotdot_dests(&root, missing) {
            let out = cli_backup_dest(src.path(), src.path(), "cli-dotdot", &dest);
            let outcome = match out.status.code() {
                Some(0) => Ok(text(&out)),
                Some(1) => Err(text(&out)),
                _ => Ok(format!("exit code is not 1: {}", text(&out))),
            };
            problems.extend(dotdot_row_problems(&root, &before, &dest, outcome));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// Why (#9282, option C): `--dest` names a directory under the backup root,
/// not under the caller's cwd; `palaceſ/bk` must not alias `<root>/palaces`.
/// What: a flat-layout data dir, no daemon, cwd = an unrelated dir. `--dest
/// bk` and `--dest palaceſ/bk` land at `<backup root>/<name>`; the cwd stays
/// empty, no `<root>/palaces` appears, and the registry is still the root.
/// Test: itself.
#[test]
fn cli_backup_long_s_and_cwd_names_land_under_the_backup_root() {
    let src = tempfile::tempdir().expect("src");
    let cwd = tempfile::tempdir().expect("cwd");
    seed_flat_on_disk(src.path(), "cli-name");
    let root = data_dir(src.path());
    for name in ["bk", "palace\u{17f}/bk"] {
        let out = cli_backup_dest(src.path(), cwd.path(), "cli-name", Path::new(name));
        assert!(out.status.success(), "{name}: {}", text(&out));
        assert_eq!(written_dir(&out), flat_manual(&root).join(name), "{name}");
    }
    assert_eq!(std::fs::read_dir(cwd.path()).expect("ls").count(), 0);
    assert!(!root.join("palaces").exists(), "<root>/palaces was created");
    assert_eq!(
        trusty_common::palace_alias::palace_registry_dir_from(root.clone()),
        root
    );
}
