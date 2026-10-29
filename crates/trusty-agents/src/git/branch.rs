//! Branch listing, creation, and checkout.
//!
//! Why: List operations use libgit2 for speed; creation and checkout shell
//! out to `git` so the user's hooks (`post-checkout`), credential helpers,
//! and any `safe.directory` config still apply.
//! What: `list_branches` (libgit2); `create_branch` and `checkout`
//! (`git -C root ...`).
//! Test: `list_branches_includes_current` lists a temporary repo and asserts
//! `main` is the current branch.

use std::path::Path;

use git2::BranchType;

use super::repo::GitRepo;

/// Branch metadata.
///
/// Why: Carry the upstream tracking ref so the LLM can advise about push
/// targets without an extra round-trip.
/// What: `name` is the short ref name (no `refs/heads/`); `upstream` is
/// the remote-tracking branch name when set.
#[derive(Debug, Clone)]
pub struct BranchInfo {
    pub name: String,
    pub is_current: bool,
    pub upstream: Option<String>,
}

/// List local (and optionally remote) branches.
///
/// Why: Enumerating branches via libgit2 avoids parsing `git branch --list`.
/// What: Iterates `Repository::branches`; for each local branch, queries
/// upstream and compares to HEAD to populate `is_current`.
/// Test: `list_branches_includes_current`.
pub fn list_branches(repo: &GitRepo, include_remote: bool) -> anyhow::Result<Vec<BranchInfo>> {
    let r = repo.inner();
    let head_short = r.head().ok().and_then(|h| h.shorthand().map(String::from));
    let filter = if include_remote {
        None
    } else {
        Some(BranchType::Local)
    };
    let branches = r
        .branches(filter)
        .map_err(|e| anyhow::anyhow!("failed to list branches: {e}"))?;
    let mut out = Vec::new();
    for branch_res in branches {
        let (branch, btype) = match branch_res {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(error = %e, "skip branch entry");
                continue;
            }
        };
        let name = branch.name().ok().flatten().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        let upstream = if matches!(btype, BranchType::Local) {
            branch
                .upstream()
                .ok()
                .and_then(|u| u.name().ok().flatten().map(String::from))
        } else {
            None
        };
        let is_current =
            matches!(btype, BranchType::Local) && head_short.as_deref() == Some(name.as_str());
        out.push(BranchInfo {
            name,
            is_current,
            upstream,
        });
    }
    Ok(out)
}

/// Create a new local branch from HEAD via `git checkout -b`.
///
/// Why: We shell out so any `post-checkout` hooks (e.g. nvm/Volta version
/// switch, dependency reinstall) fire naturally as the user expects.
/// What: `git -C <root> checkout -b <name>` and returns combined output.
/// Test: Indirect — exercised by integration tests; happy path is straight
/// shell forwarding.
pub async fn create_branch(name: &str, root: &Path) -> anyhow::Result<String> {
    run_git(root, &["checkout", "-b", name]).await
}

/// Checkout an existing branch, tag, or commit.
///
/// Why: Shelling out preserves hooks and ref-resolution rules (e.g.
/// `core.autocrlf`) the user has configured.
/// What: `git -C <root> checkout <target>`.
pub async fn checkout(target: &str, root: &Path) -> anyhow::Result<String> {
    run_git(root, &["checkout", target]).await
}

/// Run `git -C <root> <args...>`, returning combined stdout + stderr.
///
/// Why: We surface stderr to the LLM because git writes useful progress
/// information (`Switched to a new branch`, push targets, etc.) there.
/// What: Spawns the child via `tokio::process::Command`, awaits exit,
/// returns `Ok(combined)` on success and `Err` containing stderr on
/// non-zero exit.
/// Test: `run_git_returns_version_output`.
pub async fn run_git(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to spawn git: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "git {} failed (exit {:?}): {}",
            args.join(" "),
            output.status.code(),
            if stderr.is_empty() { stdout } else { stderr }
        ));
    }
    let mut combined = stdout;
    if !stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    Ok(combined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_branches_includes_current() {
        // #9619: build a private repo with a local `main` so the test does
        // not depend on the ambient checkout (detached HEAD, no `.git`).
        let dir = tempfile::tempdir().expect("tempdir");
        let raw = git2::Repository::init(dir.path()).expect("init repo");
        raw.set_head("refs/heads/main").expect("point HEAD at main");
        let sig = git2::Signature::now("test", "test@example.com").expect("signature");
        let tree_id = raw
            .index()
            .expect("index")
            .write_tree()
            .expect("write tree");
        let tree = raw.find_tree(tree_id).expect("find tree");
        raw.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .expect("initial commit");

        let repo = GitRepo::open(dir.path()).expect("open temp repo");
        let branches = list_branches(&repo, false).expect("list local branches");
        let current: Vec<&str> = branches
            .iter()
            .filter(|b| b.is_current)
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(current, vec!["main"], "all branches: {branches:?}");
    }

    #[tokio::test]
    async fn run_git_returns_version_output() {
        let cwd = std::env::current_dir().unwrap();
        let out = run_git(&cwd, &["--version"]).await.expect("git --version");
        assert!(out.contains("git version"), "got: {out}");
    }
}
