//! End-to-end test that the `localLLM → User Memories` migration runs at
//! daemon boot (issue #124).
//!
//! Why: PR #102 added an idempotent on-disk migration that rewrites the
//! default palace's display `name` from the legacy literal `"localLLM"` to
//! `"User Memories"`. The migration helper itself (in `commands::migrations`)
//! is unit-tested directly, but only the *wiring* — calling it from
//! `run_serve` in `main.rs` — guarantees that every boot actually applies it
//! on a real install. PR #103's rebase dropped both `pub mod migrations;` and
//! the boot-time call, so the helper became dead code. This test spawns the
//! real binary, lets it boot, and asserts the on-disk `palace.json` was
//! migrated.
//!
//! What: spawn `trusty-memory serve --foreground --http 127.0.0.1:0` against
//! a tempdir-rooted data directory that already contains a legacy `localLLM`
//! palace (`name = "localLLM"`). Give the process enough time to run the
//! migration and complete the initial `load_palaces_from_disk` step, then
//! kill it and assert `palace.json` now reads `name = "User Memories"`.
//! Re-running the test path is the idempotency guarantee — re-invoking the
//! binary against an already-migrated palace must not corrupt or rewrite
//! it. `--foreground` is required because plain `serve` self-spawns and
//! exits 0 (see `commands::start`), which would race the kill below.
//! `--http 127.0.0.1:0` picks an OS-assigned port so concurrent test runs
//! cannot collide on the historic default 7070. Issue #150 removed the
//! cheaper `serve --stdio` boot path that this test originally used.
//!
//! Test: `cargo test -p trusty-memory --test migration_boot`. Requires Cargo
//! to have built the binary via `CARGO_BIN_EXE_trusty-memory`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// How long to wait for the migration to flush before killing the daemon.
///
/// Why: the migration runs synchronously inside `run_serve` before the
/// palace registry hydrates, and `load_palaces_from_disk` does I/O per
/// palace. 2 seconds is comfortably more than enough on developer hardware
/// and well below the test-runner's per-test timeout.
const BOOT_WAIT: Duration = Duration::from_millis(2000);

/// Persist a legacy-name palace to disk in the shape `PalaceStore::save_palace`
/// would produce.
///
/// Why: we cannot reach `PalaceStore::save_palace` from a `tests/` file
/// (it lives in `trusty-common`'s `memory_core` module), but the file
/// schema is stable JSON. Hand-rolling it keeps this test self-contained
/// and avoids pulling in the storage layer.
/// What: writes a minimal `palace.json` with the legacy literal as both the
/// id and the display name.
fn seed_legacy_palace(registry_root: &Path) -> PathBuf {
    let palace_dir = registry_root.join("localLLM");
    std::fs::create_dir_all(&palace_dir).expect("create palace dir");
    let palace_json = palace_dir.join("palace.json");
    // The on-disk shape matches `Palace` in `trusty-common`. Fields:
    // id, name, description, created_at (RFC3339), data_dir.
    let body = serde_json::json!({
        "id": "localLLM",
        "name": "localLLM",
        "description": null,
        "created_at": "2025-01-01T00:00:00Z",
        "data_dir": palace_dir,
    });
    let mut f = std::fs::File::create(&palace_json).expect("create palace.json");
    f.write_all(serde_json::to_string_pretty(&body).unwrap().as_bytes())
        .expect("write palace.json");
    palace_json
}

/// Read `palace.json` and return its `name` field.
fn read_palace_name(palace_json: &Path) -> String {
    let raw = std::fs::read_to_string(palace_json).expect("read palace.json");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse palace.json");
    parsed["name"].as_str().expect("name field").to_string()
}

/// Locate the binary built by Cargo for this crate's harness.
fn locate_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trusty-memory"))
}

/// Spawn `trusty-memory serve --foreground` against `data_dir`, sleep just
/// long enough for the migration to run, then kill the child. Returns once
/// the child is reaped.
///
/// Why: issue #150 removed the `serve --stdio` flag (the previous cheapest
/// boot path) because it deadlocked on redb's exclusive write lock whenever
/// a long-lived daemon was already running. `serve --foreground` is now the
/// canonical "supervisor-friendly" mode — it does not self-spawn, so the
/// child PID we kill is the actual daemon. We bind `--http 127.0.0.1:0`
/// so the OS assigns a free port: concurrent test runs (and any locally
/// running daemon on the historic 7070) never collide.
/// What: spawns the binary with `--foreground --http 127.0.0.1:0`, pipes
/// every stdio to dev-null equivalents, waits BOOT_WAIT, then sends SIGKILL
/// via `Child::kill`. Reaps via `wait`.
fn boot_briefly(data_dir: &Path) {
    let bin = locate_binary();
    let mut child = Command::new(&bin)
        .arg("serve")
        .arg("--foreground")
        .arg("--http")
        .arg("127.0.0.1:0")
        .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir)
        // Quiet the daemon — we don't read its output here.
        .env("RUST_LOG", "warn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn trusty-memory binary");
    std::thread::sleep(BOOT_WAIT);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn boot_migrates_default_palace_name_and_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // `TRUSTY_DATA_DIR_OVERRIDE` is the *base*; `resolve_data_dir` appends
    // the app name. The daemon's data root is therefore
    // `<override>/trusty-memory/`, and the migration looks for
    // `<root>/localLLM/palace.json`.
    let override_base = tmp.path();
    let data_dir = override_base.join("trusty-memory");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    // The daemon descends into <data_dir>/palaces/ when that subdir exists
    // (`resolve_palace_registry_dir`). We exercise the flat layout — no
    // `palaces/` subdir — to keep the test focused on the migration itself.
    let palace_json = seed_legacy_palace(&data_dir);
    assert_eq!(read_palace_name(&palace_json), "localLLM", "seed legacy");

    // First boot: the migration must rewrite `name`.
    boot_briefly(override_base);
    assert_eq!(
        read_palace_name(&palace_json),
        "User Memories",
        "first boot must migrate the display name"
    );

    // Second boot: idempotency — no rewrite, no corruption.
    let before = std::fs::read_to_string(&palace_json).expect("read palace.json #2");
    boot_briefly(override_base);
    let after = std::fs::read_to_string(&palace_json).expect("read palace.json #3");
    assert_eq!(read_palace_name(&palace_json), "User Memories");
    assert_eq!(
        before, after,
        "idempotent: re-running boot must not change palace.json"
    );
}
