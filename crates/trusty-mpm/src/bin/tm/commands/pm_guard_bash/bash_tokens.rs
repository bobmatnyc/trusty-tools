//! The ONE tokenizer and token classifier every Bash guard in this tree asks
//! (#7839, #7833, #7744, #7743, #7738).
//!
//! Why: each guard answered the same three questions about a command's bytes
//! its own way — where do words end, which word names a FILE a redirect
//! writes, and which word is an interpreter's PROGRAM rather than a path — and
//! four reported refusals came from those separate answers disagreeing with
//! the shell. A `sed` expression's `.*` read as a dotfile glob (#7839); a
//! `python3 -c` regex literal `.*?` read the same way (#7738); an `awk`
//! `/regex/{action}` rule's body likewise (#7744); and the file descriptor of
//! `2>&1` read as a file named `&1` (#7743) — which the sibling BYTE scanner
//! [`super::scan_file_write_redirects`], which returns every redirect target
//! in a command, had already answered correctly for years, while the argv-side parser in [`super::secret_file_copy`] had not.
//! One classifier is what stops a sixth shape getting a sixth answer.
//!
//! What: [`tokenize`] is the lexer call for the guards #7839 migrated, and it
//! reports a parse failure as an ERROR rather than as an empty argv — a guard
//! that cannot tokenize must refuse, never allow (see [`TokenizeError`]).
//! [`tokenize`]'s own doc lists the sites still to migrate.
//! [`redirect_role`] is the
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
/// Why: the lexer call site for the guards #7839 migrated — the secret-read
/// rule and [`super::secret_file_copy`] — so neither can silently substitute a
/// laxer split and neither can mistake a parse failure for an empty command.
/// What: `shlex::split`, with its `None` promoted to
/// [`TokenizeError::UnbalancedQuoting`].
///
/// Eleven non-test `shlex::split` sites have NOT migrated and still read a
/// `None` as "no tokens": `destructive_delete.rs` (2), `main_checkout.rs` (2),
/// this module's `mod.rs` (2), `persistence.rs` (1), `shell_lex.rs` (3), and
/// `commands/hook_rewrite.rs` (1). Each owns its own fail-closed fallback
/// today; moving them here is remaining work, not a claim this function
/// already made.
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
/// [`super::scan_file_write_redirects`] leaves a `>&` descriptor duplication
/// out of the redirect targets it returns (#5356, #8730) — this is that same
/// rule, stated once, for the callers that work on argv.
/// What: the token must split at `>` with a descriptor prefix that is empty,
/// `&`, or all digits. A second `>` and one clobber mark ([`strip_clobber`])
/// are stripped, then a `&` prefix on the remainder decides: a
/// [`is_descriptor_word`] after it is a descriptor operation, and any other
/// `&word` is bash's `>&word` spelling, which really does name a file. A
/// clobber mark after the `&` (zsh `>&|`/`>&!`) always names a file.
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
    let rest = strip_clobber(rest.strip_prefix('>').unwrap_or(rest));
    let target = match rest.strip_prefix('&') {
        // #8730: zsh's `>&|word`/`>&!word` opens `word` even when it is `2`.
        Some(after) if strip_clobber(after) != after => strip_clobber(after),
        // #7743: `[n]>&<digits>` duplicates a descriptor and `[n]>&-` closes
        // one. Neither opens a file.
        Some(after) if is_descriptor_word(after) => return RedirectRole::FileDescriptor,
        Some(after) => after,
        None => rest,
    };
    if target.is_empty() {
        RedirectRole::TargetFollows
    } else {
        RedirectRole::Target(target)
    }
}

/// `rest` without the one clobber mark that may open it (#8730).
///
/// Why: bash's `>|` and zsh's `>!` (and `>>|`, `>>!`, `>&|`, `>&!`) override
/// `noclobber`; the mark belongs to the operator. Left on the word, it made
/// `>!/dev/tty` a file named `!/dev/tty`, which the credential-print rule
/// read as a discarded write.
fn strip_clobber(rest: &str) -> &str {
    rest.strip_prefix(['|', '!']).unwrap_or(rest)
}

/// Whether the byte at `i` is the `|` of a clobber redirect (`>|`, `>>|`,
/// `&>|`, zsh `>&|`) rather than a pipe (#8730).
///
/// Why: every cutter that splits a command at `|` must agree on this, or one
/// of them reads `>|/dev/tty` as `>` piped into a command named `/dev/tty`.
/// Test: `bash_tokens_reads_every_redirect_spelling`,
/// `allows_a_clobber_redirect_to_a_file_8730`.
pub(crate) fn is_clobber_bar(bytes: &[u8], i: usize) -> bool {
    let before = |back: usize| i.checked_sub(back).and_then(|at| bytes.get(at));
    bytes.get(i) == Some(&b'|')
        && (before(1) == Some(&b'>') || (before(1) == Some(&b'&') && before(2) == Some(&b'>')))
}

/// Whether the word after `>&` (or `<&`) names a descriptor rather than a file.
///
/// What: `-` (close), or digits with an optional trailing `-` (move). Any
/// other word — `>&out.txt`, `>&$f` — is a file bash opens for both stdout
/// and stderr. Shared with the byte scanner [`super::scan_file_write_redirects`]
/// so the two readers cannot disagree (#8730).
/// Test: `bash_tokens_reads_every_redirect_spelling`,
/// `write_targets_read_a_descriptor_redirect_that_names_a_file`.
pub(crate) fn is_descriptor_word(word: &str) -> bool {
    let digits = word.strip_suffix('-').unwrap_or(word);
    word == "-" || (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
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

/// SQL clients whose statement flag takes SQL, not a path (#9006).
const SQL_CLIENTS: &[&str] = &["mysql", "mariadb"];

/// The separate statement flags of [`SQL_CLIENTS`]; a joined `-e…` or
/// `--execute=…` keeps the argv scan.
const SQL_STATEMENT_FLAGS: &[&str] = &["-e", "--execute"];

/// Client commands that hand text to a shell or an editor (#9006): mysql runs
/// `system`/`\!` through `/bin/sh` even in batch mode, and `pager` and `edit`
/// start a program.
const SQL_SHELL_ESCAPES: &[&str] = &["system", "pager", "edit"];

/// Whether a SQL statement can reach no shell, so its `.*` is SQL syntax.
///
/// Why: the #9001 critic showed `mysql -e '\! cat .*'` globs onto `.env` when
/// the statement is read as SQL. Any backslash (every client command has a
/// `\` short form) or any [`SQL_SHELL_ESCAPES`] word, in any case and even
/// inside an identifier, keeps the argv scan.
/// Test: `a_sql_wildcard_in_a_mysql_statement_is_no_path_9006`,
/// `a_secret_named_in_a_mysql_statement_still_denies_9001`.
fn sql_reaches_no_shell(statement: &str) -> bool {
    let lower = statement.to_ascii_lowercase();
    !lower.contains('\\') && !SQL_SHELL_ESCAPES.iter().any(|w| lower.contains(w))
}

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
/// operand that is neither a flag, nor a value-taking flag's value, nor (for
/// sed alone) the empty string BSD `sed -i ''` leaves behind. A
/// [`PROGRAM_FILE_FLAGS`] spelling withdraws the whole answer, so every
/// positional stays a path.
/// For a [`SQL_CLIENTS`] entry, each [`SQL_STATEMENT_FLAGS`] value that
/// [`sql_reaches_no_shell`] (#9006).
/// Test: `bash_tokens_finds_the_sed_expression`,
/// `bash_tokens_withdraws_on_a_program_file_flag`,
/// `guard_tokenizer_bounds_the_program_text_relaxations`,
/// `guard_7839_sed_expression_wildcard`, `guard_7744_awk_pattern_match_rule`,
/// `guard_7738_python_inline_regex_literal`,
/// `a_sql_wildcard_in_a_mysql_statement_is_no_path_9006`.
pub(crate) fn program_text_indices(program: &str, argv: &[String], start: usize) -> Vec<usize> {
    let rest_start = start + 1;
    let Some(rest) = argv.get(rest_start..) else {
        return Vec::new();
    };
    if SQL_CLIENTS.contains(&program) {
        return rest
            .iter()
            .enumerate()
            .filter(|(_, token)| SQL_STATEMENT_FLAGS.contains(&token.as_str()))
            .map(|(offset, _)| rest_start + offset + 1)
            .filter(|&index| argv.get(index).is_some_and(|s| sql_reaches_no_shell(s)))
            .collect();
    }
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
/// What: skips a value-taking flag with its value, every other flag, and — for
/// SED only — the empty token BSD `sed -i ''` leaves in argv; without that last
/// skip the empty string would be read as the expression and the real one would
/// fall back to the argv scan, which is the #7839 refusal again one token over.
/// The skip is scoped to sed because awk has no such spelling: `awk '' f` is an
/// awk program that happens to be empty, and walking past it would exempt the
/// operand `f` — a widening, where the sed skip is a narrowing.
fn first_operand(rest: &[String], is_awk: bool) -> Option<usize> {
    let mut i = 0;
    while let Some(token) = rest.get(i) {
        if is_awk && AWK_VALUE_FLAGS.contains(&token.as_str()) {
            i += 2;
        } else if (!is_awk && token.is_empty()) || (token.len() > 1 && token.starts_with('-')) {
            i += 1;
        } else {
            return Some(i);
        }
    }
    None
}

/// Whether `word` carries a regex QUANTIFIER — a `.` immediately followed by
/// `*` (#7839, #7738, #7744).
///
/// Why: `.*`, `.*?`, the `.*?pen` a bracket class collapses `/.*[Oo]pen/`
/// into, and the `r.*?,` a Python raw-string prefix glues together all match
/// the guard's secret families through its glob-OVERLAP arm — the arm that
/// asks "could this SHELL GLOB expand onto a credential file?". Inside an
/// interpreter's program text there is no shell glob: `.` plus `*` is the
/// regex wildcard, and a real filename literal (`open('.env')`) carries no
/// quantifier at all.
/// What: the two-byte `.*` test only. A lone `.?` is deliberately EXCLUDED —
/// it is a working shell glob, and `normalize_bracket_classes` rewrites the
/// dotenv glob `.[e]nv` into exactly `.?nv`, so accepting `.?` would strip
/// that word to `.nv`, name nothing, and release a deny the shell would have
/// expanded onto `.env`. It is also NOT the whole answer — the caller pairs it
/// with [`without_glob_metacharacters`], so a word whose literal core still
/// names a secret (`.env.*`, `*credentials*`) keeps its deny and only a word
/// that matched through the wildcards alone is released.
/// Test: `bash_tokens_reads_a_quantifier_as_regex`,
/// `guard_tokenizer_bounds_the_program_text_relaxations`,
/// `guard_7839_sed_expression_wildcard`, `guard_7738_python_inline_regex_literal`,
/// `guard_7744_awk_pattern_match_rule`.
pub(crate) fn has_regex_quantifier(word: &str) -> bool {
    word.as_bytes()
        .windows(2)
        .any(|pair| pair[0] == b'.' && pair[1] == b'*')
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

/// Programs whose every argument is text they PRINT, never a file they open.
///
/// Test: `bash_tokens_reads_prose_a_printer_writes_to_a_file`.
const TEXT_PRINTERS: &[&str] = &["printf", "echo"];

/// Which tokens of a `printf`/`echo` call are prose it writes into a file
/// (#8723).
///
/// Why: `printf '%s\n' '<issue prose>' > body.md` was refused because the
/// prose quoted this guard's own deny text, which names a dotenv file. Neither
/// program opens an argument as a file, so a name in one is text.
/// What: the argument indices after the program, only when all of these hold:
/// the program's basename is a [`TEXT_PRINTERS`] entry; a stdout redirect
/// (`>`, `>>`, `1>`, `&>`) names a file; `printf` carries no `-v`, which
/// stores the text in a variable a later command can read; and `segment`
/// runs nothing nested — no `$` or backtick outside single quotes, and no
/// unquoted `(`. A redirect token and its target are never prose, so
/// `echo x > .env` and `echo x < .env` stay screened; a token carrying
/// whitespace was quoted, so it is prose even when it starts with `>`. The
/// CALLER owns one
/// more gate: `segment` must be the whole command, because a pipe hands the
/// printed name to a reader (`echo .env | xargs cat`).
/// Test: `bash_tokens_reads_prose_a_printer_writes_to_a_file`,
/// `allows_issue_prose_a_lone_printer_writes_to_a_file_8723`,
/// `denies_a_secret_read_beside_a_prose_write_8723`.
pub(crate) fn prose_write_indices(segment: &str, argv: &[String]) -> Vec<usize> {
    let Some(start) = crate::commands::hook_rewrite::strip_wrapper_prefix(argv) else {
        return Vec::new();
    };
    let program = argv
        .get(start)
        .map_or("", |t| t.rsplit('/').next().unwrap_or(t));
    if !TEXT_PRINTERS.contains(&program) || runs_a_nested_command(segment) {
        return Vec::new();
    }
    let (mut prose, mut writes_a_file, mut target_next) = (Vec::new(), false, false);
    for (index, token) in argv.iter().enumerate().skip(start + 1) {
        if std::mem::take(&mut target_next) {
            continue;
        }
        // A token carrying whitespace was quoted, so it is a word: a blockquote
        // line `'> naming …'` is prose, never a redirect.
        let is_word = token.chars().any(char::is_whitespace);
        let role = if is_word {
            RedirectRole::None
        } else {
            redirect_role(token)
        };
        if role != RedirectRole::None {
            let descriptor = token.split_once('>').map_or("", |(d, _)| d);
            let names_a_file =
                matches!(role, RedirectRole::Target(_) | RedirectRole::TargetFollows);
            writes_a_file |= names_a_file && matches!(descriptor, "" | "1" | "&");
            target_next = role == RedirectRole::TargetFollows;
            continue;
        }
        // An input redirect (`<`, `0<`, `<<<`): screened, target included.
        if !is_word
            && let Some((descriptor, rest)) = token.split_once('<')
            && descriptor.chars().all(|c| c.is_ascii_digit())
        {
            target_next = rest.trim_start_matches(['<', '&']).is_empty();
            continue;
        }
        if program == "printf" && token.starts_with("-v") {
            return Vec::new();
        }
        prose.push(index);
    }
    if writes_a_file { prose } else { Vec::new() }
}

/// Whether `segment` can run or expand anything beyond its literal words.
///
/// What: unbalanced quoting, a `$` or backtick outside single quotes, or an
/// unquoted `(` (a subshell or a process substitution).
/// Test: `bash_tokens_reads_prose_a_printer_writes_to_a_file`.
fn runs_a_nested_command(segment: &str) -> bool {
    let scan = super::shell_lex::QuoteScan::new(segment);
    !scan.balanced
        || segment.bytes().enumerate().any(|(i, b)| {
            (matches!(b, b'$' | b'`') && scan.allows_substitution(i))
                || (b == b'(' && scan.is_unquoted(i))
        })
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
        assert_eq!(redirect_role("&>out.txt"), RedirectRole::Target("out.txt"));
        assert_eq!(redirect_role(">&out.txt"), RedirectRole::Target("out.txt"));
        // #8730: every clobber spelling (bash `>|`; zsh `>!`, `>>|`, `>>!`,
        // `>&|`, `>&!`) reads exactly like its plain operator.
        for op in [
            ">|", ">!", ">>|", ">>!", "2>|", "2>!", "&>|", "&>!", ">&|", ">&!",
        ] {
            assert_eq!(
                redirect_role(&format!("{op}/dev/tty")),
                RedirectRole::Target("/dev/tty"),
                "{op}"
            );
            assert_eq!(redirect_role(op), RedirectRole::TargetFollows, "{op}");
        }
        // A clobber `>&` names a file even when the word is all digits.
        assert_eq!(redirect_role(">&!2"), RedirectRole::Target("2"));
        assert_eq!(redirect_role("2>&1-"), RedirectRole::FileDescriptor);
        for (text, at) in [("a >|b", 3), ("a >>|b", 4), ("a >&|b", 4), ("a &>|b", 4)] {
            assert!(is_clobber_bar(text.as_bytes(), at), "{text}");
        }
        for (text, at) in [("a | b", 2), ("a 2>&1|b", 6), ("|b", 0), ("a &|b", 3)] {
            assert!(!is_clobber_bar(text.as_bytes(), at), "{text}");
        }
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
        // The empty-token skip is sed's alone: `awk '' .env` is an empty awk
        // PROGRAM at index 1, so the operand behind it stays a path.
        let empty_awk = words("awk '' .env");
        assert_eq!(program_text_indices("awk", &empty_awk, 0), vec![1]);
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
        for regex in [".*", ".*?", ".*?pen", "r.*?,", "PASSAGE.*"] {
            assert!(has_regex_quantifier(regex), "{regex} carries a quantifier");
        }
        // `.?nv` is what `normalize_bracket_classes` makes of the glob
        // `.[e]nv`, which expands onto `.env` — a lone `.?` is a working shell
        // glob, so it never releases a deny.
        for name in [".env", ".netrc", "id_rsa", "secrets", ".", "", ".?nv", ".?"] {
            assert!(!has_regex_quantifier(name), "{name} carries none");
        }
        // The pairing: a dotenv glob carries a quantifier too, and keeps its
        // deny because its literal core still names the family.
        assert!(has_regex_quantifier(".env.*"));
        assert_eq!(without_glob_metacharacters(".env.*"), ".env.");
        assert_eq!(without_glob_metacharacters(".*?pen"), ".pen");
        assert_eq!(without_glob_metacharacters("/.*[Oo]pen/"), "/.Oopen/");
    }

    /// #8723: a printer's arguments are prose only while stdout is a file and
    /// nothing nested runs; a redirect token and its target never are.
    #[test]
    fn bash_tokens_reads_prose_a_printer_writes_to_a_file() {
        let prose = |c: &str| prose_write_indices(c, &words(c));
        assert_eq!(prose("printf '%s\\n' 'a `.env` b' > out.md"), vec![1, 2]);
        assert_eq!(prose("echo -n a .env >> out.md"), vec![1, 2, 3]);
        assert_eq!(prose("FOO=1 echo a 2>&1 >out.md < in.txt"), vec![2]);
        assert_eq!(prose("echo a &> out.md"), vec![1]);
        assert_eq!(prose("printf '%s' '> a .env' > out.md"), vec![1, 2]);
        for command in [
            "echo a .env",
            "echo a .env 2> err.md",
            "cat .env > out.md",
            "printf -v F .env > out.md",
            "echo \"$(cat .env)\" > out.md",
            "echo \"`cat .env`\" > out.md",
            "echo .env > >(cat)",
        ] {
            assert!(prose(command).is_empty(), "no prose in `{command}`");
        }
    }
}
