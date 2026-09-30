//! Unit tests for the #8878 Q2 delete ruling: a delete of a trust anchor is
//! an anchor write (`pm_guard_bash/anchor_deletes.rs`,
//! `Anchors::judge_delete`).

use super::*;

/// Every anchor the floor lists, as a PM would spell it; the launch record,
/// its sidecar and an arming record exist.
fn anchors(fx: &Fixture) -> Vec<String> {
    launch_record(fx, ARCHITECT, &fx.project);
    let launch = fx.home.join(ANCHOR_ROOT).join(ARCHITECT_DIR);
    std::fs::write(launch.join("60.architect-session"), "tm-architect\n").expect("sidecar");
    arm(fx);
    let mut out: Vec<String> = [
        "~/.trusty-mpm/architect-launch",
        "~/.trusty-mpm/architect-launch/",
        "~/.trusty-mpm/architect-launch/60.architect",
        "~/.trusty-mpm/architect-launch/60.architect-session",
        "~/.trusty-mpm/architect-launch/60.ARCHITECT",
        "$HOME/.trusty-mpm/config.toml",
        "~/.trusty-mpm/twin/armed/1.json",
        "~/.trusty-mpm/twin/armed/1.JSON",
        "~/.trusty-mpm/twin/armed",
        "~/.trusty-mpm/twin",
        "~/.trusty-mpm",
        "~",
    ]
    .map(str::to_string)
    .to_vec();
    out.push(launch.join("60.architect").display().to_string());
    out
}

/// Each delete verb, `@` standing for the operand.
const DELETE_SHAPES: &[&str] = &[
    "rm @",
    "rm -f @",
    "rm -rf @",
    "rm -R -- @",
    "rm -d @",
    "grm -rf @",
    "unlink @",
    "trash @",
    "rmdir @",
    "shred -u @",
    "shred -n 3 -u @",
    "truncate -s 0 @",
    "truncate --size=0 @",
    ": > @",
    "> @",
    "mv @ /tmp/moved-out",
    "find @ -delete",
    "find @ -type f -exec rm {} \\;",
    "find @ -execdir rm -f {} +",
    // #8735: the program-word resolver's wrappers and precommand words.
    "command rm @",
    "env rm -f @",
    "sudo rm -rf @",
    "sudo -u root rm @",
    "nice -n 5 rm @",
    "\\rm @",
    "/bin/rm -f @",
    "FOO=1 rm @",
    // Nested bodies.
    "bash -c 'rm -f @'",
    "(rm -rf @)",
    "true && rm @",
];

/// Every delete verb on every anchor is denied to a PM.
#[test]
fn each_delete_verb_on_each_anchor_is_denied() {
    let fx = fixture();
    for anchor in anchors(&fx) {
        for shape in DELETE_SHAPES {
            let command = shape.replace('@', &anchor);
            let reason = pm_bash(&fx, &command).unwrap_or_else(|| panic!("allowed: {command}"));
            assert!(reason.contains("#8878"), "{command}: {reason}");
        }
    }
}

/// Removing the launch directory, or a directory above an anchor.
#[test]
fn directory_removal_above_an_anchor_is_denied() {
    let fx = fixture();
    launch_record(&fx, ARCHITECT, &fx.project);
    let root = fx.home.join(ANCHOR_ROOT);
    for (cwd, command) in [
        (&root, "rm -r architect-launch"),
        (&root, "rmdir architect-launch"),
        (&root, "rm -rf ./architect-launch/"),
        (&root, "rm -rf architect-launch/*"),
        (&root, "rm -f *.toml"),
        (&root, "find . -delete"),
        (&root, "find -delete"),
        (&fx.home, "rm -rf .trusty-mpm"),
        (&fx.home, "rm -rf .*"),
        (&fx.home, "rmdir -p .trusty-mpm/architect-launch/x"),
        (
            &fx.cwd,
            "rm -rf ../home/.trusty-mpm/architect-launch/../architect-launch",
        ),
        (&fx.cwd, "rm -rf ~/.trusty-mpm/architect-launch/.."),
        (&fx.cwd, "rm -rf ~/.trusty-mpm/*"),
        (&fx.cwd, "rm -rf ~/.trusty-mpm/architect-launch/*.architect"),
        (&fx.cwd, "rm -rf /"),
    ] {
        let reason = pm_bash_in(&fx, cwd, command).unwrap_or_else(|| panic!("allowed: {command}"));
        assert!(reason.contains("#8878"), "{command}: {reason}");
    }
}

/// A delete the guard cannot place — after a `cd`, through a variable, fed
/// by `xargs`, or through a path that does not resolve — is denied when it
/// could name an anchor (Fail-Open Check: the error arms deny).
#[test]
fn an_unplaceable_delete_is_denied() {
    let fx = fixture();
    std::os::unix::fs::symlink("loop", fx.cwd.join("loop")).expect("symlink loop");
    for command in [
        "cd /tmp && rm 60.architect",
        "cd /tmp && rm -f 60.Architect-Session",
        "cd /tmp && rm -rf architect-launch",
        "cd /tmp && rm -f config.toml",
        "cd /tmp && rm -rf *",
        "cd /tmp && find . -delete",
        "cd /tmp && rmdir twin",
        "pushd /tmp && unlink 1.architect",
        "rm -rf $D",
        "rm -rf \"$D\"",
        "rm -rf ${D}/architect-launch",
        "rm -f \"$D\"/60.architect",
        "rm -f $D/*",
        "rm -rf ~other/.trusty-mpm",
        "find $D -delete",
        "truncate -s 0 $F",
        "echo ~/.trusty-mpm/config.toml | xargs rm",
        "find /tmp -name x | xargs rm -f",
        "ls | xargs -0 unlink",
        "ls | xargs -I% rmdir %",
        "ls | xargs find -delete",
        // Its parent does not resolve: the guard cannot see where it lands.
        "rm -f loop/x",
        "rm -rf loop/*",
    ] {
        let reason = pm_bash(&fx, command).unwrap_or_else(|| panic!("allowed: {command}"));
        assert!(reason.contains("#8878"), "{command}: {reason}");
    }
}

/// A `find` deletes under its start points only when its expression deletes.
#[test]
fn find_deletes_under_its_start_points() {
    let fx = fixture();
    launch_record(&fx, ARCHITECT, &fx.project);
    for command in [
        "find ~/.trusty-mpm -name '*.architect' -delete",
        "find -L ~/.trusty-mpm/architect-launch -type f -delete",
        "find /tmp ~/.trusty-mpm -exec rm {} +",
        "find ~/.trusty-mpm -ok rm {} \\;",
        "find ~/.trusty-mpm -okdir unlink {} \\;",
        "find ~/.trusty-mpm -exec mv {} /tmp \\;",
        "find ~/.trusty-mpm -exec sh -c 'rm \"$1\"' _ {} \\;",
        "find ~/.trusty-mpm -exec sudo rm {} +",
        "find ~/.trusty-mpm -exec truncate -s 0 {} +",
        "find ~ -name '*.architect' -delete",
    ] {
        let reason = pm_bash(&fx, command).unwrap_or_else(|| panic!("allowed: {command}"));
        assert!(reason.contains("#8878"), "{command}: {reason}");
    }
    for command in [
        "find ~/.trusty-mpm -name '*.architect'",
        "find ~/.trusty-mpm -exec cat {} +",
        "find ~/.trusty-mpm -type f -print0 | xargs -0 ls -l",
        "find . -name '*.orig' -delete",
        "find ~/.trusty-mpm/logs -name '*.log' -delete",
        "ls | xargs find",
    ] {
        assert_eq!(pm_bash(&fx, command), None, "denied: {command}");
    }
}

/// Deletes that miss every anchor are unaffected.
#[test]
fn ordinary_deletes_stay_allowed() {
    let fx = fixture();
    launch_record(&fx, ARCHITECT, &fx.project);
    std::fs::create_dir_all(fx.cwd.join("a/b")).expect("mkdir a/b");
    std::fs::create_dir_all(fx.home.join(ANCHOR_ROOT).join("logs")).expect("mkdir logs");
    std::os::unix::fs::symlink(&fx.cwd, fx.cwd.join("here")).expect("symlink here");
    for command in [
        "rm a.txt",
        "rm -rf build target/",
        "rm -f *.log",
        "rm -f a/*.tmp",
        "rm -rf a/b",
        "rmdir -p a/b",
        "unlink a.txt",
        "trash a.txt",
        "shred -u a.txt",
        "truncate -s 0 a.txt",
        ": > a.txt",
        "rm here",
        "rm ~/.trusty-mpm/logs/tm.log",
        "rm -f ~/.trusty-mpm/logs/*.log",
        "rm -rf ~/.trusty-mpm/sessions",
        "rm -rf \"$TMPDIR\"/scratch",
        "cd /tmp && rm -rf build",
        "mv ~/.trusty-mpm/logs/tm.log /tmp/",
        "git rm a.txt",
        "echo rm ~/.trusty-mpm/config.toml",
        "ls | xargs grep rm",
    ] {
        assert_eq!(pm_bash(&fx, command), None, "denied: {command}");
    }
}

/// Only the Architect's main thread may delete an anchor; its subagent may not.
#[test]
fn the_architect_main_thread_may_delete_an_anchor() {
    let fx = fixture();
    let env = architect_env(&fx);
    for command in [
        "rm -f ~/.trusty-mpm/architect-launch/60.architect",
        "rm -rf ~/.trusty-mpm/architect-launch",
        "find ~/.trusty-mpm/twin -delete",
    ] {
        let main = payload(&fx, "Bash", json!({ "command": command }));
        assert_eq!(evaluate(&main, &env, || allowlist(&fx)), None, "{command}");
        let mut sub = main.clone();
        sub["agent_id"] = json!("agent-7");
        assert!(
            evaluate(&sub, &env, || allowlist(&fx)).is_some(),
            "subagent allowed: {command}"
        );
    }
}
