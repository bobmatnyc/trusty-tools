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
//! ## Rerunning when the commit changes, not on every build
//!
//! A `cargo:rerun-if-changed` directive names one path this build depends on;
//! Cargo reruns the script only when that path's mtime moves. Naming only
//! `.git/HEAD` — this script's first cut — misses a same-branch commit
//! entirely: `HEAD` itself is the symbolic ref `ref: refs/heads/<branch>` and
//! its OWN mtime does not change when the branch tip moves, only the ref
//! target does (a reviewer reproduced the stale-SHA binary this caused). This
//! version watches three paths instead of guessing a fixed layout:
//!
//! - the per-worktree `HEAD` file, from `git rev-parse --git-dir` — catches a
//!   branch switch or a detached-HEAD checkout;
//! - the per-worktree `logs/HEAD` reflog, whose mtime updates on every commit
//!   that moves this worktree's HEAD (loose or packed ref alike), which is
//!   what actually catches an ordinary same-branch commit;
//! - the resolved `refs/heads/<branch>` file, read out of `HEAD`'s `ref: `
//!   line and resolved against `git rev-parse --git-common-dir` — refs are
//!   shared across worktrees even though `HEAD`, `logs/HEAD` and the index are
//!   each per-worktree, so the common dir (not the per-worktree git-dir) is
//!   where that file actually lives.
//!
//! Resolving both dirs through `git` rather than a hard-coded `../../.git/`
//! keeps this correct inside a linked worktree, where `.git` is a file
//! pointing elsewhere rather than a directory. Every step here is fail-open:
//! a `git` that is missing, refuses, or emits nothing unexpected just leaves
//! the git revision stale until a full rebuild — never a broken build.
//!
//! Test: `crate::index_report::index_tests::the_git_revision_the_build_captured_is_stated`,
//! and the harness reproduction in the PR description (two commits on one
//! branch in a scratch repo, second build reruns) — a build script has no
//! `cargo test` unit test of its own to run under this crate's test target.

use std::path::Path;

/// Environment variables that redirect `git` at a repository other than the
/// one this build script means to ask about (#6144 review) — the same
/// ambient-state class `crate::git::AMBIENT` clears for this crate's runtime
/// `git` child, restated here because `build.rs` is its own compilation unit
/// and cannot depend on the library crate it belongs to.
const AMBIENT_GIT_ENV: [&str; 4] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    watch_git_head();
    bake_git_revision();
}

/// Run `git` with the ambient repository-selecting variables cleared, and
/// return its trimmed stdout on success — `None` for anything else (`git`
/// missing, a nonzero exit, non-UTF-8 output, or blank output).
fn git(args: &[&str]) -> Option<String> {
    let mut command = std::process::Command::new("git");
    command.args(args);
    for var in AMBIENT_GIT_ENV {
        command.env_remove(var);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Ask `rerun-if-changed` to watch the paths that actually move when this
/// checkout's HEAD does — see the module docs for why `.git/HEAD` alone is
/// not enough. Fail-open throughout: a step that cannot resolve just emits no
/// directive for it, which costs a stale git revision, not a broken build.
fn watch_git_head() {
    let Some(git_dir) = git(&["rev-parse", "--git-dir"]) else {
        return;
    };
    let git_dir = Path::new(&git_dir);

    let head_path = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());
    // The reflog's mtime moves on every commit that updates this worktree's
    // HEAD, which is the actual signal a same-branch commit leaves behind.
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join("logs").join("HEAD").display()
    );

    // HEAD's `ref: refs/heads/<branch>` line names the file that holds the
    // branch tip itself; refs live in the COMMON dir, shared across every
    // worktree of this repository, so a plain `git_dir.join(refname)` would
    // miss it inside a linked worktree.
    let Ok(head_contents) = std::fs::read_to_string(&head_path) else {
        return;
    };
    let Some(refname) = head_contents.trim().strip_prefix("ref: ") else {
        return; // detached HEAD — logs/HEAD above already covers it.
    };
    let Some(common_dir) = git(&["rev-parse", "--git-common-dir"]) else {
        return;
    };
    println!(
        "cargo:rerun-if-changed={}",
        Path::new(&common_dir).join(refname).display()
    );
}

/// Bake the current commit into `TRUSTY_AUDIT_GIT_REVISION`, when `git`
/// resolves one.
fn bake_git_revision() {
    if let Some(revision) = git(&["rev-parse", "--short=12", "HEAD"]) {
        println!("cargo:rustc-env=TRUSTY_AUDIT_GIT_REVISION={revision}");
    }
}
