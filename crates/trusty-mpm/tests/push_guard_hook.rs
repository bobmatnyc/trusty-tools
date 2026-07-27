//! End-to-end proof that the installed `pre-push` guard actually blocks the
//! push shape that clobbered PR #2863 (#2867).
//!
//! Why: a hook that is merely *present* proves nothing — the failure mode this
//! guards against is a real `git push` reaching a real remote. These tests
//! therefore run genuine `git push` invocations against a genuine local bare
//! remote and assert on the remote's resulting refs, not on the hook's text.
//! They also cover the three worktree shapes that matter: the base checkout
//! itself, a linked worktree, and an ad-hoc `git worktree add` worktree created
//! the way the #2867 agent created its own (which no trusty-mpm code path ever
//! configures — the hook is its only protection).
//! What: builds a bare `origin` + a clone, installs the guard via the
//! production `install_pre_push_guard`, then asserts (1) a cross-branch push is
//! refused and leaves the destination ref untouched, (2) the printed remedy
//! actually SUCCEEDS rather than being the refused command, (3) the same-name
//! push succeeds, (4) a detached HEAD permits the explicit rescue refspec but
//! still refuses a named-branch cross-branch push, (5) the name-preserving
//! push of another branch gets the same verdict attached and detached, (6)
//! deletes and tags pass through, (7) `TM_ALLOW_CROSS_BRANCH_PUSH=1` permits
//! the deliberate cross-branch push, (8) an ad-hoc worktree inherits the guard,
//! (9) a configured `remote.<name>.push` refspec cannot smuggle the clobber
//! past the guard via a BARE push from a detached HEAD, and (10) CREATING a
//! branch is permitted attached and detached while UPDATING one is not.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};
use std::process::Command;

use trusty_mpm::core::push_guard::{HookInstall, install_pre_push_guard};

/// Run a git command in `cwd`, returning (success, stdout, stderr).
fn git(cwd: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git must be spawnable");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// A bare `origin` seeded with one commit on `main`, plus a working clone.
struct Fixture {
    _root: tempfile::TempDir,
    origin: PathBuf,
    clone: PathBuf,
}

/// Build the fixture, or `None` when the local `git` binary is unavailable.
fn fixture() -> Option<Fixture> {
    let root = tempfile::Builder::new()
        .prefix("tm-test-pushguard-")
        .tempdir()
        .expect("temp dir");
    let origin = root.path().join("origin.git");
    let clone = root.path().join("clone");

    if !Command::new("git")
        .args(["init", "--bare", "-q", "-b", "main"])
        .arg(&origin)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return None;
    }
    if !Command::new("git")
        .args(["clone", "-q"])
        .arg(&origin)
        .arg(&clone)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return None;
    }
    std::fs::write(clone.join("README"), b"seed").expect("write README");
    assert!(git(&clone, &["add", "."]).0);
    assert!(git(&clone, &["commit", "-qm", "seed"]).0);
    assert!(git(&clone, &["push", "-q", "origin", "main"]).0);
    // A second branch on the remote, standing in for the foreign PR branch.
    assert!(git(&clone, &["push", "-q", "origin", "main:refs/heads/victim"]).0);

    Some(Fixture {
        _root: root,
        origin,
        clone,
    })
}

/// Resolve a ref on the bare origin, or `None` when it does not exist.
fn remote_sha(origin: &Path, refname: &str) -> Option<String> {
    let (ok, out, _) = git(origin, &["rev-parse", refname]);
    if ok {
        Some(out.trim().to_string())
    } else {
        None
    }
}

/// Add a commit on the currently checked-out branch of `repo`.
fn commit(repo: &Path, name: &str) {
    std::fs::write(repo.join(name), name.as_bytes()).expect("write file");
    assert!(git(repo, &["add", "."]).0);
    assert!(git(repo, &["commit", "-qm", name]).0);
}

/// The guard must refuse a cross-branch push and leave the victim ref intact,
/// while allowing the same-name push from the very same worktree.
#[test]
fn refuses_cross_branch_push_and_permits_same_name_push() {
    let Some(fx) = fixture() else {
        return;
    };
    assert!(matches!(
        install_pre_push_guard(&fx.clone).expect("install"),
        HookInstall::Installed(_)
    ));

    assert!(git(&fx.clone, &["checkout", "-q", "-b", "mine"]).0);
    commit(&fx.clone, "work.txt");
    let victim_before = remote_sha(&fx.origin, "refs/heads/victim").expect("victim exists");

    // 1. The #2867 shape: push MY branch onto SOMEONE ELSE'S branch.
    let (ok, _, stderr) = git(
        &fx.clone,
        &["push", "origin", "HEAD:refs/heads/victim", "--force"],
    );
    assert!(!ok, "a cross-branch push must be refused; stderr: {stderr}");
    assert!(
        stderr.contains("REFUSED cross-branch push"),
        "the refusal must be attributable to the guard; stderr: {stderr}"
    );
    assert_eq!(
        remote_sha(&fx.origin, "refs/heads/victim").as_deref(),
        Some(victim_before.as_str()),
        "the victim branch must be byte-identical after a refused push"
    );

    // 2. The refusal must never print a command the guard would itself
    //    refuse — an agent that follows the guidance would loop forever.
    let remedy = stderr
        .lines()
        .find(|l| l.trim_start().starts_with("git push origin"))
        .expect("the refusal must print a concrete remedy")
        .trim()
        .to_string();
    let remedy_args: Vec<&str> = remedy.split_whitespace().skip(1).collect();
    let (remedy_ok, _, remedy_err) = git(&fx.clone, &remedy_args);
    assert!(
        remedy_ok,
        "the printed remedy `{remedy}` must actually succeed, not be the refused command; \
         stderr: {remedy_err}"
    );

    // 3. The legitimate push — same name — must still work.
    let (own_ok, _, own_err) = git(&fx.clone, &["push", "origin", "HEAD:refs/heads/mine"]);
    assert!(
        own_ok,
        "pushing to the worktree's OWN branch must succeed; stderr: {own_err}"
    );
    assert!(remote_sha(&fx.origin, "refs/heads/mine").is_some());

    // 4. And so must a bare `git push` once upstream is set to the same name.
    commit(&fx.clone, "more.txt");
    assert!(git(&fx.clone, &["branch", "--set-upstream-to=origin/mine"]).0);
    let (bare_ok, _, bare_err) = git(&fx.clone, &["push"]);
    assert!(
        bare_ok,
        "a bare push to a same-name upstream must succeed; stderr: {bare_err}"
    );
}

/// A DETACHED HEAD must not be a blanket refusal — but the exemption is
/// CREATE-only.
///
/// Why: 13 of the 95 worktrees on this repo's own base clone sit in detached
/// HEAD at any moment, and `git push origin <sha>:refs/heads/<new-name>` is
/// the standard way to rescue work out of one. Refusing that — while printing
/// it as the remedy — burned the whole point of the guard. But the exemption
/// may only cover CREATES: an earlier revision exempted every anonymous-source
/// push from a detached HEAD, reasoning that git refuses a bare `git push`
/// while detached so all of them must be explicit refspecs. That reasoning was
/// FALSE and is now proven so by `remote_push_refspec_cannot_clobber_from_detached_head`.
#[test]
fn detached_head_permits_explicit_refspec_but_still_refuses_cross_branch() {
    let Some(fx) = fixture() else {
        return;
    };
    install_pre_push_guard(&fx.clone).expect("install");
    assert!(git(&fx.clone, &["checkout", "-q", "-b", "mine"]).0);
    commit(&fx.clone, "work.txt");
    assert!(git(&fx.clone, &["checkout", "-q", "--detach", "HEAD"]).0);

    // 1. `HEAD:refs/heads/<name>` — the canonical detached rescue push.
    let (head_ok, _, head_err) = git(&fx.clone, &["push", "origin", "HEAD:refs/heads/detachwork"]);
    assert!(
        head_ok,
        "HEAD:refs/heads/<name> from a detached HEAD must be permitted; stderr: {head_err}"
    );
    assert!(remote_sha(&fx.origin, "refs/heads/detachwork").is_some());

    // 2. The explicit-sha form of the same rescue.
    let (_, sha, _) = git(&fx.clone, &["rev-parse", "HEAD"]);
    let refspec = format!("{}:refs/heads/detachwork2", sha.trim());
    let (sha_ok, _, sha_err) = git(&fx.clone, &["push", "origin", &refspec]);
    assert!(
        sha_ok,
        "<sha>:refs/heads/<name> from a detached HEAD must be permitted; stderr: {sha_err}"
    );
    assert!(remote_sha(&fx.origin, "refs/heads/detachwork2").is_some());

    // 3. But a NAMED local branch pushed onto a different name is still the
    //    #2867 shape, detached or not — the rule does not go soft here.
    let victim_before = remote_sha(&fx.origin, "refs/heads/victim").expect("victim exists");
    let (cross_ok, _, cross_err) = git(
        &fx.clone,
        &["push", "origin", "--force", "mine:refs/heads/victim"],
    );
    assert!(
        !cross_ok,
        "a named-branch cross-branch push must be refused even when detached; stderr: {cross_err}"
    );
    assert_eq!(
        remote_sha(&fx.origin, "refs/heads/victim").as_deref(),
        Some(victim_before.as_str()),
        "the victim ref must be untouched"
    );
}

/// A configured `remote.<name>.push` refspec must not smuggle the #2867
/// clobber past the guard from a detached HEAD via a BARE `git push`.
///
/// Why: this is the exploit that disproved the previous revision's central
/// justification. That revision exempted every anonymous-source push from a
/// detached HEAD on the reasoning that git refuses a bare `git push` while
/// detached, so any such push had to be an explicit refspec someone typed.
/// The `fatal: You are not currently on a branch` comes from `push.default`
/// resolution ONLY — and git never consults `push.default` when
/// `remote.<name>.push` supplies the destination. So a bare `git push` from a
/// detached HEAD did land on a foreign branch and destroy its lineage, through
/// a fully installed guard. The exemption is now keyed on CREATE-vs-UPDATE,
/// which git states in the `<remote sha>` field, so how the push was spelled
/// and what state HEAD is in no longer matter.
#[test]
fn remote_push_refspec_cannot_clobber_from_detached_head() {
    let Some(fx) = fixture() else {
        return;
    };
    install_pre_push_guard(&fx.clone).expect("install");

    // Arm the repo the way the exploit does: no refspec is ever typed.
    assert!(
        git(
            &fx.clone,
            &["config", "remote.origin.push", "+HEAD:refs/heads/victim"],
        )
        .0
    );
    assert!(git(&fx.clone, &["checkout", "-q", "--detach", "HEAD"]).0);
    commit(&fx.clone, "unrelated-work.txt");
    let victim_before = remote_sha(&fx.origin, "refs/heads/victim").expect("victim exists");

    let (pushed, _, stderr) = git(&fx.clone, &["push"]);
    assert!(
        !pushed,
        "a BARE push from a detached HEAD onto an existing foreign branch must be refused \
         — `remote.<name>.push` bypasses push.default entirely; stderr: {stderr}"
    );
    assert!(
        stderr.contains("REFUSED cross-branch push"),
        "the refusal must come from the guard; stderr: {stderr}"
    );
    assert_eq!(
        remote_sha(&fx.origin, "refs/heads/victim").as_deref(),
        Some(victim_before.as_str()),
        "PR-branch lineage must be byte-identical — this is the #2867 regression"
    );

    // The detached refusal must ALSO print a remedy that actually works, not a
    // placeholder and not the command it just refused.
    let remedy = stderr
        .lines()
        .find(|l| l.trim_start().starts_with("git push origin"))
        .expect("the detached refusal must print a concrete remedy")
        .trim()
        .to_string();
    assert!(
        !remedy.contains('<') && !remedy.contains('>'),
        "the remedy must be concrete, not a placeholder: {remedy}"
    );
    let remedy_args: Vec<&str> = remedy.split_whitespace().skip(1).collect();
    let (remedy_ok, _, remedy_err) = git(&fx.clone, &remedy_args);
    assert!(
        remedy_ok,
        "the printed remedy `{remedy}` must actually succeed; stderr: {remedy_err}"
    );
}

/// Creating a branch that does not exist yet is always allowed — and that is
/// what makes the attached and detached rules identical.
///
/// Why: the create exemption is now the ONLY thing that permits a detached
/// rescue push, so it must be keyed on the destination not existing rather
/// than on HEAD state. Pinning both states here is what stops a future change
/// from reintroducing the asymmetry (previously: attached
/// `<sha>:refs/heads/backup` was refused while the identical detached command
/// was allowed).
#[test]
fn creating_a_new_branch_is_allowed_attached_and_detached() {
    let Some(fx) = fixture() else {
        return;
    };
    install_pre_push_guard(&fx.clone).expect("install");
    assert!(git(&fx.clone, &["checkout", "-q", "-b", "mine"]).0);
    commit(&fx.clone, "work.txt");

    // Attached on `mine`, creating a differently-named NEW branch.
    let (attached_ok, _, attached_err) =
        git(&fx.clone, &["push", "origin", "HEAD:refs/heads/backup-1"]);
    assert!(
        attached_ok,
        "creating a new branch must be allowed while attached; stderr: {attached_err}"
    );
    assert!(remote_sha(&fx.origin, "refs/heads/backup-1").is_some());

    // Detached, the identical shape must get the identical verdict.
    assert!(git(&fx.clone, &["checkout", "-q", "--detach", "HEAD"]).0);
    let (detached_ok, _, detached_err) =
        git(&fx.clone, &["push", "origin", "HEAD:refs/heads/backup-2"]);
    assert!(detached_ok, "…and while detached; stderr: {detached_err}");
    assert!(remote_sha(&fx.origin, "refs/heads/backup-2").is_some());

    // But UPDATING either of them from a differently-named source is refused
    // in both states — the create exemption must not leak into updates.
    let before = remote_sha(&fx.origin, "refs/heads/backup-1").expect("exists");
    commit(&fx.clone, "more.txt");
    let (upd_ok, _, _) = git(
        &fx.clone,
        &["push", "origin", "--force", "HEAD:refs/heads/backup-1"],
    );
    assert!(
        !upd_ok,
        "updating an existing branch from a detached HEAD must still be refused"
    );
    assert_eq!(
        remote_sha(&fx.origin, "refs/heads/backup-1").as_deref(),
        Some(before.as_str())
    );
}

/// The same-name push must get the SAME verdict attached and detached.
///
/// Why: the first revision refused `git push origin <other-branch>` when
/// attached but allowed it when detached — the identical operation, opposite
/// verdicts, and looser in the less-understood state. A name-preserving push
/// cannot land one branch's work on another branch's ref, so both states
/// permit it.
#[test]
fn name_preserving_push_of_another_branch_agrees_attached_and_detached() {
    let Some(fx) = fixture() else {
        return;
    };
    install_pre_push_guard(&fx.clone).expect("install");
    assert!(git(&fx.clone, &["checkout", "-q", "-b", "sidebranch"]).0);
    commit(&fx.clone, "side.txt");
    assert!(git(&fx.clone, &["checkout", "-q", "main"]).0);

    // Attached on `main`, pushing `sidebranch` onto `origin/sidebranch`.
    let (attached_ok, _, attached_err) = git(&fx.clone, &["push", "origin", "sidebranch"]);
    assert!(
        attached_ok,
        "a name-preserving push of another branch must be permitted while attached; \
         stderr: {attached_err}"
    );

    // Detached, the identical operation must get the identical verdict.
    commit(&fx.clone, "main-extra.txt");
    assert!(git(&fx.clone, &["checkout", "-q", "sidebranch"]).0);
    commit(&fx.clone, "side2.txt");
    assert!(git(&fx.clone, &["checkout", "-q", "--detach", "HEAD"]).0);
    let (detached_ok, _, detached_err) = git(&fx.clone, &["push", "origin", "sidebranch"]);
    assert!(detached_ok, "…and while detached; stderr: {detached_err}");
}

/// Branch DELETES and TAG pushes are out of scope and must pass through.
///
/// Why: the hook header promises both, and nothing asserted either. Deletes
/// are out of scope on a RECOVERABILITY argument, not an "it cannot happen by
/// accident" one — a configured `remote.<name>.push = :refs/heads/victim`
/// makes a BARE `git push` delete that branch, which was verified. But a
/// deleted GitHub branch is recoverable (the PR retains its commits at
/// `refs/pull/<N>/head`, and it can be re-pushed from any clone) whereas a
/// force-clobbered lineage is not; post-merge cleanup is routine; and the
/// right layer to police deletion is server-side branch protection, not a hook
/// the pusher can remove. Refusing it (as the first revision did) also produced
/// remediation text that recommended a *push* to accomplish a *delete*.
#[test]
fn deletes_and_tags_pass_through() {
    let Some(fx) = fixture() else {
        return;
    };
    install_pre_push_guard(&fx.clone).expect("install");
    assert!(git(&fx.clone, &["checkout", "-q", "-b", "mine"]).0);
    commit(&fx.clone, "work.txt");

    // A tag push, from a worktree standing on a branch of a different name.
    assert!(git(&fx.clone, &["tag", "-a", "v9.9.9", "-m", "release"]).0);
    let (tag_ok, _, tag_err) = git(&fx.clone, &["push", "origin", "v9.9.9"]);
    assert!(
        tag_ok,
        "a tag push must pass through untouched; stderr: {tag_err}"
    );
    assert!(remote_sha(&fx.origin, "refs/tags/v9.9.9").is_some());

    // Deleting a remote branch you are not standing on — routine cleanup.
    assert!(remote_sha(&fx.origin, "refs/heads/victim").is_some());
    let (del_ok, _, del_err) = git(&fx.clone, &["push", "origin", "--delete", "victim"]);
    assert!(
        del_ok,
        "a remote-branch delete must pass through; stderr: {del_err}"
    );
    assert!(
        remote_sha(&fx.origin, "refs/heads/victim").is_none(),
        "the delete must actually have taken effect"
    );
}

/// The documented override must permit a deliberate cross-branch push.
#[test]
fn override_env_var_permits_cross_branch_push() {
    let Some(fx) = fixture() else {
        return;
    };
    install_pre_push_guard(&fx.clone).expect("install");
    assert!(git(&fx.clone, &["checkout", "-q", "-b", "mine"]).0);
    commit(&fx.clone, "work.txt");
    let victim_before = remote_sha(&fx.origin, "refs/heads/victim").expect("victim exists");

    let out = Command::new("git")
        .arg("-C")
        .arg(&fx.clone)
        .args(["push", "origin", "HEAD:refs/heads/victim", "--force"])
        .env("TM_ALLOW_CROSS_BRANCH_PUSH", "1")
        .output()
        .expect("git push");
    assert!(
        out.status.success(),
        "TM_ALLOW_CROSS_BRANCH_PUSH=1 must permit the push; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(
        remote_sha(&fx.origin, "refs/heads/victim").expect("victim still exists"),
        victim_before,
        "the overridden push must actually have moved the ref"
    );
}

/// An ad-hoc `git worktree add` worktree — the #2867 shape, created by an agent
/// rather than by trusty-mpm — must inherit the guard from the shared hooks dir
/// with no per-worktree setup whatsoever.
#[test]
fn adhoc_agent_created_worktree_inherits_the_guard() {
    let Some(fx) = fixture() else {
        return;
    };
    install_pre_push_guard(&fx.clone).expect("install");

    // Exactly what the #2867 agent did: its own `git worktree add` under
    // `<repo>/.claude/worktrees/`, a path no trusty-mpm code path configures.
    let adhoc = fx.clone.join(".claude").join("worktrees").join("rebase-x");
    let (ok, _, err) = git(
        &fx.clone,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "rebase-x-work",
            adhoc.to_str().expect("utf8 path"),
        ],
    );
    assert!(ok, "ad-hoc worktree add must succeed; stderr: {err}");

    // Arm it exactly as #2867 found it: tracking a foreign PR branch.
    assert!(git(&adhoc, &["config", "branch.rebase-x-work.remote", "origin"]).0);
    assert!(
        git(
            &adhoc,
            &["config", "branch.rebase-x-work.merge", "refs/heads/victim"],
        )
        .0
    );
    // …and with `push.default = upstream`, which is what turns that tracking
    // config into a loaded gun. Git's own DEFAULT (`simple`) happens to refuse
    // a bare push whose upstream name differs — a fail-safe this test would
    // otherwise ride on instead of exercising the guard. The guard must not
    // depend on that default holding: `push.default` is a user-tunable knob,
    // and an explicit `git push origin HEAD:other` bypasses it entirely (see
    // `refuses_cross_branch_push_and_permits_same_name_push`).
    assert!(git(&adhoc, &["config", "push.default", "upstream"]).0);
    commit(&adhoc, "rebased.txt");
    let victim_before = remote_sha(&fx.origin, "refs/heads/victim").expect("victim exists");

    // The bare `git push` that clobbered PR #2863.
    let (pushed, _, stderr) = git(&adhoc, &["push", "--force"]);
    assert!(
        !pushed,
        "the bare push from an armed ad-hoc worktree must be refused; stderr: {stderr}"
    );
    assert!(
        stderr.contains("REFUSED cross-branch push"),
        "the refusal must come from the guard; stderr: {stderr}"
    );
    assert_eq!(
        remote_sha(&fx.origin, "refs/heads/victim").as_deref(),
        Some(victim_before.as_str()),
        "PR-branch lineage must be intact — this is the #2867 regression"
    );
}
