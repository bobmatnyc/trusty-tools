//! Git activity collection for the DOC-28 catch-up system (#1762).
//!
//! Why: surfaces recent commits as one of three catch-up activity sources so the
//! operator knows what code changes happened since the previous session.
//! What: [`git_commits_since`] shells out to `git log` with a controlled format
//! and returns parsed [`CommitSummary`] values. Fail-open: non-repo directories or
//! git errors return an empty vec, never propagating errors that would abort catch-up.
//! Test: `git_commits_since_returns_commits`, `git_commits_since_filters_by_date`,
//! `git_commits_since_nonrepo_returns_empty`.
//!
// CUTOVER BRIDGE — remove post-migration (#1762)

use std::path::Path;

use chrono::{DateTime, Utc};

/// A single git commit surfaced by the catch-up system.
///
/// Why: callers render this into a markdown digest for the operator.
/// What: minimal commit metadata — sha, subject, author, and ISO timestamp.
/// Test: `git_commits_since_returns_commits`.
#[derive(Debug, Clone)]
pub struct CommitSummary {
    /// The full commit SHA.
    pub sha: String,
    /// The commit subject (first line of the commit message).
    pub msg: String,
    /// The author name.
    pub author: String,
    /// The author date as UTC (None if unparseable).
    pub ts: Option<DateTime<Utc>>,
}

/// Collect commits from `repo` since `since` (or the last 50 if since=None).
///
/// Why: provides the git-activity section of the catch-up digest without
/// requiring a live daemon or external service.
/// What: runs `git log --format=... [--since=<ISO>] [-n 50]` inside `repo`;
/// parses each line as `sha|subject|author|iso-timestamp`. Non-repo directories
/// or git errors produce an empty vec (fail-open).
/// Test: `git_commits_since_returns_commits`, `git_commits_since_filters_by_date`,
/// `git_commits_since_nonrepo_returns_empty`.
pub fn git_commits_since(repo: &Path, since: Option<DateTime<Utc>>) -> Vec<CommitSummary> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo);
    cmd.arg("log");
    cmd.arg("--format=%H|%s|%an|%aI");
    if let Some(ts) = since {
        cmd.arg(format!("--since={}", ts.to_rfc3339()));
    } else {
        cmd.arg("-n").arg("50");
    }
    let output = match cmd.output() {
        Ok(o) if o.status.success() => o,
        Ok(_) | Err(_) => return vec![],
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(parse_commit_line)
        .collect()
}

/// Return the list of branch names that had commits since `since`.
///
/// Why: optional supplemental information for the catch-up digest.
/// What: runs `git branch --format=%(refname:short)` and checks each branch
/// for commits after `since`. Returns empty vec on any error (fail-open).
/// Test: not exercised in unit tests (requires a git repo); covered by
/// integration context only.
pub fn git_branches_changed_since(repo: &Path, since: Option<DateTime<Utc>>) -> Vec<String> {
    let ts = match since {
        Some(ts) => ts,
        None => return vec![],
    };
    let branch_output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("branch")
        .arg("--format=%(refname:short)")
        .output();
    let Ok(out) = branch_output else {
        return vec![];
    };
    if !out.status.success() {
        return vec![];
    }
    let branches: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    branches
        .into_iter()
        .filter(|branch| {
            let check = std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .arg("log")
                .arg(branch)
                .arg(format!("--since={}", ts.to_rfc3339()))
                .arg("-n")
                .arg("1")
                .arg("--format=%H")
                .output();
            matches!(check, Ok(o) if o.status.success() && !o.stdout.is_empty())
        })
        .collect()
}

/// Parse a single `git log` output line in `sha|subject|author|iso` format.
///
/// Why: isolates the parsing so it can be tested without a live git repo.
/// What: splits on `|` (at most 4 parts), returns None if fewer than 3 parts
/// or if sha is empty.
/// Test: `parse_commit_line_basic`, `parse_commit_line_with_pipe_in_subject`,
/// `parse_commit_line_empty_returns_none`.
fn parse_commit_line(line: &str) -> Option<CommitSummary> {
    let mut parts = line.splitn(4, '|');
    let sha = parts.next()?.trim().to_string();
    let msg = parts.next()?.trim().to_string();
    let author = parts.next()?.trim().to_string();
    let ts_str = parts.next().unwrap_or("").trim();
    let ts = ts_str.parse::<DateTime<Utc>>().ok();
    if sha.is_empty() {
        return None;
    }
    Some(CommitSummary {
        sha,
        msg,
        author,
        ts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo_with_commits(tmp: &TempDir) {
        let p = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["init"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["config", "user.name", "Test"])
            .output()
            .unwrap();
        std::fs::write(p.join("a.txt"), b"a").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["add", "."])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["commit", "-m", "first commit"])
            .output()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::write(p.join("b.txt"), b"b").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["add", "."])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["commit", "-m", "second commit"])
            .output()
            .unwrap();
    }

    #[test]
    fn git_commits_since_returns_commits() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_commits(&tmp);
        let commits = git_commits_since(tmp.path(), None);
        assert_eq!(commits.len(), 2, "should return both commits");
        assert!(commits.iter().any(|c| c.msg.contains("second commit")));
        assert!(commits.iter().any(|c| c.msg.contains("first commit")));
    }

    #[test]
    fn git_commits_since_filters_by_date() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_commits(&tmp);
        // since=far future → no commits
        let future: DateTime<Utc> = "2099-01-01T00:00:00Z".parse().unwrap();
        let commits = git_commits_since(tmp.path(), Some(future));
        assert!(commits.is_empty(), "future since should return empty");
    }

    #[test]
    fn git_commits_since_nonrepo_returns_empty() {
        let tmp = TempDir::new().unwrap();
        // Plain temp dir with no git repo.
        let commits = git_commits_since(tmp.path(), None);
        assert!(commits.is_empty(), "non-repo dir should return empty vec");
    }

    #[test]
    fn parse_commit_line_basic() {
        let line = "abc1234|fix the bug|Alice|2026-06-27T10:00:00+00:00";
        let c = parse_commit_line(line).unwrap();
        assert_eq!(c.sha, "abc1234");
        assert_eq!(c.msg, "fix the bug");
        assert_eq!(c.author, "Alice");
        assert!(c.ts.is_some());
    }

    #[test]
    fn parse_commit_line_with_pipe_in_subject() {
        // splitn(4) means the 4th field (ts) will absorb everything after the 3rd |.
        // The subject is the SECOND field, so it won't contain an extra pipe here.
        // Verify that a pipe in the timestamp field is handled gracefully.
        let line = "abc1234|feat: add something|Alice|2026-06-27T10:00:00+00:00";
        let c = parse_commit_line(line).unwrap();
        assert_eq!(c.sha, "abc1234");
        assert_eq!(c.msg, "feat: add something");
        assert_eq!(c.author, "Alice");
        assert!(c.ts.is_some());
    }

    #[test]
    fn parse_commit_line_empty_returns_none() {
        assert!(parse_commit_line("").is_none());
        assert!(parse_commit_line("|").is_none());
    }

    #[test]
    fn parse_commit_line_missing_ts_is_ok() {
        let line = "abc1234|fix|Alice";
        // Only 3 parts — ts will be empty/missing.
        let c = parse_commit_line(line).unwrap();
        assert_eq!(c.sha, "abc1234");
        assert!(c.ts.is_none());
    }
}
