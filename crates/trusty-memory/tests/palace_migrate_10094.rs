//! #10094: the `palace_migrate` tool, the `palace migrate` CLI, and a format-0
//! fixture dumper for the live check.
//!
//! Why: the tool and the CLI are the two public routes into one migration;
//! only the dispatcher and the built binary can show both answer. The live
//! check needs a real format-0 palace on disk, written from the same fixture
//! the unit tests use.
//! What: the tool test drives `transport::dispatch` by raw method name, as
//! the CLI's daemon route does. The CLI test runs the binary with
//! `TRUSTY_DATA_DIR_OVERRIDE` (no daemon there) through `--dry-run`, migrate
//! and `--verify`. The ignored dumper writes a format-0 palace.
//! Test: this *is* the test file.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};
use trusty_memory::AppState;
use trusty_memory::engine::store::format_v0_fixture::write_format_0_palace;
use trusty_memory::transport::{JsonRpcRequest, dispatch};

/// Raw-method call, the path `trusty-memory palace migrate` takes.
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

/// Why (#10094): the CLI's daemon route calls `palace_migrate` by raw name; a
/// tool missing from the raw forwarding list answers -32601.
/// What: a format-0 palace under the daemon's palace root; a dry run, a
/// verify (a gap: not yet migrated), a migrate and a verify again, each by
/// raw name; a bad `mode` is refused before anything opens.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn palace_migrate_tool_dispatches_and_rejects_a_bad_mode() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf());
    state.set_ready();
    write_format_0_palace(&tmp.path().join("tool-v0"), "tool-v0").expect("fixture");

    let dry = raw(
        &state,
        "palace_migrate",
        json!({"palace": "tool-v0", "mode": "dry_run"}),
    )
    .await
    .expect("dry run");
    assert_eq!(dry["plans"][0]["found"], 0, "{dry}");
    assert_eq!(dry["plans"][0]["status"], "needs_migration", "{dry}");

    let before = raw(
        &state,
        "palace_migrate",
        json!({"palace": "tool-v0", "mode": "verify"}),
    )
    .await
    .expect("verify");
    assert_eq!(before["ok"], false, "{before}");

    let done = raw(&state, "palace_migrate", json!({"palace": "tool-v0"}))
        .await
        .expect("migrate");
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["results"][0]["migrated"], true, "{done}");
    assert!(done["results"][0]["backup"].is_string(), "{done}");

    let after = raw(
        &state,
        "palace_migrate",
        json!({"palace": "tool-v0", "mode": "verify"}),
    )
    .await
    .expect("verify");
    assert_eq!(after["ok"], true, "{after}");

    let bad = raw(
        &state,
        "palace_migrate",
        json!({"palace": "tool-v0", "mode": "bogus"}),
    )
    .await
    .expect_err("a bad mode is refused");
    assert!(bad.contains("'mode' must be"), "{bad}");
}

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trusty-memory"))
}

/// Run the binary with `override_dir` as `TRUSTY_DATA_DIR_OVERRIDE`.
fn run(override_dir: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(override_dir)
        .env("TRUSTY_DATA_DIR_OVERRIDE", override_dir)
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

/// Write a format-0 palace `id` at `<override>/trusty-memory/palaces/<id>`.
fn dump_format_0(override_dir: &Path, id: &str) -> PathBuf {
    let dir = override_dir.join("trusty-memory").join("palaces").join(id);
    assert!(!dir.exists(), "refusing to overwrite {}", dir.display());
    write_format_0_palace(&dir, id).expect("write format-0 palace");
    dir
}

/// Why (#10094): the offline route — no daemon serves the data root — must
/// preview, migrate and verify through the binary a user runs.
/// What: `--verify` before (non-zero, names the gap), `--dry-run` (writes no
/// backup), migrate (prints the backup), `--verify` after (zero).
/// Test: itself.
#[test]
fn cli_migrate_dry_run_then_migrate_then_verify_a_format_0_palace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let o = tmp.path();
    dump_format_0(o, "cli-v0");
    let backups = o.join("trusty-memory/backups/format-migration/cli-v0");

    let out = run(o, &["palace", "migrate", "cli-v0", "--verify"]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("GAP: format status is needs migration"),
        "{}",
        text(&out)
    );

    let out = run(o, &["palace", "migrate", "cli-v0", "--dry-run"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("0-to-2-<UTC timestamp>"),
        "{}",
        text(&out)
    );
    assert!(!backups.exists(), "a dry run takes no backup");

    let out = run(o, &["palace", "migrate", "cli-v0"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("palace cli-v0: migrated format 0 -> 2"),
        "{}",
        text(&out)
    );
    assert!(
        text(&out).contains(&backups.display().to_string()),
        "{}",
        text(&out)
    );

    let out = run(o, &["palace", "migrate", "cli-v0", "--verify"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("OK: every check passed"),
        "{}",
        text(&out)
    );
}

/// The env var naming the directory the dumper writes into.
const DUMP_DIR_ENV: &str = "TRUSTY_MEMORY_FORMAT0_DUMP_DIR";

/// Write a format-0 palace for the live check on a copy (#10094).
///
/// Why: `palace migrate` is live-checked on a real format-0 palace, never a
/// user's; this writes one from the same fixture the unit tests use.
/// What: with `TRUSTY_MEMORY_FORMAT0_DUMP_DIR=<dir>` set, writes palace
/// `format0-fixture` at `<dir>/trusty-memory/palaces/format0-fixture`, so
/// `<dir>` is a `TRUSTY_DATA_DIR_OVERRIDE`; it refuses to overwrite. Unset,
/// it writes into a temp dir. Either way it checks the palace is format 0: no
/// `format_version` in `palace.json` and no `kg_schema` table in `kg.redb`.
/// Run it with:
/// `TRUSTY_MEMORY_FORMAT0_DUMP_DIR=/tmp/tm-fx cargo test -p trusty-memory --test palace_migrate_10094 write_format_0_fixture_palace_for_the_live_check -- --ignored --nocapture`,
/// copy `/tmp/tm-fx` aside, then
/// `TRUSTY_DATA_DIR_OVERRIDE=/tmp/tm-fx trusty-memory palace migrate format0-fixture --dry-run`.
/// Test: itself.
#[test]
#[ignore = "fixture dumper for the #10094 live check; writes into $TRUSTY_MEMORY_FORMAT0_DUMP_DIR"]
fn write_format_0_fixture_palace_for_the_live_check() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root =
        std::env::var_os(DUMP_DIR_ENV).map_or_else(|| tmp.path().to_path_buf(), PathBuf::from);
    let dir = dump_format_0(&root, "format0-fixture");
    println!("wrote format-0 palace at {}", dir.display());
    println!("TRUSTY_DATA_DIR_OVERRIDE={}", root.display());

    let meta: Value =
        serde_json::from_slice(&std::fs::read(dir.join("palace.json")).expect("palace.json"))
            .expect("json");
    assert!(meta.get("format_version").is_none(), "{meta}");
    let db = trusty_common::redb_cache::open_palace_db_read_only(&dir.join("kg.redb"))
        .expect("read-only kg.redb");
    let rtx = redb::ReadableDatabase::begin_read(&db).expect("read txn");
    let schema: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new("kg_schema");
    assert!(
        matches!(
            rtx.open_table(schema),
            Err(redb::TableError::TableDoesNotExist(_))
        ),
        "a format-0 kg.redb has no kg_schema table"
    );
}
