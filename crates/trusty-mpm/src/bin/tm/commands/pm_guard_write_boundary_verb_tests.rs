//! #7228: the write boundary over `cp`, `mv`, `install`, `rsync`, `dd of=`
//! and `sed -i` — the deny rows, the allow rows bounding them, and the
//! fail-closed arm for a verb whose arguments do not lex.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::evaluate_main_checkout_write;

/// A temp root holding a main checkout `repo/` (a `.git` DIRECTORY), its
/// linked worktree `repo/.claude/worktrees/agent-x/` (a `.git` FILE), and a
/// sibling `repo-other/` that only shares the checkout's name prefix.
struct Tree {
    _dir: TempDir,
    root: PathBuf,
    main: PathBuf,
    worktree: PathBuf,
}

fn tree() -> Tree {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical tempdir");
    let main = root.join("repo");
    let worktree = main.join(".claude/worktrees/agent-x");
    std::fs::create_dir_all(main.join(".git")).expect("mkdir .git");
    std::fs::create_dir_all(main.join("src")).expect("mkdir src");
    std::fs::create_dir_all(worktree.join("src")).expect("mkdir worktree");
    std::fs::write(worktree.join(".git"), "gitdir: /elsewhere").expect("write .git");
    std::fs::create_dir_all(root.join("repo-other/src")).expect("mkdir sibling");
    Tree {
        _dir: dir,
        root,
        main,
        worktree,
    }
}

fn verdict(command: &str, cwd: &Path) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    evaluate_main_checkout_write("Bash", Some(&input), cwd)
}

fn assert_denied(commands: &[String], cwd: &Path) {
    for command in commands {
        let reason = verdict(command, cwd)
            .unwrap_or_else(|| panic!("`{command}` writes source into a main checkout"));
        assert!(reason.contains("ADR-0044"), "{reason}");
    }
}

fn assert_allowed(commands: &[String], cwd: &Path) {
    for command in commands {
        assert_eq!(verdict(command, cwd), None, "`{command}` must stay allowed");
    }
}

// #7228: the agent stands in its worktree and copies back into the main
// checkout by absolute path; every destination spelling denies.
#[test]
fn denies_a_cp_of_a_source_into_a_main_checkout() {
    let t = tree();
    let main = t.main.display();
    assert_denied(
        &[
            format!("cp src/lib.rs {main}/src/lib.rs"),
            format!("cp src/lib.rs {main}/src/"),
            format!("cp src/lib.rs {main}/src"),
            format!("cp a.rs b.rs {main}/src/"),
            format!("cp -t {main}/src src/lib.rs"),
            format!("cp --target-directory={main}/src src/lib.rs"),
            format!("cp -- src/lib.rs {main}/src/new.rs"),
            format!("mv src/lib.rs {main}/src/lib.rs"),
            format!("echo $(cp src/lib.rs {main}/src/lib.rs)"),
        ],
        &t.worktree,
    );
}

// #7228: `install` and `rsync` land a destination the same way.
#[test]
fn denies_an_install_and_rsync_into_a_main_checkout() {
    let t = tree();
    let main = t.main.display();
    assert_denied(
        &[
            format!("install -m 0644 src/lib.rs {main}/src/lib.rs"),
            format!("install -t {main}/src src/lib.rs"),
            format!("rsync -a src/lib.rs {main}/src/"),
            format!("rsync -av --exclude '*.md' src/lib.rs {main}/src/lib.rs"),
            format!("rsync src/lib.rs {main}/src"),
        ],
        &t.worktree,
    );
}

// #7228: `dd of=` and every `sed -i` spelling edit their file in place.
#[test]
fn denies_a_sed_i_and_dd_write_in_a_main_checkout() {
    let t = tree();
    let main = t.main.display();
    assert_denied(
        &[
            format!("dd if=src/lib.rs of={main}/src/lib.rs"),
            format!("sed -i 's/a/b/' {main}/src/lib.rs"),
            format!("sed -i '' 's/a/b/' {main}/src/lib.rs"),
            format!("sed -i.bak 's/a/b/' {main}/src/lib.rs"),
            format!("sed --in-place -e 's/a/b/' {main}/src/lib.rs"),
        ],
        &t.worktree,
    );
    // The PM in the checkout itself, by relative path: every caller.
    assert_denied(&["sed -i '' 's/a/b/' src/lib.rs".to_string()], &t.main);
}

// #7228 fail closed: a write verb whose arguments do not lex cannot be
// placed, so it denies wherever the hook stands — as an unlexable `tee` does.
#[test]
fn denies_a_write_verb_whose_arguments_do_not_lex() {
    let dir = tempfile::tempdir().expect("tempdir");
    for command in [
        "cp a.rs 'src/lib.rs",
        "sed -i 's/a/b/ src/lib.rs",
        "rsync a.rs \"src/",
    ] {
        let reason = verdict(command, dir.path())
            .unwrap_or_else(|| panic!("`{command}` cannot be placed and must deny"));
        assert!(reason.contains("#8730"), "{reason}");
    }
}

// #7228: a source is a read. Copying OUT of a main checkout into the agent's
// worktree (absolute or relative) or a temp directory stays allowed.
#[test]
fn allows_a_copy_out_of_a_main_checkout() {
    let t = tree();
    let (main, wt) = (t.main.display(), t.worktree.display());
    assert_allowed(
        &[
            format!("cp {main}/src/lib.rs {wt}/src/lib.rs"),
            format!("cp {main}/src/lib.rs {wt}/src/"),
            format!("cp {main}/src/lib.rs src/lib.rs"),
            format!("cp {main}/src/lib.rs ./src"),
            format!("rsync -a {main}/src/lib.rs {wt}/src/"),
            format!("cp {main}/src/lib.rs /tmp/7228-probe/lib.rs"),
            format!("cp {main}/src/lib.rs /private/tmp/7228-probe/lib.rs"),
            format!("sed -i '' 's/a/b/' {wt}/src/lib.rs"),
            "sed -i 's/a/b/' src/lib.rs".to_string(),
        ],
        &t.worktree,
    );
}

// #7228: what the boundary never protected stays writable — documents and
// configuration in the checkout, a sibling that only shares its name prefix,
// tm state outside any checkout, and a scratchpad-rooted clone.
#[test]
fn allows_copy_verbs_outside_a_main_checkouts_source() {
    let t = tree();
    let (main, root) = (t.main.display(), t.root.display());
    let clone = t.root.join("scratchpad/clone");
    std::fs::create_dir_all(clone.join(".git")).expect("mkdir clone");
    std::fs::create_dir_all(t.root.join("home/.trusty-mpm")).expect("mkdir state");
    let clone = clone.display();
    assert_allowed(
        &[
            format!("cp notes.md {main}/docs/notes.md"),
            format!("cp src/lib.rs {main}/docs/lib.md"),
            format!("sed -i '' 's/a/b/' {main}/README.md"),
            format!("dd if=x of={main}/Cargo.toml"),
            format!("cp src/lib.rs {root}/repo-other/src/lib.rs"),
            format!("cp src/lib.rs {root}/repo-other/src/"),
            format!("cp src/lib.rs {root}/home/.trusty-mpm/probe.rs"),
            format!("cp src/lib.rs {clone}/src/lib.rs"),
            format!("rsync -a src/lib.rs user@host:{main}/src/lib.rs"),
        ],
        &t.worktree,
    );
}

// #7228: a symlinked destination resolves exactly as a redirect to the same
// path does — lexically, through the one `deny_reason_for_target` — so a
// link inside the worktree allows both, and a link to the checkout ROOT
// denies both.
#[test]
fn a_symlinked_destination_resolves_as_a_redirect_does() {
    let t = tree();
    let into_src = t.worktree.join("link");
    std::os::unix::fs::symlink(t.main.join("src"), &into_src).expect("symlink src");
    let to_root = t.root.join("rootlink");
    std::os::unix::fs::symlink(&t.main, &to_root).expect("symlink root");
    for (dir, file, denied) in [
        (into_src.clone(), into_src.join("lib.rs"), false),
        (to_root.join("src"), to_root.join("src/lib.rs"), true),
    ] {
        let redirect = format!("echo x > {}", file.display());
        let copy = format!("cp src/lib.rs {}/", dir.display());
        assert_eq!(
            verdict(&redirect, &t.worktree).is_some(),
            denied,
            "{redirect}"
        );
        assert_eq!(verdict(&copy, &t.worktree).is_some(), denied, "{copy}");
    }
}
