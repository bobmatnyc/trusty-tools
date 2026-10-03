//! Unit tests for [`super`] — the operator-listed checkout rules (#8524, #7905).

use super::*;

/// A main checkout fixture: a directory whose `.git` is a directory.
fn checkout() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("cron-reports");
    std::fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
    (dir, repo)
}

fn tail(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}

/// Run the policy for `command` (verb `reset`, tail `args`) in `repo`.
fn exempt(
    command: &str,
    args: &[&str],
    repo: &Path,
    list: &[PathBuf],
    probe: Option<bool>,
) -> bool {
    reset_keep_exempt_with(
        (command, "reset", &tail(args), repo),
        || list.to_vec(),
        |_, _| probe,
    )
}

/// 🔴 REGRESSION (#8524): the reported `git reset --keep origin/main` in a
/// listed runtime checkout whose content already equals the target.
#[test]
fn reset_keep_exempt_allows_the_reported_shape() {
    let (_dir, repo) = checkout();
    let list = [repo.clone()];
    for (command, args) in [
        (
            "git reset --keep origin/main",
            &["--keep", "origin/main"][..],
        ),
        ("git reset --keep", &["--keep"][..]),
        (
            "cd /x && git reset -q --keep HEAD~1",
            &["-q", "--keep", "HEAD~1"][..],
        ),
    ] {
        assert!(exempt(command, args, &repo, &list, Some(true)), "{command}");
    }
}

#[test]
fn reset_keep_exempt_refuses_everything_else() {
    let (_dir, repo) = checkout();
    let list = [repo.clone()];
    let keep = &["--keep", "origin/main"][..];
    let cmd = "git reset --keep origin/main";
    // Not listed; content differs.
    assert!(!exempt(cmd, keep, &repo, &[], Some(true)));
    assert!(!exempt(cmd, keep, &repo, &list, Some(false)));
    // Other destructive forms on the same listed path stay blocked.
    for args in [
        &["--hard", "origin/main"][..],
        &["--keep", "--hard"][..],
        &["--keep", "a", "b"][..],
        &["--keep", "--", "f"][..],
        &["--merge"][..],
    ] {
        assert!(!exempt(cmd, args, &repo, &list, Some(true)), "{args:?}");
    }
    // Anything chained could change the tree after the probe.
    for command in [
        "echo x > a && git reset --keep origin/main",
        "git reset --keep origin/main && git reset --keep HEAD~1",
    ] {
        assert!(
            !exempt(command, keep, &repo, &list, Some(true)),
            "{command}"
        );
    }
    // A verb other than reset.
    let checkout_tail = tail(&["--keep"]);
    assert!(!reset_keep_exempt_with(
        (cmd, "checkout", &checkout_tail, &repo),
        || list.to_vec(),
        |_, _| Some(true),
    ));
}

/// Fail-Open Check (#8524): a probe git could not answer is not exempt.
#[test]
fn reset_keep_exempt_fails_closed_when_the_probe_cannot_answer() {
    let (_dir, repo) = checkout();
    let list = [repo.clone()];
    assert!(!exempt(
        "git reset --keep origin/main",
        &["--keep", "origin/main"],
        &repo,
        &list,
        None
    ));
    // The real probe against a fabricated `.git` cannot run: also not exempt.
    assert_eq!(content_matches(&repo, "HEAD"), None);
}

#[test]
fn listed_matches_by_canonical_path_only() {
    let (dir, repo) = checkout();
    assert!(listed(&repo, &[repo.join(".git/..")]));
    assert!(!listed(&repo, &[dir.path().to_path_buf()]));
    assert!(!listed(
        &repo,
        &[PathBuf::from("/nonexistent/cron-reports")]
    ));
    assert!(!listed(
        Path::new("/nonexistent"),
        std::slice::from_ref(&repo)
    ));
}
