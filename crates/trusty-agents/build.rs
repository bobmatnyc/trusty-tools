//! Build script exposing git commit metadata to the binary.
//!
//! Why: Users of the binary benefit from knowing which exact commit produced
//! the running artifact; pairing `CARGO_PKG_VERSION` with the short git SHA
//! gives a deterministic identifier for bug reports and log correlation.
//! What: Queries git at build time and exposes the results to the compiled
//! crate via `cargo:rustc-env` — `GIT_COMMIT_HASH` (short SHA),
//! `GIT_COMMIT_HASH_FULL`, `GIT_COMMIT_DATE` (strict ISO 8601 committer
//! date), and `GIT_DIRTY` (`"1"` when the working tree had uncommitted or
//! untracked changes at build time). Falls back to `"unknown"` when git isn't
//! available or the directory isn't a git repo. #11632: the embedded Svelte
//! UI and its `pnpm build` step are gone with the HTTP API.
//! Test: `crates/trusty-agents/tests/version_provenance.rs` asserts the
//! binary's `--version` carries `GIT_COMMIT_HASH`.

use std::process::Command;

// #8787: the pure git-dir resolution, shared verbatim with the test target
// that covers it. See `build_git_paths.rs` for why it is `include!`d.
include!("build_git_paths.rs");

fn main() {
    // #8787: `.git/HEAD` and `.git/index` are repo-root-relative, not
    // crate-relative. Resolve the real paths (following a worktree's
    // `gitdir:` pointer where applicable) so cargo reruns this script only
    // when a commit or `git add` actually changes them.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    if let Some((head, index)) = resolve_git_watch_paths(&manifest_dir) {
        println!("cargo:rerun-if-changed={}", head.display());
        println!("cargo:rerun-if-changed={}", index.display());
    }
    // This script emits `rerun-if-changed`, which switches cargo off its
    // whole-package default, so the `include!`d file must be named.
    println!("cargo:rerun-if-changed=build_git_paths.rs");

    emit_git_provenance();
}

/// Capture the git provenance of this build and expose it via `cargo:rustc-env`.
///
/// Why: Bug reports and log lines need a deterministic identifier for the
/// running binary — `CARGO_PKG_VERSION` alone collapses every commit on a
/// dev branch into the same version string, which is how a ~3.5h-stale
/// `tagent` was reported as a code bug (#4260). The commit date answers "is
/// this binary older than that merge?" directly, and the dirty flag says
/// whether the SHA describes the whole binary or only most of it.
/// What: Runs `git rev-parse --short HEAD`, `git rev-parse HEAD`,
/// `git log -1 --format=%cI`, and `git status --porcelain`, exposing them as
/// `GIT_COMMIT_HASH`, `GIT_COMMIT_HASH_FULL`, `GIT_COMMIT_DATE`, and
/// `GIT_DIRTY` (`"1"`/`"0"`). Each git value falls back to `"unknown"` (dirty
/// to `"0"`) when git is unavailable or this is not a repo. The dirty flag
/// describes the tree as of the last build-script run: #8787 resolved the
/// `cargo:rerun-if-changed` paths above to the real `HEAD`/`index` files, so
/// this script (and the flag) now refresh on a commit or `git add`, not on
/// every unrelated build — trading always-current staleness detection for
/// not rebuilding this crate on every invocation.
/// Test: `crates/trusty-agents/tests/version_provenance.rs` asserts the
/// binary's `--version` carries `GIT_COMMIT_HASH`;
/// `build_info::commit_mark_matches_the_dirty_flag` covers the rendering.
fn emit_git_provenance() {
    let short = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let full = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let date = git(&["log", "-1", "--format=%cI"]).unwrap_or_else(|| "unknown".to_string());
    // #4260: `--porcelain` prints one line per changed or untracked path, so
    // any output at all means the tree does not match the SHA above.
    let dirty = match git(&["status", "--porcelain"]) {
        Some(_) => "1",
        None => "0",
    };

    println!("cargo:rustc-env=GIT_COMMIT_HASH={short}");
    println!("cargo:rustc-env=GIT_COMMIT_HASH_FULL={full}");
    println!("cargo:rustc-env=GIT_COMMIT_DATE={date}");
    println!("cargo:rustc-env=GIT_DIRTY={dirty}");
}

/// Run `git` with `args`, returning trimmed stdout, or `None` when the command
/// fails or produces no output.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}
