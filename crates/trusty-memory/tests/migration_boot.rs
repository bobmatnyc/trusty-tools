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
//! What: spawn `trusty-memory serve --foreground` against a tempdir-rooted
//! data directory that already contains a legacy `localLLM` palace
//! (`name = "localLLM"`). Wait until the daemon logs that startup palace
//! hydration finished, kill it, and assert `palace.json` now reads
//! `name = "User Memories"`. Re-running the boot is the idempotency
//! guarantee — a second full boot must not rewrite `palace.json`.
//! `--foreground` is required because plain `serve` self-spawns and exits 0
//! (see `commands::start`), which would race the kill below. #11945: both
//! boots poll that log line with a deadline instead of a fixed sleep.
//!
//! Test: `cargo test -p trusty-memory --test migration_boot`. Requires Cargo
//! to have built the binary via `CARGO_BIN_EXE_trusty-memory`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Upper bound on how long one boot may take to reach its ready condition.
///
/// Why: #11945 — a fixed 2 s sleep lost the race under `--workspace` gate
/// load. The test now polls an observable condition; this is only the
/// give-up bound, so it can be generous without slowing the normal run.
const BOOT_DEADLINE: Duration = Duration::from_secs(60);

/// Interval between two checks of the ready condition.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

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

/// The daemon's log line once startup palace hydration has finished.
///
/// Why: #11945 — hydration (a background task, `startup_tasks.rs`) opens the
/// palace with writer intent and rewrites the `palace.json` format mirror, so
/// `palace.json` is only settled after this line. A boot killed earlier leaves
/// that rewrite to the next boot, which breaks the idempotency assertion.
const HYDRATION_DONE: &str = "background palace hydration complete";

/// Spawn `trusty-memory serve --foreground` against `data_dir`, poll its
/// stderr until startup hydration has finished, then kill the child.
///
/// Why: `serve --foreground` is the canonical "supervisor-friendly" mode —
/// it does not self-spawn, so the child PID we kill is the actual daemon.
/// #11945: waiting on hydration replaces a fixed 2 s sleep that lost the
/// race under load.
/// What: sends stderr to `log` at `info` for this crate, polls it for
/// `HYDRATION_DONE` every `POLL_INTERVAL`, then SIGKILLs and reaps the
/// child. Panics with the log when the daemon exits first or
/// `BOOT_DEADLINE` passes.
fn boot_until_hydrated(data_dir: &Path, log: &Path) {
    let bin = locate_binary();
    let stderr = std::fs::File::create(log).expect("create daemon stderr log");
    // #7085: the kill below is skipped entirely if this test binary is itself
    // SIGKILLed — the stamp makes the daemon watch us and self-exit instead.
    let mut child = trusty_common::parent_death::exit_with_parent(
        Command::new(&bin)
            .arg("serve")
            .arg("--foreground")
            .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir)
            // #11945: `info` for this crate only, so the hydration line shows.
            .env("RUST_LOG", "warn,trusty_memory=info")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr)),
    )
    .spawn()
    .expect("spawn trusty-memory binary");
    let hydrated = || {
        std::fs::read_to_string(log)
            .map(|s| s.contains(HYDRATION_DONE))
            .unwrap_or(false)
    };
    // #11945: poll the condition the assertions need, bounded by a deadline.
    let start = Instant::now();
    let failure = loop {
        if hydrated() {
            break None;
        }
        if let Ok(Some(status)) = child.try_wait() {
            // The line may have landed between the check and the exit.
            if hydrated() {
                break None;
            }
            break Some(format!("daemon exited ({status}) before logging"));
        }
        if start.elapsed() >= BOOT_DEADLINE {
            break Some(format!("timed out after {BOOT_DEADLINE:?} waiting for"));
        }
        std::thread::sleep(POLL_INTERVAL);
    };
    let _ = child.kill();
    let _ = child.wait();
    if let Some(msg) = failure {
        panic!(
            "{msg} {HYDRATION_DONE:?}; daemon stderr:\n{}",
            std::fs::read_to_string(log).unwrap_or_default()
        );
    }
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
    // The logs sit outside the data root so the daemon never sees them.
    boot_until_hydrated(override_base, &override_base.join("boot-1.stderr"));
    assert_eq!(
        read_palace_name(&palace_json),
        "User Memories",
        "first boot must migrate the display name"
    );

    // Second boot: idempotency — no rewrite, no corruption.
    let before = std::fs::read_to_string(&palace_json).expect("read palace.json #2");
    boot_until_hydrated(override_base, &override_base.join("boot-2.stderr"));
    let after = std::fs::read_to_string(&palace_json).expect("read palace.json #3");
    assert_eq!(read_palace_name(&palace_json), "User Memories");
    assert_eq!(
        before, after,
        "idempotent: re-running boot must not change palace.json"
    );
}
