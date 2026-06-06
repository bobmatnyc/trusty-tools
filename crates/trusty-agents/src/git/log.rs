//! Commit history walking and search.
//!
//! Why: Agents need to inspect recent history ("what changed lately?",
//! "is there a feat: commit about X?") without forking `git log`. libgit2's
//! revwalk gives us indexed traversal that's faster than spawning a process
//! for short queries.
//! What: `get_log` walks HEAD and returns up to `limit` `CommitInfo`s;
//! `search_commits` filters by substring on the commit message.
//! `format_log` renders a compact per-commit block.
//! Test: Unit tests against the trusty-agents repo.

use chrono::{TimeZone, Utc};
use git2::Sort;

use super::repo::GitRepo;

/// One commit's metadata.
///
/// Why: Stable shape for serialization; carries everything the LLM needs
/// to reason about a commit (sha, who, when, headline) without dragging
/// the full diff.
/// What: `sha` is the 8-char short prefix; `message` is the first line
/// only; `timestamp` is ISO 8601 UTC.
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub sha: String,
    pub author: String,
    pub message: String,
    pub timestamp: String,
}

fn build_commit_info(commit: &git2::Commit<'_>) -> CommitInfo {
    let sha = commit.id().to_string();
    let short = if sha.len() >= 8 { &sha[..8] } else { &sha[..] };
    let author = commit.author();
    let author_name = author.name().unwrap_or("<unknown>").to_string();
    let raw_msg = commit.message().unwrap_or("");
    let first_line = raw_msg.lines().next().unwrap_or("").to_string();
    let ts = Utc.timestamp_opt(commit.time().seconds(), 0).single();
    let timestamp = ts
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| "0000-00-00T00:00:00Z".to_string());
    CommitInfo {
        sha: short.to_string(),
        author: author_name,
        message: first_line,
        timestamp,
    }
}

/// Get recent commits from HEAD.
///
/// Why: The most common log query is "the last N commits". A bounded walk
/// keeps the tool result small.
/// What: Walks HEAD with `TIME` sort (newest first), takes `limit`,
/// resolves each oid to a `Commit`, and returns the `CommitInfo` list.
/// Test: `get_log_returns_recent_commits`.
pub fn get_log(repo: &GitRepo, limit: usize) -> anyhow::Result<Vec<CommitInfo>> {
    let r = repo.inner();
    let mut walk = r.revwalk().map_err(|e| anyhow::anyhow!("revwalk: {e}"))?;
    walk.set_sorting(Sort::TIME)
        .map_err(|e| anyhow::anyhow!("revwalk sort: {e}"))?;
    walk.push_head()
        .map_err(|e| anyhow::anyhow!("revwalk push_head: {e}"))?;
    let mut out = Vec::with_capacity(limit);
    for oid_res in walk.take(limit) {
        let oid = match oid_res {
            Ok(o) => o,
            Err(e) => {
                tracing::debug!(error = %e, "revwalk skipped an entry");
                continue;
            }
        };
        if let Ok(c) = r.find_commit(oid) {
            out.push(build_commit_info(&c));
        }
    }
    Ok(out)
}

/// Search commits whose message contains `query` (case-insensitive).
///
/// Why: A free-form substring search avoids forcing the LLM to learn
/// `git log --grep` syntax. Case-insensitive matches casual queries.
/// What: Walks the full revwalk and filters by message substring; stops
/// after collecting `limit` matches.
/// Test: `search_commits_finds_known_keyword`.
pub fn search_commits(
    repo: &GitRepo,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<CommitInfo>> {
    let needle = query.to_lowercase();
    let r = repo.inner();
    let mut walk = r.revwalk().map_err(|e| anyhow::anyhow!("revwalk: {e}"))?;
    walk.set_sorting(Sort::TIME)
        .map_err(|e| anyhow::anyhow!("revwalk sort: {e}"))?;
    walk.push_head()
        .map_err(|e| anyhow::anyhow!("revwalk push_head: {e}"))?;
    let mut out = Vec::with_capacity(limit);
    for oid_res in walk {
        let oid = match oid_res {
            Ok(o) => o,
            Err(_) => continue,
        };
        let Ok(c) = r.find_commit(oid) else { continue };
        let msg = c.message().unwrap_or("").to_lowercase();
        if msg.contains(&needle) {
            out.push(build_commit_info(&c));
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(out)
}

/// Format commits into a compact block.
///
/// Why: Multiline blocks are more readable than CSV and stable to diff in
/// LLM context windows.
/// What: One paragraph per commit: `<sha>  <timestamp>  <author>` then a
/// blank-indented headline.
/// Test: `format_log_includes_sha_and_message`.
pub fn format_log(commits: &[CommitInfo]) -> String {
    if commits.is_empty() {
        return "No commits".to_string();
    }
    let mut s = String::with_capacity(commits.len() * 80);
    for c in commits {
        s.push_str(&format!(
            "{}  {}  {}\n    {}\n",
            c.sha, c.timestamp, c.author, c.message
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_log_returns_recent_commits() {
        let cwd = std::env::current_dir().unwrap();
        let repo = GitRepo::open(&cwd).unwrap();
        let log = get_log(&repo, 3).expect("walk repo log");
        assert!(!log.is_empty(), "trusty-agents has commits");
        assert!(log.len() <= 3);
        for c in &log {
            assert_eq!(c.sha.len(), 8, "sha should be short prefix");
        }
    }

    #[test]
    fn search_commits_finds_known_keyword() {
        let cwd = std::env::current_dir().unwrap();
        let repo = GitRepo::open(&cwd).unwrap();
        // The repo has many "feat:" / "fix:" commits per recent log; one of
        // these should be present.
        let hits = search_commits(&repo, "fix", 5).expect("search ok");
        assert!(
            !hits.is_empty(),
            "expected at least one commit message containing 'fix'"
        );
    }

    #[test]
    fn format_log_includes_sha_and_message() {
        let commits = vec![CommitInfo {
            sha: "abcd1234".into(),
            author: "Alice".into(),
            message: "feat: cool thing".into(),
            timestamp: "2026-04-30T00:00:00+00:00".into(),
        }];
        let out = format_log(&commits);
        assert!(out.contains("abcd1234"));
        assert!(out.contains("feat: cool thing"));
        assert!(out.contains("Alice"));
    }

    #[test]
    fn format_log_empty() {
        assert_eq!(format_log(&[]), "No commits");
    }
}
