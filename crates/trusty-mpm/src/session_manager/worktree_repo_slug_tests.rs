//! Tests for the per-directory repository resolution (#7057).
//!
//! Why: the bug this module exists to close is invisible in behaviour — a
//! lookup aimed at the wrong repository answers "no pull request" exactly as a
//! correct lookup against a branch with no pull request does. Only the resolved
//! slug distinguishes them, so every test here asserts on the slug.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::session_manager::ssh_host_alias::SshHostAliases;

/// The alias table of a machine whose `~/.ssh/config` renames nothing.
///
/// Why: every test states its own table. Reading the operator's real config
/// would make these tests answer differently on two machines, and the crate may
/// not redirect `$HOME` to fake one (#7196).
fn no_aliases() -> SshHostAliases {
    SshHostAliases::empty()
}

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
        // Hostnames are case-insensitive, so this is still the default host and
        // still earns the bare two-segment slug.
        "https://GitHub.com/1m-consulting/adaptive-crm.git",
    ] {
        assert_eq!(
            parse_repo_slug(url, &no_aliases()).as_deref(),
            Ok("1m-consulting/adaptive-crm"),
            "{url}"
        );
    }
}

/// 🔴 #7057: a remote on any host but `github.com` keeps that host, in every URL
/// shape. `gh --repo` takes `[HOST/]OWNER/REPO`, so dropping the host aimed the
/// lookup at whatever `gh`'s default host is — silently answering for a
/// same-named repository there, which is the substitution this module exists to
/// stop.
#[test]
fn a_non_default_host_survives_into_the_slug() {
    for (url, want) in [
        (
            "http://github.example.invalid/1m-consulting/adaptive-crm.git",
            "github.example.invalid/1m-consulting/adaptive-crm",
        ),
        (
            "https://ghe.example/owner/repo.git",
            "ghe.example/owner/repo",
        ),
        // The port addresses the SERVER, not the repository — kept out of the
        // slug, while the host it qualifies is kept in.
        (
            "ssh://git@ghe.example:22/owner/repo",
            "ghe.example/owner/repo",
        ),
        ("git://ghe.example/owner/repo.git", "ghe.example/owner/repo"),
        ("git@ghe.example:owner/repo.git", "ghe.example/owner/repo"),
    ] {
        assert_eq!(
            parse_repo_slug(url, &no_aliases()).as_deref(),
            Ok(want),
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
            parse_repo_slug(url, &no_aliases()).as_deref(),
            Ok("hotstats/hotstats-product-poc"),
            "{url}"
        );
    }
}

#[test]
fn scp_like_remote_yields_its_owner_and_repo() {
    assert_eq!(
        parse_repo_slug("git@github.com:bobmatnyc/trusty-tools.git", &no_aliases()).as_deref(),
        Ok("bobmatnyc/trusty-tools")
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
        assert_eq!(
            parse_repo_slug(url, &no_aliases()),
            Err(SlugRefusal::NoRepository),
            "{url}"
        );
    }
}

#[test]
fn a_file_url_remote_names_no_repository() {
    // `file://` reaches the scp-like branch, whose path is absolute — the guard
    // that keeps a local clone from resolving to a GitHub slug.
    assert_eq!(
        parse_repo_slug("file:///tmp/fixtures/remote.git", &no_aliases()),
        Err(SlugRefusal::NoRepository)
    );
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
        repo_slug_with(&a, &no_aliases()).expect("a resolves"),
        "1m-consulting/adaptive-crm"
    );
    assert_eq!(
        repo_slug_with(&b, &no_aliases()).expect("b resolves"),
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
        remote_url(&wt, DEFAULT_REMOTE).is_none(),
        "fixture must leave the worktree without an origin"
    );
    assert_eq!(
        repo_slug_with(&wt, &no_aliases()).expect("the owning checkout answers"),
        "1m-consulting/adaptive-crm"
    );
}

/// 🔴 Fail-closed: a directory git does not root a repository at refuses, and
/// the refusal names the directory so the operator can see WHERE it looked.
#[test]
fn a_non_repository_directory_resolves_to_no_repository() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let reason =
        repo_slug_with(tmp.path(), &no_aliases()).expect_err("a non-repository must refuse");
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
    let reason = repo_slug_with(&repo, &no_aliases()).expect_err("a local-path origin must refuse");
    assert!(reason.contains("names no GitHub"), "{reason}");
    assert!(reason.contains("/tmp/fixtures/remote.git"), "{reason}");
}

// ---------------------------------------------------------------------------
// SSH `Host` aliases (#7196)
// ---------------------------------------------------------------------------

/// The `~/.ssh/config` a multi-account operator writes, as a table.
fn work_aliases() -> SshHostAliases {
    SshHostAliases::parse(
        "Host gh-work\n  HostName github.com\n  User git\n\n\
         Host ghe-work\n  HostName ghe.example\n",
    )
}

/// 🔴 #7196: the alias in the remote is not the host. `gh-work` is an
/// `~/.ssh/config` name for `github.com`, and reading it as the host produced
/// `gh --repo gh-work/acme-corp/widgets`, which fails with "error
/// connecting to gh-work" — so four worktrees whose pull requests had
/// MERGED were refused at gate 5. Fails on 9b57099a5, where the slug carries
/// the alias.
#[test]
fn an_ssh_alias_resolves_to_the_host_it_names() {
    for url in [
        "git@gh-work:acme-corp/widgets.git",
        "ssh://git@gh-work/acme-corp/widgets.git",
        "ssh://git@gh-work:22/acme-corp/widgets",
    ] {
        assert_eq!(
            parse_repo_slug(url, &work_aliases()).as_deref(),
            Ok("acme-corp/widgets"),
            "{url}"
        );
    }
    // An alias for an ENTERPRISE host keeps that host — the resolution answers
    // WHICH server, it does not collapse everything onto github.com.
    assert_eq!(
        parse_repo_slug("git@ghe-work:owner/repo.git", &work_aliases()).as_deref(),
        Ok("ghe.example/owner/repo")
    );
}

/// 🔴 Fail-closed: an alias the config does not declare is refused, and the
/// refusal names the alias so the operator knows which `Host` block is missing.
/// Guessing a server for it would be the wrong-repository substitution #7057
/// closed, arriving by a different door.
#[test]
fn an_unresolvable_ssh_alias_refuses_and_names_the_alias() {
    assert_eq!(
        parse_repo_slug("git@gh-bob:bobmatnyc/trusty-tools.git", &no_aliases()),
        Err(SlugRefusal::UnresolvedSshAlias("gh-bob".to_string()))
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = checkout_with_origin(
        tmp.path(),
        "aliased",
        "git@gh-bob:bobmatnyc/trusty-tools.git",
    );
    let reason = repo_slug_with(&repo, &no_aliases()).expect_err("an undeclared alias must refuse");
    assert!(reason.contains("gh-bob"), "{reason}");
    assert!(reason.contains("#7196"), "{reason}");
}

/// An SSH host nothing renames but that carries a `.` is a machine name, and is
/// taken at face value — this is what keeps the #7057 GitHub Enterprise case
/// working for an operator who declares no alias for it.
#[test]
fn an_undeclared_dotted_ssh_host_is_taken_at_face_value() {
    assert_eq!(
        parse_repo_slug("git@ghe.example:owner/repo.git", &no_aliases()).as_deref(),
        Ok("ghe.example/owner/repo")
    );
}

/// The rewrite follows the transport git actually hands to `ssh`. An `https://`
/// remote never reaches `ssh`, so its host must not be rewritten by a `Host`
/// block that happens to share the name — that would change which SERVER is
/// asked, on evidence that does not apply.
#[test]
fn an_https_remote_is_not_rewritten_by_an_ssh_alias() {
    let aliases = SshHostAliases::parse("Host gh-work\n  HostName github.com\n");
    assert_eq!(
        parse_repo_slug("https://gh-work/acme-corp/widgets.git", &aliases).as_deref(),
        Ok("gh-work/acme-corp/widgets")
    );
}

/// 🔴 #7196 end to end: the same worktree resolves against a test-controlled
/// ssh config, and the argv `gh` is handed names `acme-corp/widgets` on
/// github.com rather than the unreachable `gh-work/acme-corp/widgets`.
#[test]
fn an_aliased_origin_resolves_through_the_given_ssh_config() {
    use crate::core::gh_identity::GhEnv;
    use crate::session_manager::worktree_reclaim_gh::gh_pr_list_command;

    let tmp = tempfile::tempdir().expect("tempdir");
    // A config FILE, not a literal — the production path reads one, and this is
    // the only way to exercise that read without touching `~/.ssh/config`.
    let config = tmp.path().join("ssh-config");
    std::fs::write(&config, "Host gh-work\n  HostName github.com\n")
        .expect("fixture: write ssh config");
    let aliases = SshHostAliases::load(&config);

    let repo = checkout_with_origin(tmp.path(), "widgets", "git@gh-work:acme-corp/widgets.git");
    let slug = repo_slug_with(&repo, &aliases).expect("the aliased origin resolves");
    assert_eq!(slug, "acme-corp/widgets");
    assert_eq!(
        repo_flag(&gh_pr_list_command(&repo, &GhEnv::default(), &slug)).as_deref(),
        Some("acme-corp/widgets"),
        "the alias must never reach `gh --repo` — that is the connection error in #7196"
    );
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
    let cmd_a = gh_pr_list_command(
        &a,
        &env,
        &repo_slug_with(&a, &no_aliases()).expect("a resolves"),
    );
    let cmd_b = gh_pr_list_command(
        &b,
        &env,
        &repo_slug_with(&b, &no_aliases()).expect("b resolves"),
    );
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

/// 🔴 #7057: an enterprise worktree's origin reaches the argv host and all.
///
/// Why: `gh --repo owner/repo` resolves against `gh`'s DEFAULT host, so a bare
/// slug built from a GitHub Enterprise remote asks github.com — and answers, if
/// a repository with those two names exists there. Nothing in the reply says it
/// came from another host, so the argv is the only place the difference is
/// visible. Fails against a448fb807, where the host is discarded and the flag
/// reads `--repo owner/repo`.
#[test]
fn an_enterprise_worktree_names_its_host_in_the_repo_flag() {
    use crate::core::gh_identity::GhEnv;
    use crate::session_manager::worktree_reclaim_gh::gh_pr_list_command;

    let tmp = tempfile::tempdir().expect("tempdir");
    let ghe = checkout_with_origin(tmp.path(), "ghe", "ssh://git@ghe.example:22/owner/repo.git");
    let scp = checkout_with_origin(tmp.path(), "scp", "git@ghe.example:owner/repo.git");
    let com = checkout_with_origin(
        tmp.path(),
        "dotcom",
        "https://github.com/1m-consulting/adaptive-crm.git",
    );

    let slug = repo_slug_with(&ghe, &no_aliases()).expect("the enterprise origin resolves");
    assert_eq!(slug, "ghe.example/owner/repo");
    assert_eq!(
        repo_slug_with(&scp, &no_aliases()).expect("the scp-like enterprise origin resolves"),
        "ghe.example/owner/repo",
        "the two spellings of one remote must not disagree"
    );

    let env = GhEnv::default();
    assert_eq!(
        repo_flag(&gh_pr_list_command(&ghe, &env, &slug)).as_deref(),
        Some("ghe.example/owner/repo")
    );
    // A github.com worktree in the same run keeps the bare two-segment form —
    // the host qualification is per-remote, not a blanket rewrite.
    assert_eq!(
        repo_flag(&gh_pr_list_command(
            &com,
            &env,
            &repo_slug_with(&com, &no_aliases()).expect("the github.com origin resolves"),
        ))
        .as_deref(),
        Some("1m-consulting/adaptive-crm")
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

/// 🔴 #7850: a branch pushed to a FORK is searched in the fork's repository,
/// not in `origin`.
///
/// Why: the reported case verbatim. In `breezeblue-ai/breeze-tts` local `main`
/// tracks `fork` (`bobmatnyc/breeze-tts`) because `origin` 403s for the
/// operator's account; `fix/matsuoka-respelling` merged as
/// `bobmatnyc/breeze-tts#5`, and the ADR-0057 removal gate still refused the
/// worktree because it asked `origin` for a pull request never opened there.
/// The assertion is on the SLUG for the same reason as every test in this file:
/// a lookup aimed at the wrong repository answers "no pull request" exactly as
/// a correct lookup against a branch with none.
/// What: asserts the branch's push remote picks the fork's repository, while
/// `repo_slug_with`'s unchanged `origin` answer still names the upstream.
/// Test: itself.
#[test]
fn a_fork_branch_is_searched_in_the_repository_it_was_pushed_to() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = checkout_with_origin(
        tmp.path(),
        "breeze-tts",
        "https://github.com/breezeblue-ai/breeze-tts.git",
    );
    git_ok(
        &repo,
        &[
            "remote",
            "add",
            "fork",
            "https://github.com/bobmatnyc/breeze-tts.git",
        ],
    );
    git_ok(&repo, &["checkout", "-b", "fix/matsuoka-respelling"]);
    git_ok(
        &repo,
        &[
            "config",
            "branch.fix/matsuoka-respelling.pushRemote",
            "fork",
        ],
    );

    let remote = push_remote_for_branch(&repo, "fix/matsuoka-respelling")
        .expect("the branch names its push remote");
    assert_eq!(remote, "fork");
    assert_eq!(
        repo_slug_with_remote(&repo, &remote, &no_aliases()).expect("the fork resolves"),
        "bobmatnyc/breeze-tts",
        "the merged pull request lives in the repository the branch was pushed to"
    );
    // The unchanged `origin` answer, which is what refused the removal.
    assert_eq!(
        repo_slug_with(&repo, &no_aliases()).expect("origin resolves"),
        "breezeblue-ai/breeze-tts"
    );
}

/// #7850: git's own three push keys, in git's own precedence.
///
/// Why: the resolution must not invent an order. `branch.<name>.pushRemote`
/// beats `remote.pushDefault`, which beats `branch.<name>.remote` — and a
/// repository that sets none of them answers `None`, which is what keeps every
/// ordinary worktree on the pre-#7850 `origin` path.
/// Test: itself.
#[test]
fn push_remote_follows_gits_own_precedence_and_is_none_when_unset() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = checkout_with_origin(
        tmp.path(),
        "precedence",
        "https://github.com/acme/widgets.git",
    );
    git_ok(&repo, &["checkout", "-b", "feat/x"]);

    assert_eq!(
        push_remote_for_branch(&repo, "feat/x"),
        None,
        "nothing configured must answer None, so the caller uses origin"
    );

    git_ok(&repo, &["config", "branch.feat/x.remote", "tracked"]);
    assert_eq!(
        push_remote_for_branch(&repo, "feat/x").as_deref(),
        Some("tracked"),
        "the tracking remote is the last resort"
    );

    git_ok(&repo, &["config", "remote.pushDefault", "pushdefault"]);
    assert_eq!(
        push_remote_for_branch(&repo, "feat/x").as_deref(),
        Some("pushdefault"),
        "remote.pushDefault outranks the tracking remote"
    );

    git_ok(&repo, &["config", "branch.feat/x.pushRemote", "branchpush"]);
    assert_eq!(
        push_remote_for_branch(&repo, "feat/x").as_deref(),
        Some("branchpush"),
        "the branch's own pushRemote outranks both"
    );
}

/// 🔴 #7850 fail-closed: naming a remote that does not exist never silently
/// falls back to `origin`.
///
/// Why: the relaxation must be a CORRECTION of which repository is asked, never
/// a second chance at a different one. A remote git has no URL for has no
/// owning checkout to fall back to either here, so the lookup refuses — which
/// denies the removal, the direction ADR-0057 decision 6 requires.
/// Test: itself.
#[test]
fn an_unknown_named_remote_refuses_rather_than_answering_for_origin() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = checkout_with_origin(tmp.path(), "unknown", "https://github.com/acme/widgets.git");
    let reason = repo_slug_with_remote(&repo, "nosuchremote", &no_aliases())
        .expect_err("an unknown remote must refuse");
    assert!(reason.contains("cannot be established"), "{reason}");
    // And `origin` still answers for itself, so the refusal is about the named
    // remote rather than about the repository.
    assert_eq!(
        repo_slug_with(&repo, &no_aliases()).expect("origin resolves"),
        "acme/widgets"
    );
}

/// A checkout whose `origin` is the org repository and whose branch pushes to
/// the operator's fork remote — the #8403 `duettoresearch/jev-matching` shape.
fn fork_push_checkout(root: &Path, origin: &str) -> PathBuf {
    let repo = checkout_with_origin(root, "jev-matching", origin);
    git_ok(
        &repo,
        &[
            "remote",
            "add",
            "bob-duetto",
            "https://github.com/bob-duetto/jev-matching.git",
        ],
    );
    git_ok(&repo, &["checkout", "-b", "fix/x"]);
    git_ok(&repo, &["config", "remote.pushDefault", "bob-duetto"]);
    repo
}

/// 🔴 REGRESSION (#8403): repository identity comes from `origin`. A branch
/// pushed to a fork remote searches `origin` FIRST, then the fork; on
/// origin/main it searched the fork (`<account>/<repo>`) alone.
#[test]
fn a_fork_branch_searches_origin_first_then_its_push_remote() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = fork_push_checkout(
        tmp.path(),
        "https://github.com/duettoresearch/jev-matching.git",
    );
    assert_eq!(
        merged_pr_search_repos_with(&repo, "fix/x", &no_aliases()).expect("both resolve"),
        vec![
            "duettoresearch/jev-matching".to_string(),
            "bob-duetto/jev-matching".to_string()
        ]
    );
}

/// 🔴 REGRESSION (#8403, fail-closed arm): an `origin` URL that names no
/// repository refuses even when the push remote is valid. On origin/main the
/// push remote replaced `origin`, so an unparseable `origin` was never read.
#[test]
fn an_unparseable_origin_refuses_even_with_a_valid_push_remote() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = fork_push_checkout(tmp.path(), "/srv/git/jev-matching.git");
    let err = merged_pr_search_repos_with(&repo, "fix/x", &no_aliases())
        .expect_err("an unparseable origin must refuse");
    assert!(err.contains("names no GitHub"), "{err}");
}

/// #8403: a merged PR in `origin` grants even when the fork cannot be
/// resolved — the reported failure, where the fork lookup errored.
#[test]
fn a_merged_pr_in_origin_grants_when_the_fork_cannot_be_resolved() {
    let repos = vec!["org/r".to_string(), "fork/r".to_string()];
    let found = first_merged(
        &repos,
        |repo| match repo {
            "org/r" => Ok((1, repo.to_string())),
            _ => Err("Could not resolve to a Repository".to_string()),
        },
        |(count, _)| *count > 0,
    )
    .expect("origin's merged PR is positive evidence");
    assert_eq!(found, (1, "org/r".to_string()));
}

/// #8403 (fail-closed arm): when no repository found a merge and one could not
/// be asked, the lookup is an `Err` — an unanswerable repository is never read
/// as "no merged PR".
#[test]
fn an_unanswerable_repository_denies_when_no_other_found_a_merge() {
    let repos = vec!["org/r".to_string(), "fork/r".to_string()];
    let err = first_merged(
        &repos,
        |repo| match repo {
            "org/r" => Ok(0),
            _ => Err(format!("gh timed out (repository searched: {repo})")),
        },
        |count| *count > 0,
    )
    .expect_err("an unanswerable repository must deny");
    assert!(err.contains("fork/r"), "{err}");
    assert_eq!(first_merged(&repos, |_| Ok(0), |c| *c > 0), Ok(0));
}
