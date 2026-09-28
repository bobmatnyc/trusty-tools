//! `#[serial_test::file_serial]` compiles for a crate on the workspace
//! `serial_test` dependency row (#7542).
//!
//! Why: the root `CLAUDE.md` prescribes `#[serial_test::file_serial]` for a
//! resource shared ACROSS processes, because `cargo nextest` — which the
//! eight-shard pre-publish gate runs — gives every test its own process and an
//! in-process `#[serial]` lock therefore serializes nothing (#4162). PR #6335
//! wrote that guidance without enabling `serial_test`'s `file_locks` feature on
//! the root `[workspace.dependencies]` row, so the prescribed attribute failed
//! to resolve in every crate that takes `serial_test` from the workspace — the
//! documented pattern could not be followed by the crates it was written for.
//! Nothing else in the workspace uses the attribute off the shared row, so
//! without this file the row can silently lose the feature again.
//!
//! What: one `#[file_serial]` test that drives the resource class the attribute
//! exists for — a FIXED path, the same one in every process — so the test is a
//! real use of the lock rather than a compile-only token. Its assertion is that
//! it observed its own write at that path; a concurrently running copy in
//! another process cannot interleave, which is the property `file_serial`
//! provides and `serial` does not.
//!
//! Test: this file.

use std::path::PathBuf;

/// The one fixed, cross-process path this test contends on.
///
/// A per-test `TempDir` would remove the very contention `file_serial` is here
/// to serialize, so the path is deliberately stable across processes and runs.
fn probe_path() -> PathBuf {
    std::env::temp_dir().join("tm-7542-file-serial-probe.txt")
}

/// `#[serial_test::file_serial]` resolves, and holds the lock across processes
/// (#7542).
///
/// Why: this is the regression pin for the workspace `serial_test` row carrying
/// `features = ["file_locks"]`. RED before that row changed:
/// `error[E0433]: failed to resolve: could not find 'file_serial' in
/// 'serial_test'` — "the item is gated behind the `file_locks` feature".
/// What: writes a token this process owns to the fixed probe path, reads it
/// back, and asserts it survived — which it can only do while no other process
/// running this test holds the same path.
/// Test: this is the test.
#[test]
#[serial_test::file_serial]
fn file_serial_compiles_and_guards_a_cross_process_path() {
    let path = probe_path();
    let token = format!("{}", std::process::id());
    std::fs::write(&path, &token).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    let read_back =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert_eq!(
        read_back,
        token,
        "#7542: another process interleaved on {} — `file_serial` did not hold \
         the cross-process lock",
        path.display()
    );
    let _ = std::fs::remove_file(&path);
}
