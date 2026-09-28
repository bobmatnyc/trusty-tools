// Shared build-script policy, `include!`d by `build.rs` and by
// `tests/build_git_paths.rs` (#8787).
//
// Why: cargo never compiles a build script's own `#[cfg(test)]` module, so
// the git-dir resolution used to pick `cargo:rerun-if-changed` paths had no
// way to be tested directly. Splicing the same pure function into both the
// build script and a test target gives it real coverage, following the same
// pattern `build_ui_policy.rs` established for #8094.
// What: `resolve_git_watch_paths` — walks up from a starting directory to
// find the repository's real `.git` entry (directory or worktree pointer
// file) and returns the `HEAD`/`index` paths cargo should actually watch.
// Neither this file nor anything it adds may carry a `use` item or an inner
// `//!` doc comment: `include!` splices it into two different crate roots,
// where a `use` would collide with the includer's own imports and an inner
// attribute is not in a legal position.
// Test: `crates/trusty-agents/tests/build_git_paths.rs`.

/// Resolve the real `HEAD` and `index` paths this crate's checkout should
/// watch, walking up from `start_dir` (normally `CARGO_MANIFEST_DIR`).
///
/// Why (#8787): `cargo:rerun-if-changed` paths are resolved relative to the
/// crate directory, but a crate under `crates/<name>/` is never the
/// repository root — `.git` lives one or more levels up. A path that never
/// exists next to the crate makes cargo treat the build script as always
/// stale and rerun it (recompiling the whole crate) on every invocation,
/// which is the reported #8787 2-10 minute cost. Inside a `git worktree`,
/// `.git` is a FILE containing `gitdir: <path>` rather than a directory, and
/// that pointed-to directory — not the main checkout's `.git` — owns the
/// `HEAD` and `index` that describe THIS worktree's commit and staged index
/// (see `git-worktree(1)`).
/// What: walks `start_dir` and its ancestors looking for a `.git` entry. A
/// `.git` directory is used directly. A `.git` file is parsed for its
/// `gitdir: ` line, resolved relative to the file's own parent directory and
/// canonicalized when possible. Returns `None` when no `.git` is found
/// anywhere up to the filesystem root (e.g. a crates.io source tarball
/// build, which has no `.git` at all) — the caller emits no git rerun lines
/// in that case, and the build still succeeds using the `"unknown"`
/// fallbacks in `emit_git_provenance`.
/// Test: `finds_a_plain_git_dir`, `resolves_a_worktree_gitdir_pointer`,
/// `returns_none_with_no_git_dir_anywhere`, `walks_up_past_the_crate_dir`.
fn resolve_git_watch_paths(
    start_dir: &std::path::Path,
) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            return Some((candidate.join("HEAD"), candidate.join("index")));
        }
        if candidate.is_file() {
            let contents = std::fs::read_to_string(&candidate).ok()?;
            let gitdir_line = contents
                .lines()
                .find_map(|line| line.trim().strip_prefix("gitdir:"))?;
            let worktree_git_dir = dir.join(gitdir_line.trim());
            let worktree_git_dir = worktree_git_dir
                .canonicalize()
                .unwrap_or(worktree_git_dir);
            return Some((
                worktree_git_dir.join("HEAD"),
                worktree_git_dir.join("index"),
            ));
        }
        if !dir.pop() {
            return None;
        }
    }
}
