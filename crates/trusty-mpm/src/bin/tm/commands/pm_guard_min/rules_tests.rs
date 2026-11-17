//! Tests for [`super`] and [`super::super::rules_git`] (#11431).

use std::path::Path;
use std::process::Command;

use serde_json::json;
use tempfile::TempDir;

use super::super::rules_git::repo_slug;
use super::*;

fn env_in(home: &Path, cwd: &Path) -> GuardEnv {
    GuardEnv {
        home: home.to_path_buf(),
        project_dir: None,
        cwd: cwd.to_path_buf(),
        r5_allow: Vec::new(),
        architect: false,
    }
}

fn bash(cmd: &str, env: &GuardEnv) -> Option<&'static str> {
    evaluate("Bash", &json!({ "command": cmd }), env)
        .0
        .map(|h| h.rule)
}

fn home() -> TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn r1_flags_writes_to_each_anchor() {
    let h = home();
    let env = env_in(h.path(), h.path());
    let settings = h.path().join(".claude/settings.json");
    let write = json!({ "file_path": settings, "content": "{}" });
    assert_eq!(
        evaluate("Write", &write, &env).0.map(|h| h.rule),
        Some("R1")
    );
    for cmd in [
        "echo x > ~/.cargo/bin/tm",
        "cp ./tm ~/.cargo/bin/trusty-mpm",
        "tee -a ~/.trusty-mpm/config.toml < /dev/null",
        "plutil -extract ProgramArguments json ~/Library/LaunchAgents/com.example.a.plist",
        "sed -i '' s/a/b/ ~/.claude/settings.local.json",
        "rm ~/Library/LaunchAgents/com.trusty.mpm.plist",
        "plutil -convert xml1 ~/Library/Preferences/com.example.plist",
        "perl -pi -e s/a/b/ ~/Library/Preferences/com.example.plist",
        "plutil -convert xml1 -o ~/.cargo/bin/tm in.plist",
    ] {
        assert_eq!(bash(cmd, &env), Some("R1"), "{cmd}");
    }
    for cmd in [
        "plutil -extract ProgramArguments json -o - ~/Library/LaunchAgents/com.example.a.plist",
        "cat ~/.claude/settings.json",
        "echo x > ~/notes.md",
    ] {
        assert_eq!(bash(cmd, &env), None, "{cmd}");
    }
    // A project-tier settings Edit is a rule-1 fact only when it names the guard.
    let project = h.path().join("repo/.claude/settings.json");
    let plain = json!({ "file_path": project, "old_string": "\"a\"", "new_string": "\"b\"" });
    assert_eq!(evaluate("Edit", &plain, &env).0, None);
    let guard =
        json!({ "file_path": project, "old_string": "tm hook --pm-guard", "new_string": "" });
    assert_eq!(evaluate("Edit", &guard, &env).0.map(|h| h.rule), Some("R1"));
}

#[test]
fn r2_flags_each_destructive_shape() {
    let h = home();
    let env = env_in(h.path(), h.path());
    for cmd in [
        "git push --force origin main",
        "git push -f origin main 2>&1 | tail -3",
        "git push origin +main",
        "git push --force-with-lease=main origin HEAD:refs/heads/main",
        "git -C /tmp reset --hard origin/main",
        "git clean -fdx",
        "git clean -f -d",
        "git branch -D master",
        "rm -rf ~",
        "rm -r -f /",
        "rm -rf ~/*",
        "rm -fr \"$HOME\"",
        "git worktree remove --force ~",
        "echo $(rm -rf ~)",
        "git branch -D feat/unmerged",
        "git branch --delete --force feat/x",
        "git worktree remove --force .claude/worktrees/agent-1",
        "sudo dd if=/dev/zero of=/dev/rdisk4 bs=1m",
        "wipefs -a /dev/disk3",
        "diskutil eraseDisk APFS X disk3",
    ] {
        assert_eq!(bash(cmd, &env), Some("R2"), "{cmd}");
    }
}

#[test]
fn r2_leaves_everyday_git_alone() {
    let h = home();
    let env = env_in(h.path(), h.path());
    for cmd in [
        "git push origin feat/x",
        "git push --force-with-lease origin feat/x",
        "git push -u origin HEAD:feat/x",
        "git reset --soft HEAD~1",
        "git clean -n",
        "git branch -d feat/x",
        "git checkout -- src/lib.rs",
        "dd if=a.img of=b.img",
        "rm -rf target/debug ~/scratch/x",
        "git worktree remove .claude/worktrees/agent-1",
        "rm -rf $DIR",
        "cargo test -p trusty-mpm --no-fail-fast",
    ] {
        assert_eq!(bash(cmd, &env), None, "{cmd}");
    }
}

#[test]
fn the_architect_runs_rule_one_only() {
    let h = home();
    let env = GuardEnv {
        architect: true,
        ..env_in(h.path(), h.path())
    };
    assert_eq!(bash("rm -rf ~", &env), None);
    assert_eq!(bash("gh api -X DELETE repos/a/b", &env), None);
    assert_eq!(bash("echo x > ~/.claude/settings.json", &env), Some("R1"));
}

fn git_repo(dir: &Path, origin: &str) {
    std::fs::create_dir_all(dir).unwrap();
    for args in [vec!["init", "-q"], vec!["remote", "add", "origin", origin]] {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(&args)
            .status()
            .unwrap();
        assert!(ok.success());
    }
}

#[test]
fn r2_discarding_work_counts_only_in_a_main_checkout() {
    let h = home();
    let main = h.path().join("main");
    git_repo(&main, "git@github.com:owner/repo.git");
    let env = env_in(h.path(), &main);
    for cmd in [
        "git checkout -- notes.md",
        "git checkout HEAD -- notes.md",
        "git checkout 5fc18e4^ -- notes.md",
        "git checkout .",
        "git restore notes.md",
    ] {
        assert_eq!(bash(cmd, &env), Some("R2"), "{cmd}");
    }
    for cmd in [
        "git checkout -b feat/x",
        "git restore --staged notes.md",
        "git checkout main",
    ] {
        assert_eq!(bash(cmd, &env), None, "{cmd}");
    }
}

#[test]
fn r3_flags_a_push_to_another_repo() {
    let h = home();
    let session = h.path().join("session");
    let other = h.path().join("other");
    git_repo(&session, "git@github.com:Owner/Repo.git");
    git_repo(&other, "https://github.com/someone/else");
    let env = GuardEnv {
        project_dir: Some(session.clone()),
        ..env_in(h.path(), &other)
    };
    assert_eq!(bash("git push origin feat/x", &env), Some("R3"));
    let env = GuardEnv {
        cwd: session,
        ..env
    };
    assert_eq!(
        bash("git push https://github.com/x/y feat/x", &env),
        Some("R3")
    );
}

#[test]
fn r3_allows_a_push_to_the_session_repo() {
    let h = home();
    let session = h.path().join("session");
    git_repo(&session, "git@github.com:Owner/Repo.git");
    let env = GuardEnv {
        project_dir: Some(session.clone()),
        ..env_in(h.path(), &session)
    };
    assert_eq!(bash("git push origin feat/x", &env), None);
    assert_eq!(bash("git push -o ci.skip origin feat/x", &env), None);
    assert_eq!(bash("git push ../mirror feat/x", &env), None);
    assert_eq!(bash("git push /srv/backup.git feat/x", &env), None);
    assert_eq!(
        bash("git push https://github.com/owner/repo.git feat/x", &env),
        None
    );
    assert_eq!(repo_slug("git@github.com:Owner/Repo.git"), "owner/repo");
    assert_eq!(repo_slug("https://github.com/owner/repo/"), "owner/repo");
}

#[test]
fn r5_flags_delete_verbs_unless_allowed() {
    let h = home();
    let env = env_in(h.path(), h.path());
    for cmd in [
        "gh api -X DELETE repos/x/y",
        "gh api --method=delete repos/x/y/git/refs/heads/z",
        "gh api -XDELETE /repos/x/y",
        "gh repo delete x/y --yes",
        "aws s3 rb s3://bucket --force",
        "aws ec2 terminate-instances --instance-ids i-1",
        "aws dynamodb delete-table --table-name t",
        "vercel rm my-app --yes",
        "fly apps destroy my-app",
        "kubectl delete pod p",
    ] {
        assert_eq!(bash(cmd, &env), Some("R5"), "{cmd}");
    }
    for cmd in [
        "gh api repos/x/y",
        "gh api -X PATCH repos/x/y",
        "aws s3 ls",
        "vercel deploy",
    ] {
        assert_eq!(bash(cmd, &env), None, "{cmd}");
    }
    let allowed = GuardEnv {
        r5_allow: vec!["gh api -X DELETE repos/x/".into()],
        ..env
    };
    assert_eq!(bash("gh api -X DELETE repos/x/y", &allowed), None);
    assert_eq!(bash("gh api -X DELETE repos/z/y", &allowed), Some("R5"));
}
