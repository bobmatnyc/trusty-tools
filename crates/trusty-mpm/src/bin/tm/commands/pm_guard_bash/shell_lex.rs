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

use crate::commands::hook_rewrite::{COMMAND_WRAPPERS, is_env_assignment, strip_wrapper_prefix};

/// Shell programs that run their `-c` argument as a command string.
///
/// Why (#6660): the guard resolved a segment's program basename and required it
/// to be `git`. `sh -c "git worktree remove …"` resolves to `sh`, so every rule
/// built on [`git_subcommand`] — the #5791 worktree-remove deny, the ADR-0048
/// main-checkout HEAD-move rule, the destructive-delete rule — saw a program it
/// had no opinion about and allowed the command underneath. Naming the shells
/// in one list is what stops the fix from covering a single spelling.
/// What: the basenames whose `-c` argument is an inner command to re-classify.
/// Test: `wrappers_do_not_hide_the_inner_command_from_the_git_verb_rules`.
const DASH_C_SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "ash"];

/// `xargs` options that consume the FOLLOWING token as their value.
///
/// Why: skipping only the flag would leave its value (`4` in `xargs -n 4 git …`)
/// read as the program. The separated spelling is the one that needs a table;
/// an attached (`-n4`) or `=`-joined (`--max-args=4`) value is one token and
/// skips itself.
/// What: the separated-value option spellings, short and long.
/// Test: `wrappers_do_not_hide_the_inner_command_from_the_git_verb_rules`.
const XARGS_OPTS_WITH_ARG: &[&str] = &[
    "-a",
    "-d",
    "-E",
    "-e",
    "-I",
    "-i",
    "-L",
    "-l",
    "-n",
    "-P",
    "-s",
    "--arg-file",
    "--delimiter",
    "--eof",
    "--replace",
    "--max-lines",
    "--max-args",
    "--max-procs",
    "--max-chars",
    "--process-slot-var",
];

/// Whether `segment` contains live `$'…'` or `$"…"` quoting (#6660 review).
///
/// Why: `shlex` 1.3.0 does not implement bash's ANSI-C (`$'…'`) or
/// locale-translation (`$"…"`) quoting. It drops the quotes and keeps the `$`,
/// so `sh -c $'git worktree remove x'` tokenizes as
/// `["sh", "-c", "$git worktree remove x"]` and the program token becomes
/// `$git`, which matches no rule. That is a one-character bypass of every
/// git-verb rule, wrapper or not — `git $'worktree' remove x` mangles the same
/// way with no wrapper at all. Since the guard cannot decode the escapes, the
/// only correct answer is to refuse to classify the segment.
/// What: true when a `$` immediately followed by `'` or `"` appears OUTSIDE any
/// quoted span. An unbalanced-quote segment has an untrustworthy
/// [`QuoteScan`] map, so every such `$` reads as live — the conservative
/// direction. A `$'` that is itself quoted (`echo "cost: $'5'"`) is literal
/// text and is not flagged; when it sits inside a wrapper's `-c` string the
/// caller re-scans that string on its own, where it IS live.
/// Test: `unclassifiable_command_flags_ansi_c_quoting`,
/// `unclassifiable_command_allows_ordinary_commands`, and end to end in
/// `pm_guard_denies_a_wrapped_command_it_cannot_classify_via_subagent_payload`.
pub(super) fn has_live_ansi_c_quoting(segment: &str) -> bool {
    let scan = QuoteScan::new(segment);
    let bytes = segment.as_bytes();
    bytes.iter().enumerate().any(|(i, b)| {
        *b == b'$'
            && matches!(bytes.get(i + 1), Some(b'\'') | Some(b'"'))
            && (!scan.balanced || scan.is_unquoted(i))
    })
}

/// What a segment's leading wrapper hides, if anything (#6660).
///
/// Why: three answers, not two. A segment that is not a wrapper classifies
/// as-is; a wrapper hands back the inner command text to classify instead; and
/// a wrapper whose inner text cannot be lexed is a command the guard cannot
/// classify AT ALL, which must deny rather than fall through — otherwise broken
/// quoting is the bypass.
/// What: see the variants.
/// Test: `wrappers_do_not_hide_the_inner_command_from_the_git_verb_rules`,
/// `an_unparseable_wrapped_command_is_denied`,
/// `wrappers_around_benign_commands_still_allow`.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum WrappedCommand {
    /// No wrapper — classify the segment as given.
    None,
    /// The command text the wrapper would actually run.
    Inner(String),
    /// A wrapper carrying an inner command the guard cannot lex. Fail closed.
    Unlexable,
}

/// Resolve the command a leading `sh -c` / `bash -c` / `env -S` / `xargs`
/// wrapper would actually run (#6660).
///
/// Why: [`strip_wrapper_prefix`] advances past a wrapper TOKEN but never
/// descends into an argument that is itself a command — a `-c` string or an
/// `xargs` argv. Every rule downstream therefore classified the wrapper instead
/// of the command. Doing the descent here, on the shared segment text, is what
/// makes one fix reach all of them.
/// What: shlex-splits `segment`, skips leading `KEY=value` assignments and
/// [`COMMAND_WRAPPERS`] tokens, then: a [`DASH_C_SHELLS`] program yields the
/// token after its `-c` (a short cluster ending in `c`, such as `-lc`, counts);
/// `env` yields the value of `-S`/`--split-string` in any of its three
/// spellings; `xargs` yields its argv past [`XARGS_OPTS_WITH_ARG`], re-joined.
/// The result is [`WrappedCommand::Unlexable`] when the inner text will not
/// shlex-split, and [`WrappedCommand::None`] when the segment carries no such
/// wrapper — an unlexable OUTER segment included, since that case already falls
/// back to the quote-unaware scan and is not this function's to change.
/// Test: as [`WrappedCommand`].
pub(super) fn wrapped_command(segment: &str) -> WrappedCommand {
    let Some(argv) = shlex::split(segment) else {
        return WrappedCommand::None;
    };
    let mut i = 0;
    while i < argv.len() {
        let raw = argv[i].as_str();
        let tok = raw.strip_prefix('\\').unwrap_or(raw);
        if is_env_assignment(tok) {
            i += 1;
            continue;
        }
        let base = tok.rsplit('/').next().unwrap_or(tok);
        let rest = &argv[i + 1..];
        if DASH_C_SHELLS.contains(&base) {
            return inner_or_none(dash_c_argument(rest));
        }
        if base == "env" {
            match env_split_string(rest) {
                Some(inner) => return inner_or_none(Some(inner)),
                // Plain `env` / `env FOO=1 cmd` — keep walking to the real
                // program, exactly as `strip_wrapper_prefix` would.
                None => {
                    i += 1;
                    continue;
                }
            }
        }
        if base == "xargs" {
            return inner_or_none(xargs_argument(rest));
        }
        if COMMAND_WRAPPERS.contains(&tok) {
            i += 1;
            continue;
        }
        return WrappedCommand::None;
    }
    WrappedCommand::None
}

/// Classify an extracted inner command string.
///
/// Why: the fail-closed decision lives in ONE place so no wrapper shape can
/// forget it.
/// What: lexable → [`WrappedCommand::Inner`], not → [`WrappedCommand::Unlexable`],
/// absent or whitespace-only (it runs nothing) → [`WrappedCommand::None`].
/// Test: `an_unparseable_wrapped_command_is_denied`.
fn inner_or_none(inner: Option<String>) -> WrappedCommand {
    let Some(inner) = inner else {
        return WrappedCommand::None;
    };
    if inner.trim().is_empty() {
        return WrappedCommand::None;
    }
    match shlex::split(&inner) {
        Some(_) => WrappedCommand::Inner(inner),
        None => WrappedCommand::Unlexable,
    }
}

/// The command string a shell's `-c` option carries, if present.
///
/// Why: shells accept their options in any order and in clusters, so scanning
/// for the exact token `-c` alone would miss `bash -lc "…"`, a spelling a
/// wrapper script reaches for routinely.
/// What: the token after the first `-c`, or after the first short cluster
/// (`-`-led, not `--`-led) ending in `c`. `None` when the shell runs a script
/// file instead, or when `-c` ends the argv.
/// Test: `wrappers_do_not_hide_the_inner_command_from_the_git_verb_rules`.
fn dash_c_argument(rest: &[String]) -> Option<String> {
    for (n, tok) in rest.iter().enumerate() {
        let cluster =
            tok.starts_with('-') && !tok.starts_with("--") && tok.len() > 1 && tok.ends_with('c');
        if tok == "-c" || cluster {
            return rest.get(n + 1).cloned();
        }
    }
    None
}

/// The command string `env -S` / `--split-string` carries, if present.
///
/// Why: `env -S "git worktree remove …"` runs the string as a command, and
/// [`strip_wrapper_prefix`] refuses a wrapper followed by a flag — so this shape
/// resolved to nothing at all and every rule allowed it.
/// What: handles `-S <str>`, `-S<str>` and `--split-string=<str>`. `None` for a
/// plain `env`, which stays an ordinary wrapper for the caller to walk past.
/// Test: `wrappers_do_not_hide_the_inner_command_from_the_git_verb_rules`.
fn env_split_string(rest: &[String]) -> Option<String> {
    for (n, tok) in rest.iter().enumerate() {
        if tok == "-S" || tok == "--split-string" {
            return rest.get(n + 1).cloned();
        }
        if let Some(value) = tok.strip_prefix("--split-string=") {
            return Some(value.to_string());
        }
        if let Some(value) = tok.strip_prefix("-S")
            && !value.is_empty()
        {
            return Some(value.to_string());
        }
    }
    None
}

/// The command `xargs` would run, re-joined into a command string.
///
/// Why: `xargs git worktree remove <path>` runs `git`, and reading only the
/// first token gave `xargs`. The stdin-supplied arguments are unknowable here,
/// but the verb and every literal argument on the command line are not — and
/// those are what the git-verb rules classify.
/// What: skips xargs' own options (those in [`XARGS_OPTS_WITH_ARG`] consume the
/// next token; attached and `=`-joined values consume none) and re-quotes the
/// remaining argv with `shlex::try_join`. `None` when nothing follows the
/// options, or when a token cannot be re-quoted (it contains a NUL).
/// Test: `wrappers_do_not_hide_the_inner_command_from_the_git_verb_rules`.
fn xargs_argument(rest: &[String]) -> Option<String> {
    let mut i = 0;
    while i < rest.len() {
        let tok = rest[i].as_str();
        if !tok.starts_with('-') || tok == "-" {
            break;
        }
        if XARGS_OPTS_WITH_ARG.contains(&tok) {
            i += 2;
            continue;
        }
        i += 1;
    }
    if i >= rest.len() {
        return None;
    }
    shlex::try_join(rest[i..].iter().map(String::as_str)).ok()
}

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
/// false negative. Issue #4031 review (pass 2): this function had its OWN
/// `sudo`/`env`-only prefix skip, a second enumeration site that inherited the
/// same weakness `hook_rewrite::first_command_token` did — `command git apply
/// -` and `command git worktree remove --force <path>` reached the git guards
/// unresolved. It now shares [`crate::commands::hook_rewrite::strip_wrapper_prefix`]
/// with that function instead of re-enumerating.
/// What: shlex-splits `segment` (quote-aware; `None` on unbalanced quotes →
/// caller falls back), delegates the leading `KEY=value`/wrapper skip to
/// [`strip_wrapper_prefix`], requires the program basename to be `git`, then
/// walks past global options — those in [`GIT_GLOBAL_OPTS_WITH_ARG`] consume
/// an extra token, `=`-joined long options consume none, other dash-prefixed
/// tokens are valueless global flags — and returns the first non-option token
/// (the subcommand). `None` when the segment is not `git`, is unparseable, or
/// has no subcommand after the options.
/// Test: `git_subcommand_skips_global_flags`, `git_subcommand_plain`,
/// `git_subcommand_none_for_non_git`, `git_subcommand_none_when_unbalanced`,
/// `git_subcommand_resolves_through_command_and_nice_wrappers`.
pub(super) fn git_subcommand(segment: &str) -> Option<String> {
    let argv = shlex::split(segment)?;
    let mut i = strip_wrapper_prefix(&argv)?;
    let program = argv.get(i)?;
    let program = program.strip_prefix('\\').unwrap_or(program);
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

    #[test]
    fn git_subcommand_resolves_through_command_and_nice_wrappers() {
        // #4031 review, item 4: `git_subcommand` had its own sudo/env-only
        // prefix skip, a second enumeration site independent of
        // `hook_rewrite::first_command_token` — `command git apply -` and a
        // `nice`-wrapped git call both reached the git guards unresolved
        // before sharing `strip_wrapper_prefix`.
        for command in ["command git apply -", "nice git reset --hard"] {
            assert!(
                git_subcommand(command).is_some(),
                "expected a resolved subcommand for: {command}"
            );
        }
        assert_eq!(
            git_subcommand("command git apply -").as_deref(),
            Some("apply")
        );
        assert_eq!(
            git_subcommand("nice git reset --hard").as_deref(),
            Some("reset")
        );
    }
}
