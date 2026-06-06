//! Staging and commit creation.
//!
//! Why: Both must shell out: staging because users sometimes have
//! `pre-add` clean filters, and commit because `pre-commit` /
//! `commit-msg` / `prepare-commit-msg` hooks plus GPG signing are
//! essential to keeping the harness's commits indistinguishable from a
//! human user's.
//! What: `stage_files(files, root)` runs `git add -- file ...`;
//! `create_commit(message, root)` runs `git commit -m <msg>`.
//! Test: We don't drive a real commit in unit tests (would dirty the
//! working tree). Argument-passing is exercised via the tools layer.

use std::path::Path;

use super::branch::run_git;

/// Stage specific files via `git add`.
///
/// Why: Per-file staging is the safest default for an LLM — it never
/// accidentally commits unrelated work in progress.
/// What: `git -C root add -- file1 file2 ...`. The `--` guards against
/// filenames that look like flags.
/// Test: `stage_files_rejects_empty_list`.
pub async fn stage_files(files: &[String], root: &Path) -> anyhow::Result<String> {
    if files.is_empty() {
        anyhow::bail!("no files to stage");
    }
    let mut args: Vec<&str> = vec!["add", "--"];
    for f in files {
        args.push(f.as_str());
    }
    run_git(root, &args).await
}

/// Create a commit with the given message.
///
/// Why: Preserves hooks and signing. The LLM is responsible for staging
/// what it wants committed beforehand (via `git_stage`).
/// What: `git -C root commit -m <message>`. Returns the combined output
/// which includes the new commit's sha line.
/// Test: Argument-passing is covered by the tools-layer schema tests.
pub async fn create_commit(message: &str, root: &Path) -> anyhow::Result<String> {
    if message.trim().is_empty() {
        anyhow::bail!("commit message must not be empty");
    }
    run_git(root, &["commit", "-m", message]).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stage_files_rejects_empty_list() {
        let cwd = std::env::current_dir().unwrap();
        let res = stage_files(&[], &cwd).await;
        assert!(res.is_err(), "empty file list should error");
    }

    #[tokio::test]
    async fn create_commit_rejects_empty_message() {
        let cwd = std::env::current_dir().unwrap();
        let res = create_commit("   ", &cwd).await;
        assert!(res.is_err());
    }
}
