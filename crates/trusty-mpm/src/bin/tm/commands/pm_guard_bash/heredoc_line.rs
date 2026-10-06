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
    /// Whether the word sits where the shell reads a program name.
    program: bool,
    /// Whether the word was a `NAME=value` assignment.
    assignment: bool,
    /// `false` when a `$`, a substitution or a glued close builds the word.
    literal: bool,
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
pub(super) fn line_runs_a_shell(line: &str) -> bool {
    operator_words(line).iter().any(|word| {
        is_shell(&word.text)
            || (word.program && (is_parameter(&word.text) || OTHER_SHELLS.contains(&&*word.text)))
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
/// Test: `operator_lines_read_as_data_only_for_a_literal_reader_9180`.
pub(super) fn line_reads_as_data(line: &str) -> bool {
    operator_words(line)
        .iter()
        .filter(|word| word.program && !word.assignment)
        .all(|word| word.literal && DATA_READERS.contains(&&*word.text))
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
/// output is then the program.
fn operator_words(line: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let mut word = Vec::new();
    let mut program = true;
    // Whether the word being read is glued to a substitution.
    let mut glued = false;
    // Whether the last byte closed a `)` or a backtick.
    let mut after_close = false;
    let mut in_tick = false;
    let mut flush = |word: &mut Vec<u8>, program: &mut bool, glued: &mut bool| {
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
            text: base,
            program: *program,
            assignment: assignment.is_some(),
        });
        *glued = false;
        if assignment.is_none() {
            *program = false;
        }
    };
    for &byte in line.as_bytes() {
        match byte {
            b'\'' | b'"' | b'\\' => {}
            b' ' | b'\t' | b'\n' | b'\r' => {
                flush(&mut word, &mut program, &mut glued);
                after_close = false;
            }
            b'`' if !in_tick => {
                in_tick = true;
                // `x=` takes the substitution as its value; any other word
                // the backtick runs into is built by it.
                let takes_value = strip_assignment(&String::from_utf8_lossy(&word)) == Some("");
                glued |= !word.is_empty() && !takes_value;
                if word.is_empty() && program && !after_close {
                    word.push(b'`');
                    glued = true;
                }
                flush(&mut word, &mut program, &mut glued);
                program = true;
                after_close = false;
            }
            _ if BREAKS.contains(&byte) => {
                flush(&mut word, &mut program, &mut glued);
                if byte == b'`' {
                    in_tick = false;
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
    flush(&mut word, &mut program, &mut glued);
    words
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
