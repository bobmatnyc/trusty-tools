//! The ONE tokenizer and token classifier every Bash guard in this tree asks
//! (#7839, #7833, #7744, #7743, #7738, #7190).
//!
//! Why: each guard answered the same three questions about a command's bytes
//! its own way — where do words end, which word names a FILE a redirect
//! writes, and which word is an interpreter's PROGRAM rather than a path — and
//! four reported refusals came from those separate answers disagreeing with
//! the shell. A `sed` expression's `.*` read as a dotfile glob (#7839); a
//! `python3 -c` regex literal `.*?` read the same way (#7738); an `awk`
//! `/regex/{action}` rule's body likewise (#7744); and the file descriptor of
//! `2>&1` read as a file named `&1` (#7743) — which the sibling BYTE scanner
//! [`super::scan_file_write_redirect`] had already answered correctly for
//! years, while the argv-side parser in [`super::secret_file_copy`] had not.
//! One classifier is what stops a sixth shape getting a sixth answer.
//!
//! What: [`tokenize`] is the single lexer call, and it reports a parse failure
//! as an ERROR rather than as an empty argv — a guard that cannot tokenize
//! must refuse, never allow (see [`TokenizeError`]). [`redirect_role`] is the
//! single answer to "does this token name a file?". [`program_text_indices`]
//! is the single answer to "which tokens are source text handed to an
//! interpreter?", and it now covers `sed`'s expression alongside the
//! interpreter and `awk` shapes #7397 already knew.
//! [`has_regex_quantifier`] and [`without_glob_metacharacters`] are the
//! single answer to "did this word match only through its wildcards, as a
//! regex fragment would?".
//!
//! Test: `bash_tokens_*` in this module's `tests` submodule, and the per-issue
//! rows in the sibling `guard_tokenizer_tests` module.

/// Why a command's bytes could not be cut into words.
///
/// Why: a guard that treats "the lexer failed" as "there were no tokens"
/// allows exactly the command it could not read. Making the failure a typed
/// error rather than a `None` forces every caller to say what it does with it,
/// and [`TokenizeError::reason`] gives the refusal a message that names the
/// parse problem instead of naming a program the guard never resolved.
/// What: one variant today — `shlex` reports only this failure — kept as an
/// enum so a second parse failure gets its own message rather than this one.
/// Test: `bash_tokens_reports_unbalanced_quoting`,
/// `guard_tokenizer_parse_failure_refuses_naming_the_parse_problem`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TokenizeError {
    /// A `'` or `"` is opened and never closed.
    UnbalancedQuoting,
}

impl TokenizeError {
    /// How a refusal names this parse problem.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::UnbalancedQuoting => {
                "a command this guard cannot parse (unbalanced `'`/`\"` quoting)"
            }
        }
    }
}

/// Cut `segment` into the words a shell would pass as argv.
///
/// Why: the one lexer call site, so a caller cannot silently substitute a
/// laxer split and a parse failure cannot be mistaken for an empty command.
/// What: `shlex::split`, with its `None` promoted to
/// [`TokenizeError::UnbalancedQuoting`].
/// Test: `bash_tokens_reports_unbalanced_quoting`.
pub(crate) fn tokenize(segment: &str) -> Result<Vec<String>, TokenizeError> {
    shlex::split(segment).ok_or(TokenizeError::UnbalancedQuoting)
}

/// What a redirection token does with the word beside it.
///
/// Test: `bash_tokens_reads_every_redirect_spelling`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RedirectRole<'a> {
    /// Not an output redirection at all.
    None,
    /// `2>&1`, `>&2`, `2>&-` — duplicates or closes a descriptor, names no file.
    FileDescriptor,
    /// `>`, `>>`, `2>`, `&>` alone: the file is the NEXT token.
    TargetFollows,
    /// `>out.txt`, `>>out.txt`, `&>out.txt`, `>|out.txt`: the file is attached.
    Target(&'a str),
}

/// Classify one argv token as an output redirection (#7743).
///
/// Why: `ls .env.local 2>&1` was refused as laundering the dotenv file into a
/// file named `&1`. `2>&1` points stderr at stdout's descriptor; it opens no
/// file, so there is nothing to launder. The byte scanner
/// [`super::scan_file_write_redirect`] has skipped `>&` since #5356 — this is
/// that same rule, stated once, for the callers that work on argv.
/// What: the token must split at `>` with a descriptor prefix that is empty,
/// `&`, or all digits. `>>` and `>|` are stripped, then a `&` prefix on the
/// remainder decides: `&<digits>` and `&-` are descriptor operations, and any
/// other `&word` is bash's `&>word` spelling, which really does name a file.
/// Test: `bash_tokens_reads_every_redirect_spelling`,
/// `guard_7743_ls_with_a_stderr_redirect`.
pub(crate) fn redirect_role(token: &str) -> RedirectRole<'_> {
    let Some((descriptor, rest)) = token.split_once('>') else {
        return RedirectRole::None;
    };
    if !(descriptor.is_empty()
        || descriptor == "&"
        || descriptor.chars().all(|c| c.is_ascii_digit()))
    {
        return RedirectRole::None;
    }
    let rest = rest.strip_prefix('>').unwrap_or(rest);
    let rest = rest.strip_prefix('|').unwrap_or(rest);
    if let Some(after) = rest.strip_prefix('&') {
        // #7743: `[n]>&<digits>` duplicates a descriptor and `[n]>&-` closes
        // one. Neither opens a file.
        if after == "-" || (!after.is_empty() && after.chars().all(|c| c.is_ascii_digit())) {
            return RedirectRole::FileDescriptor;
        }
        return if after.is_empty() {
            RedirectRole::TargetFollows
        } else {
            RedirectRole::Target(after)
        };
    }
    if rest.is_empty() {
        RedirectRole::TargetFollows
    } else {
        RedirectRole::Target(rest)
    }
}

/// Programs that take an inline PROGRAM behind [`INLINE_PROGRAM_FLAGS`].
///
/// What: `sh`/`bash` are listed for completeness — `super::split_shell_segments`
/// already re-scans a `sh -c` string as its own segment, so the inner command
/// is still classified as argv there.
const INLINE_PROGRAM_INTERPRETERS: &[&str] = &[
    "python", "python3", "perl", "ruby", "node", "deno", "php", "sh", "bash", "zsh", "dash",
];

/// The flags whose next token is an inline program rather than a path.
///
/// What: `-c` (`python`, `sh`), `-e` (`perl`, `node`, `ruby`), `-r` (`php`),
/// and the long spellings. Only the SEPARATE form is recognised; a joined
/// `perl -e'…'` lexes as one token and keeps the argv scan, which over-refuses
/// rather than under-refuses.
const INLINE_PROGRAM_FLAGS: &[&str] = &["-c", "-e", "-r", "--eval", "--command"];

/// The awk-family programs whose first positional argument IS the program.
const AWK_PROGRAMS: &[&str] = &["awk", "gawk", "nawk", "mawk"];

/// awk flags taking a SEPARATE value, so the token after them is not the
/// program (`awk -F ';' '{…}'`, `awk -v n=1 '{…}'`).
const AWK_VALUE_FLAGS: &[&str] = &["-v", "-F", "--assign", "--field-separator"];

/// The sed-family programs whose expression is a PROGRAM, not a path (#7839).
///
/// Why: `sed`'s script is source text in exactly the sense `awk`'s is — it is
/// `s///` commands and regexes, never a filename — but the classifier only
/// knew the awk shape, so `sed -i '' 's/^.*$/x/' patch.sh` was refused for
/// "naming `.*`". `ssed` and `gsed` are the same program under the spellings
/// Homebrew and older GNU installs use.
const SED_PROGRAMS: &[&str] = &["sed", "gsed", "ssed"];

/// sed flags whose value is an EXPRESSION, i.e. program text (#7839).
const SED_EXPRESSION_FLAGS: &[&str] = &["-e", "--expression"];

/// Flags that load the program from a FILE, so every positional is a path.
///
/// Why: `awk -f prog.awk data.txt` and `sed -f script.sed .env` have no inline
/// program at all — exempting a positional there would skip the file operand,
/// which is a bypass rather than a false-positive fix.
const PROGRAM_FILE_FLAGS: &[&str] = &["-f", "--file"];

/// Which tokens of `argv` are an inline PROGRAM rather than a path.
///
/// Why: source text handed to an interpreter is where a word scan reads syntax
/// as a filename. Identified by POSITION, so this adds no verb to any rule's
/// allowlist — a token named here is still screened, just as program text
/// rather than as an argv operand.
/// What: `program` is the caller's already-resolved program basename and
/// `start` its index in `argv`. For an [`INLINE_PROGRAM_INTERPRETERS`] entry,
/// every token following an [`INLINE_PROGRAM_FLAGS`] spelling. For an
/// [`AWK_PROGRAMS`] or [`SED_PROGRAMS`] entry, every
/// [`SED_EXPRESSION_FLAGS`] value, or — when none is present — the first
/// operand that is neither a flag, nor a value-taking flag's value, nor the
/// empty string BSD `sed -i ''` leaves behind. A [`PROGRAM_FILE_FLAGS`]
/// spelling withdraws the whole answer, so every positional stays a path.
/// Test: `bash_tokens_finds_the_sed_expression`,
/// `bash_tokens_withdraws_on_a_program_file_flag`,
/// `guard_7839_sed_expression_wildcard`, `guard_7744_awk_pattern_match_rule`,
/// `guard_7738_python_inline_regex_literal`.
pub(crate) fn program_text_indices(program: &str, argv: &[String], start: usize) -> Vec<usize> {
    let rest_start = start + 1;
    let Some(rest) = argv.get(rest_start..) else {
        return Vec::new();
    };
    if INLINE_PROGRAM_INTERPRETERS.contains(&program) {
        return rest
            .iter()
            .enumerate()
            .filter(|(_, token)| INLINE_PROGRAM_FLAGS.contains(&token.as_str()))
            .map(|(offset, _)| rest_start + offset + 1)
            .filter(|index| *index < argv.len())
            .collect();
    }
    let is_awk = AWK_PROGRAMS.contains(&program);
    if !is_awk && !SED_PROGRAMS.contains(&program) {
        return Vec::new();
    }
    if rest.iter().any(|t| loads_a_program_file(t)) {
        return Vec::new();
    }
    let expressions: Vec<usize> = rest
        .iter()
        .enumerate()
        .filter(|(_, token)| SED_EXPRESSION_FLAGS.contains(&token.as_str()))
        .map(|(offset, _)| rest_start + offset + 1)
        .filter(|index| *index < argv.len())
        .collect();
    if !expressions.is_empty() {
        return expressions;
    }
    first_operand(rest, is_awk)
        .map(|offset| vec![rest_start + offset])
        .unwrap_or_default()
}

/// Whether `token` is a [`PROGRAM_FILE_FLAGS`] spelling, joined or separate.
fn loads_a_program_file(token: &str) -> bool {
    PROGRAM_FILE_FLAGS.contains(&token) || token.starts_with("--file=") || token.starts_with("-f")
}

/// The offset in `rest` of the first token that is the awk/sed PROGRAM.
///
/// What: skips a value-taking flag with its value, every other flag, and the
/// empty token BSD `sed -i ''` leaves in argv — without that last skip the
/// empty string would be read as the expression and the real one would fall
/// back to the argv scan, which is the #7839 refusal again one token over.
fn first_operand(rest: &[String], is_awk: bool) -> Option<usize> {
    let mut i = 0;
    while let Some(token) = rest.get(i) {
        if is_awk && AWK_VALUE_FLAGS.contains(&token.as_str()) {
            i += 2;
        } else if token.is_empty() || (token.len() > 1 && token.starts_with('-')) {
            i += 1;
        } else {
            return Some(i);
        }
    }
    None
}

/// Whether `word` carries a regex QUANTIFIER — a `.` immediately followed by
/// `*` or `?` (#7839, #7738, #7744).
///
/// Why: `.*`, `.*?`, the `.*?pen` a bracket class collapses `/.*[Oo]pen/`
/// into, and the `r.*?,` a Python raw-string prefix glues together all match
/// the guard's secret families through its glob-OVERLAP arm — the arm that
/// asks "could this SHELL GLOB expand onto a credential file?". Inside an
/// interpreter's program text there is no shell glob: `.` plus a quantifier is
/// the regex wildcard, and a real filename literal (`open('.env')`) carries no
/// quantifier at all.
/// What: the two-byte test only. It is deliberately NOT the whole answer — the
/// caller pairs it with [`without_glob_metacharacters`], so a word whose
/// literal core still names a secret (`.env.*`, `*credentials*`) keeps its
/// deny and only a word that matched through the wildcards alone is released.
/// Test: `bash_tokens_reads_a_quantifier_as_regex`,
/// `guard_7839_sed_expression_wildcard`, `guard_7738_python_inline_regex_literal`,
/// `guard_7744_awk_pattern_match_rule`.
pub(crate) fn has_regex_quantifier(word: &str) -> bool {
    word.as_bytes()
        .windows(2)
        .any(|pair| pair[0] == b'.' && matches!(pair[1], b'*' | b'?'))
}

/// `word` with every glob metacharacter removed, leaving its literal core.
///
/// Why: the second half of the program-text question above. A word releases
/// its deny only when the wildcards are what earned it — strip them and ask
/// the ordinary name matcher again. `.env.*` leaves `.env.`, which still names
/// a dotenv file; `.*` leaves `.`, which names nothing.
/// What: drops `*`, `?`, `[` and `]`; every other byte survives, so the
/// caller's matcher sees the same literal the shell would.
/// Test: `bash_tokens_reads_a_quantifier_as_regex`.
pub(crate) fn without_glob_metacharacters(word: &str) -> String {
    word.chars()
        .filter(|c| !matches!(c, '*' | '?' | '[' | ']'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(command: &str) -> Vec<String> {
        tokenize(command).expect("lexes")
    }

    /// A lexer failure is an ERROR that names the parse problem, never an
    /// empty argv a caller could read as "nothing to check".
    #[test]
    fn bash_tokens_reports_unbalanced_quoting() {
        assert_eq!(
            tokenize("awk '{print} terraform.tfvars"),
            Err(TokenizeError::UnbalancedQuoting)
        );
        assert!(
            TokenizeError::UnbalancedQuoting
                .reason()
                .contains("unbalanced")
        );
        assert_eq!(tokenize("ls -la").as_deref().map(<[String]>::len), Ok(2));
    }

    /// Every redirect spelling, including the `2>&1` the argv parser read as a
    /// file named `&1` (#7743).
    #[test]
    fn bash_tokens_reads_every_redirect_spelling() {
        assert_eq!(redirect_role("2>&1"), RedirectRole::FileDescriptor);
        assert_eq!(redirect_role(">&2"), RedirectRole::FileDescriptor);
        assert_eq!(redirect_role("2>&-"), RedirectRole::FileDescriptor);
        assert_eq!(redirect_role(">"), RedirectRole::TargetFollows);
        assert_eq!(redirect_role(">>"), RedirectRole::TargetFollows);
        assert_eq!(redirect_role("2>"), RedirectRole::TargetFollows);
        assert_eq!(redirect_role(">out.txt"), RedirectRole::Target("out.txt"));
        assert_eq!(redirect_role(">>out.txt"), RedirectRole::Target("out.txt"));
        assert_eq!(redirect_role(">|out.txt"), RedirectRole::Target("out.txt"));
        assert_eq!(redirect_role("&>out.txt"), RedirectRole::Target("out.txt"));
        assert_eq!(redirect_role(">&out.txt"), RedirectRole::Target("out.txt"));
        assert_eq!(redirect_role("-rf"), RedirectRole::None);
        assert_eq!(redirect_role("->"), RedirectRole::None);
    }

    /// `sed`'s expression is program text in both the flagged and the
    /// positional spelling, including past BSD `sed -i ''` (#7839).
    #[test]
    fn bash_tokens_finds_the_sed_expression() {
        let argv = words("sed -i '' 's/^.*$/x/' patch.sh");
        assert_eq!(program_text_indices("sed", &argv, 0), vec![3]);
        let flagged = words("sed -e 's|.*foo|bar|' -e 'd' patch.sh");
        assert_eq!(program_text_indices("sed", &flagged, 0), vec![2, 4]);
        let awk = words("awk '/.*[Oo]pen/ {print}' spec.md");
        assert_eq!(program_text_indices("awk", &awk, 0), vec![1]);
        let inline = words("python3 -c 'import re'");
        assert_eq!(program_text_indices("python3", &inline, 0), vec![2]);
    }

    /// A `-f`/`--file` program source withdraws the answer entirely, so every
    /// positional stays a path the operand rules screen.
    #[test]
    fn bash_tokens_withdraws_on_a_program_file_flag() {
        let awk = words("awk -f prog.awk .env");
        assert!(program_text_indices("awk", &awk, 0).is_empty());
        let sed = words("sed -f script.sed .env");
        assert!(program_text_indices("sed", &sed, 0).is_empty());
        let plain = words("cat .env");
        assert!(program_text_indices("cat", &plain, 0).is_empty());
    }

    /// A `.` plus a quantifier is a regex quantifier, and stripping the
    /// metacharacters leaves the literal core the caller re-screens.
    #[test]
    fn bash_tokens_reads_a_quantifier_as_regex() {
        for regex in [".*", ".*?", ".?", ".*?pen", "r.*?,", "PASSAGE.*"] {
            assert!(has_regex_quantifier(regex), "{regex} carries a quantifier");
        }
        for name in [".env", ".netrc", "id_rsa", "secrets", ".", ""] {
            assert!(!has_regex_quantifier(name), "{name} carries none");
        }
        // The pairing: a dotenv glob carries a quantifier too, and keeps its
        // deny because its literal core still names the family.
        assert!(has_regex_quantifier(".env.*"));
        assert_eq!(without_glob_metacharacters(".env.*"), ".env.");
        assert_eq!(without_glob_metacharacters(".*?pen"), ".pen");
        assert_eq!(without_glob_metacharacters("/.*[Oo]pen/"), "/.Oopen/");
    }
}
