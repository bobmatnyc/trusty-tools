//! Bakes the build-time git revision into the binary (#6144).
//!
//! Why: the run index states which tool versions produced a directory's
//! reports (`crate::index_report`) and, until this, said flatly that
//! `trusty-audit`'s own git revision is "not recorded" — there was no build
//! script to capture one. A recipient auditing a delivered report could tell
//! which released version built the binary but not which commit, which is the
//! finer-grained fact that matters between releases and for a `cargo install
//! --path` build that carries no tag at all.
//!
//! What: runs `git rev-parse --short=12 HEAD` at build time and, when it
//! succeeds, emits the result as the `TRUSTY_AUDIT_GIT_REVISION` compile-time
//! environment variable via `cargo:rustc-env`. `crate::index_report::render`
//! reads it back with `option_env!`. A build with no `git` on `PATH`, or one
//! from a source tree with no `.git` (a `crates.io` source tarball, for
//! example), leaves the variable unset — `render` already has a documented
//! rendering for that case, which this script does not change.
//!
//! Test: `crate::index_report::index_tests::the_git_revision_the_build_captured_is_stated`,
//! which asserts whichever branch this build actually took; a build script has
//! no unit test of its own to run under `cargo test`.

fn main() {
    // #6144: rerun when the checked-out commit changes, not on every build —
    // the default (no `rerun-if-changed` at all) would rerun on ANY file
    // change anywhere in the crate, which is far too eager for a value that
    // only moves when HEAD does.
    println!("cargo:rerun-if-changed=build.rs");
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        // The crate lives two levels below the repository root
        // (`crates/trusty-audit`); a worktree's `.git` is a file pointing
        // elsewhere, and `git` itself resolves either shape correctly, but
        // `rerun-if-changed` just needs *a* path that changes when HEAD does.
        println!("cargo:rerun-if-changed={manifest_dir}/../../.git/HEAD");
    }

    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output();
    if let Ok(output) = output
        && output.status.success()
        && let Ok(revision) = String::from_utf8(output.stdout)
    {
        let revision = revision.trim();
        if !revision.is_empty() {
            println!("cargo:rustc-env=TRUSTY_AUDIT_GIT_REVISION={revision}");
        }
    }
}
