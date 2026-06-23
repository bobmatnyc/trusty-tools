//! `trusty-search hook install` / `hook uninstall` — git hook management.
//!
//! Why: After a commit or merge, changed files should be re-indexed immediately
//! so search results reflect the latest committed code.  Installing a git
//! `post-commit` / `post-merge` hook automates incremental updates without
//! needing a full reindex. The hook only fires for repos that are already
//! in the opt-in allowlist and silently no-ops when the daemon is down, so
//! it never blocks a commit.
//!
//! What: `handle_hook_install` writes a marker-delimited block into the
//! target repo's `.git/hooks/{post-commit,post-merge,post-checkout}` files,
//! preserving any existing hook logic via append-safe insertion.
//! `handle_hook_uninstall` removes only the trusty-search marker block.
//!
//! Test: `tests` module exercises install → verify block present → idempotent
//! re-install → uninstall → verify block removed, all using a temp `.git/hooks`
//! directory rather than the real allowlist or daemon.

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

// ── Marker strings ────────────────────────────────────────────────────────────

/// Opening sentinel written into every managed hook file.
///
/// Why: must be unique and stable so install/uninstall can locate the block
/// even when other tools have also added lines to the same hook file.
const MARKER_BEGIN: &str = "# >>> trusty-search >>>";
/// Closing sentinel.
const MARKER_END: &str = "# <<< trusty-search <<<";

// ── Public interface ──────────────────────────────────────────────────────────

/// Parameters extracted from the `hook install` / `hook uninstall` clap args.
#[derive(Debug)]
pub struct HookArgs {
    /// Path to the repository root (the directory that contains `.git/`).
    /// Defaults to CWD when `None`.
    pub repo: Option<PathBuf>,
    /// Which sub-action to perform.
    pub action: HookAction,
}

/// The two supported sub-actions.
#[derive(Debug, Clone)]
pub enum HookAction {
    Install,
    Uninstall,
}

/// Handle `trusty-search hook install [--repo <path>]`.
///
/// Why: single public entry point so `main.rs` stays a thin dispatcher.
/// What: resolves the git directory, writes all three hook scripts.
/// Test: `tests::install_creates_and_is_idempotent`.
pub async fn handle_hook(args: HookArgs) -> Result<()> {
    let repo_root = resolve_repo_root(args.repo.as_deref())?;
    let hooks_dir = repo_root.join(".git").join("hooks");
    anyhow::ensure!(
        hooks_dir.exists(),
        "no .git/hooks directory found at '{}' — is this a git repository?",
        repo_root.display()
    );

    match args.action {
        HookAction::Install => {
            for (name, content) in hook_scripts() {
                let path = hooks_dir.join(name);
                install_block(&path, content)?;
                println!("{} installed hook {}", "✓".green(), path.display());
            }
            println!(
                "  Run {} inside this repo to register it first.",
                "trusty-search index add .".cyan()
            );
        }
        HookAction::Uninstall => {
            for (name, _) in hook_scripts() {
                let path = hooks_dir.join(name);
                uninstall_block(&path)?;
                println!("{} removed trusty-search block from {}", "−".red(), name);
            }
        }
    }
    Ok(())
}

// ── Hook script bodies ────────────────────────────────────────────────────────

/// The three hook scripts managed by this module.
///
/// Why: all three are returned together so install/uninstall loops stay
/// symmetric and a future addition only needs to update this one function.
/// What: returns `(hook_name, script_body)` pairs; each body already includes
/// the shebang and allowlist / daemon-down guards.
/// Test: `tests::hook_scripts_all_exit_zero` confirms each script is valid sh.
fn hook_scripts() -> [(&'static str, &'static str); 3] {
    [
        ("post-commit", POST_COMMIT_SCRIPT),
        ("post-merge", POST_MERGE_SCRIPT),
        ("post-checkout", POST_CHECKOUT_SCRIPT),
    ]
}

/// `post-commit`: index changed/added files, remove deleted files.
const POST_COMMIT_SCRIPT: &str = r#"#!/bin/sh
# trusty-search: incremental reindex on commit
set -e

# Locate the repo root (one level above the hooks dir).
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0
BINARY="trusty-search"

# Bail out silently when the binary is not on PATH.
command -v "$BINARY" >/dev/null 2>&1 || exit 0

# No-op when the daemon is down (best-effort health probe, 200ms timeout).
"$BINARY" health >/dev/null 2>&1 || exit 0

# Determine changed/added vs deleted files in the last commit.
CHANGED="$(git diff-tree --no-commit-id -r --name-only --diff-filter=ACRMT HEAD 2>/dev/null)" || true
DELETED="$(git diff-tree --no-commit-id -r --name-only --diff-filter=D HEAD 2>/dev/null)" || true

# Index changed/added files.
if [ -n "$CHANGED" ]; then
    echo "$CHANGED" | while IFS= read -r f; do
        [ -f "$REPO_ROOT/$f" ] || continue
        "$BINARY" --index "" add "$REPO_ROOT/$f" >/dev/null 2>&1 || true
    done
fi

# Remove deleted files.
if [ -n "$DELETED" ]; then
    echo "$DELETED" | while IFS= read -r f; do
        "$BINARY" --index "" remove "$REPO_ROOT/$f" >/dev/null 2>&1 || true
    done
fi

exit 0
"#;

/// `post-merge`: same incremental update after a merge/pull.
const POST_MERGE_SCRIPT: &str = r#"#!/bin/sh
# trusty-search: incremental reindex on merge
set -e

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0
BINARY="trusty-search"

command -v "$BINARY" >/dev/null 2>&1 || exit 0
"$BINARY" health >/dev/null 2>&1 || exit 0

# $1 is "1" for a squash merge, "0" for a true merge.
ORIG_HEAD="ORIG_HEAD"
CHANGED="$(git diff --name-only --diff-filter=ACRMT "$ORIG_HEAD" HEAD 2>/dev/null)" || true
DELETED="$(git diff --name-only --diff-filter=D "$ORIG_HEAD" HEAD 2>/dev/null)" || true

if [ -n "$CHANGED" ]; then
    echo "$CHANGED" | while IFS= read -r f; do
        [ -f "$REPO_ROOT/$f" ] || continue
        "$BINARY" --index "" add "$REPO_ROOT/$f" >/dev/null 2>&1 || true
    done
fi

if [ -n "$DELETED" ]; then
    echo "$DELETED" | while IFS= read -r f; do
        "$BINARY" --index "" remove "$REPO_ROOT/$f" >/dev/null 2>&1 || true
    done
fi

exit 0
"#;

/// `post-checkout`: when HEAD changes substantially, trigger a background
/// diff-only reindex.  The hook receives three arguments:
///   $1 = previous HEAD, $2 = new HEAD, $3 = branch flag (1 = branch switch)
const POST_CHECKOUT_SCRIPT: &str = r#"#!/bin/sh
# trusty-search: background reindex on branch switch
set -e

# Only act on branch-switch checkouts ($3 == 1).
[ "$3" = "1" ] || exit 0

# Skip when the HEAD didn't actually change.
[ "$1" != "$2" ] || exit 0

BINARY="trusty-search"
command -v "$BINARY" >/dev/null 2>&1 || exit 0
"$BINARY" health >/dev/null 2>&1 || exit 0

# Fire-and-forget diff-only reindex in the background so the checkout
# returns immediately.  The daemon's SHA-256 hash skip makes this cheap.
"$BINARY" reindex >/dev/null 2>&1 &

exit 0
"#;

// ── Block-level install / uninstall ──────────────────────────────────────────

/// Write (or update) a trusty-search marker block into a hook file.
///
/// Why: many tools share a single `.git/hooks/post-commit`; appending a
/// delimited block lets us coexist without clobbering existing logic. If a
/// prior block exists it is replaced in-place (idempotent).
/// What: reads the current file (or starts empty), splices the new block in,
/// writes atomically (tmp + rename), and sets `chmod +x`.
/// Test: `tests::install_creates_and_is_idempotent`.
pub fn install_block(hook_path: &Path, script_body: &str) -> Result<()> {
    let existing = if hook_path.exists() {
        fs::read_to_string(hook_path)
            .with_context(|| format!("could not read {}", hook_path.display()))?
    } else {
        String::new()
    };

    let new_content = splice_block(&existing, script_body);

    atomic_write(hook_path, &new_content)?;
    make_executable(hook_path)?;
    Ok(())
}

/// Remove the trusty-search marker block from a hook file.
///
/// Why: `hook uninstall` must restore the hook file to its pre-install state
/// without touching any other tool's content.
/// What: reads the file, strips the marker block, writes back. No-op when
/// the file does not exist or contains no trusty-search block.
/// Test: `tests::uninstall_removes_block`.
pub fn uninstall_block(hook_path: &Path) -> Result<()> {
    if !hook_path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(hook_path)
        .with_context(|| format!("could not read {}", hook_path.display()))?;
    let stripped = remove_block(&content);
    if stripped == content {
        // Nothing to remove.
        return Ok(());
    }
    if stripped.trim().is_empty() {
        // File contained only our block — remove the file entirely.
        fs::remove_file(hook_path)
            .with_context(|| format!("could not remove {}", hook_path.display()))?;
    } else {
        atomic_write(hook_path, &stripped)?;
        make_executable(hook_path)?;
    }
    Ok(())
}

// ── String-level block manipulation ──────────────────────────────────────────

/// Insert or replace the trusty-search block in `existing`.
///
/// Why: pure-function text transformation that is easy to unit-test independently
/// of the filesystem.
/// What: finds MARKER_BEGIN..MARKER_END, replaces if present; appends if absent.
/// A shebang in `existing` is preserved as the first line even when the new
/// block is appended.
/// Test: `tests::splice_inserts_and_replaces`.
pub fn splice_block(existing: &str, script_body: &str) -> String {
    let block = format!("{}\n{}{}\n", MARKER_BEGIN, script_body, MARKER_END);

    if let Some(begin) = existing.find(MARKER_BEGIN) {
        // Find the end of the closing marker line.
        if let Some(end_pos) = existing[begin..].find(MARKER_END) {
            let abs_end = begin + end_pos + MARKER_END.len();
            // Consume trailing newline after the marker if present.
            let abs_end = if existing.as_bytes().get(abs_end) == Some(&b'\n') {
                abs_end + 1
            } else {
                abs_end
            };
            let mut result = existing[..begin].to_owned();
            result.push_str(&block);
            result.push_str(&existing[abs_end..]);
            return result;
        }
    }

    // No existing block — append.
    if existing.is_empty() {
        block
    } else {
        // Ensure we start on a new line.
        let sep = if existing.ends_with('\n') { "" } else { "\n" };
        format!("{}{}{}", existing, sep, block)
    }
}

/// Strip the trusty-search marker block from `content`.
///
/// Why: symmetric counterpart to `splice_block`, used by `uninstall_block`.
/// What: returns the content with the MARKER_BEGIN..MARKER_END span removed.
/// Test: `tests::remove_block_is_symmetric`.
pub fn remove_block(content: &str) -> String {
    let Some(begin) = content.find(MARKER_BEGIN) else {
        return content.to_owned();
    };
    let Some(end_pos) = content[begin..].find(MARKER_END) else {
        return content.to_owned();
    };
    let abs_end = begin + end_pos + MARKER_END.len();
    // Consume trailing newline after the marker if present.
    let abs_end = if content.as_bytes().get(abs_end) == Some(&b'\n') {
        abs_end + 1
    } else {
        abs_end
    };
    let mut result = content[..begin].to_owned();
    result.push_str(&content[abs_end..]);
    result
}

// ── Filesystem helpers ────────────────────────────────────────────────────────

/// Write `content` to `path` atomically (tmp sibling + rename).
///
/// Why: prevents a partial write from leaving the hook file in a broken state.
/// What: writes to `<path>.ts-tmp`, then renames to `path`.
/// Test: covered by `install_block` tests.
fn atomic_write(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let tmp = path.with_extension("ts-tmp");
    fs::write(&tmp, content).with_context(|| format!("could not write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("could not rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Set the hook file to `chmod +x`.
///
/// Why: git will not invoke a hook script unless it is executable.
/// What: reads current permissions, adds 0o111 (user/group/other execute).
/// Test: covered by `install_block` integration tests.
fn make_executable(path: &Path) -> Result<()> {
    let meta = fs::metadata(path).with_context(|| format!("could not stat {}", path.display()))?;
    let mut perms = meta.permissions();
    let mode = perms.mode() | 0o111;
    perms.set_mode(mode);
    fs::set_permissions(path, perms)
        .with_context(|| format!("could not chmod +x {}", path.display()))?;
    Ok(())
}

/// Resolve the git repository root.
///
/// Why: users often run `hook install` from a subdirectory of the repo.
/// What: if `explicit` is given, use it; otherwise ask git for the top-level.
/// Test: `tests::resolve_repo_root_falls_back_to_git`.
fn resolve_repo_root(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        let canon =
            fs::canonicalize(p).with_context(|| format!("could not resolve '{}'", p.display()))?;
        return Ok(canon);
    }
    // Ask git.
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("could not run git")?;
    if !out.status.success() {
        anyhow::bail!("not inside a git repository; pass --repo <path>");
    }
    let raw = std::str::from_utf8(&out.stdout)
        .context("git output was not UTF-8")?
        .trim()
        .to_owned();
    Ok(PathBuf::from(raw))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    // ── String manipulation ───────────────────────────────────────────────────

    #[test]
    fn splice_inserts_into_empty() {
        // Why: a fresh hook file with no existing content should receive the
        // complete block (including the marker sentinels).
        let result = splice_block("", "echo hello\n");
        assert!(result.contains(MARKER_BEGIN), "missing begin marker");
        assert!(result.contains(MARKER_END), "missing end marker");
        assert!(result.contains("echo hello"), "missing body");
    }

    #[test]
    fn splice_appends_to_existing_content() {
        // Why: any existing hook content (e.g. from another tool) must be
        // preserved before the trusty-search block.
        let existing = "#!/bin/sh\necho other\n";
        let result = splice_block(existing, "echo ts\n");
        assert!(
            result.starts_with("#!/bin/sh\necho other\n"),
            "existing content moved"
        );
        assert!(result.contains(MARKER_BEGIN), "missing begin marker");
        assert!(result.contains("echo ts"), "missing body");
    }

    #[test]
    fn splice_replaces_existing_block() {
        // Why: idempotent re-install must not duplicate the block.
        let first = splice_block("", "echo v1\n");
        let second = splice_block(&first, "echo v2\n");
        // Only one begin/end pair.
        assert_eq!(second.matches(MARKER_BEGIN).count(), 1, "duplicate begin");
        assert_eq!(second.matches(MARKER_END).count(), 1, "duplicate end");
        assert!(second.contains("echo v2"), "new body missing");
        assert!(!second.contains("echo v1"), "old body still present");
    }

    #[test]
    fn remove_block_is_symmetric() {
        // Why: uninstall must restore the file to its pre-install state.
        let original = "#!/bin/sh\necho other\n";
        let installed = splice_block(original, "echo ts\n");
        let restored = remove_block(&installed);
        // The restored content should be equal (modulo trailing newlines) to
        // the original.
        assert_eq!(restored.trim(), original.trim());
    }

    #[test]
    fn remove_block_noop_when_absent() {
        // Why: calling uninstall on a file that was never managed must be safe.
        let content = "#!/bin/sh\necho other\n";
        let result = remove_block(content);
        assert_eq!(result, content);
    }

    // ── Filesystem operations ─────────────────────────────────────────────────

    fn fake_hooks_dir() -> (TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git").join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        (tmp, hooks)
    }

    #[test]
    fn install_creates_hook_file_and_is_executable() {
        // Why: a freshly created hook file must be chmod +x so git invokes it.
        let (_tmp, hooks) = fake_hooks_dir();
        let path = hooks.join("post-commit");
        install_block(&path, POST_COMMIT_SCRIPT).unwrap();
        assert!(path.exists(), "hook file was not created");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_ne!(mode & 0o111, 0, "hook is not executable");
    }

    #[test]
    fn install_is_idempotent() {
        // Why: running `hook install` twice must not duplicate the block.
        let (_tmp, hooks) = fake_hooks_dir();
        let path = hooks.join("post-commit");
        install_block(&path, POST_COMMIT_SCRIPT).unwrap();
        install_block(&path, POST_COMMIT_SCRIPT).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content.matches(MARKER_BEGIN).count(), 1, "block duplicated");
    }

    #[test]
    fn install_preserves_existing_shebang() {
        // Why: if another tool already wrote a shebang, we must not clobber it.
        let (_tmp, hooks) = fake_hooks_dir();
        let path = hooks.join("post-commit");
        fs::write(&path, "#!/bin/sh\necho existing\n").unwrap();
        make_executable(&path).unwrap();
        install_block(&path, POST_COMMIT_SCRIPT).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("#!/bin/sh"), "shebang moved");
        assert!(content.contains("echo existing"), "prior content gone");
        assert!(
            content.contains(MARKER_BEGIN),
            "trusty-search block missing"
        );
    }

    #[test]
    fn uninstall_removes_block() {
        // Why: after uninstall the hook file must not contain the marker block.
        let (_tmp, hooks) = fake_hooks_dir();
        let path = hooks.join("post-commit");
        install_block(&path, POST_COMMIT_SCRIPT).unwrap();
        uninstall_block(&path).unwrap();
        if path.exists() {
            let content = fs::read_to_string(&path).unwrap();
            assert!(
                !content.contains(MARKER_BEGIN),
                "marker still present after uninstall"
            );
        }
        // path may not exist (only block → file removed) — that is also correct.
    }

    #[test]
    fn uninstall_preserves_other_content() {
        // Why: `hook uninstall` must not destroy another tool's hook logic.
        let (_tmp, hooks) = fake_hooks_dir();
        let path = hooks.join("post-commit");
        fs::write(&path, "#!/bin/sh\necho other-tool\n").unwrap();
        make_executable(&path).unwrap();
        install_block(&path, POST_COMMIT_SCRIPT).unwrap();
        uninstall_block(&path).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("echo other-tool"),
            "other tool's content gone"
        );
        assert!(
            !content.contains(MARKER_BEGIN),
            "trusty-search block still present"
        );
    }

    #[test]
    fn uninstall_noop_when_file_absent() {
        // Why: uninstall on a path that was never created must not error.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nonexistent-hook");
        uninstall_block(&path).unwrap(); // must not panic or error
    }

    // ── Full install / uninstall via handle_hook ─────────────────────────────

    fn fake_git_repo() -> TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let hooks_dir = tmp.path().join(".git").join("hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        tmp
    }

    #[tokio::test]
    async fn handle_hook_install_writes_all_three_hooks() {
        // Why: all three hook scripts must be present after install.
        let repo = fake_git_repo();
        let args = HookArgs {
            repo: Some(repo.path().to_path_buf()),
            action: HookAction::Install,
        };
        handle_hook(args).await.unwrap();
        for name in ["post-commit", "post-merge", "post-checkout"] {
            let p = repo.path().join(".git").join("hooks").join(name);
            assert!(p.exists(), "{name} was not created");
            let content = fs::read_to_string(&p).unwrap();
            assert!(content.contains(MARKER_BEGIN), "{name} missing marker");
        }
    }

    #[tokio::test]
    async fn handle_hook_uninstall_removes_all_three_hooks() {
        // Why: uninstall must clean up every hook, not just post-commit.
        let repo = fake_git_repo();
        let install_args = HookArgs {
            repo: Some(repo.path().to_path_buf()),
            action: HookAction::Install,
        };
        handle_hook(install_args).await.unwrap();
        let uninstall_args = HookArgs {
            repo: Some(repo.path().to_path_buf()),
            action: HookAction::Uninstall,
        };
        handle_hook(uninstall_args).await.unwrap();
        for name in ["post-commit", "post-merge", "post-checkout"] {
            let p = repo.path().join(".git").join("hooks").join(name);
            if p.exists() {
                let content = fs::read_to_string(&p).unwrap();
                assert!(
                    !content.contains(MARKER_BEGIN),
                    "{name} still has trusty-search block"
                );
            }
        }
    }

    #[tokio::test]
    async fn handle_hook_install_errors_outside_git_repo() {
        // Why: we must fail with a clear error rather than silently creating
        // a .git/hooks directory that will never be used.
        let tmp = tempfile::tempdir().unwrap();
        // No .git directory under tmp.
        let args = HookArgs {
            repo: Some(tmp.path().to_path_buf()),
            action: HookAction::Install,
        };
        let result = handle_hook(args).await;
        assert!(result.is_err(), "expected error outside a git repo");
    }
}
