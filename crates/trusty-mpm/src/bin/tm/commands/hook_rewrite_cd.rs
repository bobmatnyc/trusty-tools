//! The one chained form the compression rewrite accepts: `cd <dir> && <cmd>`.
//!
//! Why (D60, Refs #10973): `hook_rewrite::has_unsafe_pipe_composition` refuses
//! every `&`, so the common `cd <dir> && cargo test` got no compression and the
//! savings-opportunity row recorded the refusal. Owner ruling D60: "Allow the
//! simple 'cd X && cmd' form now."
//! What: [`split_leading_cd`] recognises exactly one leading `cd <dir> && ` and
//! returns it with the command after it. `hook_rewrite` checks that command
//! against every existing guard on its own, wraps only it, and keeps the `cd`
//! prefix verbatim in front: `cd <dir> && { <cmd>; printf …; } | tm compress …`.
//! `a && b | c` parses as `a && (b | c)`, so a failing `cd` short-circuits with
//! its own status and message and the filter never starts, and the `cd` runs
//! in the calling shell, not in a pipeline subshell, so the shell's cwd after
//! the call is the same as the original's. The isolation-worktree shape
//! (`hook_rewrite_isolated`) still refuses the form, as before.
//! Test: `rewrite_accepts_one_leading_cd_before_a_covered_command`,
//! `rewrite_refuses_every_other_cd_chain`,
//! `a_cd_rewrite_runs_like_the_original_in_a_real_shell`,
//! `an_isolation_worktree_still_refuses_the_cd_form`.

/// Split `command` into its leading `cd <dir> && ` prefix and the command after.
///
/// Why: see the module doc. The narrowest grammar that covers the agent habit.
/// What: `Some((prefix, cmd))` only for
/// `cd` BLANK+ DIR BLANK+ `&&` BLANK+ CMD, where BLANK is a space or a tab
/// (never a newline), CMD is non-empty, and DIR is one word that is either
/// bare, `'…'` or `"…"` as a whole (no mixed quoting, nothing touching the
/// closing quote). DIR's text, quotes removed, is non-empty, does not start
/// with `-` (so `cd -`, `cd -P x` and `cd --` are refused), and holds only
/// ASCII letters, digits and `/ . _ - + , : @`, plus a space inside quotes.
/// A bare DIR may also be `~` or start with `~/`. No `$`, backtick, `\`,
/// glob, brace, paren, redirect or other operator can appear in DIR, quoted
/// or not, so the prefix has no expansion beyond `~`. Everything else,
/// including a bare `cd`, `\cd`, `builtin cd` and a `cd` after any other word,
/// is `None`. CMD is not checked here; the caller runs the existing guards on
/// it, which refuse any further `&`, `|`, `;`, `>` or `<`.
/// Test: `rewrite_accepts_one_leading_cd_before_a_covered_command`,
/// `rewrite_refuses_every_other_cd_chain`.
pub(crate) fn split_leading_cd(command: &str) -> Option<(&str, &str)> {
    let after_cd = skip_blanks(command.strip_prefix("cd")?)?;
    let after_dir = &after_cd[dir_word_len(after_cd)?..];
    let after_and = skip_blanks(after_dir)?.strip_prefix("&&")?;
    let cmd = skip_blanks(after_and)?;
    if cmd.is_empty() {
        return None;
    }
    Some((&command[..command.len() - cmd.len()], cmd))
}

/// `s` past one or more leading spaces or tabs; `None` when there is none.
fn skip_blanks(s: &str) -> Option<&str> {
    let rest = s.trim_start_matches([' ', '\t']);
    (rest.len() < s.len()).then_some(rest)
}

/// A character DIR may hold anywhere; see [`split_leading_cd`].
fn is_dir_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | ',' | ':' | '@')
}

/// The byte length of the DIR word at the start of `s`, quotes included.
///
/// What: `None` when the word breaks [`split_leading_cd`]'s DIR rules. A bare
/// word ends at the first character [`is_dir_char`] (or a leading `~`) does not
/// cover; the caller then requires a blank there, so `x;`, `x&&` or `x$y` fail.
fn dir_word_len(s: &str) -> Option<usize> {
    let (len, body, quoted) = match s.chars().next()? {
        q @ ('\'' | '"') => {
            let close = s[1..].find(q)?;
            (close + 2, &s[1..=close], true)
        }
        _ => {
            let tilde = usize::from(s.starts_with('~'));
            let end = s[tilde..]
                .find(|c| !is_dir_char(c))
                .map_or(s.len(), |i| i + tilde);
            let body = &s[tilde..end];
            // `~` alone or `~/…` only: `~user`, `~-` and `~+` expand to more.
            if tilde == 1 && !(body.is_empty() || body.starts_with('/')) {
                return None;
            }
            (end, body, false)
        }
    };
    let bare_tilde = !quoted && len == 1;
    if body.starts_with('-') || (body.is_empty() && !bare_tilde) {
        return None;
    }
    body.chars()
        .all(|c| is_dir_char(c) || (quoted && c == ' '))
        .then_some(len)
}

#[cfg(test)]
mod tests {
    use super::super::hook_rewrite::rewrite_bash_command_for_compression;
    use super::super::hook_rewrite_isolated::rewrite_bash_command_for_cwd;
    use std::path::Path;

    /// What `hook_rewrite` emits for `<prefix><cmd>`, built from the producer.
    fn expected(prefix: &str, cmd: &str, tool: &str) -> String {
        format!(
            "{prefix}{} | tm compress --tool \"{tool}\"",
            crate::commands::compress::wrap_command_reporting_exit(cmd)
        )
    }

    /// D60 regression: on main every one of these returned `None`.
    #[test]
    fn rewrite_accepts_one_leading_cd_before_a_covered_command() {
        for (command, prefix, cmd, tool) in [
            ("cd /tmp && git diff", "cd /tmp && ", "git diff", "git diff"),
            (
                "cd crates/x && cargo test -p x",
                "cd crates/x && ",
                "cargo test -p x",
                "cargo test",
            ),
            ("cd\t..\t&&\tls -la", "cd\t..\t&&\t", "ls -la", "ls -la"),
            ("cd ~ && git log", "cd ~ && ", "git log", "git log"),
            (
                "cd ~/src && git diff",
                "cd ~/src && ",
                "git diff",
                "git diff",
            ),
            (
                "cd 'my dir' && git diff",
                "cd 'my dir' && ",
                "git diff",
                "git diff",
            ),
            (
                "cd \"a/b c\" && ls -la",
                "cd \"a/b c\" && ",
                "ls -la",
                "ls -la",
            ),
            (
                "  cd /repo  &&  git diff HEAD~1  ",
                "cd /repo  &&  ",
                "git diff HEAD~1",
                "git diff",
            ),
        ] {
            assert_eq!(
                rewrite_bash_command_for_compression(command).as_deref(),
                Some(expected(prefix, cmd, tool).as_str()),
                "{command}"
            );
        }
    }

    #[test]
    fn rewrite_refuses_every_other_cd_chain() {
        for command in [
            // two `cd`s
            "cd a && cd b && git diff",
            "cd a && cd b",
            "cd a && \\cd b",
            // `cd` with `;`, `||`, `|` or `&`
            "cd a; git diff",
            "cd a ; git diff",
            "cd a || git diff",
            "cd a | git diff",
            "cd a & git diff",
            "cd a &&& git diff",
            "cd a && & git diff",
            // `cmd && cd`
            "git diff && cd a",
            // `cd` with no `&&`
            "cd a",
            "cd a git diff",
            // a second `&&`, or another operator after the first
            "cd a && git diff && ls -la",
            "cd a && git diff; ls -la",
            "cd a && git diff | head",
            "cd a && git diff > out.txt",
            // subshells, substitution, expansion, redirects or operators in DIR
            "(cd a && git diff)",
            "(cd a) && git diff",
            "cd $(pwd) && git diff",
            "cd `pwd` && git diff",
            "cd $HOME && git diff",
            "cd \"$HOME\" && git diff",
            "cd a>b && git diff",
            "cd a<b && git diff",
            "cd 'a;b' && git diff",
            "cd 'a|b' && git diff",
            "cd \"a&&b\" && git diff",
            "cd a\\ b && git diff",
            "cd {a,b} && git diff",
            "cd a* && git diff",
            "cd 'a'b && git diff",
            "cd 'a''b' && git diff",
            "cd \"a\" b && git diff",
            "cd ~user && git diff",
            "cd a#b && git diff",
            // `cd -`, a bare `cd`, options, an empty DIR
            "cd - && git diff",
            "cd -P a && git diff",
            "cd -- a && git diff",
            "cd && git diff",
            "cd",
            "cd '' && git diff",
            // spacing the grammar does not cover
            "cd a&& git diff",
            "cd a &&git diff",
            "cd\na && git diff",
            "cd a\n&& git diff",
            "cd a && ",
            // not a literal leading `cd`
            "\\cd a && git diff",
            "builtin cd a && git diff",
            "FOO=1 cd a && git diff",
            "cdx a && git diff",
            // CMD that the existing rules already refuse on its own
            "cd a && make build",
            "cd a && git status",
            "cd a && cat src/lib.rs",
            "cd a && git diff # note",
            "cd a && git diff \\",
            "cd a && echo $(id)",
        ] {
            assert_eq!(
                rewrite_bash_command_for_compression(command),
                None,
                "{command:?}"
            );
        }
    }

    #[test]
    fn an_isolation_worktree_still_refuses_the_cd_form() {
        // Requirement: the isolated shape behaves as before for every form.
        let tree = Some(Path::new("/repo/.claude/worktrees/agent-a"));
        assert!(
            rewrite_bash_command_for_cwd("git diff", tree).is_some(),
            "a covered command is rewritten there, or this test proves nothing"
        );
        assert!(rewrite_bash_command_for_cwd("cd /tmp && git diff", tree).is_none());
        let outside = rewrite_bash_command_for_cwd("cd /tmp && git diff", Some(Path::new("/repo")))
            .map(|r| r.command);
        assert_eq!(
            outside,
            rewrite_bash_command_for_compression("cd /tmp && git diff")
        );
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

    /// The original and its rewrite, run for real, must agree.
    ///
    /// Why: requirement 3 of D60 — `<cmd>` runs in `<dir>`, with the same output,
    /// status and `cd` failure, and the shell's cwd afterwards is unchanged.
    /// What: in a temp tree, each case runs bare and rewritten under sh, bash and
    /// zsh, each followed by `pwd -P` on a new line, and compares exit code,
    /// stdout and stderr. The `tm compress` stage is replaced by a filter that
    /// strips the #7384 sentinel and exits with the status it carries, which is
    /// the status `tm compress` reads (`tm_compress_reports_the_wrapped_commands_exit_status`);
    /// the real binary always exits 0 (#4837), for this form as for a plain
    /// rewrite, and the end-to-end hook test runs it.
    /// Test: itself.
    #[test]
    fn a_cd_rewrite_runs_like_the_original_in_a_real_shell() {
        let root = tempfile::tempdir().expect("temp dir");
        let root = root.path().canonicalize().expect("canonical temp dir");
        for dir in ["sub", "with space"] {
            std::fs::create_dir_all(root.join(dir)).expect("mkdir");
            std::fs::write(root.join(dir).join("marker.txt"), "alpha\nhi there\n").expect("write");
        }
        let filter = root.join("filter.sh");
        std::fs::write(
            &filter,
            "all=$(cat)\nstatus=${all##*__tm_compress_exit=}\nstatus=${status%__}\n\
             printf '%s\\n' \"${all%__tm_compress_exit=*}\"\nexit \"$status\"\n",
        )
        .expect("write filter");
        let cases = [
            "cd sub && grep -n hi marker.txt",
            "cd 'with space' && grep -n hi marker.txt",
            "cd \"sub\" && ls -la",
            "cd sub && grep -n nomatch marker.txt",
            "cd sub && ls -la no-such-file",
            "cd missing && grep -n hi marker.txt",
        ];
        for shell in ["sh", "bash", "zsh"] {
            if std::process::Command::new(shell)
                .args(["-c", "exit 0"])
                .output()
                .is_err()
            {
                eprintln!("SKIP a_cd_rewrite_runs_like_the_original_in_a_real_shell: no {shell}");
                continue;
            }
            for command in cases {
                let rewritten = rewrite_bash_command_for_compression(command)
                    .unwrap_or_else(|| panic!("{command} must be rewritten"));
                let (head, _tool) = rewritten
                    .split_once("| tm compress")
                    .expect("the rewrite ends in the compress stage");
                let script = format!("{head}| sh '{}'", filter.display());
                // The filter's `$(cat)` drops the blank line before the
                // sentinel; compare non-empty lines, not that byte.
                let lines = |s: &str| {
                    s.lines()
                        .filter(|l| !l.is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                };
                let (code, out, err) = run_in(shell, &root, command);
                let (rcode, rout, rerr) = run_in(shell, &root, &script);
                assert_eq!(
                    code, rcode,
                    "{shell}: exit status of `{command}` vs `{script}`"
                );
                assert_eq!(lines(&out), lines(&rout), "{shell}: stdout of `{command}`");
                assert_eq!(err, rerr, "{shell}: stderr of `{command}`");
                // The `cd` runs in the calling shell, so the cwd it leaves
                // behind matches too.
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
}
