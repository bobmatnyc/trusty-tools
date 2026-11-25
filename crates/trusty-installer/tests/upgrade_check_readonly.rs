//! `tctl upgrade --check` writes nothing, even with `--yes` (#11268).
//!
//! Why: `--check` is the read-only alias for `tctl updates`. Asserting only its
//! exit code let a `--check` that fell through to the apply path pass, since an
//! apply also exits 0. Running the binary in a sandboxed home is what lets the
//! test see a write.
//!
//! What: runs `trusty-installer --yes --json upgrade --check tga` with every
//! location the installer writes to (`HOME`, `CARGO_HOME`, `TMPDIR`) inside
//! one temp directory and `PATH` limited to the system dirs, then asserts the
//! directory holds exactly the empty skeleton it started with.
//!
//! Test: `run_check_is_readonly`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Every path under `root`, relative to it, sorted.
fn tree(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            out.push(path.strip_prefix(root).expect("under root").to_path_buf());
        }
    }
    out.sort();
    out
}

/// Why: `--check` must be read-only and never reach the apply path, even when
/// `--yes` would let an apply run unattended. This performs a live crates.io
/// and GitHub probe, so it is `#[ignore]`-tagged to keep CI offline.
/// What: runs the command in a sandboxed home; asserts exit 0 and that the
/// sandbox tree is unchanged.
/// Test: `cargo test -p trusty-installer --test upgrade_check_readonly -- --include-ignored`.
#[test]
#[ignore = "performs a live crates.io probe; run with --include-ignored"]
fn run_check_is_readonly() {
    let sandbox = tempfile::tempdir().expect("tempdir");
    let root = sandbox.path();
    for dir in ["home", "cargo", "tmp"] {
        std::fs::create_dir(root.join(dir)).expect("mkdir");
    }
    let before = tree(root);

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trusty-installer"));
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.join("home"))
        .env("CARGO_HOME", root.join("cargo"))
        .env("TMPDIR", root.join("tmp"));
    for token in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Some(v) = std::env::var_os(token) {
            cmd.env(token, v);
        }
    }
    let out = cmd
        .args(["--yes", "--json", "upgrade", "--check", "tga"])
        .output()
        .expect("spawn trusty-installer");

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        tree(root),
        before,
        "`upgrade --check` wrote into the sandbox; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
