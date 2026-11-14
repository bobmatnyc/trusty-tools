//! Regression rows for the #10761 follow-up: secret-name reads the bracket fix
//! (PR #11205) still ALLOWED.
//!
//! What: (A) a quoted `!`/`^` sigil the lexer turned into negation; (B, C) a
//! `Grep` glob read with shell rules instead of ripgrep's; (E) the refusal for
//! a bracket word the guard cannot read. Every row the reviewers confirmed as
//! an allow is pinned here too.
//! Test: itself.

use crate::commands::pm_guard_secret_read::{
    evaluate_secret_file_read_command, evaluate_secret_file_read_tool,
};
use crate::commands::pm_guard_secret_rg_glob::{MAX_RG_GLOB_BYTES, rg_glob_readings};

fn denies(command: &str) -> bool {
    evaluate_secret_file_read_command(command).is_some()
}

fn grep_glob_denies(glob: &str) -> bool {
    let input = serde_json::json!({"pattern": "KEY", "path": "/repo", "glob": glob});
    evaluate_secret_file_read_tool("Grep", Some(&input)).is_some()
}

/// (A) bash and zsh read a quoted sigil as a member, so `.[\!eE]nv` expands
/// to `.env`; the lexer hands the guard `.[!eE]nv`.
#[test]
fn a_quoted_negation_sigil_is_judged_as_a_member() {
    for command in [
        r"cat .[\!eE]nv",
        r"cat .['!'eE]nv",
        r"cat .[\^eE]nv",
        r"head .[\!eE]nv",
        r"cat .e[\^nN]v",
        r#"cat .["!"eE]nv"#,
        // Under a directory prefix, and with other readers.
        r"cat ./conf/.[\!eE]nv",
        r"cat /srv/app/.e[\^nN]v",
        r"tail -n 5 .[\!eE]nv",
        r"grep KEY .[\!eE]nv",
        r"sed -n 1p .['!'eE]nv",
        r"less ~/.ssh/id_[\!a-zA-Z]sa",
        // Inside the wrappers the bracket rows already cover.
        r#"echo "$(cat .[\!eE]nv)""#,
        r"bash -c 'cat .[\!eE]nv'",
        r"grep -r KEY --include=.[\!eE]nv .",
        // The unquoted spelling is the same token, so it denies too.
        "cat .[!eE]nv",
        "cat id_[!a-zA-Z]sa",
    ] {
        assert!(denies(command), "`{command}` must deny");
    }
}

/// (B, C) ripgrep reads `\` inside a class as a member, `\x` outside one as
/// `x`, and `{a,b}` before any basename cut.
#[test]
fn a_grep_glob_is_read_by_ripgrep_rules() {
    for glob in [
        r".e[!\]v",
        r".e[^\]v",
        r".e\nv",
        r"\.env",
        "{src/x,.env}",
        "**/{src/x,.env}",
        "{a,{b,.env}}",
        r"conf/.e[^\]v",
        r"**/.e[!\]v",
        r"{x,.e\nv}",
        "id_{rsa,x}",
        ".e[a-c-n]v",
    ] {
        assert!(grep_glob_denies(glob), "glob `{glob}` must deny");
    }
    for glob in [
        "*.json",
        "src/[a-z]*.rs",
        "[!/]*.md",
        "*.{rs,toml}",
        "{src,tests}/**/*.rs",
        r"\[x\].md",
    ] {
        assert!(!grep_glob_denies(glob), "glob `{glob}` must allow");
    }
}

/// (B, C) a glob the shell-rule reading refused before the follow-up stays
/// refused though ripgrep's own rules reach no secret name: a brace sequence,
/// an ambiguous `]`, and a nested alternation the brace expander cannot read.
#[test]
fn a_grep_glob_refused_by_shell_rules_stays_refused() {
    for glob in [".{d..f}nv", ".e[x]n]v", "{a,{b,c}}.rs"] {
        assert!(rg_glob_readings(glob).is_some(), "glob `{glob}`");
        assert!(grep_glob_denies(glob), "glob `{glob}` must still deny");
    }
}

/// (B, C) a glob ripgrep would reject, or one with more readings than the
/// guard enumerates, fails closed.
#[test]
fn a_malformed_or_oversized_grep_glob_fails_closed() {
    let oversized = "{a,b}".repeat(9) + ".txt";
    for glob in ["[abc", "{a,b", "a}", r"abc\", "[z-a]x", oversized.as_str()] {
        assert_eq!(rg_glob_readings(glob), None, "glob `{glob}`");
        assert!(grep_glob_denies(glob), "glob `{glob}` must fail closed");
    }
    // Many classes alone are read as `?` rather than refused outright.
    assert!(rg_glob_readings("[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9].log").is_some());
}

/// (B, C) a glob nested far deeper than any routine glob fails closed and
/// cannot overflow the stack, through the guard or the reader alone.
#[test]
fn a_deeply_nested_grep_glob_fails_closed_without_overflow() {
    let depth = 100_000;
    let deep = "{".repeat(depth) + "x" + &"}".repeat(depth);
    assert!(grep_glob_denies(&deep), "a deep glob must fail closed");
    assert_eq!(rg_glob_readings(&deep), None);
    let nested = |n: usize| "{".repeat(n) + "x" + &"}".repeat(n);
    assert!(
        rg_glob_readings(&nested(32)).is_some(),
        "32 levels are read"
    );
    assert_eq!(rg_glob_readings(&nested(33)), None, "33 levels fail closed");
    // #10761: a deep glob under the length cap reaches the readers and denies.
    let under_cap = nested(1_000);
    assert!(under_cap.len() <= MAX_RG_GLOB_BYTES);
    let denied = within(10, move || grep_glob_denies(&under_cap));
    assert_eq!(denied, Some(true), "a deep glob under the cap must deny");
}

/// `run` on its own thread, answered within `secs` seconds, else `None`.
fn within<T: Send + 'static>(secs: u64, run: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(run());
    });
    rx.recv_timeout(std::time::Duration::from_secs(secs)).ok()
}

/// (B, C) a `Grep` glob over the cap is refused as too long to judge; the
/// costliest glob just under it is judged in bounded time.
#[test]
fn a_grep_glob_over_the_cap_is_refused_in_bounded_time() {
    let cap = MAX_RG_GLOB_BYTES;
    // Names no secret, so only the cap can refuse it.
    let over = "a".repeat(cap + 1) + ".rs";
    let input = serde_json::json!({"pattern": "KEY", "path": "/repo", "glob": over});
    let reason = evaluate_secret_file_read_tool("Grep", Some(&input)).unwrap_or_default();
    assert!(reason.contains("too long to judge"), "{reason}");
    // At the cap the same glob is read, and allowed.
    let at_cap = "a".repeat(cap - 3) + ".rs";
    assert!(
        !grep_glob_denies(&at_cap),
        "a harmless glob at the cap allows"
    );

    // 256 readings, each nearly the whole cap long.
    let head = "{a,b}".repeat(8);
    let under = head.clone() + &"c".repeat(cap - head.len());
    assert_eq!(under.len(), cap);
    let started = std::time::Instant::now();
    let judged = within(60, move || grep_glob_denies(&under));
    eprintln!(
        "worst Grep glob just under the cap: {:?}",
        started.elapsed()
    );
    assert!(judged.is_some(), "the guard must answer within 60 s");
}

/// (B, C) a long glob is read in bounded time by the reader alone. Pinned,
/// not a fix: it already passed before the depth cap.
#[test]
fn a_long_grep_glob_is_read_in_bounded_time() {
    let read = within(10, || {
        let glob = "a".repeat(200_000);
        rg_glob_readings(&glob).is_some_and(|r| r == [glob])
    });
    assert_eq!(read, Some(true), "the reader must finish within 10 s");
}

/// (E) a bracket word the guard cannot read exactly fails closed with its own
/// reason, not the claim that its name is in the secret class.
#[test]
fn an_unreadable_bracket_word_is_refused_with_its_own_reason() {
    let class_claim = "its name is in this guard's secret-bearing file class";
    let closes = format!("cat .[x{}nv", "]".repeat(17));
    for (command, why) in [
        (
            "find . -name '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]*.log'",
            "each class was read as `?`",
        ),
        (
            "jq '.a[0][1][2][3][4][5][6][7][8]' data.json",
            "each class was read as `?`",
        ),
        (closes.as_str(), "more readings than this guard can check"),
    ] {
        let reason = evaluate_secret_file_read_command(command).unwrap_or_default();
        assert!(reason.contains("issue #10761"), "{command}: {reason}");
        assert!(reason.contains(why), "{command}: {reason}");
        assert!(!reason.contains(class_claim), "{reason}");
    }
    let reason = evaluate_secret_file_read_command("cat .env").unwrap_or_default();
    assert!(reason.contains(class_claim), "{reason}");
}

/// Every allow row the #10761 reviewers confirmed stays an allow.
#[test]
fn confirmed_allow_rows_stay_allowed() {
    for command in [
        "ls [a-z]*.rs",
        "cat src/[a-z]*.rs",
        "test -f x && [ -n \"$v\" ]",
        "grep '[^a]' file.txt",
        "echo ${a[0]}",
        "printf '%s\\n' \"${arr[@]}\"",
        "sed -E 's/[a-z]+/X/' notes.txt",
        "rg '[A-Z]{3}' src",
        "awk '/[0-9]+/' data.txt",
        "grep -E '^[[:space:]]*#' config.txt",
        "git log --format='[%h] %s'",
        "jq '.items[] | .name' data.json",
        "grep -rn 'env' src/[!.]*.rs",
        "ls .[!.]*",
        "if [[ -f a.txt && -n \"$x\" ]]; then echo ok; fi",
        "grep -E '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' ids.txt",
        "grep -E '^[0-9]{4}-[0-9]{2}' log.txt",
        "ls src/[a-z]*/[a-z]*.rs",
        "tr '[a-z]' '[A-Z]' < notes.txt",
        r"cat .[x\]y]nv",
        "cat a[0]b[1].txt",
    ] {
        assert_eq!(
            evaluate_secret_file_read_command(command),
            None,
            "`{command}` must allow"
        );
    }
}
