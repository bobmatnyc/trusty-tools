//! Has the repo moved past the snapshot a resume is about to act on? (#7501)
//!
//! Why: a pause snapshot records the work in flight at pause time, and the
//! session that wrote it usually keeps working afterwards. The observed case
//! (adaptive-crm, 2026-09-11) had the snapshot's `in_progress` still naming
//! agents that had since finished and its `next_steps` fully stale within ten
//! minutes — so the resuming PM re-planned work that was already merged. The
//! repo itself is the evidence that settles it: commits landed after the
//! snapshot's recorded `Last commit` are work the snapshot cannot know about.
//! What: `assess_snapshot` reads the resolved snapshot's `## Git Context`,
//! takes the commit it recorded, and asks git what has landed since. Every
//! failure — an unreadable snapshot, no recorded commit, a commit git no longer
//! knows, a directory that is not a checkout — answers "not superseded", because
//! a wrong `superseded: true` would tell a PM to discard state it still needs.
//! Test: `supersedes_when_commits_landed_after_the_snapshot`,
//! `not_superseded_when_the_snapshot_commit_is_head`,
//! `an_unknown_commit_is_not_superseded`,
//! `a_snapshot_without_a_git_context_is_not_superseded`,
//! `the_commit_list_is_capped_but_the_total_is_not`.

use std::path::Path;

/// How many commits-since lines reach the response.
///
/// Why: the catch-up body is already budgeted (#5557) and a long-abandoned
/// snapshot can sit thousands of commits back; the PM needs enough to recognize
/// what landed, not the whole log. The uncapped count still travels, so a capped
/// list never reads as a complete one.
/// What: 20 lines, newest first.
/// Test: `the_commit_list_is_capped_but_the_total_is_not`.
const MAX_COMMITS_LISTED: usize = 20;

/// The prefix `catchup::pause` writes for the snapshot's recorded commit.
const LAST_COMMIT_PREFIX: &str = "Last commit:";

/// The prefix `catchup::pause` writes for the branch the snapshot was paused on.
const BRANCH_PREFIX: &str = "Branch:";

/// The section that prefix lives under.
const GIT_CONTEXT_HEADING: &str = "## Git Context";

/// What the repo has done since a snapshot was written (#7501).
///
/// Why: the resuming PM needs three separable facts — whether to distrust the
/// snapshot's plan at all, what landed, and how much landed beyond the listed
/// sample. Collapsing them into one boolean would leave a PM told "superseded"
/// with nothing to re-plan from.
/// What: `superseded` is `total_since > 0`; `commits_since` is at most
/// [`MAX_COMMITS_LISTED`] `<short-sha> <subject>` lines, newest first;
/// `total_since` is the uncapped count. `Default` is the fail-open answer every
/// unresolvable case returns.
/// Test: see the module doc.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SnapshotFreshness {
    /// Whether commits landed after the snapshot's recorded commit.
    pub(crate) superseded: bool,
    /// The newest commits since, capped at [`MAX_COMMITS_LISTED`].
    pub(crate) commits_since: Vec<String>,
    /// How many commits landed since, uncapped.
    pub(crate) total_since: usize,
}

/// Decide whether `snapshot` has been overtaken by `project_dir`'s history.
///
/// Why: the one entry point `session_context_catchup` calls, so the fail-open
/// rule lives in a single place. See the module doc for why every failure
/// answers "not superseded".
/// What: reads the snapshot file, extracts its recorded commit via
/// [`recorded_commit`], and hands it to [`commits_since`]. Any missing step
/// yields [`SnapshotFreshness::default`].
/// Test: `supersedes_when_commits_landed_after_the_snapshot`,
/// `a_snapshot_without_a_git_context_is_not_superseded`.
pub(crate) fn assess_snapshot(project_dir: &Path, snapshot: &Path) -> SnapshotFreshness {
    let Ok(markdown) = std::fs::read_to_string(snapshot) else {
        return SnapshotFreshness::default();
    };
    let Some(commit) = recorded_commit(&markdown) else {
        return SnapshotFreshness::default();
    };
    let Some(since) = commits_since(project_dir, &commit) else {
        return SnapshotFreshness::default();
    };
    let total_since = since.len();
    SnapshotFreshness {
        superseded: total_since > 0,
        commits_since: since.into_iter().take(MAX_COMMITS_LISTED).collect(),
        total_since,
    }
}

/// The commit id a pause snapshot recorded, if it recorded one.
///
/// Why: the recorded line is `Last commit: <short-sha> <subject>`, and the
/// subject is free text an operator wrote — feeding the whole line to git would
/// turn a commit message containing a space into a revision-parse error. Taking
/// only the first token, and only inside `## Git Context`, is what keeps a
/// `Last commit:` line quoted in a summary from being read as the real one.
/// What: scans for [`GIT_CONTEXT_HEADING`], then for [`LAST_COMMIT_PREFIX`]
/// before the next `## ` heading, and returns the first whitespace-delimited
/// token when it looks like an abbreviated object id (at least four hex digits).
/// Test: `a_snapshot_without_a_git_context_is_not_superseded`,
/// `a_non_hex_commit_token_is_rejected`.
pub(crate) fn recorded_commit(markdown: &str) -> Option<String> {
    let token = git_context_token(markdown, LAST_COMMIT_PREFIX)?;
    let looks_like_a_commit = token.len() >= 4 && token.chars().all(|c| c.is_ascii_hexdigit());
    looks_like_a_commit.then(|| token.to_string())
}

/// The branch a pause snapshot recorded in its `## Git Context`, if any.
///
/// Why (#9390): the resume verify pass compares a `git rev-parse <branch>`
/// result with [`recorded_commit`] only when the snapshot was paused on that
/// branch; this is the same section reader, so the two cannot disagree.
/// Test: `the_recorded_branch_comes_from_the_git_context_only`.
pub(crate) fn recorded_branch(markdown: &str) -> Option<String> {
    git_context_token(markdown, BRANCH_PREFIX).map(str::to_string)
}

/// The first token after `prefix` on a line inside `## Git Context`.
fn git_context_token<'a>(markdown: &'a str, prefix: &str) -> Option<&'a str> {
    let mut in_git_context = false;
    for line in markdown.lines() {
        if line.starts_with("## ") {
            in_git_context = line.trim_end() == GIT_CONTEXT_HEADING;
            continue;
        }
        if in_git_context && let Some(rest) = line.strip_prefix(prefix) {
            return rest.split_whitespace().next();
        }
    }
    None
}

/// The commits `project_dir`'s `HEAD` has gained since `commit`.
///
/// Why: `HEAD` rather than a named branch. A PM session runs in its own
/// checkout, and that checkout's `HEAD` is the history the resuming session will
/// actually read — on a main checkout it IS main, and on a branch checkout main
/// is not what the snapshot's plan was written against. A commit git no longer
/// resolves (a squash-merged branch tip, a pruned worktree) makes `git log` exit
/// non-zero, which is reported as "cannot tell", never as "superseded".
/// What: `git -C <dir> log --oneline --no-decorate <commit>..HEAD`. `None` on any
/// spawn failure or non-zero exit; `Some(vec![])` when the range is empty.
/// Test: `supersedes_when_commits_landed_after_the_snapshot`,
/// `not_superseded_when_the_snapshot_commit_is_head`,
/// `an_unknown_commit_is_not_superseded`.
fn commits_since(project_dir: &Path, commit: &str) -> Option<Vec<String>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(["log", "--oneline", "--no-decorate"])
        .arg(format!("{commit}..HEAD"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git must be runnable in this fixture");
        assert!(status.status.success(), "git {args:?} failed");
    }

    /// A repo with one commit, plus a snapshot naming that commit.
    fn repo_with_snapshot(tmp: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = tmp.path().to_path_buf();
        git(&dir, &["init"]);
        git(&dir, &["config", "user.email", "t@t.com"]);
        git(&dir, &["config", "user.name", "T"]);
        std::fs::write(dir.join("a.txt"), b"a").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "first"]);
        let head = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["log", "-1", "--format=%h %s"])
            .output()
            .unwrap();
        let last_commit = String::from_utf8_lossy(&head.stdout).trim().to_string();
        let snapshot = tmp.path().join("session-x.md");
        std::fs::write(
            &snapshot,
            format!(
                "# Session Pause - now\n\n## Summary\nWork.\n\n## Git Context\nBranch: main\n\
                 Last commit: {last_commit}\nUncommitted changes: clean\n"
            ),
        )
        .unwrap();
        (dir, snapshot)
    }

    fn commit_file(dir: &Path, name: &str, subject: &str) {
        std::fs::write(dir.join(name), name.as_bytes()).unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", subject]);
    }

    /// Why (#9390): PM free text loses its SHA-like tokens, and #7501 reads the
    /// one SHA a snapshot must keep: the code-generated `Last commit:` line.
    /// What: a snapshot written by the real writer, whose summary also names the
    /// HEAD commit, keeps `Last commit: <short> <subject>` verbatim and drops the
    /// summary's copy; a commit made after it is still reported as superseding.
    /// Test: itself.
    #[test]
    fn last_commit_baseline_survives_and_superseded_still_detects_a_moved_head() {
        use trusty_common::catchup::pause::{PauseSnapshotInput, write_pause_snapshot};

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        git(dir, &["init"]);
        git(dir, &["config", "user.email", "t@t.com"]);
        git(dir, &["config", "user.name", "T"]);
        commit_file(dir, "a.txt", "first");
        let rev = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let short = rev(&["log", "-1", "--format=%h"]);
        // The full id: a 7-char short id can be all digits, which no rule flags.
        let full = rev(&["rev-parse", "HEAD"]);

        let summary = format!("Paused at {full}; PR #12 head d3963848ab.");
        let input = PauseSnapshotInput {
            session_id: "s-7501",
            summary: &summary,
            completed: &[],
            in_progress: &[],
            next_steps: &[],
            tmux_window: None,
        };
        let snapshot = write_pause_snapshot(dir, &input).unwrap().snapshot_path;
        let md = std::fs::read_to_string(&snapshot).unwrap();

        assert!(
            md.lines()
                .any(|l| l == format!("Last commit: {short} first")),
            "the #7501 baseline stays exactly as git reports it:\n{md}"
        );
        assert_eq!(recorded_commit(&md).as_deref(), Some(short.as_str()));
        let summary_body = md
            .split("## Summary\n")
            .nth(1)
            .and_then(|rest| rest.split("\n## ").next())
            .unwrap_or_default();
        assert!(
            !summary_body.contains(&full) && !summary_body.contains("d3963848ab"),
            "the PM's copy of a head id is flagged, not kept:\n{md}"
        );
        assert_eq!(
            assess_snapshot(dir, &snapshot),
            SnapshotFreshness::default(),
            "nothing landed after the pause yet"
        );

        commit_file(dir, "b.txt", "landed after the pause");
        let freshness = assess_snapshot(dir, &snapshot);
        assert!(freshness.superseded, "a moved HEAD must still be detected");
        assert_eq!(freshness.total_since, 1);
    }

    #[test]
    fn not_superseded_when_the_snapshot_commit_is_head() {
        let tmp = TempDir::new().unwrap();
        let (dir, snapshot) = repo_with_snapshot(&tmp);

        assert_eq!(
            assess_snapshot(&dir, &snapshot),
            SnapshotFreshness::default(),
            "nothing landed after the pause, so the snapshot still describes HEAD"
        );
    }

    /// THE #7501 REGRESSION TEST — the case the tool used to report nothing for.
    #[test]
    fn supersedes_when_commits_landed_after_the_snapshot() {
        let tmp = TempDir::new().unwrap();
        let (dir, snapshot) = repo_with_snapshot(&tmp);
        commit_file(
            &dir,
            "b.txt",
            "the work the snapshot still calls in progress",
        );

        let freshness = assess_snapshot(&dir, &snapshot);

        assert!(freshness.superseded, "a commit landed after the pause");
        assert_eq!(freshness.total_since, 1);
        assert!(
            freshness.commits_since[0].ends_with("the work the snapshot still calls in progress"),
            "the commit made since must be listed: {:?}",
            freshness.commits_since
        );
    }

    #[test]
    fn the_commit_list_is_capped_but_the_total_is_not() {
        let tmp = TempDir::new().unwrap();
        let (dir, snapshot) = repo_with_snapshot(&tmp);
        for n in 0..MAX_COMMITS_LISTED + 3 {
            commit_file(&dir, &format!("f{n}.txt"), &format!("commit {n}"));
        }

        let freshness = assess_snapshot(&dir, &snapshot);

        assert_eq!(freshness.commits_since.len(), MAX_COMMITS_LISTED);
        assert_eq!(freshness.total_since, MAX_COMMITS_LISTED + 3);
    }

    #[test]
    fn an_unknown_commit_is_not_superseded() {
        let tmp = TempDir::new().unwrap();
        let (dir, _) = repo_with_snapshot(&tmp);
        let snapshot = tmp.path().join("orphan.md");
        std::fs::write(
            &snapshot,
            "## Git Context\nLast commit: deadbeef gone with a squash merge\n",
        )
        .unwrap();

        assert_eq!(
            assess_snapshot(&dir, &snapshot),
            SnapshotFreshness::default(),
            "a commit git cannot resolve is 'cannot tell', never 'superseded'"
        );
    }

    #[test]
    fn a_snapshot_without_a_git_context_is_not_superseded() {
        let tmp = TempDir::new().unwrap();
        let (dir, _) = repo_with_snapshot(&tmp);
        let snapshot = tmp.path().join("bare.md");
        std::fs::write(
            &snapshot,
            "# Session Pause - now\n\n## Summary\nLast commit: x\n",
        )
        .unwrap();

        assert_eq!(
            assess_snapshot(&dir, &snapshot),
            SnapshotFreshness::default()
        );
        assert_eq!(
            assess_snapshot(&dir, &tmp.path().join("absent.md")),
            SnapshotFreshness::default(),
            "an unreadable snapshot answers the same fail-open way"
        );
    }

    #[test]
    fn a_non_hex_commit_token_is_rejected() {
        assert_eq!(
            recorded_commit("## Git Context\nLast commit: HEAD~1 some subject\n"),
            None,
            "only an abbreviated object id may reach git"
        );
        assert_eq!(
            recorded_commit("## Git Context\nLast commit: abc1234 subject\n").as_deref(),
            Some("abc1234")
        );
    }

    /// #9390: a `Branch:` line quoted in the PM's summary is not the recorded one.
    #[test]
    fn the_recorded_branch_comes_from_the_git_context_only() {
        let markdown = "## Summary\nBranch: quoted\n\n## Git Context\nBranch: feat/x\n\
                        Last commit: abc1234 s\n\n## Tmux Window\nBranch: later\n";
        assert_eq!(recorded_branch(markdown).as_deref(), Some("feat/x"));
        assert_eq!(recorded_branch("## Summary\nBranch: quoted\n"), None);
    }
}
