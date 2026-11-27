//! Shared by the replay tests: builds a Rust stand-in example (#12028).

use std::path::{Path, PathBuf};

/// Builds example `name` into this test binary's own target dir and profile
/// and returns its path; `CARGO_BIN_EXE_*` covers `[[bin]]` targets only (the
/// pattern of trusty-code's `mcp_loader_e2e.rs`, #7951). A failed build fails.
pub fn build_example(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("the test binary has a path");
    // `<profile>/deps/<bin>`, or `<profile>/build/<crate>/<hash>/out/<bin>` (cargo#16807).
    fn named(d: &Path, n: &str) -> bool {
        d.file_name().is_some_and(|f| f == n)
    }
    let profile_dir = exe
        .ancestors()
        .find(|d| named(d, "deps"))
        .or_else(|| exe.ancestors().find(|d| named(d, "build")))
        .and_then(Path::parent)
        .expect("a cargo profile directory above the test binary")
        .to_path_buf();
    let profile = match profile_dir.file_name().and_then(|n| n.to_str()) {
        Some("debug") => "dev".to_owned(),
        Some(other) => other.to_owned(),
        None => panic!("no profile directory in {}", exe.display()),
    };
    let target_dir = profile_dir.parent().expect("a profile dir has a parent");
    let out = std::process::Command::new(env!("CARGO"))
        .args(["build", "--example", name, "--profile", &profile])
        .arg("--manifest-path")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .arg("--target-dir")
        .arg(target_dir)
        .output()
        .expect("`cargo build --example` runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let path = profile_dir
        .join("examples")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(path.exists(), "{} not built", path.display());
    path
}
