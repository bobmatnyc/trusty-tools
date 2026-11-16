//! Tests for `hook_rewrite_chain` (#10973, #11037).

use super::super::hook_rewrite::rewrite_bash_command_for_compression;
use super::super::hook_rewrite_isolated::cwd_rewrite_decision;
use std::path::Path;

/// What `hook_rewrite` emits for `<prefix><body>`, built from the producer.
fn expected(prefix: &str, body: &str, tool: &str) -> String {
    format!(
        "{prefix}{} | tm compress --tool \"{tool}\"",
        crate::commands::compress::wrap_command_reporting_exit(body)
    )
}

/// #10973: on the pre-fix commit every one of these returned `None`.
#[test]
fn a_safe_chain_is_compressed() {
    for (command, prefix, body, tool) in [
        (
            "cargo build && cargo test",
            "",
            "cargo build && cargo test",
            "cargo test",
        ),
        (
            "cd crates/x && cargo build -p x && cargo test -p x",
            "cd crates/x && ",
            "cargo build -p x && cargo test -p x",
            "cargo test",
        ),
        (
            "git diff; git log -3",
            "",
            "git diff; git log -3",
            "git log",
        ),
        (
            "cargo test -p x | tail -40",
            "",
            "cargo test -p x | tail -40",
            "cargo test",
        ),
        (
            "cd /repo && git diff HEAD~1 && ls -la | head -20",
            "cd /repo && ",
            "git diff HEAD~1 && ls -la | head -20",
            "ls -la",
        ),
        (
            "export RUST_LOG=debug && cargo test",
            "",
            "export RUST_LOG=debug && cargo test",
            "cargo test",
        ),
        (
            "true; grep -n 'a b' notes.txt",
            "",
            "true; grep -n 'a b' notes.txt",
            "grep -n",
        ),
    ] {
        assert_eq!(
            rewrite_bash_command_for_compression(command).as_deref(),
            Some(expected(prefix, body, tool).as_str()),
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
        "git diff && catalog-rm x",
        "cargo clippy --fix && cargo test",
        "cargo fmt && cargo test",
        "cargo install x && cargo test",
        "git diff --output=x && ls",
    ] {
        assert_eq!(
            rewrite_bash_command_for_compression(command),
            None,
            "{command:?}"
        );
    }
}

/// #10973: an ambiguous shape fails closed, uncompressed.
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
        "git diff && # note",
        "true && true",
    ] {
        assert_eq!(
            rewrite_bash_command_for_compression(command),
            None,
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
        "git diff; git log -3",
        "cargo test | tail -40",
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

/// The original chain and its rewrite, run for real, must agree.
///
/// What: as `a_cd_rewrite_runs_like_the_original_in_a_real_shell`, with the
/// `tm compress` stage replaced by a filter that strips the #7384 sentinel and
/// exits with the status it carries. Covers `&&` and `;` short-circuit and
/// status, a trailing reader, and the leading `cd`'s cwd afterwards.
/// Test: itself.
#[test]
fn a_chain_rewrite_runs_like_the_original_in_a_real_shell() {
    let root = tempfile::tempdir().expect("temp dir");
    let root = root.path().canonicalize().expect("canonical temp dir");
    std::fs::create_dir_all(root.join("sub")).expect("mkdir");
    std::fs::write(root.join("sub/marker.txt"), "alpha\nhi there\nhi again\n").expect("write");
    let filter = root.join("filter.sh");
    std::fs::write(
        &filter,
        "all=$(cat)\nstatus=${all##*__tm_compress_exit=}\nstatus=${status%__}\n\
         printf '%s\\n' \"${all%__tm_compress_exit=*}\"\nexit \"$status\"\n",
    )
    .expect("write filter");
    let cases = [
        "cd sub && grep -n hi marker.txt && ls -la marker.txt",
        "cd sub && grep -n nomatch marker.txt && ls -la marker.txt",
        "grep -n nomatch sub/marker.txt; ls -la sub/marker.txt",
        "ls -la no-such-file; grep -n hi sub/marker.txt",
        "grep -n hi sub/marker.txt; ls -la no-such-file",
        "cd sub && grep -n hi marker.txt | head -1",
        "cd sub && grep -n nomatch marker.txt | wc -l",
        "cd missing && grep -n hi marker.txt && ls -la",
        "grep -n hi sub/marker.txt; ls -la no-such-file; ls -la sub",
    ];
    let lines = |s: &str| {
        s.lines()
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
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
            let rewritten = rewrite_bash_command_for_compression(command)
                .unwrap_or_else(|| panic!("{command} must be rewritten"));
            let (head, _tool) = rewritten
                .split_once("| tm compress")
                .expect("the rewrite ends in the compress stage");
            let script = format!("{head}| sh '{}'", filter.display());
            let (code, out, err) = run_in(shell, &root, command);
            let (rcode, rout, rerr) = run_in(shell, &root, &script);
            assert_eq!(code, rcode, "{shell}: status of `{command}` vs `{script}`");
            assert_eq!(lines(&out), lines(&rout), "{shell}: stdout of `{command}`");
            assert_eq!(err, rerr, "{shell}: stderr of `{command}`");
            let (_, cwd, _) = run_in(shell, &root, &format!("{command}\npwd -P"));
            let (_, rcwd, _) = run_in(shell, &root, &format!("{script}\npwd -P"));
            assert_eq!(
                cwd.lines().last(),
                rcwd.lines().last(),
                "{shell}: cwd after `{command}`"
            );
        }
    }
}
