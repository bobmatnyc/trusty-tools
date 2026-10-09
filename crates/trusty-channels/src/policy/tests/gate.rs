//! The default-branch gate (#8454 Bob Db1, Architect G2/G3): each refusal
//! is named, and the accepted shapes load.

use std::os::unix::process::ExitStatusExt as _;
use std::process::{ExitStatus, Output};
use std::time::Duration;

use super::repo::{mkfifo, release_fifo, slack_routes, tempdir, within, Repo};
use crate::policy::gate::{check_committed_at_head, object_id, with_git_timeout};
use crate::policy::{check_default_branch, GateError};

fn gate(repo: &Repo) -> Result<crate::policy::BranchState, GateError> {
    let bytes = std::fs::read(repo.routes_file()).expect("read routes");
    check_default_branch(repo.dir(), &bytes)
}

fn routes() -> String {
    slack_routes("bob-dm", "U0ABCDEF1")
}

#[test]
fn routes_file_refused_when_head_is_on_a_feature_branch() {
    // The plan's case: the file is committed on the feature branch only.
    let repo = Repo::init("main");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.commit_routes(&routes());
    let refused = GateError::NotOnDefaultBranch {
        head: "feature".into(),
        default: "main".into(),
    };
    assert_eq!(gate(&repo), Err(refused.clone()));
    // The same bytes on main too: HEAD on the feature branch still refuses.
    let both = Repo::init("main");
    both.commit_routes(&routes());
    both.git(&["checkout", "-q", "-b", "feature"]);
    assert_eq!(gate(&both), Err(refused));
}

#[test]
fn routes_file_refused_on_detached_head_at_default_tip() {
    let repo = Repo::init("main");
    repo.commit_routes(&routes());
    repo.git(&["checkout", "-q", "--detach"]);
    assert_eq!(gate(&repo), Err(GateError::DetachedHead));
}

#[test]
fn local_commit_on_default_branch_ahead_of_origin_is_accepted() {
    let repo = Repo::init("main");
    let root = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["update-ref", "refs/remotes/origin/main", &root]);
    repo.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
    repo.commit_routes(&routes());
    let tip = repo.git(&["rev-parse", "HEAD"]);
    assert_ne!(tip, root, "the routes commit is ahead of origin");
    let state = gate(&repo).expect("a local commit on the default branch counts");
    assert_eq!(state.default, "main");
    assert_eq!(state.commit, tip);
    assert_eq!(state.head.as_deref(), Some("refs/heads/main"));
}

#[test]
fn local_only_repo_uses_main_without_origin() {
    for branch in ["main", "master"] {
        let repo = Repo::init(branch);
        repo.commit_routes(&routes());
        let state = gate(&repo).expect(branch);
        assert_eq!(state.default, branch);
        assert_eq!(state.commit, repo.git(&["rev-parse", "HEAD"]));
    }
}

#[test]
fn origin_head_names_the_default_branch() {
    let repo = Repo::init("trunk");
    repo.commit_routes(&routes());
    let tip = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["update-ref", "refs/remotes/origin/trunk", &tip]);
    repo.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ]);
    // A `main` with the same file must not win over origin/HEAD.
    repo.git(&["branch", "main"]);
    let state = gate(&repo).expect("HEAD on trunk is accepted");
    assert_eq!(state.default, "trunk");
    repo.git(&["checkout", "-q", "main"]);
    assert_eq!(
        gate(&repo),
        Err(GateError::NotOnDefaultBranch {
            head: "main".into(),
            default: "trunk".into(),
        })
    );
}

#[test]
fn default_branch_unknown_refused() {
    // No origin/HEAD and both main and master.
    let both = Repo::init("main");
    both.commit_routes(&routes());
    both.git(&["branch", "master"]);
    assert_eq!(gate(&both), Err(GateError::DefaultBranchUnknown));
    // Neither main nor master.
    let neither = Repo::init("trunk");
    neither.commit_routes(&routes());
    assert_eq!(gate(&neither), Err(GateError::DefaultBranchUnknown));
    // An origin/HEAD that is not a symbolic ref names no branch.
    let plain = Repo::init("main");
    plain.commit_routes(&routes());
    let tip = plain.git(&["rev-parse", "HEAD"]);
    plain.git(&["update-ref", "refs/remotes/origin/HEAD", &tip]);
    assert_eq!(gate(&plain), Err(GateError::DefaultBranchUnknown));
}

#[test]
fn project_dir_nested_in_a_parent_repo_refused() {
    let parent = Repo::init("main");
    let nested = parent.dir().join("sub");
    std::fs::create_dir_all(nested.join(".trusty-channels")).expect("mkdir");
    let path = nested.join(".trusty-channels/routes.toml");
    std::fs::write(&path, routes()).expect("write");
    parent.git(&["add", "sub/.trusty-channels/routes.toml"]);
    parent.git(&["commit", "-q", "-m", "nested routes"]);
    let bytes = std::fs::read(&path).expect("read");
    assert_eq!(
        check_default_branch(&nested, &bytes),
        Err(GateError::NotTopLevel)
    );
    // Outside any repo: refused too.
    let (_tmp, bare) = tempdir();
    assert_eq!(
        check_default_branch(&bare, &bytes),
        Err(GateError::NotARepository)
    );
}

#[test]
fn routes_not_committed_or_differing_at_the_default_commit_refused() {
    let repo = Repo::init("main");
    repo.write_routes(&routes());
    assert_eq!(
        gate(&repo),
        Err(GateError::NotCommitted { at: "main".into() })
    );
    repo.commit_routes(&routes());
    gate(&repo).expect("committed");
    repo.write_routes(&(routes() + "# edit\n"));
    assert_eq!(
        gate(&repo),
        Err(GateError::ContentDiffers { at: "main".into() })
    );
}

#[test]
fn default_branch_without_a_local_commit_refused() {
    // origin/HEAD names trunk, but no local trunk exists.
    let repo = Repo::init("main");
    repo.commit_routes(&routes());
    let tip = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["update-ref", "refs/remotes/origin/trunk", &tip]);
    repo.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ]);
    assert_eq!(
        gate(&repo),
        Err(GateError::NoDefaultCommit {
            branch: "trunk".into()
        })
    );
}

#[test]
fn gchat_gate_behaviour_unchanged() {
    // #8454 G1: gchat stays HEAD-only in S2b. A file committed on a feature
    // branch passes gchat's gate; the policy loader's Db1 gate refuses it.
    let repo = Repo::init("main");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.commit_routes(&routes());
    let bytes = std::fs::read(repo.routes_file()).expect("read");
    crate::gchat::load_gate::check_committed(&repo.routes_file(), &bytes)
        .expect("gchat's HEAD-only gate accepts a HEAD commit");
    assert!(check_default_branch(repo.dir(), &bytes).is_err());
}

#[test]
fn origin_head_outside_the_origin_remote_is_unknown() {
    // #8454 G2: origin/HEAD names a default branch only through
    // refs/remotes/origin/; a target elsewhere names none.
    let repo = Repo::init("main");
    repo.commit_routes(&routes());
    repo.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/heads/main",
    ]);
    assert_eq!(gate(&repo), Err(GateError::DefaultBranchUnknown));
}

#[test]
fn object_id_refuses_empty_or_non_hex_output() {
    let out = |stdout: &str| Output {
        status: ExitStatus::from_raw(0),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    };
    let hex = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(object_id(&out(&format!("{hex}\n")), "step"), Ok(hex));
    for (what, stdout) in [("empty", String::new()), ("non-hex", "g".repeat(40))] {
        assert_eq!(
            object_id(&out(&stdout), "step"),
            Err(GateError::GitFailed { step: "step" }),
            "{what}"
        );
    }
}

#[test]
fn git_blocked_on_a_fifo_config_include_times_out() {
    // #8454: git opens an include.path file with a blocking open, so a FIFO
    // there stops every git call until a writer appears.
    let repo = Repo::init("main");
    repo.commit_routes(&routes());
    let bytes = std::fs::read(repo.routes_file()).expect("read");
    let (_tmp, root) = tempdir();
    let fifo = root.join("include");
    mkfifo(&fifo);
    let config = repo.dir().join(".git/config");
    let mut text = std::fs::read_to_string(&config).expect("read config");
    text.push_str(&format!("[include]\n\tpath = {}\n", fifo.display()));
    std::fs::write(&config, text).expect("write config");
    let (dir, file) = (repo.dir().to_path_buf(), repo.routes_file());
    let got = within(Duration::from_secs(30), move || {
        with_git_timeout(Duration::from_secs(1), || {
            (
                check_default_branch(&dir, &bytes),
                check_committed_at_head(&file, &bytes),
            )
        })
    });
    let Some((db1, head)) = got else {
        release_fifo(&fifo);
        panic!("a git call blocked on the FIFO past 30s");
    };
    assert!(matches!(db1, Err(GateError::GitTimedOut { .. })), "{db1:?}");
    assert!(
        matches!(head, Err(GateError::GitTimedOut { .. })),
        "{head:?}"
    );
}
