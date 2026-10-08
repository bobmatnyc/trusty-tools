//! trusty-search's shared integration target (#9302).
//!
//! Why: every file under `tests/` used to link into its own test binary, and
//! most of them statically link ONNX Runtime at 50-130 MB each. On macOS each
//! new executable waits for a serialized XProtect scan on its first exec, so
//! every extra binary is one more scan per build.
//! What: each module below is one former test binary, unchanged apart from
//! reaching the shared helpers as `crate::test_daemon`, `crate::socket_daemon`
//! and `crate::real_allowlist_guard`. libtest runs them in parallel in one
//! process, so a module belongs here only if it leaves process-global state
//! alone. Files that set environment variables or memory limits, re-exec
//! themselves by bare test name, or are benchmark, profile or doc-generation
//! targets named elsewhere keep a `[[test]]` target of their own.
//! Test: `every_tests_file_is_mounted_9302` fails if a file under `tests/` is
//! mounted by no target. Run one former binary with a module filter:
//! `cargo test -p trusty-search --test integration typeahead::`.

// #9450: seeded clustered vectors shared by the HNSW recall tests.
#[path = "../src/core/store/clustered_vectors.rs"]
mod clustered_vectors;
#[path = "support/real_allowlist_guard.rs"]
mod real_allowlist_guard;
#[path = "support/socket_daemon.rs"]
mod socket_daemon;
#[path = "support/test_daemon.rs"]
mod test_daemon;

mod bundled_install;
mod config_mount;
mod corpus_corruption_quarantine_4227;
mod corpus_open_quarantine_4122;
mod daemon_env_precedence;
mod data_dir_forward;
// #9450: compaction cost at 150K (ignored measurement).
mod hnsw_compact_9450;
mod hnsw_recall_9414;
mod index_remove_env_conflict_8175;
mod integration_tests;
mod mcp_reindex_quarantine_8105;
mod mcp_stdio_e2e_5264;
mod mcp_structured_503_5350;
mod migration_e2e;
mod no_auto_discover_env;
mod port_dashboard_no_http_9214;
mod registry_isolation;
mod reindex_quantize_env_conflict_8737;
mod residency_cold_park;
mod src_bin_coverage_7694;
mod typeahead;
mod warm_boot_corpus_open_failure;
mod watcher_chunk_cap_orphans_100;

/// Why: `autotests = false` (#9302) means a new `tests/*.rs` file compiles
/// into no binary until something mounts it, so its tests would never run.
/// What: names every top-level `tests/*.rs` file that is neither a `mod` line
/// here nor a `[[test]]` `path` in `Cargo.toml`.
/// Test: this is the test.
#[test]
fn every_tests_file_is_mounted_9302() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("read Cargo.toml");
    let this = std::fs::read_to_string(root.join("tests/integration.rs")).expect("read root");
    let mut orphans = Vec::new();
    for entry in std::fs::read_dir(root.join("tests")).expect("read tests/") {
        let path = entry.expect("tests/ entry").path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("utf-8 name");
        let mounted = this
            .lines()
            .any(|line| line.trim() == format!("mod {stem};"));
        let own_target = manifest.contains(&format!("path = \"tests/{stem}.rs\""));
        if !mounted && !own_target {
            orphans.push(stem.to_owned());
        }
    }
    orphans.sort();
    assert!(
        orphans.is_empty(),
        "tests/*.rs files no target compiles: {orphans:?}"
    );
}
