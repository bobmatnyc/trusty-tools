//! Tests for [`super`] and [`super::super::paths`] (#11431).

use std::path::{Path, PathBuf};

use super::super::paths::{canon, hits_protected, is_trust_anchor};
use super::*;

const HOME: &str = "/home/u";

fn parse(src: &str) -> Facts {
    parse_bash(src, Path::new("/work"), Path::new(HOME))
}

fn names(f: &Facts) -> Vec<&str> {
    f.cmds.iter().map(Cmd::name).collect()
}

#[test]
fn heredoc_body_is_data_not_commands() {
    let f = parse("cat <<'EOF' > /tmp/out\nps aux\nrm -rf ~\nEOF\n");
    assert_eq!(names(&f), ["cat"]);
    assert_eq!(f.redirects.len(), 1);
    assert_eq!(f.redirects[0].target, Word::Lit("/tmp/out".into()));
    assert_eq!(f.unparsed, 0);
}

#[test]
fn substitution_marks_unparsed_and_still_yields_inner() {
    let f = parse("echo $(rm -rf ~)");
    let rm = f.cmds.iter().find(|c| c.name() == "rm").expect("inner rm");
    assert!(!rm.unparsed);
    assert_eq!(rm.lit_args(), ["-rf", HOME]);
    let echo = f.cmds.iter().find(|c| c.name() == "echo").expect("echo");
    assert!(echo.unparsed);
    assert_eq!(f.unparsed, 1);
    assert!(parse("eval \"$X\"").cmds[0].unparsed);
}

#[test]
fn quoting_wrappers_cd_and_shell_literals() {
    let f = parse("cd /tmp && sudo -E rm -rf 'a b' \"$HOME/.claude/settings.json\" $F");
    let rm = &f.cmds[1];
    assert_eq!(rm.name(), "rm");
    assert_eq!(rm.cwd, PathBuf::from("/tmp"));
    assert_eq!(rm.argv[2], Word::Lit("a b".into()));
    assert_eq!(
        rm.argv[3],
        Word::Lit(format!("{HOME}/.claude/settings.json"))
    );
    assert_eq!(rm.argv[4], Word::Dynamic);
    assert!(
        !rm.unparsed,
        "a plain variable is a dynamic word, not unparsed"
    );

    let f = parse("bash -c 'git push --force origin main' 2>&1");
    assert_eq!(names(&f), ["git", "bash"]);
    assert!(f.redirects.is_empty(), "2>&1 is not a file write");
    assert_eq!(names(&parse("/usr/bin/env FOO=1 git status")), ["git"]);
}

#[test]
fn prose_and_python_strings_are_arguments() {
    let f = parse("python3 -c \"import os; print(os.environ.get('TOKEN'))\"");
    assert_eq!(names(&f), ["python3"]);
    let f = parse("gh pr create --body \"$(cat <<'EOF'\nnever run `rm -rf ~`\nEOF\n)\"");
    assert!(!names(&f).contains(&"rm"), "{:?}", names(&f));
}

#[test]
fn a_broken_input_marks_every_command_unparsed() {
    let f = parse("git push --force origin main )(");
    assert!(f.cmds.iter().all(|c| c.unparsed));
    assert!(f.unparsed >= 1);
}

#[test]
fn anchors_resolve_through_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::os::unix::fs::symlink(home.join(".claude"), home.join("link")).unwrap();
    let real = canon(&home.join("link/settings.local.json"));
    assert!(is_trust_anchor(&real, home, true));
    assert!(is_trust_anchor(
        &canon(&home.join(".cargo/bin/tm")),
        home,
        true
    ));
    assert!(!is_trust_anchor(
        &canon(&home.join(".cargo/bin/rg")),
        home,
        true
    ));
    let project = canon(&home.join("repo/.claude/settings.json"));
    assert!(!is_trust_anchor(&project, home, true));
    assert!(is_trust_anchor(&project, home, false));
}

#[test]
fn protected_covers_ancestors_and_globbed_contents() {
    let protected = [
        PathBuf::from("/"),
        PathBuf::from(HOME),
        PathBuf::from("/repo"),
    ];
    assert!(hits_protected(Path::new(HOME), &protected));
    assert!(hits_protected(Path::new("/home/u/*"), &protected));
    assert!(hits_protected(Path::new("/repo/*"), &protected));
    assert!(!hits_protected(Path::new("/repo/target"), &protected));
    assert!(!hits_protected(
        Path::new("/repo/.claude/worktrees/a"),
        &protected
    ));
}
