//! Tests for `hook_rewrite_chain` (#10973, #11037, #11507).

use super::super::compress::split_exit_sentinel;
use super::super::compress_chain::{parse_chain_spec, split_chain};
use super::super::hook_rewrite::{
    compression_rewrite_decision, rewrite_bash_command_for_compression,
};
use super::super::hook_rewrite_isolated::cwd_rewrite_decision;
use super::chain_rewrite_with_nonce;
use std::path::Path;
use trusty_mpm::core::savings_opportunity::SkipReason;

/// The rewrite of `command` with the marker nonce fixed to `n0`.
fn with_nonce(command: &str) -> Option<String> {
    let command = command.trim();
    let (prefix, inner) =
        super::super::hook_rewrite_cd::split_leading_cd(command).unwrap_or(("", command));
    chain_rewrite_with_nonce(prefix, inner, "n0").ok()
}

/// The `printf` the rewrite inserts before step `i`.
fn m(i: usize) -> String {
    format!("printf '\\n__tm_chain_n0_{i}__\\n'")
}

/// #11507: the chain text is kept byte for byte, with only marker steps added.
#[test]
fn a_safe_chain_is_compressed() {
    let s = "printf '\\n__tm_compress_exit=%s__\\n' \"$?\"; }";
    for (command, expected) in [
        (
            "cargo build && cargo test",
            format!(
                "{{ cargo build && {} && cargo test; {s} | tm compress --tool \"cargo build && cargo test\" --chain n0",
                m(1)
            ),
        ),
        (
            "cd crates/x && cargo build -p x && cargo test -p x",
            format!(
                "cd crates/x && {{ cargo build -p x && {} && cargo test -p x; {s} | tm compress --tool \"cargo build && cargo test\" --chain n0",
                m(1)
            ),
        ),
        (
            "cd x; git diff",
            format!("cd x; {{ git diff; {s} | tm compress --tool \"git diff\" --chain n0"),
        ),
        (
            "git diff;git log -3",
            format!(
                "{{ git diff;{}; git log -3; {s} | tm compress --tool \"git diff ; git log\" --chain n0",
                m(1)
            ),
        ),
        (
            "cargo test -p x | tail -40",
            format!(
                "{{ cargo test -p x | tail -40; {s} | tm compress --tool \"cargo test | tail\" --chain n0"
            ),
        ),
    ] {
        assert_eq!(with_nonce(command), Some(expected), "{command}");
    }
    for command in [
        "export RUST_LOG=debug && cargo test",
        "true; grep -n 'a b' notes.txt",
        "cd /repo && git diff HEAD~1 && ls -la | head -20",
        "cargo test && git diff",
        "echo start; date +%s; git diff",
        "cd 'my dir'; git diff; git log -3",
    ] {
        assert!(
            rewrite_bash_command_for_compression(command).is_some(),
            "{command}"
        );
    }
}

/// #10973: a destructive segment anywhere refuses the whole chain.
#[test]
fn a_destructive_segment_refuses_the_chain() {
    for command in [
        "git diff && rm -rf /",
        "cargo test && rm -rf target",
        "git diff; git push --force",
        "git log && git reset --hard HEAD~1",
        "ls -la && find . -delete",
        "find . -name x -exec rm {} ; && ls",
        "cargo test && git worktree remove --force x",
        "git diff && git checkout main",
        "cargo test | tee out.log",
        "cargo test | sort -o out.txt",
        "cargo test | xargs rm",
        "git diff && make build",
        "git diff && cat src/lib.rs",
        "git diff && sudo ls",
        "git grep -O foo && git diff",
        "rg --pre ./x foo && ls",
        "cargo test | sort --compress-program=x",
        "cargo test | uniq in.txt out.txt",
        "git diff && catalog-rm x",
        "cargo clippy --fix && cargo test",
        "cargo fmt && cargo test",
        "cargo install x && cargo test",
        "git diff --output=x && ls",
        "date -s 12:00; git diff",
    ] {
        assert_eq!(
            rewrite_bash_command_for_compression(command),
            None,
            "{command:?}"
        );
    }
}

/// #11507: every refused shape keeps its skip reason and is not rewritten.
#[test]
fn an_ambiguous_shape_is_not_compressed() {
    for command in [
        "git diff || ls -la",
        "cargo test & ls",
        "git diff && echo $(id)",
        "git diff && echo `id`",
        "(cd a && git diff)",
        "git diff && (ls)",
        "{ git diff; } && ls",
        "cargo test > out.log && ls",
        "cargo test 2>&1 | tail -5",
        "cargo test >> out.log; ls",
        "git diff && grep x < f",
        "cat <<EOF && ls\nhi\nEOF",
        "git diff && grep 'unclosed",
        "git diff && cd b && ls",
        "cd a && git diff; ls -la",
        "git diff | head | tail",
        "git diff | head && ls",
        "git diff |& head",
        "git diff;",
        "git diff\nls -la",
        "for f in a b; do git diff; done",
        "cd a; cd b; git diff",
        "cd a; git diff; cd b",
        "pushd a; git diff",
        "cd -; git diff",
        "cd $HOME; git diff",
        "cd a && cd b; git diff",
        "set -o pipefail; cargo test | tail",
        "set -e; git diff",
        "trap 'x' EXIT; git diff",
        "exit 1; git diff",
        "eval git diff; ls",
        "git diff; ls 'open",
    ] {
        assert_eq!(
            compression_rewrite_decision(command),
            Err(SkipReason::Chained),
            "{command:?}"
        );
    }
    // A chain with no covered step has nothing to filter.
    for command in ["true && true", "cd a; echo hi", "echo a; date"] {
        assert_eq!(
            compression_rewrite_decision(command),
            Err(SkipReason::NoFilter),
            "{command:?}"
        );
    }
}

/// #10973 (#7477): the no-shell isolated shape cannot carry a chain, so a
/// chain the brace rewrite accepts is still refused inside an isolation
/// worktree.
#[test]
fn an_isolation_worktree_refuses_every_chain() {
    let tree = Some(Path::new("/repo/.claude/worktrees/agent-a/crates/x"));
    for command in [
        "cargo build && cargo test",
        "git diff; git diff --stat",
        "cargo test | tail -40",
        "cd x; git diff",
    ] {
        assert!(
            rewrite_bash_command_for_compression(command).is_some(),
            "{command} is accepted outside, or this test proves nothing"
        );
        assert!(cwd_rewrite_decision(command, tree).is_err(), "{command}");
    }
}

/// Run `script` under `shell` in `cwd`: (exit code, stdout, stderr).
fn run_in(shell: &str, cwd: &Path, script: &str) -> (Option<i32>, String, String) {
    let out = std::process::Command::new(shell)
        .arg("-c")
        .arg(script)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("failed to run {shell}: {e}"));
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Run `command`'s rewrite with `tm compress --chain` replaced by its own
/// splitter and an identity filter: (exit code, stdout, stderr).
fn run_rewrite(
    shell: &str,
    cwd: &Path,
    setup: &str,
    command: &str,
) -> (Option<i32>, String, String) {
    let rewritten = rewrite_bash_command_for_compression(command)
        .unwrap_or_else(|| panic!("{command} must be rewritten"));
    let (head, tail) = rewritten
        .split_once("| tm compress --tool \"")
        .expect("the rewrite ends in the compress stage");
    let (spec, nonce) = tail.split_once("\" --chain ").expect("a chain rewrite");
    // A failed `cd <dir> && ` prefix never starts the pipeline: the shell's
    // own status stands, as it would in front of the real `tm compress`.
    let (code, out, err) = run_in(shell, cwd, &format!("{setup}{head}"));
    let (body, exit) = split_exit_sentinel(&out);
    let merged = split_chain(body, nonce, parse_chain_spec(spec).len()).concat();
    (exit.or(code), merged, err)
}

/// The original chain and its rewrite, run for real, must agree.
///
/// Why (#11507): requirement 1 and 2 — stdout byte for byte, stderr and status.
/// What: in a temp tree, each case runs bare and rewritten under sh, bash and
/// zsh. The filter stage is the real splitter (`compress_chain::split_chain`)
/// with an identity filter, since a real filter changes bytes by design.
/// `grep -q nomatch` stands in for `false`, which no chain admits. The cwd
/// after the command is checked in
/// `a_cd_semicolon_chain_leaves_the_same_cwd_in_a_real_shell`.
/// Test: itself.
#[test]
fn a_chain_rewrite_runs_like_the_original_in_a_real_shell() {
    let root = tempfile::tempdir().expect("temp dir");
    let root = root.path().canonicalize().expect("canonical temp dir");
    std::fs::create_dir_all(root.join("sub")).expect("mkdir");
    std::fs::write(root.join("sub/marker.txt"), "alpha\nhi there\nhi again\n").expect("write");
    let cases = [
        // `&&`, `;`, `| reader`, `cd X;`, `cd X &&`
        "grep -n hi sub/marker.txt && grep -c hi sub/marker.txt",
        "grep -n hi sub/marker.txt; grep -c alpha sub/marker.txt",
        "grep -n hi sub/marker.txt | head -1",
        "cd sub; grep -n hi marker.txt; grep -c hi marker.txt",
        "cd sub && grep -n hi marker.txt && grep -c hi marker.txt",
        // `false && echo b`: status 1, no `b`; `false; echo b`: `b`, status 0
        "grep -q nomatch sub/marker.txt && echo b",
        "grep -q nomatch sub/marker.txt; echo b",
        // a non-zero last step's status and stderr pass through
        "echo a; grep -n hi no-such-file",
        "grep -n hi no-such-file; grep -n hi sub/marker.txt",
        "cd missing; grep -n hi marker.txt",
        "cd missing && grep -n hi marker.txt && grep -c hi marker.txt",
        "cd sub && grep -n nomatch marker.txt | wc -l",
        "export X=1 && grep -c hi sub/marker.txt; date +%Y; true",
    ];
    for shell in ["sh", "bash", "zsh"] {
        if std::process::Command::new(shell)
            .args(["-c", "exit 0"])
            .output()
            .is_err()
        {
            eprintln!("SKIP a_chain_rewrite_runs_like_the_original_in_a_real_shell: no {shell}");
            continue;
        }
        for command in cases {
            let original = run_in(shell, &root, command);
            let rewritten = run_rewrite(shell, &root, "", command);
            assert_eq!(original, rewritten, "{shell}: `{command}`");
            if command.starts_with("cd ") && !command.contains(';') {
                let (_, cwd, _) = run_in(shell, &root, &format!("{command}\npwd -P"));
                let rewrite = rewrite_bash_command_for_compression(command).unwrap_or_default();
                let head = rewrite.split("| tm compress").next().unwrap_or_default();
                let (_, rcwd, _) = run_in(shell, &root, &format!("{head}| cat >/dev/null\npwd -P"));
                assert_eq!(
                    cwd.lines().last(),
                    rcwd.lines().last(),
                    "{shell}: cwd after `{command}`"
                );
            }
        }
        assert_eq!(
            run_in(shell, &root, cases[5]).0,
            Some(1),
            "{shell}: false && echo b"
        );
        assert_eq!(
            run_in(shell, &root, cases[6]),
            (Some(0), "b\n".into(), String::new())
        );
        // #10973 critic: under `set -o pipefail` a failing producer piped to a
        // reader keeps its status 1 through the rewrite too.
        if run_in(shell, &root, "set -o pipefail").0 != Some(0) {
            eprintln!("SKIP pipefail parity: {shell} has no pipefail");
            continue;
        }
        for command in [
            "grep -n nomatch sub/marker.txt | head -1",
            "cd sub && grep -n nomatch marker.txt | tail -3",
            "grep -n hi sub/marker.txt; grep -n nomatch sub/marker.txt | wc -l",
        ] {
            let setup = "set -o pipefail\n";
            let original = run_in(shell, &root, &format!("{setup}{command}"));
            assert_eq!(
                original.0,
                Some(1),
                "{shell}: pipefail status of `{command}`"
            );
            assert_eq!(
                original,
                run_rewrite(shell, &root, setup, command),
                "{shell}: `{command}`"
            );
        }
    }
}

/// #10973 critic, #11507: more than 30 diff lines followed by a log keep every
/// diff line, because each step now runs through its own filter.
/// Test: itself.
#[test]
fn a_diff_then_log_chain_keeps_every_diff_line_through_the_real_filter() {
    use trusty_agents_common::compress::tool_output::compress_tool_output;
    let diff: String = (0..40)
        .map(|i| format!("+added line {i} of the patch\n"))
        .collect();
    let diff = format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,0 +1,40 @@\n{diff}");
    let rewrite = with_nonce("git diff; git log -3").expect("a mixed chain is rewritten");
    let spec = rewrite
        .split_once("--tool \"")
        .and_then(|(_, t)| t.split_once('"'))
        .map(|(s, _)| s)
        .expect("a spec");
    let tools = parse_chain_spec(spec);
    assert_eq!(tools, [Some("git diff".into()), Some("git log".into())]);
    let filtered = compress_tool_output("git diff", &diff);
    for i in 0..40 {
        assert!(
            filtered.contains(&format!("+added line {i} of the patch")),
            "the diff step lost line {i}:\n{filtered}"
        );
    }
}

/// The shells of `["sh", "bash", "zsh"]` this host can run.
///
/// See #11644: `run_in` panics on a missing shell, so the `exit 0` guards in
/// the two tests below never skipped anything and a host without zsh (a Linux build box)
/// failed instead. The rewrite is POSIX-shaped and every case already runs
/// under `sh` and `bash`, so a missing zsh drops only the zsh leg, named on
/// stderr. `sh` must exist: a host without it fails rather than passing empty.
fn available_shells() -> Vec<&'static str> {
    let shells: Vec<&'static str> = ["sh", "bash", "zsh"]
        .into_iter()
        .filter(|shell| {
            let present = std::process::Command::new(shell)
                .args(["-c", "exit 0"])
                .output()
                .is_ok();
            if !present {
                eprintln!("no {shell} on this host; its leg of the shell parity test is not run");
            }
            present
        })
        .collect();
    assert!(shells.contains(&"sh"), "sh is required: {shells:?}");
    shells
}

/// #11507 (Architect, 07:45Z): a `cd <dir>;` or `cd <dir> && ` chain leaves the
/// caller's shell in the same directory as the original, and a `cd` into a
/// missing directory under `;` still runs the rest where the shell already was.
/// On 2fb258b432 the `cd <dir>;` ran inside the piped group, a subshell, so
/// the cwd after the rewrite was the start directory.
/// What: each case runs bare and rewritten (filter stage `cat >/dev/null`)
/// with `; pwd -P` appended, under sh, bash and zsh, and compares the last
/// line; output and status go through `run_rewrite` as well.
/// Test: itself.
#[test]
fn a_cd_semicolon_chain_leaves_the_same_cwd_in_a_real_shell() {
    let root = tempfile::tempdir().expect("temp dir");
    let root = root.path().canonicalize().expect("canonical temp dir");
    std::fs::create_dir_all(root.join("sub")).expect("mkdir");
    std::fs::write(root.join("sub/marker.txt"), "hi there\n").expect("write");
    std::fs::write(root.join("marker.txt"), "hi root\n").expect("write");
    // See #11644: zsh is run where present.
    for shell in available_shells() {
        for command in [
            "cd sub; grep -n hi marker.txt",
            "cd sub && grep -n hi marker.txt",
            "cd missing; grep -n hi marker.txt",
            "cd sub; grep -n hi marker.txt; grep -c hi marker.txt",
        ] {
            let rewrite = rewrite_bash_command_for_compression(command)
                .unwrap_or_else(|| panic!("{command} must be rewritten"));
            let head = rewrite.split("| tm compress").next().unwrap_or_default();
            let (_, cwd, _) = run_in(shell, &root, &format!("{command}; pwd -P"));
            let (_, rcwd, _) = run_in(shell, &root, &format!("{head}| cat >/dev/null; pwd -P"));
            assert_eq!(
                cwd.lines().last(),
                rcwd.lines().last(),
                "{shell}: cwd after `{command}` vs `{head}`"
            );
            // `cd sub && <one command>` is D60's single-tool form, whose
            // output parity `a_cd_rewrite_runs_like_the_original_in_a_real_shell` owns.
            if rewrite.contains("--chain ") {
                assert_eq!(
                    run_in(shell, &root, command),
                    run_rewrite(shell, &root, "", command),
                    "{shell}: `{command}`"
                );
            }
        }
    }
}

/// #11507 critic (HIGH): a failing covered step, then `&& x; y`. The `&&`
/// skips `x` and its marker, `y`'s marker still prints. On 2d7fded31a the
/// splitter stopped at the first missing marker, so the literal marker line
/// reached the caller with `y`'s output.
/// What: under sh, bash and zsh, the original and the rewrite (real splitter,
/// identity filter) agree on status, stdout, stderr and the cwd afterwards,
/// and no `__tm_chain_` text survives.
/// Test: itself.
#[test]
fn a_short_circuit_before_a_semicolon_runs_like_the_original() {
    let root = tempfile::tempdir().expect("temp dir");
    let root = root.path().canonicalize().expect("canonical temp dir");
    std::fs::create_dir_all(root.join("sub")).expect("mkdir");
    std::fs::write(root.join("sub/marker.txt"), "hi there\nhi again\n").expect("write");
    // See #11644: zsh is run where present.
    for shell in available_shells() {
        for command in [
            "grep -q nomatch sub/marker.txt && grep -n hi sub/marker.txt; grep -c hi sub/marker.txt",
            "cd sub; grep -q nomatch marker.txt && grep -n hi marker.txt; grep -c hi marker.txt",
            "grep -n hi sub/marker.txt; grep -q nomatch sub/marker.txt && grep -n hi sub/marker.txt; grep -n there sub/marker.txt",
            "grep -q nomatch sub/marker.txt && grep -n hi sub/marker.txt; grep -n hi no-such-file",
        ] {
            let original = run_in(shell, &root, command);
            let rewritten = run_rewrite(shell, &root, "", command);
            assert!(
                !rewritten.1.contains("__tm_chain_"),
                "{shell}: marker leaked: {rewritten:?}"
            );
            assert_eq!(original, rewritten, "{shell}: `{command}`");
            let rewrite = rewrite_bash_command_for_compression(command).unwrap_or_default();
            let head = rewrite.split("| tm compress").next().unwrap_or_default();
            let (_, cwd, _) = run_in(shell, &root, &format!("{command}; pwd -P"));
            let (_, rcwd, _) = run_in(shell, &root, &format!("{head}| cat >/dev/null; pwd -P"));
            assert_eq!(
                cwd.lines().last(),
                rcwd.lines().last(),
                "{shell}: cwd after `{command}`"
            );
        }
    }
}
