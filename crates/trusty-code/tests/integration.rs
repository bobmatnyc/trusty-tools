//! trusty-code's shared integration target (#9302).
//!
//! Why: every file under `tests/` used to link into its own test binary. On
//! macOS each new executable waits for a serialized XProtect scan on its
//! first exec, so twenty binaries cost twenty scans per build. One target
//! links and is scanned once.
//! What: each module below is one former test binary, unchanged apart from
//! reaching the shared helper as `crate::support`. libtest runs them in
//! parallel in one process, so a module belongs here only if it leaves
//! process-global state alone. `hermetic_palace_e2e` (sets `$HOME`) and
//! `logging_e2e` (re-execs itself by bare test name) keep their own `[[test]]`
//! targets in `Cargo.toml`.
//! Test: `every_tests_file_is_mounted_9302` fails if a file under `tests/` is
//! mounted by no target. Run one former binary with a module filter:
//! `cargo test -p trusty-code --test integration session_e2e::`.

mod support;

mod agent_fallback_e2e;
mod agent_id_e2e;
mod agents_e2e;
mod bakeoff_gate_e2e;
mod cli_e2e;
mod config_mount;
mod connector_e2e;
mod inference_shared_adapter_e2e;
mod m1_cutline_e2e;
mod mcp_loader_e2e;
mod no_content_e2e;
mod paths_import_mpm_catalog_e2e;
mod permission_prompt_e2e;
mod readiness_e2e;
mod recall_content_e2e;
mod roster_deploy_e2e;
mod search_hits_e2e;
mod session_e2e;
mod task_e2e;
mod tui_client_engine;

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
