//! Tests for the per-directory repository resolution (#7057).
//!
//! Why: the bug this module exists to close is invisible in behaviour — a
//! lookup aimed at the wrong repository answers "no pull request" exactly as a
//! correct lookup against a branch with no pull request does. Only the resolved
//! slug distinguishes them, so every test here asserts on the slug.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

/// Run `git -C <dir> <args>`, panicking with git's own stderr on failure.
fn git_ok(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("fixture: `git {}` could not be run: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "fixture: `git {}` failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A checkout at `<tmp>/<name>` with one commit and `origin` set to `origin`.
///
/// The remote is never contacted — `git config remote.origin.url` is a local
/// read — so a URL naming a repository that does not exist is fine here and is
/// what keeps these tests hermetic.
fn checkout_with_origin(tmp: &Path, name: &str, origin: &str) -> PathBuf {
    let repo = tmp.join(name);
    std::fs::create_dir_all(&repo).expect("fixture: create repo dir");
    git_ok(&repo, &["init", "--initial-branch=main"]);
    git_ok(&repo, &["config", "user.email", "ci@test.invalid"]);
    git_ok(&repo, &["config", "user.name", "CI"]);
    git_ok(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("README.md"), "base\n").expect("fixture: write README");
    git_ok(&repo, &["add", "README.md"]);
    git_ok(&repo, &["commit", "-m", "base"]);
    git_ok(&repo, &["remote", "add", "origin", origin]);
    repo
}

#[test]
fn https_remote_yields_its_owner_and_repo() {
    for url in [
        "https://github.com/1m-consulting/adaptive-crm.git",
        "https://github.com/1m-consulting/adaptive-crm",
        "https://user@github.com/1m-consulting/adaptive-crm.git/",
        "http://github.example.invalid/1m-consulting/adaptive-crm.git",
    ] {
        assert_eq!(
            parse_repo_slug(url).as_deref(),
            Some("1m-consulting/adaptive-crm"),
            "{url}"
        );
    }
}

#[test]
fn ssh_remote_yields_its_owner_and_repo() {
    for url in [
        "ssh://git@github.com/hotstats/hotstats-product-poc.git",
        "ssh://git@github.com:22/hotstats/hotstats-product-poc",
        "git://github.com/hotstats/hotstats-product-poc.git",
    ] {
        assert_eq!(
            parse_repo_slug(url).as_deref(),
            Some("hotstats/hotstats-product-poc"),
            "{url}"
        );
    }
}

#[test]
fn scp_like_remote_yields_its_owner_and_repo() {
    assert_eq!(
        parse_repo_slug("git@github.com:bobmatnyc/trusty-tools.git").as_deref(),
        Some("bobmatnyc/trusty-tools")
    );
}

/// 🔴 The fail-closed case that matters most: a filesystem remote's tail LOOKS
/// like `owner/repo`, and accepting it would aim a live `gh` call at whatever
/// repository happened to share those two names.
#[test]
fn a_local_path_remote_names_no_repository() {
    for url in [
        "/tmp/fixtures/remote.git",
        "../sibling/remote.git",
        "C:\\repos\\remote.git",
        "",
        "github.com",
    ] {
        assert_eq!(parse_repo_slug(url), None, "{url}");
    }
}

#[test]
fn a_file_url_remote_names_no_repository() {
    // `file://` reaches the scp-like branch, whose path is absolute — the guard
    // that keeps a local clone from resolving to a GitHub slug.
    assert_eq!(parse_repo_slug("file:///tmp/fixtures/remote.git"), None);
}

/// 🔴 #7057: the whole point. Two directories on ONE machine, two origins, two
/// answers — never one repository standing in for the other.
#[test]
fn two_worktrees_with_different_origins_resolve_to_different_repos() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let a = checkout_with_origin(
        tmp.path(),
        "adaptive-crm",
        "https://github.com/1m-consulting/adaptive-crm.git",
    );
    let b = checkout_with_origin(
        tmp.path(),
        "hotstats-product-poc",
        "git@github.com:hotstats/hotstats-product-poc.git",
    );
    assert_eq!(
        repo_slug_for(&a).expect("a resolves"),
        "1m-consulting/adaptive-crm"
    );
    assert_eq!(
        repo_slug_for(&b).expect("b resolves"),
        "hotstats/hotstats-product-poc"
    );
}

/// A linked worktree shares its checkout's config, so the fallback is normally
/// unreachable — this pins that it EXISTS, and that it reaches the owning
/// checkout rather than any other registered project.
#[test]
fn a_directory_with_no_origin_falls_back_to_its_owning_checkout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).unwrap_or_else(|_| tmp.path().into());
    let repo = checkout_with_origin(
        &root,
        "adaptive-crm",
        "https://github.com/1m-consulting/adaptive-crm.git",
    );
    let wt = repo.join(".worktrees").join("fallback-7057");
    git_ok(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "feat/fallback-7057",
            wt.to_str().expect("utf8 worktree path"),
        ],
    );
    // A worktree-scoped override with an EMPTY value is how this fixture spells
    // "this worktree carries no origin of its own" without disturbing the
    // shared config the owning checkout reads.
    git_ok(&wt, &["config", "extensions.worktreeConfig", "true"]);
    git_ok(&wt, &["config", "--worktree", "remote.origin.url", ""]);
    assert!(
        origin_url(&wt).is_none(),
        "fixture must leave the worktree without an origin"
    );
    assert_eq!(
        repo_slug_for(&wt).expect("the owning checkout answers"),
        "1m-consulting/adaptive-crm"
    );
}

/// 🔴 Fail-closed: a directory git does not root a repository at refuses, and
/// the refusal names the directory so the operator can see WHERE it looked.
#[test]
fn a_non_repository_directory_resolves_to_no_repository() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let reason = repo_slug_for(tmp.path()).expect_err("a non-repository must refuse");
    assert!(reason.contains("cannot be established"), "{reason}");
    assert!(
        reason.contains(&tmp.path().display().to_string()),
        "{reason}"
    );
}

/// A repository whose `origin` is a local clone path refuses, and says what it
/// read — the shape every `GitWorktreeFixture`-backed test now takes.
#[test]
fn an_unparseable_origin_refuses_and_quotes_the_url() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = checkout_with_origin(tmp.path(), "local-remote", "/tmp/fixtures/remote.git");
    let reason = repo_slug_for(&repo).expect_err("a local-path origin must refuse");
    assert!(reason.contains("names no GitHub"), "{reason}");
    assert!(reason.contains("/tmp/fixtures/remote.git"), "{reason}");
}

// ---------------------------------------------------------------------------
// The argv the resolved slug produces (#7057)
// ---------------------------------------------------------------------------

/// The `--repo` value in a rendered `gh` argv, or `None` when the flag is
/// absent — which is what `origin/main` produces for every call.
fn repo_flag(cmd: &Command) -> Option<String> {
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let at = args.iter().position(|a| a == "--repo")?;
    args.get(at + 1).cloned()
}

/// 🔴 #7057: two worktrees, two origins, ONE run — and the argv `gh` is handed
/// names a different repository for each.
///
/// Why an argv test and not a behaviour test: a lookup aimed at the wrong
/// repository answers "no pull request" exactly as a correct lookup against a
/// branch with no pull request does, so behaviour cannot tell them apart. Only
/// the argv can. Fails on `origin/main`, where no call names a repository.
#[test]
fn two_worktrees_with_different_origins_produce_different_repo_flags() {
    use crate::core::gh_identity::GhEnv;
    use crate::session_manager::worktree_reclaim_gh::gh_pr_list_command;

    let tmp = tempfile::tempdir().expect("tempdir");
    let a = checkout_with_origin(
        tmp.path(),
        "adaptive-crm",
        "https://github.com/1m-consulting/adaptive-crm.git",
    );
    let b = checkout_with_origin(
        tmp.path(),
        "hotstats-product-poc",
        "git@github.com:hotstats/hotstats-product-poc.git",
    );
    let env = GhEnv::default();
    let cmd_a = gh_pr_list_command(&a, &env, &repo_slug_for(&a).expect("a resolves"));
    let cmd_b = gh_pr_list_command(&b, &env, &repo_slug_for(&b).expect("b resolves"));
    assert_eq!(
        repo_flag(&cmd_a).as_deref(),
        Some("1m-consulting/adaptive-crm")
    );
    assert_eq!(
        repo_flag(&cmd_b).as_deref(),
        Some("hotstats/hotstats-product-poc")
    );
    assert_ne!(
        repo_flag(&cmd_a),
        repo_flag(&cmd_b),
        "one run must not aim both worktrees at one repository — that IS the bug"
    );
}

/// `--repo` belongs to `pr list`, so it has to follow the subcommand; `gh` has
/// no such flag on its root command.
#[test]
fn gh_pr_list_command_names_the_repository_before_its_filters() {
    use crate::core::gh_identity::GhEnv;
    use crate::session_manager::worktree_reclaim_gh::gh_pr_list_command;

    let cmd = gh_pr_list_command(
        Path::new("/tmp"),
        &GhEnv::default(),
        "1m-consulting/adaptive-crm",
    );
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        vec!["pr", "list", "--repo", "1m-consulting/adaptive-crm"],
        "callers append their own filters after this prefix"
    );
}

/// 🔴 Fail-closed end to end: a root whose repository cannot be established
/// yields `LookupFailed` — never `NoPr`, never `Merged`, and never a lookup
/// against some other repository.
///
/// Why: `LookupFailed` is what `classify` gate 5 blocks on, so this is the
/// assertion that keeps an unresolvable origin from becoming a removal. Nothing
/// here spawns `gh`: the refusal happens before the poll.
#[test]
fn an_unresolvable_repository_blocks_instead_of_answering() {
    use crate::session_manager::worktree_reclaim::{BranchPrState, PrIndex, pr_state_for_branch};

    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = checkout_with_origin(tmp.path(), "local-remote", "/tmp/fixtures/remote.git");

    let index = PrIndex::from_gh(&repo);
    let BranchPrState::LookupFailed { reason } = index.state_for(Some("feat/anything")) else {
        panic!("an unresolvable repository must block, not answer");
    };
    assert!(reason.contains("names no GitHub"), "{reason}");

    let per_branch = pr_state_for_branch(&repo, "feat/anything");
    let BranchPrState::LookupFailed { reason } = per_branch else {
        panic!("the per-branch fallback must block too; got {per_branch:?}");
    };
    assert!(reason.contains("#7057"), "{reason}");
}
