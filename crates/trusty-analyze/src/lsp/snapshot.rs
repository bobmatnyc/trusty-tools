//! The workspace snapshot stamp, `<git head sha>+<dirty hash>` (#9078, L3).
//!
//! Why: DOC-72 §3 stamps every LSP answer with the exact tree it was computed
//! against, so two answers carrying one snapshot provably saw the same bytes.
//! What: [`workspace_snapshot`] reads `HEAD` and the porcelain status of the
//! work tree, then digests the status listing together with the current bytes
//! of every modified or untracked file (xxh3, 16 hex digits). A clean tree
//! digests the empty listing, so it still carries a dirty hash. The whole
//! computation runs under one deadline and a file-count and byte budget
//! ([`SnapshotBudget`]); past any bound it fails closed, never reading an
//! unbounded tree on every call.
//!
//! Untracked files are listed one by one (`--untracked-files=all`, which
//! overrides `status.showUntrackedFiles`): git's `normal` mode collapses a new
//! directory to one `dir/` entry, so an edit inside it would not move the
//! stamp, and a new module is exactly what a language server analyses (#9078).
//! The budget above bounds what that costs and fails closed past it.
//! Test: `snapshot_moves_with_the_dirty_bytes_and_only_with_them`,
//! `snapshot_sees_an_edit_inside_a_new_untracked_directory`,
//! `snapshot_over_its_byte_budget_fails_closed`,
//! `snapshot_budget_bounds_files_bytes_and_time`.
//!
//! # Spec References
//! - [`SPEC-ANALYZELSP-03~draft`](docs/specs/DOC-72-analyze-on-demand-language-server.md#SPEC-ANALYZELSP-03~draft)

use std::path::Path;
use std::time::Duration;

use xxhash_rust::xxh3::Xxh3;

use super::LspError;

/// Bounds on one snapshot; exceeding any one is a typed refusal.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotBudget {
    /// Deadline for the whole snapshot, both `git` calls and every read.
    pub deadline: Duration,
    /// Most dirty or untracked entries hashed.
    pub max_files: usize,
    /// Most bytes of dirty or untracked file content read.
    pub max_bytes: u64,
}

impl Default for SnapshotBudget {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(10),
            max_files: 2_000,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

/// The `<git head sha>+<dirty hash>` stamp of the work tree at `root`, under
/// the default [`SnapshotBudget`].
///
/// # Errors
/// [`LspError::Snapshot`] when `git` cannot run or fails (no commit yet, not a
/// work tree), or a budget bound is exceeded. There is no unstamped fallback.
///
/// Test: `snapshot_moves_with_the_dirty_bytes_and_only_with_them`,
/// `snapshot_over_its_byte_budget_fails_closed`.
pub async fn workspace_snapshot(root: &Path) -> Result<String, LspError> {
    snapshot_within(root, SnapshotBudget::default()).await
}

/// [`workspace_snapshot`] under an explicit `budget`.
///
/// # Errors
/// As [`workspace_snapshot`].
///
/// Test: `snapshot_budget_bounds_files_bytes_and_time`.
pub async fn snapshot_within(root: &Path, budget: SnapshotBudget) -> Result<String, LspError> {
    let fail = |reason: String| LspError::Snapshot {
        root: root.display().to_string(),
        reason,
    };
    tokio::time::timeout(budget.deadline, compute(root, budget))
        .await
        .map_err(|_| fail(format!("over its time budget of {:?}", budget.deadline)))?
}

async fn compute(root: &Path, budget: SnapshotBudget) -> Result<String, LspError> {
    let fail = |reason: String| LspError::Snapshot {
        root: root.display().to_string(),
        reason,
    };
    let head = git(root, &["rev-parse", "--verify", "HEAD"]).await?;
    let head = String::from_utf8_lossy(&head).trim().to_string();
    let status = git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .await?;
    let paths = dirty_paths(&status);
    if paths.len() > budget.max_files {
        return Err(fail(format!(
            "{} dirty entries exceed the file budget of {}",
            paths.len(),
            budget.max_files
        )));
    }
    let mut digest = Xxh3::new();
    digest.update(&status);
    let mut read = 0u64;
    for rel in paths {
        let path = root.join(rel);
        let len = match tokio::fs::metadata(&path).await {
            Ok(m) if m.is_file() => m.len(),
            // A deleted file or a directory entry: its status line is hashed.
            _ => {
                digest.update(&[0]);
                continue;
            }
        };
        read = read.saturating_add(len);
        if read > budget.max_bytes {
            return Err(fail(format!(
                "dirty content exceeds the byte budget of {} bytes",
                budget.max_bytes
            )));
        }
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| fail(format!("read {rel}: {e}")))?;
        digest.update(&[1]);
        digest.update(&(bytes.len() as u64).to_le_bytes());
        digest.update(&bytes);
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
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        // #9078: the stamp describes `root`, whatever the caller's git env says.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(std::process::Stdio::null())
        // The caller's deadline drops this future; the child dies with it.
        .kill_on_drop(true)
        .output()
        .await
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
