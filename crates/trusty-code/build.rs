//! Build script: expose git commit metadata as compile-time environment
//! variables so `build_info::{GIT_HASH, COMMIT_DATE}` are always populated.
//!
//! Why: Run artifacts must be attributable to the exact binary that produced
//! them. `CARGO_PKG_VERSION` alone collapses every commit on a dev branch into
//! the same version string — during the 2026-07-16 L4 validation that gap
//! forced provenance to be reverse-inferred from `cargo install`'s mtime reset
//! and produced a WRONG conclusion (#2823). Pairing the version with the short
//! git SHA and the commit date gives a deterministic identifier for
//! correlation.
//! What: Queries `git rev-parse --short HEAD` and `git log -1 --date=short
//! --format=%cd` at build time, exposing them as `GIT_COMMIT_HASH` and
//! `GIT_COMMIT_DATE`. Both fall back to `"unknown"` when git is unavailable or
//! the source is not a git checkout (e.g. a crates.io tarball), so the build
//! never breaks. The commit *date* is used rather than a wall-clock build time
//! so the values stay deterministic and reproducible across rebuilds.
//! Test: `build_info::tests::{provenance_constants_are_well_formed,
//! version_string_contains_version}` assert both render; after `cargo build`,
//! `tcode --version` prints either real values or the literal `"unknown"`.

use std::path::Path;
use std::process::Command;

/// Run a `git` invocation and return its trimmed stdout, or `None` when git is
/// missing, errors, or the directory is not a checkout.
///
/// Why: Every provenance probe needs the same "never fail the build" semantics;
/// centralising the fallible path keeps `main` declarative and guarantees a
/// non-checkout source tree degrades to `"unknown"` instead of breaking.
/// What: Spawns `git <args>`, requires a success status, and rejects empty
/// output so a blank result never masquerades as a real value.
/// Test: Exercised on every build; the `None` path is what a crates.io tarball
/// build takes.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

/// Emit `cargo:rerun-if-changed` for `path` only when it actually exists.
///
/// Why: Cargo treats a `rerun-if-changed` path that does NOT exist as
/// perpetually changed, so declaring a missing path re-runs this script on
/// every single `cargo build`. That is not hypothetical: the original directive
/// here was a RELATIVE `.git/HEAD`, which Cargo resolves against the *package*
/// root (`crates/trusty-code/.git/HEAD` — absent even in a normal checkout), so
/// this script never cached in any tree. Guarding on existence keeps a build
/// with nothing to watch (the crates.io tarball) on Cargo's default "re-run
/// when sources change" behaviour instead of thrashing.
/// What: Canonicalizes `path` and prints the directive only if that succeeds
/// (which itself proves existence); otherwise does nothing. Canonicalizing
/// matters because `git rev-parse --git-path` returns a path relative to the
/// CWD in a plain checkout but an absolute one in a linked worktree — emitting
/// an absolute path either way removes any doubt about what Cargo resolves the
/// directive against.
/// Test: `build_info::tests::packed_branch_ref_leaves_only_the_reflog_witnessing_a_commit`
/// pins the packed-ref premise; also observable as a second consecutive
/// `cargo build` being a no-op.
fn rerun_if_exists(path: &str) {
    if let Ok(abs) = Path::new(path).canonicalize() {
        println!("cargo:rerun-if-changed={}", abs.display());
    }
}

fn main() {
    // Re-run whenever HEAD moves. In a plain checkout that is `.git/HEAD`; in a
    // linked worktree `.git` is a FILE and HEAD lives in the worktree's gitdir,
    // so ask git to resolve the real path rather than hardcoding `.git/HEAD`.
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        rerun_if_exists(&head);
    }
    // Committing on the same branch rewrites the branch ref, not HEAD itself
    // (HEAD keeps saying `ref: refs/heads/<branch>`), so watch the ref file too.
    if let Some(name) = git(&["symbolic-ref", "--quiet", "HEAD"])
        && let Some(ref_path) = git(&["rev-parse", "--git-path", &name])
    {
        rerun_if_exists(&ref_path);
    }
    // The loose ref above is NOT sufficient on its own. Emitting any
    // `rerun-if-changed` opts this script out of Cargo's default "re-run when
    // any package file changes" heuristic — from here on ONLY declared paths
    // are watched. If the branch ref happens to be PACKED at build time
    // (`git gc --auto`, `git pack-refs`), no loose ref file exists for
    // `rerun_if_exists` to watch; a later commit would then recreate the loose
    // ref while touching no watched path, so this script would never re-run and
    // the embedded SHA/date would go silently STALE until a `cargo clean`. A
    // binary reporting a stale SHA is the exact failure #2823 exists to
    // eliminate. `logs/HEAD` (the reflog) closes the gap: it is append-only and
    // touched by effectively every ref-changing operation — commit, checkout,
    // merge, reset — whether or not the branch ref is packed. Watching a third
    // narrow path is far cheaper than an unconditional always-re-run.
    if let Some(reflog) = git(&["rev-parse", "--git-path", "logs/HEAD"]) {
        rerun_if_exists(&reflog);
    }

    let git_hash = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let commit_date = git(&["log", "-1", "--date=short", "--format=%cd"])
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_COMMIT_HASH={git_hash}");
    println!("cargo:rustc-env=GIT_COMMIT_DATE={commit_date}");
}
