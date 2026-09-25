//! Tests for the linked-worktree HEAD-move rule (#8161).

use std::path::{Path, PathBuf};

use super::*;

const REPO: &str = "/repo";
const PARKED: &str = "/repo/.claude/worktrees/agent-parked";
const AGENT: &str = "/repo/.claude/worktrees/agent-fresh";

fn env() -> PathEnv {
    PathEnv {
        tmpdir: None,
        tmp: None,
        home: Some("/home/u".to_string()),
    }
}

fn classify(command: &str, cwd: &str, subagent: bool) -> Option<LinkedHeadMoveCheck> {
    classify_in(command, Path::new(cwd), subagent, &env())
}

fn parked_move(verb: &str) -> LinkedHeadMove {
    LinkedHeadMove {
        verb: verb.to_string(),
        target: PathBuf::from(PARKED),
        root: PathBuf::from(PARKED),
    }
}

fn tail(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

#[test]
fn moves_a_linked_head_covers_the_consolidation_verbs() {
    for (verb, args) in [
        ("reset", &["--keep", "FETCH_HEAD"][..]),
        ("reset", &["--hard", "origin/main"][..]),
        ("reset", &["--merge", "x"][..]),
        ("merge", &["--ff-only", "FETCH_HEAD"][..]),
        ("rebase", &["origin/main"][..]),
    ] {
        assert!(moves_a_linked_head(verb, &tail(args)), "{verb} {args:?}");
    }
    for (verb, args) in [
        ("reset", &["HEAD"][..]),
        ("reset", &["--soft", "HEAD~1"][..]),
        ("merge", &["--abort"][..]),
        ("rebase", &["--continue"][..]),
        ("fetch", &["origin"][..]),
        ("pull", &[][..]),
    ] {
        assert!(!moves_a_linked_head(verb, &tail(args)), "{verb} {args:?}");
    }
}

#[test]
fn classify_queries_the_consolidation_command_into_a_parked_worktree() {
    let command =
        format!("git -C {PARKED} fetch {AGENT} fix/x && git -C {PARKED} reset --keep FETCH_HEAD");
    for (cwd, subagent) in [(REPO, false), (REPO, true), (AGENT, false)] {
        assert_eq!(
            classify(&command, cwd, subagent),
            Some(LinkedHeadMoveCheck::Query(parked_move("reset"))),
            "from {cwd}, subagent={subagent}"
        );
    }
    let ff = format!("cd {PARKED} && git merge --ff-only FETCH_HEAD");
    assert_eq!(
        classify(&ff, REPO, false),
        Some(LinkedHeadMoveCheck::Query(parked_move("merge")))
    );
}

#[test]
fn classify_exempts_a_subagent_moving_its_own_tree_only() {
    // Recipe step 2: the isolated agent resets its OWN tree onto the parked tip.
    assert_eq!(classify("git reset --keep fix/x", AGENT, true), None);
    assert_eq!(
        classify("git rebase origin/main", &format!("{AGENT}/crates/a"), true),
        None
    );
    // The PM gets no such exemption: its cwd can be moved into a worktree (#8535).
    assert_eq!(
        classify("git reset --keep FETCH_HEAD", PARKED, false),
        Some(LinkedHeadMoveCheck::Query(parked_move("reset")))
    );
}

#[test]
fn classify_leaves_the_main_checkout_and_ordinary_work_alone() {
    assert_eq!(classify("git reset --keep origin/main", REPO, false), None);
    assert_eq!(classify("git merge origin/main", REPO, false), None);
    assert_eq!(
        classify(&format!("git -C {PARKED} status"), REPO, false),
        None
    );
    assert_eq!(
        classify(&format!("git -C {PARKED} fetch {AGENT} b"), REPO, false),
        None
    );
}

#[test]
fn classify_denies_an_unresolved_target() {
    let Some(LinkedHeadMoveCheck::Deny(reason)) =
        classify("git -C \"$WT\" reset --keep FETCH_HEAD", PARKED, false)
    else {
        panic!("an unresolved target must be refused");
    };
    assert!(reason.contains("$WT"), "{reason}");
    assert!(reason.contains("#8161"), "{reason}");
}

#[test]
fn verdict_denies_a_live_writer() {
    let live = vec!["rust-engineer".to_string()];
    let reason = linked_head_move_verdict(&parked_move("reset"), Ok(&live))
        .expect("a live writer in the parked tree must deny");
    assert!(reason.contains("rust-engineer"), "{reason}");
    assert!(reason.contains(PARKED), "{reason}");
    assert!(reason.contains("tm repair delegation"), "{reason}");
}

#[test]
fn verdict_allows_an_idle_worktree() {
    assert_eq!(
        linked_head_move_verdict(&parked_move("reset"), Ok(&[])),
        None
    );
}

#[test]
fn verdict_fails_closed_when_the_lookup_cannot_answer() {
    let reason = linked_head_move_verdict(
        &parked_move("merge"),
        Err("the daemon answered 500 Internal Server Error"),
    )
    .expect("an unanswered lookup must deny");
    assert!(reason.contains("500"), "{reason}");
    assert!(reason.contains("fails closed"), "{reason}");
    assert!(reason.contains("tm repair delegation"), "{reason}");
}
