//! The workspace snapshot stamp, `<git head sha>+<dirty hash>` (#9078, L3).
//!
//! Why: DOC-72 §3 stamps every LSP answer with the exact tree it was computed
//! against, so two answers carrying one snapshot provably saw the same bytes.
//! What: [`workspace_snapshot`] reads `HEAD` and the porcelain status of the
//! work tree, then digests the status listing together with the current bytes
//! of every modified or untracked file (xxh3, 16 hex digits). A clean tree
//! digests the empty listing, so it still carries a dirty hash.
//! Test: `snapshot_moves_with_the_dirty_bytes_and_only_with_them`,
//! `lsp_references_returns_stamped_locations`.
//!
//! # Spec References
//! - [`SPEC-ANALYZELSP-03~draft`](docs/specs/DOC-72-analyze-on-demand-language-server.md#SPEC-ANALYZELSP-03~draft)

use std::path::Path;
use std::time::Duration;

use xxhash_rust::xxh3::Xxh3;

use super::LspError;

/// Bound on each `git` call, so a wedged repository cannot hold a request.
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// The `<git head sha>+<dirty hash>` stamp of the work tree at `root`.
///
/// # Errors
/// [`LspError::Snapshot`] when `git` cannot run, fails (no commit yet, not a
/// work tree) or exceeds its bound. There is no unstamped fallback.
///
/// Test: `snapshot_moves_with_the_dirty_bytes_and_only_with_them`.
pub async fn workspace_snapshot(root: &Path) -> Result<String, LspError> {
    let head = git(root, &["rev-parse", "--verify", "HEAD"]).await?;
    let head = String::from_utf8_lossy(&head).trim().to_string();
    let status = git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .await?;
    let mut digest = Xxh3::new();
    digest.update(&status);
    for rel in dirty_paths(&status) {
        match tokio::fs::read(root.join(rel)).await {
            Ok(bytes) => {
                digest.update(&[1]);
                digest.update(&(bytes.len() as u64).to_le_bytes());
                digest.update(&bytes);
            }
            // A deleted file or a directory entry: its status line is hashed.
            Err(_) => digest.update(&[0]),
        }
    }
    Ok(format!("{head}+{:016x}", digest.digest()))
}

/// The paths named by `git status --porcelain=v1 -z`, in order. A rename or
/// copy entry is followed by its source path, which names no current file.
fn dirty_paths(status: &[u8]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut fields = status.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(entry) = fields.next() {
        let Some(path) = entry.get(3..).and_then(|p| std::str::from_utf8(p).ok()) else {
            continue;
        };
        out.push(path);
        if matches!(entry.first(), Some(b'R' | b'C')) {
            fields.next();
        }
    }
    out
}

async fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, LspError> {
    let fail = |reason: String| LspError::Snapshot {
        root: root.display().to_string(),
        reason,
    };
    let verb = args.first().copied().unwrap_or_default();
    let run = tokio::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        // #9078: the stamp describes `root`, whatever the caller's git env says.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(GIT_TIMEOUT, run)
        .await
        .map_err(|_| fail(format!("git {verb} took longer than {GIT_TIMEOUT:?}")))?
        .map_err(|e| fail(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(fail(format!("git {verb}: {}", stderr.trim())));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::dirty_paths;

    #[test]
    fn dirty_paths_skips_rename_sources() {
        let status = b" M a.rs\0R  new.rs\0old.rs\0?? dir/b.rs\0";
        assert_eq!(dirty_paths(status), vec!["a.rs", "new.rs", "dir/b.rs"]);
    }
}
