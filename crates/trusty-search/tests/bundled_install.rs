//! Bundled-install acceptance test.
//!
//! Why: validates that `cargo install --path crates/trusty-search` produces
//! BOTH the `trusty-search` and `trusty-embedderd` binaries, confirming the
//! one-install-command goal (issue #187 Phase 2 follow-up). Without this test
//! a future accidental removal of the `[[bin]]` entry or the
//! `trusty-embedderd` dep from `trusty-search/Cargo.toml` would be silently
//! invisible until a user hit it.
//!
//! What: shells out to `cargo install --path . --locked --root <tempdir>
//! --force` from the trusty-search crate directory, then asserts both
//! `<tempdir>/bin/trusty-search` and `<tempdir>/bin/trusty-embedderd` exist
//! and are executable.
//!
//! Test: this file. The test is `#[ignore]` because it does a full release
//! build (~minutes) and should only run in explicit release-validation
//! contexts, not in normal `cargo test` CI sweeps. Run with:
//!   cargo test -p trusty-search --test bundled_install -- --include-ignored

use std::process::Command;

#[test]
#[ignore = "shells out to `cargo install` — slow (~minutes); run with --include-ignored"]
fn cargo_install_trusty_search_produces_both_binaries() {
    // Why: use a temp root so we don't pollute ~/.cargo/bin and the test is
    // self-contained / repeatable.
    // What: create a temp dir, run cargo install --root into it, check both
    // binaries exist and are executable.
    let install_root = tempfile::tempdir().expect("tempdir");
    let install_root_path = install_root.path();

    // Locate the trusty-search crate directory relative to this test file.
    // CARGO_MANIFEST_DIR is set by Cargo to the crate root.
    let crate_dir =
        std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));

    let status = Command::new("cargo")
        .args([
            "install",
            "--path",
            crate_dir.to_str().expect("crate_dir is valid UTF-8"),
            "--locked",
            "--root",
            install_root_path
                .to_str()
                .expect("install_root is valid UTF-8"),
            "--force",
        ])
        // Suppress output — the build is noisy and takes a while.
        .status()
        .expect("failed to invoke cargo install");

    assert!(
        status.success(),
        "cargo install trusty-search exited with status {status}"
    );

    let bin_dir = install_root_path.join("bin");

    // Check trusty-search binary.
    let ts_bin = bin_dir.join("trusty-search");
    let ts_bin_exe = bin_dir.join("trusty-search.exe"); // Windows
    assert!(
        ts_bin.is_file() || ts_bin_exe.is_file(),
        "expected trusty-search binary in {bin_dir:?}; found: {:?}",
        std::fs::read_dir(&bin_dir)
            .ok()
            .map(|d| d
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );

    // Check trusty-embedderd binary (the bundled sidecar).
    let te_bin = bin_dir.join("trusty-embedderd");
    let te_bin_exe = bin_dir.join("trusty-embedderd.exe"); // Windows
    assert!(
        te_bin.is_file() || te_bin_exe.is_file(),
        "expected trusty-embedderd binary in {bin_dir:?}; \
         cargo install trusty-search must produce both binaries. \
         Found: {:?}",
        std::fs::read_dir(&bin_dir)
            .ok()
            .map(|d| d
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );

    // On Unix: assert both are executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let ts_path = if ts_bin.is_file() {
            &ts_bin
        } else {
            &ts_bin_exe
        };
        let ts_mode = ts_path.metadata().unwrap().permissions().mode();
        assert!(
            ts_mode & 0o111 != 0,
            "trusty-search at {ts_path:?} is not executable (mode {ts_mode:#o})"
        );

        let te_path = if te_bin.is_file() {
            &te_bin
        } else {
            &te_bin_exe
        };
        let te_mode = te_path.metadata().unwrap().permissions().mode();
        assert!(
            te_mode & 0o111 != 0,
            "trusty-embedderd at {te_path:?} is not executable (mode {te_mode:#o})"
        );
    }
}
