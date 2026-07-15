//! Quote-aware shell lexing helpers for the PM Bash guard (issue #2734).
//!
//! Why: the original guard byte-scanned the RAW command string for composition
//! operators, `>` redirections, and `$(`/backtick substitutions WITHOUT
//! tracking quotes. That over-blocked legitimate PM commands whose *quoted
//! argument content* merely contained those characters — most visibly
//! `git commit -m 'spec -> code'`, whose `>`/`->` inside the single-quoted
//! message tripped the file-write-redirection heuristic (Bob's live repro). It
//! also mis-parsed `git`'s own global flags: a leading `-C <path>` (or
//! `-c <kv>`, `--git-dir=…`) hid the real subcommand from the two-token
//! matcher, so `git -C <path> commit` wasn't recognised as the allowlisted
//! `git commit` and `git -C <path> apply` wasn't recognised as the forbidden
//! `git apply` (a true-positive miss). This module centralises the quote-state
//! tracking and git-argv parsing the scanners need to make policy decisions on
//! command *structure*, not substrings of string literals.
//! What: [`QuoteScan`] records, per byte, whether it lies inside a single- or
//! double-quoted span (with a balanced-quote flag); [`git_subcommand`]
//! shlex-splits a segment and returns the real git subcommand after skipping
//! leading env/`sudo` noise and git global options.
//! Test: `shell_lex::tests`.

/// Quote context of a single byte of a shell command.
///
/// Why: the guard's operator/redirection/substitution scanners must treat a
/// metacharacter inside quotes as literal data, not shell syntax. `Single` and
/// `Double` differ because command substitution (`$(…)`, backticks) is still
/// active inside double quotes but fully suppressed inside single quotes.
/// What: the three POSIX quoting states.
/// Test: `shell_lex::tests` (via [`QuoteScan`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Quote {
    /// Outside any quotes — metacharacters are live shell syntax.
    None,
    /// Inside `'…'` — everything literal, no substitution, no operators.
    Single,
    /// Inside `"…"` — operators literal, but `$(…)`/backticks still active.
    Double,
}

/// Per-byte quote-context map of a shell command, plus a balanced flag.
///
/// Why: lets the sibling scanners ask "is byte `i` inside quotes?" so a `>`,
/// `|`, `&&`, `$(`, or backtick that is merely quoted argument content is not
/// mistaken for shell syntax. When quotes are *unbalanced* the parse is
/// untrustworthy, so callers fall back to their proven quote-unaware scan (the
/// conservative, over-blocking direction) rather than trust a partial map.
/// What: [`QuoteScan::new`] walks the bytes once tracking quote state (POSIX
/// backslash-escaping honoured outside single quotes), recording each byte's
/// context; `balanced` is `true` iff every opened quote was closed.
/// Test: `quote_scan_*`.
pub(super) struct QuoteScan {
    states: Vec<Quote>,
    /// `true` when the command's quotes are all closed.
    pub(super) balanced: bool,
}

impl QuoteScan {
    /// Build the per-byte quote map for `command`.
    ///
    /// Why: single left-to-right pass so every scanner shares one definition of
    /// "what is quoted", matching real shell tokenisation closely enough to
    /// exclude quoted content while erring toward `None` (live syntax) on any
    /// ambiguity.
    /// What: tracks the current [`Quote`] state; outside single quotes a `\`
    /// escapes the next byte (recorded in the same state); `'`/`"` open/close
    /// their span only when not already inside the other kind. Records one
    /// [`Quote`] per byte so `states[i]` is byte `i`'s context.
    /// Test: `quote_scan_marks_single_and_double`, `quote_scan_backslash_escape`,
    /// `quote_scan_reports_unbalanced`.
    pub(super) fn new(command: &str) -> Self {
        let bytes = command.as_bytes();
        let mut states = Vec::with_capacity(bytes.len());
        let mut cur = Quote::None;
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            match cur {
                Quote::Single => {
                    states.push(Quote::Single);
                    if c == b'\'' {
                        cur = Quote::None;
                    }
                    i += 1;
                }
                Quote::Double => {
                    states.push(Quote::Double);
                    if c == b'\\' && i + 1 < bytes.len() {
                        // Backslash escapes the next byte inside double quotes.
                        states.push(Quote::Double);
                        i += 2;
                    } else {
                        if c == b'"' {
                            cur = Quote::None;
                        }
                        i += 1;
                    }
                }
                Quote::None => {
                    if c == b'\\' && i + 1 < bytes.len() {
                        states.push(Quote::None);
                        states.push(Quote::None);
                        i += 2;
                    } else if c == b'\'' {
                        states.push(Quote::Single);
                        cur = Quote::Single;
                        i += 1;
                    } else if c == b'"' {
                        states.push(Quote::Double);
                        cur = Quote::Double;
                        i += 1;
                    } else {
                        states.push(Quote::None);
                        i += 1;
                    }
                }
            }
        }
        QuoteScan {
            states,
            balanced: cur == Quote::None,
        }
    }

    /// Quote context of byte `idx` (defaults to `None` past the end).
    fn at(&self, idx: usize) -> Quote {
        self.states.get(idx).copied().unwrap_or(Quote::None)
    }

    /// Whether byte `idx` is outside all quotes (a live shell metacharacter).
    ///
    /// Why: composition operators (`|`, `&&`, `;`, `&`, newline) and file-write
    /// redirection (`>`) are only real syntax when unquoted.
    /// What: `true` iff `at(idx) == None`.
    /// Test: `quote_scan_marks_single_and_double`.
    pub(super) fn is_unquoted(&self, idx: usize) -> bool {
        self.at(idx) == Quote::None
    }

    /// Whether a command substitution (`$(`, backtick) at byte `idx` is live.
    ///
    /// Why: single quotes suppress substitution entirely, but double quotes do
    /// NOT — `echo "$(sed -i …)"` still runs `sed`, so that case must remain
    /// scannable while `echo '$(sed -i …)'` (literal) is excluded.
    /// What: `true` when the byte is not inside single quotes.
    /// Test: `quote_scan_substitution_live_in_double_quotes`.
    pub(super) fn allows_substitution(&self, idx: usize) -> bool {
        self.at(idx) != Quote::Single
    }
}

/// Git global options that consume a following *separate* argv token.
///
/// Why: to reach the real subcommand the parser must skip both the flag and its
/// argument (`git -C /path commit` → skip `-C` and `/path`, land on `commit`).
/// The `=`-joined forms (`--git-dir=/p`) carry their value in the same token
/// and are handled separately.
/// What: the exhaustive set of value-taking git global options in their
/// space-separated spelling.
const GIT_GLOBAL_OPTS_WITH_ARG: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--super-prefix",
    "--config-env",
];

/// Resolve the real `git` subcommand of a single pipeline segment.
///
/// Why (issue #2734): the guard must recognise an allowlisted git subcommand
/// (`commit`, `status`, …) — and the one forbidden one (`apply`) — through any
/// leading git global flags, which the earlier two-token `effective_tool_name`
/// matcher could not see past. Parsing the argv properly closes both the
/// `git -C <path> commit` false positive and the `git -C <path> apply`
/// false negative.
/// What: shlex-splits `segment` (quote-aware; `None` on unbalanced quotes →
/// caller falls back), skips leading `KEY=value`/`sudo`/`env` prefixes, requires
/// the program basename to be `git`, then walks past global options — those in
/// [`GIT_GLOBAL_OPTS_WITH_ARG`] consume an extra token, `=`-joined long options
/// consume none, other dash-prefixed tokens are valueless global flags — and
/// returns the first non-option token (the subcommand). `None` when the segment
/// is not `git`, is unparseable, or has no subcommand after the options.
/// Test: `git_subcommand_skips_global_flags`, `git_subcommand_plain`,
/// `git_subcommand_none_for_non_git`, `git_subcommand_none_when_unbalanced`.
pub(super) fn git_subcommand(segment: &str) -> Option<String> {
    let argv = shlex::split(segment)?;
    let mut i = 0;
    // Skip env-assignment / sudo / env prefixes (mirrors first_command_token).
    while i < argv.len() {
        let tok = &argv[i];
        if is_env_assignment(tok) {
            i += 1;
            continue;
        }
        if tok == "sudo" || tok == "env" {
            match argv.get(i + 1) {
                Some(next) if !next.starts_with('-') => {
                    i += 1;
                    continue;
                }
                // Flag or nothing after sudo/env — needs arg-aware parsing we
                // don't do; not resolvable as git.
                _ => return None,
            }
        }
        break;
    }
    let program = argv.get(i)?;
    if program.rsplit('/').next().unwrap_or(program) != "git" {
        return None;
    }
    i += 1;
    // Skip git global options to reach the subcommand.
    while i < argv.len() {
        let tok = &argv[i];
        if tok == "--" {
            // POSIX end-of-options; git has no subcommand before `--`, so
            // whatever follows is not a subcommand context we allowlist.
            return None;
        }
        if tok.starts_with("--") {
            if tok.contains('=') {
                i += 1;
                continue;
            }
            if GIT_GLOBAL_OPTS_WITH_ARG.contains(&tok.as_str()) {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if tok.starts_with('-') && tok.len() > 1 {
            if GIT_GLOBAL_OPTS_WITH_ARG.contains(&tok.as_str()) {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        return Some(tok.clone());
    }
    None
}

/// Whether `token` looks like a `KEY=value` shell environment assignment.
///
/// Why: a leading env assignment (`FOO=bar git …`) must be skipped before the
/// `git` program token, mirroring `hook_rewrite::first_command_token`'s rule so
/// both code paths agree on what a prefix is.
/// What: `KEY` non-empty, starting with a letter/underscore, alphanumerics or
/// underscores up to the first `=`.
/// Test: covered via `git_subcommand_skips_global_flags`.
fn is_env_assignment(token: &str) -> bool {
    let Some((key, _)) = token.split_once('=') else {
        return false;
    };
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_scan_marks_single_and_double() {
        let scan = QuoteScan::new("a '>' b \">\" c");
        assert!(scan.balanced);
        // Byte 0 'a' unquoted; the two `>` are inside quotes.
        assert!(scan.is_unquoted(0));
        let s: String = "a '>' b \">\" c".to_string();
        let first_gt = s.find('>').unwrap();
        let last_gt = s.rfind('>').unwrap();
        assert!(!scan.is_unquoted(first_gt), "first > is single-quoted");
        assert!(!scan.is_unquoted(last_gt), "second > is double-quoted");
    }

    #[test]
    fn quote_scan_backslash_escape() {
        // An escaped quote outside quotes must NOT open a span.
        let scan = QuoteScan::new(r#"echo \" ok"#);
        assert!(scan.balanced, "escaped quote must not open a span");
    }

    #[test]
    fn quote_scan_reports_unbalanced() {
        assert!(!QuoteScan::new("echo 'unterminated").balanced);
        assert!(!QuoteScan::new("echo \"unterminated").balanced);
        assert!(QuoteScan::new("echo 'closed'").balanced);
    }

    #[test]
    fn quote_scan_substitution_live_in_double_quotes() {
        // `$(` inside double quotes is still live; inside single quotes it is
        // literal.
        let dq = "echo \"$(x)\"";
        let scan = QuoteScan::new(dq);
        let dollar = dq.find('$').unwrap();
        assert!(scan.allows_substitution(dollar), "double-quoted $( is live");

        let sq = "echo '$(x)'";
        let scan = QuoteScan::new(sq);
        let dollar = sq.find('$').unwrap();
        assert!(
            !scan.allows_substitution(dollar),
            "single-quoted $( is literal"
        );
    }

    #[test]
    fn git_subcommand_plain() {
        assert_eq!(git_subcommand("git commit -m x").as_deref(), Some("commit"));
        assert_eq!(git_subcommand("git status").as_deref(), Some("status"));
        assert_eq!(git_subcommand("git log --oneline").as_deref(), Some("log"));
        assert_eq!(git_subcommand("git apply p.diff").as_deref(), Some("apply"));
    }

    #[test]
    fn git_subcommand_skips_global_flags() {
        assert_eq!(
            git_subcommand("git -C /some/path commit -m x").as_deref(),
            Some("commit")
        );
        assert_eq!(
            git_subcommand("git -C /some/path apply p.diff").as_deref(),
            Some("apply")
        );
        assert_eq!(
            git_subcommand("git -c user.name=x commit").as_deref(),
            Some("commit")
        );
        assert_eq!(
            git_subcommand("git --git-dir=/p/.git status").as_deref(),
            Some("status")
        );
        assert_eq!(
            git_subcommand("git --work-tree /p -C /p diff").as_deref(),
            Some("diff")
        );
        assert_eq!(
            git_subcommand("FOO=bar git -C /p push").as_deref(),
            Some("push")
        );
    }

    #[test]
    fn git_subcommand_none_for_non_git() {
        assert_eq!(git_subcommand("ls -la"), None);
        assert_eq!(git_subcommand("cargo test"), None);
        assert_eq!(git_subcommand("gitfoo status"), None);
    }

    #[test]
    fn git_subcommand_none_when_unbalanced() {
        // Unbalanced quotes → shlex returns None → caller falls back.
        assert_eq!(git_subcommand("git commit -m 'unterminated"), None);
    }

    #[test]
    fn git_subcommand_resolves_path_program() {
        assert_eq!(
            git_subcommand("/usr/bin/git -C /p commit").as_deref(),
            Some("commit")
        );
    }
}
