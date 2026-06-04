//! Build script: expose git commit metadata as a compile-time environment
//! variable so `build_info::GIT_HASH` is always populated.
//!
//! Why: Bug reports and log lines need a deterministic identifier for the
//! running binary — `CARGO_PKG_VERSION` alone collapses every commit on a
//! dev branch into the same version string. Pairing it with the short git
//! SHA gives a deterministic identifier for correlation.
//! What: Queries `git rev-parse --short HEAD` at build time and exposes the
//! result as `GIT_COMMIT_HASH`, or falls back to `"unknown"` if git is
//! unavailable.
//! Test: After `cargo build`, the compiled binary's startup banner should
//! include either a 7-char hash or the literal `"unknown"`.

use std::process::Command;

fn main() {
    // Re-run whenever HEAD moves so a new commit triggers a rebuild.
    println!("cargo:rerun-if-changed=.git/HEAD");

    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_COMMIT_HASH={git_hash}");
}
