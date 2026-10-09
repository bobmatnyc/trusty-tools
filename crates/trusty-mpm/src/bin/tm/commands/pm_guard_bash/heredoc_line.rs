//! The program words of a here-document operator line (#9180).
//!
//! Why: whether a here-document body is shell source or stdin data is decided
//! from its operator line, and that line was split on whitespace alone. A
//! shell name glued to an operator (`bash<<'O'`), opened by a subshell paren
//! (`(bash <<'O'`), a separator (`true;bash <<'O'`) or a substitution
//! (`$(bash <<'O'`, a backtick), or quoted (`"bash" <<'O'`) was then read as
//! data, so nothing nested in its body was scanned and a delete placed there
//! passed the rm-root floor. A list of shells cannot be complete either: a
//! program word built by a substitution (`$(printf bas)h`), a parameter
//! (`b$X`), a shell function, `.`, `eval` or `tcsh` ran the body too.
//! What: [`operator_words`] cuts the line at blanks, `;&|()<>` and backticks,
//! drops quotes and backslashes and a `NAME=` prefix from each word, and
//! reduces it to its basename, marking a word that a `$`, a substitution or a
//! glued close makes unknown. [`line_reads_as_data`] is the fail-closed
//! question: every program word is a literal [`DATA_READERS`] name.
//! [`line_runs_a_shell`] is the narrower, positive question the separator
//! framing asks. [`heredoc_refusal`] refuses a command that redefines a
//! data reader as a function or alias.
//! Test: `operator_lines_name_a_glued_or_grouped_shell_9180`,
//! `operator_lines_keep_a_data_reader_as_data_9180`,
//! `operator_lines_read_as_data_only_for_a_literal_reader_9180`,
//! `shadowing_a_data_reader_is_refused_9180`.

use super::heredoc::{HEREDOC_DELIMITER_REASON, HeredocBodies};
use super::shell_lex::DASH_C_SHELLS;

/// The bytes that end a word on an operator line, besides blanks.
const BREAKS: &[u8] = b";&|()<>`";

/// The bytes after which the next word is in program position.
const COMMAND_STARTS: &[u8] = b";&|(`";

/// Programs that read a here-document body as data and never run it (#9180).
///
/// Why: the fail-closed reading needs a short list of what is safe, not a
/// list of what runs. Each name here is one the allowed shapes use — a commit
/// message (`git commit -F -`, `gh pr create --body-file -`, `tm … -`), a
/// file written with `cat`/`tee`, or `$(cat <<'EOF')` under `echo`/`printf`.
/// #9344: git, gh and tm keep the trust only per [`reader_keeps_trust`].
const DATA_READERS: &[&str] = &["cat", "tee", "git", "gh", "tm", "echo", "printf", "true"];

/// Programs that run a here-document body as shell source though no
/// [`DASH_C_SHELLS`] entry names them (#9180): read in program position only,
/// since `.` and `source` are ordinary argument words too.
const OTHER_SHELLS: &[&str] = &[
    ".", "source", "eval", "csh", "tcsh", "mksh", "yash", "rbash",
];

/// Deny reason for a command that redefines a here-document reader (#9180).
pub(crate) const SHADOWED_READER_REASON: &str = "this command defines a shell function or \
     alias named like a program the guard reads a here-document body as data for (`cat`, `tee`, \
     `git`, `gh`, `tm`, `echo`, `printf`, `true`), so a body handed to that name may run as shell \
     source (#9180). Use the program under its own name, or give the function another name.";

/// One word of an operator line.
struct Word {
    /// Quotes, backslashes and a `NAME=` prefix removed, basename kept.
    text: String,
    /// #9344: quotes and backslashes removed, nothing else.
    raw: String,
    /// Whether the word sits where the shell reads a program name.
    program: bool,
    /// Whether the word was a `NAME=value` assignment.
    assignment: bool,
    /// `false` when a `$`, a substitution or a glued close builds the word.
    literal: bool,
    /// #9344: how many substitutions opened in assignment values hold the word.
    depth: usize,
    /// #9344: an assignment whose value substitution closed on the line.
    closed: bool,
    /// #9344: a `closed` assignment whose value held a quote or `\`, so
    /// the `)` or backtick read as its close may be quoted text. #9360: or
    /// an assignment whose value substitution the line leaves open, when a
    /// quote or `\` means bash may have closed it or never opened it.
    quoted: bool,
}

/// Whether a here-document operator line hands its body to a shell.
///
/// Why (#6946 fail-open check, #9180): `bash <<'EOF'` runs the body as shell
/// source, so treating it as data hides `rm -rf …` from every rule.
/// What: `true` when any [`operator_words`] word is a
/// [`DASH_C_SHELLS`] shell, a version suffix (`bash5.2`) dropped, or a
/// program-position word is a parameter expansion or an [`OTHER_SHELLS`]
/// name. Scanning every word rather than the leading one keeps `sudo bash`,
/// `env - bash` and `foo && bash <<EOF` on the conservative side; a false
/// positive only restores the pre-#6946 splitting.
/// Test: `heredoc_frames_are_empty_for_a_shell_operator_line`,
/// `operator_lines_name_a_glued_or_grouped_shell_9180`.
/// #9344: a git word that injects config (`git -c alias.x='!sh' x`) runs one too.
pub(super) fn line_runs_a_shell(line: &str) -> bool {
    let words = operator_words(line);
    words.iter().enumerate().any(|(at, word)| {
        is_shell(&word.text)
            || (word.program && (is_parameter(&word.text) || OTHER_SHELLS.contains(&&*word.text)))
            // #9344: a `!`-alias or a config-made program runs through `sh`.
            || (word.program && git_injects_config(&words, at))
    })
}

/// Whether a here-document body opened on `line` is stdin data and no shell
/// source (#9180).
///
/// Why: a program the guard cannot name (`$(printf bas)h`, `b$X`, a function,
/// `tcsh`) may run the body, and a body read as data hides what it nests.
/// What: `true` only when every program-position word that is not an
/// assignment is a literal [`DATA_READERS`] name. Anything else — a word with
/// a `$` or built by a substitution, a program read from a substitution, any
/// other name — is shell-run.
/// #9344: and every git, gh or tm among them keeps its trust
/// ([`reader_keeps_trust`]), and no word is `quoted`: the shell may not have
/// closed that substitution where the scan did. #9360: or left it open.
/// Test: `operator_lines_read_as_data_only_for_a_literal_reader_9180`,
/// `injected_reader_lines_are_not_data_9344`,
/// `unclosed_quoted_value_substitution_lines_are_not_data_9360`,
/// `parameter_expansion_parens_reach_the_floor_9360`.
pub(super) fn line_reads_as_data(line: &str) -> bool {
    let words = operator_words(line);
    // #9360 critic round: the scan does not track `${…}`, where a paren
    // opens or closes no substitution, so such a line is shell-run.
    !expansion_holds_a_paren(line.as_bytes())
        && !words.iter().any(|word| word.quoted)
        && words
            .iter()
            .enumerate()
            .filter(|(_, word)| word.program && !word.assignment)
            .all(|(at, word)| {
                word.literal
                    && DATA_READERS.contains(&&*word.text)
                    && reader_keeps_trust(&words, at)
            })
}

/// Whether a `${…}` parameter expansion on `line` holds a `(`, `)`, backtick,
/// quote or `\`, read to the end of the line when its `}` never comes (#9360
/// critic round).
///
/// Why: bash opens and closes no substitution on such a paren, but
/// [`operator_words`] does, so `X=${x:-( cat } . f <<'O'` read `cat` as the
/// program and the body as data.
/// What: from each `${`, counts `{`/`}` depth to the matching `}`. Quotes are
/// not tracked, so a `"`, `'` or `\` inside the expansion is itself a `true`:
/// a quoted or escaped `}` (`${x:-"}"( cat }`) does not end the expansion in
/// bash, and the walk cannot tell where it does end.
/// Test: `parameter_expansion_parens_reach_the_floor_9360`,
/// `plain_parameter_expansions_stay_data_9360`.
fn expansion_holds_a_paren(line: &[u8]) -> bool {
    let mut at = 0;
    while let Some(start) = line[at..].windows(2).position(|w| w == b"${") {
        let mut depth = 0usize;
        for &byte in &line[at + start + 1..] {
            match byte {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                // #9360 delta critic: a quote or `\` may hide the closing `}`.
                b'(' | b')' | b'`' | b'"' | b'\'' | b'\\' => return true,
                _ => {}
            }
            if depth == 0 {
                break;
            }
        }
        at += start + 2;
    }
    false
}

/// git global options whose value is the next word (#9344).
const GIT_VALUE_OPTIONS: &[&str] = &[
    "-C",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--super-prefix",
    "--attr-source",
];

/// git builtins a here-document body is fed to as data (#9344). git ignores
/// an alias that would hide a builtin, so only these names are sure to be no
/// alias and no `git-<name>` program on `PATH`.
const GIT_DATA_SUBCOMMANDS: &[&str] = &[
    "commit",
    "tag",
    "notes",
    "apply",
    "am",
    "hash-object",
    "update-ref",
    "update-index",
    "cat-file",
    "mktree",
    "mktag",
    "check-ignore",
    "check-attr",
    "check-mailmap",
    "interpret-trailers",
    "stripspace",
    "fast-import",
    "patch-id",
    "mailinfo",
];

/// gh core commands a here-document body is fed to as data (#9344). gh
/// refuses an alias or an extension named like a core command.
const GH_DATA_SUBCOMMANDS: &[&str] = &[
    "api", "gist", "issue", "label", "pr", "project", "release", "repo", "run", "search", "secret",
    "variable", "workflow",
];

/// Whether data reader `words[at]` still reads its body as data (#9344).
///
/// Why: `git -c alias.x='!sh' x <<'O'` runs the body through `sh`, which
/// reads git's stdin, so trusting the word `git` let `rm -rf` through unjudged.
/// A gh alias or extension, or a git alias set earlier, does the same.
/// What: git, gh and tm lose trust behind a prefix assignment other than the
/// `x=` a substitution opens; git also on [`git_injects_config`] or a
/// subcommand outside [`GIT_DATA_SUBCOMMANDS`], gh on a first argument
/// outside [`GH_DATA_SUBCOMMANDS`]. tm has no config or alias form. Every
/// other reader keeps its trust. Fail-closed: no subcommand is no trust.
/// Test: `injected_reader_lines_are_not_data_9344`,
/// `commit_and_pr_body_shapes_stay_allowed_9344`.
fn reader_keeps_trust(words: &[Word], at: usize) -> bool {
    // #9344: a value substitution closed before the reader is no bare `x=`.
    let bare =
        |w: &Word| !w.closed && matches!(w.raw.split_once('=').map(|(_, v)| v), Some("" | "$"));
    let prefix_ok = prefix(words, at).all(bare);
    let mut args = arguments(words, at);
    match words[at].text.as_str() {
        "git" => {
            prefix_ok
                && !git_injects_config(words, at)
                && git_subcommand(words, at).is_some_and(|s| GIT_DATA_SUBCOMMANDS.contains(&s))
        }
        "gh" => {
            prefix_ok
                && args
                    .next()
                    .is_some_and(|w| GH_DATA_SUBCOMMANDS.contains(&&*w.raw))
        }
        "tm" => prefix_ok,
        _ => true,
    }
}

/// Whether git word `words[at]` injects config (#9344).
///
/// Why: git's `-c`/`--config-env` and the `GIT_CONFIG_*` variables make an
/// alias or a program out of nothing, so the line needs no earlier state.
/// What: `true` for a git word with a `GIT_CONFIG*` or `GIT_EXEC_PATH`
/// prefix assignment, or an argument starting `-c`, `--config` or
/// `--exec-path`, or naming an `alias.` key in any case.
/// Test: `injected_reader_lines_are_not_data_9344`.
fn git_injects_config(words: &[Word], at: usize) -> bool {
    words[at].text == "git"
        && (prefix(words, at)
            .any(|w| w.raw.starts_with("GIT_CONFIG") || w.raw.starts_with("GIT_EXEC_PATH"))
            || arguments(words, at).any(|w| {
                ["-c", "--config", "--exec-path"]
                    .iter()
                    .any(|p| w.raw.starts_with(p))
                    || w.raw.to_ascii_lowercase().contains("alias.")
            }))
}

/// The first git argument past the global options, or `None` (#9344).
fn git_subcommand(words: &[Word], at: usize) -> Option<&str> {
    let mut args = arguments(words, at);
    while let Some(word) = args.next() {
        if GIT_VALUE_OPTIONS.contains(&&*word.raw) {
            args.next();
        } else if !word.raw.starts_with('-') {
            return Some(&word.raw);
        }
    }
    None
}

/// The prefix assignments of program word `words[at]`, nearest first; #9344:
/// the words of a value substitution deeper than `words[at]` are skipped, so
/// `X=$(GIT_EXEC_PATH=… git` keeps git's own prefix.
fn prefix(words: &[Word], at: usize) -> impl Iterator<Item = &Word> {
    let depth = words[at].depth;
    words[..at]
        .iter()
        .rev()
        .filter(move |w| w.depth <= depth)
        .take_while(|w| w.program && w.assignment)
}

/// The words after program word `words[at]` up to the next program word.
fn arguments(words: &[Word], at: usize) -> impl Iterator<Item = &Word> {
    words[at + 1..].iter().take_while(|w| !w.program)
}

/// Whether any [`operator_words`] word of `line` satisfies `names` (#9180).
pub(super) fn line_names(line: &str, names: impl Fn(&str) -> bool) -> bool {
    operator_words(line).iter().any(|word| names(&word.text))
}

/// Whether `word` is a parameter expansion (`$SHELL`, `${SHELL}`); a bare `$`
/// is what `$(` and `$((` leave, so it is not one.
fn is_parameter(word: &str) -> bool {
    word.len() > 1 && word.starts_with('$')
}

/// Whether `word` names a shell, a version suffix dropped (`bash5.2`).
fn is_shell(word: &str) -> bool {
    let bare = word.trim_end_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | '-'));
    DASH_C_SHELLS.contains(&word) || (!bare.is_empty() && DASH_C_SHELLS.contains(&bare))
}

/// The words of an operator line (#9180).
///
/// What: cuts at blanks and [`BREAKS`]; removes `'`, `"` and `\`; drops a
/// leading `NAME=` assignment; keeps the basename. A word is in program
/// position when no word other than an assignment has followed the line's
/// start or the last [`COMMAND_STARTS`] byte. `$(` cuts to `$` and the word
/// after it, so a substitution's program is read too. A word is not literal
/// when it holds a `$`, starts right after a `)` or a closing backtick, or
/// runs into an opening backtick; a backtick opened in program position
/// yields a non-literal program word of its own, since the substitution's
/// output is then the program. #9344: the `)` or backtick that closes a
/// substitution opened in a prefix assignment's value (`X=$(…) git`) puts
/// the next word back in program position, marks that assignment closed,
/// and records each word's value-substitution depth, so [`prefix`] reaches
/// the assignment. Quotes are not tracked, so a close after a quote or `\`
/// inside the value marks the assignment `quoted`.
/// #9360: so does a value substitution still open at the end of the line
/// when a quote or `\` the scan cannot place — any but a plain here-document
/// delimiter's own ([`delimiter_quotes`]) — came after it opened, or a `'`
/// or `\` came before it in the assignment word: bash may never have opened
/// it, and the words the scan holds inside it may be programs.
/// Test: `injected_reader_lines_are_not_data_9344`,
/// `unclosed_quoted_value_substitution_lines_are_not_data_9360`,
/// `delimiter_quoted_value_substitutions_stay_data_9360`.
fn operator_words(line: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let mut word = Vec::new();
    let mut program = true;
    // Whether the word being read is glued to a substitution.
    let mut glued = false;
    // Whether the last byte closed a `)` or a backtick.
    let mut after_close = false;
    let mut in_tick = false;
    // #9344: per open `(`, and for the open backtick, the [`ValueOpen`] of
    // the prefix assignment whose value it opened, so its close restores
    // program position and marks that assignment closed, and `quoted` when a
    // quote or `\` came between.
    let mut parens: Vec<Option<ValueOpen>> = Vec::new();
    let mut tick_value: Option<ValueOpen> = None;
    // #9360: a `'` or `\` in the word being read.
    let mut escaped = false;
    let bytes = line.as_bytes();
    let is_quote = |b: &u8| matches!(b, b'\'' | b'"' | b'\\');
    let in_value = |word: &[u8], program: bool| {
        program && strip_assignment(&String::from_utf8_lossy(word)).is_some()
    };
    let flush = |words: &mut Vec<Word>,
                 word: &mut Vec<u8>,
                 program: &mut bool,
                 glued: &mut bool,
                 depth: usize| {
        if word.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(word).into_owned();
        word.clear();
        let assignment = strip_assignment(&text);
        let value = assignment.unwrap_or(&text);
        let base = value.rsplit('/').next().unwrap_or(value).to_string();
        words.push(Word {
            literal: !*glued && !text.contains('$'),
            raw: text.clone(),
            text: base,
            program: *program,
            assignment: assignment.is_some(),
            depth,
            closed: false,
            quoted: false,
        });
        *glued = false;
        if assignment.is_none() {
            *program = false;
        }
    };
    for (i, &byte) in bytes.iter().enumerate() {
        let depth = usize::from(tick_value.is_some()) + parens.iter().flatten().count();
        match byte {
            b'\'' | b'"' | b'\\' => escaped |= byte != b'"',
            b' ' | b'\t' | b'\n' | b'\r' => {
                flush(&mut words, &mut word, &mut program, &mut glued, depth);
                after_close = false;
                escaped = false;
            }
            b'`' if !in_tick => {
                in_tick = true;
                // `x=` takes the substitution as its value; any other word
                // the backtick runs into is built by it.
                let takes_value = strip_assignment(&String::from_utf8_lossy(&word)) == Some("");
                let opens_value = in_value(&word, program);
                glued |= !word.is_empty() && !takes_value;
                if word.is_empty() && program && !after_close {
                    word.push(b'`');
                    glued = true;
                }
                flush(&mut words, &mut word, &mut program, &mut glued, depth);
                tick_value = opens_value.then(|| (words.len() - 1, i, escaped));
                program = true;
                after_close = false;
                escaped = false;
            }
            _ if BREAKS.contains(&byte) => {
                let opens_value = byte == b'(' && in_value(&word, program);
                let escaped_open = std::mem::take(&mut escaped);
                flush(&mut words, &mut word, &mut program, &mut glued, depth);
                // #9344: `X=$(…) git`, `X=`…` git` — the close hands back
                // program position and marks the assignment closed.
                let closes = match byte {
                    b'`' => {
                        in_tick = false;
                        tick_value.take()
                    }
                    b'(' => {
                        parens.push(opens_value.then(|| (words.len() - 1, i, escaped_open)));
                        None
                    }
                    b')' => parens.pop().flatten(),
                    _ => None,
                };
                if let Some((at, open, _)) = closes {
                    words[at].closed = true;
                    // #9344: `X=$(echo ')' cat) f` — a quoted `)` closed it.
                    words[at].quoted = bytes[open..i].iter().any(is_quote);
                    program = true;
                }
                after_close = matches!(byte, b')' | b'`');
                if COMMAND_STARTS.contains(&byte) && byte != b'`' {
                    program = true;
                }
            }
            _ => {
                glued |= word.is_empty() && after_close;
                after_close = false;
                word.push(byte);
            }
        }
    }
    let depth = usize::from(tick_value.is_some()) + parens.iter().flatten().count();
    flush(&mut words, &mut word, &mut program, &mut glued, depth);
    // #9360: `X=$(echo '(' cat) . f <<'O'`, `X='$(true' . f <<'O'` — bash
    // closed or never opened what the scan still holds open.
    let skip = delimiter_quotes(bytes);
    for (at, open, escaped_open) in parens.into_iter().flatten().chain(tick_value) {
        let loose = (open..bytes.len()).any(|k| is_quote(&bytes[k]) && !skip[k]);
        words[at].quoted |= escaped_open || loose;
    }
    words
}

/// The open of a prefix assignment's value substitution (#9344, #9360): the
/// assignment's word index, the byte offset of its `(` or backtick, and
/// whether a `'` or `\` came before that byte in the assignment word.
type ValueOpen = (usize, usize, bool);

/// Per byte of `line`, whether it is a quote or `\` of a plain here-document
/// delimiter (#9360): `<<` or `<<-`, optional blanks, then `'NAME'`,
/// `"NAME"` or `\NAME` with `NAME` of letters, digits, `_`, `.` and `-`.
///
/// Why: `msg=$(cat <<'EOF'` leaves its value substitution open by design, and
/// the delimiter's quotes change only how the body is read, never where the
/// substitution closes. Any other quote still counts, failing closed.
/// Test: `delimiter_quoted_value_substitutions_stay_data_9360`.
fn delimiter_quotes(line: &[u8]) -> Vec<bool> {
    let mut skip = vec![false; line.len()];
    let name = |s: &[u8]| {
        !s.is_empty()
            && s.iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    };
    let mut i = 0;
    while i + 1 < line.len() {
        let here = line[i] == b'<'
            && line[i + 1] == b'<'
            && line.get(i + 2) != Some(&b'<')
            && (i == 0 || line[i - 1] != b'<');
        if !here {
            i += 1;
            continue;
        }
        let mut start = i + 2;
        start += usize::from(line.get(start) == Some(&b'-'));
        while matches!(line.get(start), Some(b' ' | b'\t')) {
            start += 1;
        }
        let end = line[start..]
            .iter()
            .position(|b| b.is_ascii_whitespace() || BREAKS.contains(b))
            .map_or(line.len(), |n| start + n);
        let plain = match &line[start..end] {
            [b'\'', inner @ .., b'\''] | [b'"', inner @ .., b'"'] => name(inner),
            [b'\\', inner @ ..] => name(inner),
            _ => false,
        };
        if plain {
            for at in start..end {
                skip[at] = matches!(line[at], b'\'' | b'"' | b'\\');
            }
        }
        i = end.max(i + 2);
    }
    skip
}

/// The value of a `NAME=value` word, or `None` when `word` is no assignment.
fn strip_assignment(word: &str) -> Option<&str> {
    let (name, value) = word.split_once('=')?;
    let mut chars = name.chars();
    let first = chars.next()?;
    let valid = (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then_some(value)
}

/// The here-document refusals [`super::unclassifiable_command`] makes.
///
/// Why: #9150 refuses a delimiter the scan cannot read. #9180: a command that
/// opens a here-document and redefines one of [`DATA_READERS`] — `cat(){
/// bash; }`, `alias git=bash` — runs a body the guard reads as data, at any
/// nesting depth, since a function reaches every shell the command starts.
/// What: [`HEREDOC_DELIMITER_REASON`] for an unscannable command, then
/// [`SHADOWED_READER_REASON`] when the command opens a here-document and
/// [`defined_names`] holds a data reader.
/// Test: `shadowing_a_data_reader_is_refused_9180`,
/// `heredoc_bodies_refuse_a_delimiter_split_by_a_word_break`.
pub(super) fn heredoc_refusal(command: &str) -> Option<&'static str> {
    if HeredocBodies::scan(command).is_unscannable() {
        return Some(HEREDOC_DELIMITER_REASON);
    }
    let shadows = command.contains("<<")
        && defined_names(command)
            .iter()
            .any(|name| DATA_READERS.contains(&name.as_str()));
    shadows.then_some(SHADOWED_READER_REASON)
}

/// The function and alias names `command` defines, read loosely (#9180):
/// `NAME()` before a compound command, `function NAME`, and each
/// `NAME=` word after `alias`, with quotes, `\` and line continuations
/// removed and the basename kept. Over-reading only refuses more.
fn defined_names(command: &str) -> Vec<String> {
    let plain: Vec<u8> = command
        .replace("\\\n", "")
        .bytes()
        .filter(|b| !matches!(b, b'\'' | b'"' | b'\\'))
        .collect();
    let mut tokens: Vec<&[u8]> = Vec::new();
    let mut start = None;
    for (i, &byte) in plain.iter().enumerate() {
        let single = b";&|(){}<>`".contains(&byte);
        if single || byte.is_ascii_whitespace() {
            if let Some(s) = start.take() {
                tokens.push(&plain[s..i]);
            }
            if single {
                tokens.push(&plain[i..=i]);
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        tokens.push(&plain[s..]);
    }
    let separator = |t: &[u8]| t.len() == 1 && b";&|(){}<>`".contains(&t[0]);
    let body = |t: Option<&&[u8]>| {
        t.is_some_and(|t| {
            matches!(
                *t,
                b"{" | b"(" | b"[[" | b"if" | b"for" | b"while" | b"until" | b"case" | b"select"
            )
        })
    };
    let mut names = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let next = tokens.get(i + 1);
        if !separator(token)
            && next == Some(&&b"("[..])
            && tokens.get(i + 2) == Some(&&b")"[..])
            && body(tokens.get(i + 3))
        {
            names.push(*token);
        } else if *token == b"function" && next.is_some_and(|t| !separator(t)) {
            names.extend(next);
        } else if *token == b"alias" {
            let words = tokens[i + 1..].iter().take_while(|t| !separator(t));
            names
                .extend(words.filter_map(|w| w.iter().position(|b| *b == b'=').map(|at| &w[..at])));
        }
    }
    names
        .into_iter()
        .map(|name| {
            let name = String::from_utf8_lossy(name);
            name.rsplit('/').next().unwrap_or(&name).to_string()
        })
        .collect()
}

// #9344: config/alias-injected readers and the data-body floor.
#[cfg(test)]
#[path = "data_reader_tests.rs"]
mod data_reader_tests;

// #9360: an open value substitution the scan cannot place is no data line.
#[cfg(test)]
#[path = "unclosed_value_substitution_tests.rs"]
mod unclosed_value_substitution_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// #9180: a shell glued to an operator, opened by a paren, a separator or
    /// a substitution, quoted, versioned, or a parameter in program position,
    /// runs the body.
    #[test]
    fn operator_lines_name_a_glued_or_grouped_shell_9180() {
        for line in [
            "bash<<'O'",
            "(bash <<'O'",
            "true;bash <<'O'",
            "true&&bash <<'O'",
            "echo x|bash <<'O'",
            "x=$(bash <<'O'",
            "`bash <<'O'",
            "\"bash\" <<'O'",
            "b'a'sh <<'O'",
            "\\bash <<'O'",
            "/bin/bash5.2 <<'O'",
            "X=1 bash<<'O'",
            "$SHELL <<'O'",
            "FOO=1 \"${SHELL}\" <<'O'",
            "{ sh <<'O'",
        ] {
            assert!(line_runs_a_shell(line), "{line:?}");
        }
    }

    /// #9180: a data reader stays data, and a parameter that is a redirect
    /// target or an argument is not read as a program.
    #[test]
    fn operator_lines_keep_a_data_reader_as_data_9180() {
        for line in [
            "cat <<'EOF'",
            "git commit -F - <<'EOF'",
            "cat > \"$OUT\" <<'EOF'",
            "python3 - \"$ARG\" <<'PY'",
            "cat <<EOF >>$LOG",
            "tee notes.md <<'EOF'",
            "x=$(cat <<'EOF'",
            "git commit -m \"$(cat <<'EOF'",
        ] {
            assert!(!line_runs_a_shell(line), "{line:?}");
        }
    }

    /// #9180: only a line whose every program word is a literal data reader
    /// reads its body as data; a built, parameter, substitution-fed, unknown
    /// or `.`-style program runs it.
    #[test]
    fn operator_lines_read_as_data_only_for_a_literal_reader_9180() {
        for data in [
            "cat <<'EOF'",
            "git commit -F - <<'EOF'",
            "gh pr create --body-file - <<'EOF'",
            "cat <<'EOF' | git commit -F -",
            "x=$(cat <<'EOF'",
            "x=`cat <<'EOF'",
            "git commit -m \"$(cat <<'EOF'",
            "echo \"$(cat <<EOF",
            "cat > \"$OUT\" <<'EOF'",
            "tee notes.md <<'EOF'",
        ] {
            assert!(line_reads_as_data(data), "{data:?}");
        }
        for shell_run in [
            "$(printf bas)h <<'O'",
            "b$X <<'O'",
            "f <<'O'",
            ". /dev/stdin <<'O'",
            "source /dev/stdin <<'O'",
            "tcsh <<'O'",
            "/bin/csh <<'O'",
            "eval \"$(cat <<'O'",
            "`printf ba`sh <<'O'",
            "ba`printf sh` <<'O'",
            "python3 - <<'PY'",
            "sudo -s <<'O'",
            "cat <<'EOF' | bash",
        ] {
            assert!(!line_reads_as_data(shell_run), "{shell_run:?}");
        }
    }

    /// #9180: redefining a data reader in a command that opens a
    /// here-document is refused; another name, or no here-document, is not.
    #[test]
    fn shadowing_a_data_reader_is_refused_9180() {
        for refused in [
            "cat(){ bash; }\ncat <<'O'\nx\nO",
            "function git { bash; }; git <<'O'\nx\nO",
            "alias cat=bash\ncat <<'O'\nx\nO",
            "c\\\nat () { bash; }; cat <<'O'\nx\nO",
            "'tee'() ( bash ); tee <<'O'\nx\nO",
        ] {
            assert_eq!(
                heredoc_refusal(refused),
                Some(SHADOWED_READER_REASON),
                "{refused:?}"
            );
        }
        for allowed in [
            "f(){ bash; }\nf <<'O'\nx\nO",
            "cat(){ bash; }; cat x",
            "cat > a.py <<'EOF'\ndef cat():\n    pass\nEOF",
            "cat <<'EOF'\nprint()\nEOF",
        ] {
            assert_eq!(heredoc_refusal(allowed), None, "{allowed:?}");
        }
    }
}
