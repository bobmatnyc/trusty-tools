//! Tests for the `session_target_dir` doctor row (#10215).

use super::*;

fn probed(session: SessionTarget, local_targets: Vec<PathBuf>) -> DoctorCheck {
    build_check(&Facts::Probed {
        session,
        local_targets,
    })
}

#[test]
fn a_pooled_session_with_no_local_targets_is_ok() {
    let check = probed(SessionTarget::Pooled, vec![]);
    assert_eq!(check.name, CHECK);
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    let check = probed(SessionTarget::Private("/elsewhere/t".into()), vec![]);
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
}

#[test]
fn an_unset_session_target_warns_with_the_fix() {
    let check = probed(SessionTarget::Unset(PathBuf::from("/repo/wt")), vec![]);
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(
        check.message.contains("/repo/wt/target"),
        "{}",
        check.message
    );
    assert!(check.message.contains("Fix:"), "{}", check.message);
}

#[test]
fn a_checkout_local_target_dir_warns() {
    let check = probed(
        SessionTarget::CheckoutLocal("/repo/wt/target".into()),
        vec![],
    );
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(
        check.message.contains("inside a checkout"),
        "{}",
        check.message
    );
    let check = probed(SessionTarget::Refused("no pool".into()), vec![]);
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
}

#[test]
fn worktrees_holding_a_local_target_warn() {
    let many: Vec<PathBuf> = (0..7).map(|i| PathBuf::from(format!("/wt{i}"))).collect();
    let check = probed(SessionTarget::Pooled, many);
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(check.message.contains("7 worktree(s)"), "{}", check.message);
    assert!(check.message.contains("/wt0/target"), "{}", check.message);
    assert!(check.message.contains("and 2 more"), "{}", check.message);
}

/// Fail-open check: a probe that failed must never read as OK.
#[test]
fn an_undetermined_probe_is_unknown_not_ok() {
    let check = build_check(&Facts::Undetermined("git failed".into()));
    assert_eq!(check.status, CheckStatus::Unknown, "{}", check.message);
    assert!(check.message.contains("git failed"), "{}", check.message);
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?}");
}

/// A Rust repo with an `origin`, one commit, and a linked worktree `wt`.
fn rust_repo_with_worktree(root: &Path) -> (PathBuf, PathBuf) {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"w\"\n").expect("marker");
    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widget.git",
        ],
    );
    git(&repo, &["add", "Cargo.toml"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    let wt = root.join("wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "task",
            wt.to_str().expect("utf-8"),
        ],
    );
    (repo, wt)
}

#[test]
fn gather_lists_a_worktree_holding_a_local_target() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical");
    let (repo, wt) = rust_repo_with_worktree(&root);
    std::fs::create_dir_all(wt.join("target")).expect("local target");
    let facts = gather(Some(&repo), Ok(None), &BuildersConfig::default(), &root);
    let Facts::Probed {
        session,
        local_targets,
    } = &facts
    else {
        panic!("{facts:?}");
    };
    // The main checkout keeps cargo's `./target`; that is reported too.
    assert_eq!(session, &SessionTarget::Unset(repo.clone()));
    let canon: Vec<PathBuf> = local_targets
        .iter()
        .map(|p| p.canonicalize().expect("canonical"))
        .collect();
    assert_eq!(canon, vec![wt.clone()]);
    assert_eq!(build_check(&facts).status, CheckStatus::Warn);
}

/// #10215: unset in a linked worktree with a pool is a slot, so the row is OK.
#[test]
fn gather_reports_a_linked_worktree_with_no_target_dir_as_pooled() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical");
    let (_, wt) = rust_repo_with_worktree(&root);
    let facts = gather(Some(&wt), Ok(None), &BuildersConfig::default(), &root);
    assert!(
        matches!(&facts, Facts::Probed { session: SessionTarget::Pooled, local_targets } if local_targets.is_empty()),
        "{facts:?}"
    );
    let inside = wt.join("target").display().to_string();
    let facts = gather(
        Some(&wt),
        Ok(Some(inside)),
        &BuildersConfig::default(),
        &root,
    );
    assert!(
        matches!(
            &facts,
            Facts::Probed {
                session: SessionTarget::CheckoutLocal(_),
                ..
            }
        ),
        "{facts:?}"
    );
}

/// Fail-open check: git failing is UNKNOWN, never OK.
#[test]
fn gather_is_undetermined_outside_a_git_repository() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"w\"\n").expect("marker");
    let facts = gather(
        Some(tmp.path()),
        Ok(None),
        &BuildersConfig::default(),
        tmp.path(),
    );
    assert!(matches!(&facts, Facts::Undetermined(why) if why.contains("git worktree list")));
    assert_eq!(build_check(&facts).status, CheckStatus::Unknown);
    let facts = gather(
        Some(tmp.path()),
        Err("not unicode".into()),
        &BuildersConfig::default(),
        tmp.path(),
    );
    assert_eq!(
        build_check(&facts).status,
        CheckStatus::Unknown,
        "{facts:?}"
    );
}

/// #10215 (critic MEDIUM): a target dir reached through a symlink or `..`
/// still lies inside the checkout, so the row must WARN, not read Private.
#[test]
fn gather_sees_a_symlinked_or_dotdot_target_dir_inside_a_checkout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical");
    let (_, wt) = rust_repo_with_worktree(&root);
    let link = root.join("link-to-wt");
    std::os::unix::fs::symlink(&wt, &link).expect("symlink");
    std::fs::create_dir_all(root.join("sibling")).expect("sibling");
    let through_link = link.join("target").display().to_string();
    let through_dotdot = root.join("sibling/../wt/target").display().to_string();
    for dir in [through_link, through_dotdot] {
        let facts = gather(
            Some(&wt),
            Ok(Some(dir.clone())),
            &BuildersConfig::default(),
            &root,
        );
        assert!(
            matches!(
                &facts,
                Facts::Probed {
                    session: SessionTarget::CheckoutLocal(_),
                    ..
                }
            ),
            "{dir}: {facts:?}"
        );
        assert_eq!(build_check(&facts).status, CheckStatus::Warn, "{dir}");
    }
}
