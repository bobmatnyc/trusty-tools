//! Tests for the Bash `git worktree` parser (#8994 S2b).

use std::path::{Path, PathBuf};

use super::*;

const CWD: &str = "/r/main";

fn parse(command: &str) -> ParsedCommand {
    parse_worktree_command(command, Some(Path::new(CWD)))
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

/// The first candidate of each op, so a table row reads as "where git put it".
fn first_paths(parsed: &ParsedCommand) -> Vec<(&'static str, PathBuf)> {
    parsed
        .ops
        .iter()
        .map(|op| match op {
            WorktreeOp::Add(c) => ("add", c[0].clone()),
            WorktreeOp::Remove(c) => ("remove", c[0].clone()),
            WorktreeOp::Prune => ("prune", PathBuf::new()),
        })
        .collect()
}

/// Criterion 1: every real form resolves to one absolute path.
#[test]
fn real_worktree_calls_resolve_to_absolute_paths() {
    let cases: &[(&str, &[(&str, &str)])] = &[
        ("git worktree add ../wt", &[("add", "/r/wt")]),
        ("git worktree add -b feat/x ../wt", &[("add", "/r/wt")]),
        ("git worktree add -b feat/x /abs/wt main", &[("add", "/abs/wt")]),
        ("git worktree add -bfeat ./wt", &[("add", "/r/main/wt")]),
        ("git worktree add --lock --reason \"agent x\" wt", &[("add", "/r/main/wt")]),
        ("git worktree add -B b --detach wt HEAD~1", &[("add", "/r/main/wt")]),
        ("git worktree add -- wt", &[("add", "/r/main/wt")]),
        ("git -C /other/repo worktree add wt", &[("add", "/other/repo/wt")]),
        ("git -C sub -C deeper worktree add ../wt", &[("add", "/r/main/sub/wt")]),
        ("git -c core.x=1 --no-pager worktree add wt", &[("add", "/r/main/wt")]),
        ("cd /x/repo && git worktree add .worktrees/t", &[("add", "/x/repo/.worktrees/t")]),
        ("cd sub; git worktree add wt", &[("add", "/r/main/sub/wt")]),
        ("(cd /x && git worktree add wt)", &[("add", "/x/wt")]),
        ("FOO=1 git worktree add wt", &[("add", "/r/main/wt")]),
        ("env GIT_TRACE=0 /usr/bin/git worktree add wt", &[("add", "/r/main/wt")]),
        ("git worktree add wt 2>&1 | tail -3", &[("add", "/r/main/wt")]),
        ("git fetch && git worktree add wt origin/main", &[("add", "/r/main/wt")]),
        (
            "git worktree add a\ngit worktree add /b",
            &[("add", "/r/main/a"), ("add", "/b")],
        ),
        ("git worktree remove --force ../wt", &[("remove", "/r/wt")]),
        ("git -C /x worktree remove wt", &[("remove", "/x/wt")]),
        ("git worktree prune", &[("prune", "")]),
        ("git worktree prune -v --expire now", &[("prune", "")]),
        (
            "cat <<EOF > notes\ngit worktree add fake\nEOF\ngit worktree add real",
            &[("add", "/r/main/real")],
        ),
    ];
    for (command, expected) in cases {
        let parsed = parse(command);
        assert_eq!(parsed.errors, Vec::<String>::new(), "{command}");
        let want: Vec<(&str, PathBuf)> = expected.iter().map(|(k, v)| (*k, p(v))).collect();
        assert_eq!(first_paths(&parsed), want, "{command}");
    }
}

/// Criterion 4: a command that only mentions the words runs no git worktree.
#[test]
fn mentions_of_the_words_are_not_calls() {
    let cases = [
        "echo \"git worktree add x\"",
        "echo git worktree add x",
        "grep 'git worktree add' notes.md",
        "grep -rn \"git worktree add\" .",
        "rg 'worktree (add|remove)' src",
        "cat <<EOF\ngit worktree add x\nEOF",
        "cat <<'EOF' > f.md\ngit worktree add x\ngit worktree prune\nEOF\n",
        "cat <<-EOF\n\tgit worktree add x\n\tEOF",
        "cat <<< \"git worktree add x\"",
        "# git worktree add x",
        "true # git worktree add x",
        "git commit -m \"git worktree add x\"",
        "printf '%s\\n' 'git worktree add x; git worktree remove y'",
        "git log --grep='worktree add'",
        "git worktree list --porcelain",
        "git worktree prune -n",
        "git worktree prune --dry-run",
        "git worktree add",
        "gh pr view 1 --json body # mentions git worktree remove",
    ];
    for command in cases {
        assert_eq!(parse(command), ParsedCommand::default(), "{command}");
    }
}

/// A `cd` may already be reflected in the hook's `cwd`, so a relative `cd`
/// yields a second candidate resolved against the unmoved `cwd`.
#[test]
fn a_relative_cd_adds_the_unmoved_cwd_as_a_second_candidate() {
    let parsed = parse("cd sub && git worktree add wt");
    assert_eq!(
        parsed.ops,
        vec![WorktreeOp::Add(vec![p("/r/main/sub/wt"), p("/r/main/wt")])]
    );
    let plain = parse("git worktree add wt");
    assert_eq!(plain.ops, vec![WorktreeOp::Add(vec![p("/r/main/wt")])]);
}

/// Criterion 6 (parse side): a call that cannot be resolved is an error, never
/// a guess and never silence.
#[test]
fn unresolvable_calls_are_reported_not_guessed() {
    let cases = [
        "git worktree add \"$WT\"",
        "git worktree add ~/wt",
        "cd \"$DIR\" && git worktree add wt",
        "git -C $REPO worktree remove wt",
        "git worktree add \"unbalanced",
    ];
    for command in cases {
        let parsed = parse(command);
        assert!(parsed.ops.is_empty(), "{command}: {parsed:?}");
        assert_eq!(parsed.errors.len(), 1, "{command}: {parsed:?}");
    }
    let no_cwd = parse_worktree_command("git worktree add wt", None);
    assert!(no_cwd.ops.is_empty() && no_cwd.errors.len() == 1, "{no_cwd:?}");
    let absolute = parse_worktree_command("git worktree add /abs/wt", None);
    assert_eq!(absolute.ops, vec![WorktreeOp::Add(vec![p("/abs/wt")])]);
}
