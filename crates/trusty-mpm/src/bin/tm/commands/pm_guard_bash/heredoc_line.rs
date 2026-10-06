//! The program words of a here-document operator line (#9180).
//!
//! Why: whether a here-document body is shell source or stdin data is decided
//! from its operator line, and that line was split on whitespace alone. A
//! shell name glued to an operator (`bash<<'O'`), opened by a subshell paren
//! (`(bash <<'O'`), a separator (`true;bash <<'O'`) or a substitution
//! (`$(bash <<'O'`, a backtick), or quoted (`"bash" <<'O'`) was then read as
//! data, so nothing nested in its body was scanned and a delete placed there
//! passed the rm-root floor.
//! What: [`operator_words`] cuts the line at blanks, `;&|()<>` and backticks,
//! drops quotes and backslashes and a `NAME=` prefix from each word, and
//! reduces it to its basename. [`line_names`] asks whether any such word
//! satisfies a predicate; [`line_runs_a_shell`] is that question for the
//! shells, plus a parameter expansion (`$SHELL`) in program position, whose
//! program the guard cannot name. Every misreading leans toward "a shell runs
//! it", which only keeps a body under scrutiny.
//! Test: `operator_lines_name_a_glued_or_grouped_shell_9180`,
//! `operator_lines_keep_a_data_reader_as_data_9180`.

use super::shell_lex::DASH_C_SHELLS;

/// The bytes that end a word on an operator line, besides blanks.
const BREAKS: &[u8] = b";&|()<>`";

/// The bytes after which the next word is in program position.
const COMMAND_STARTS: &[u8] = b";&|(`";

/// Whether a here-document operator line hands its body to a shell.
///
/// Why (#6946 fail-open check, #9180): `bash <<'EOF'` runs the body as shell
/// source, so treating it as data hides `rm -rf …` from every rule.
/// What: `true` when any [`operator_words`] word is a
/// [`DASH_C_SHELLS`] shell, a version suffix (`bash5.2`) dropped, or a
/// program-position word is a parameter expansion. Scanning every word
/// rather than the leading one keeps `sudo bash`, `env - bash` and
/// `foo && bash <<EOF` on the conservative side; a false positive only
/// restores the pre-#6946 splitting.
/// Test: `heredoc_frames_are_empty_for_a_shell_operator_line`,
/// `operator_lines_name_a_glued_or_grouped_shell_9180`.
pub(super) fn line_runs_a_shell(line: &str) -> bool {
    operator_words(line)
        .iter()
        .any(|(word, program)| is_shell(word) || (*program && is_parameter(word)))
}

/// Whether any [`operator_words`] word of `line` satisfies `names` (#9180).
pub(super) fn line_names(line: &str, names: impl Fn(&str) -> bool) -> bool {
    operator_words(line).iter().any(|(word, _)| names(word))
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

/// The words of an operator line, each with whether it sits in program
/// position (#9180).
///
/// What: cuts at blanks and [`BREAKS`]; removes `'`, `"` and `\`; drops a
/// leading `NAME=` assignment; keeps the basename. A word is in program
/// position when no word other than an assignment has followed the line's
/// start or the last [`COMMAND_STARTS`] byte. `$(` cuts to `$` and the word
/// after it, so a substitution's program is read too.
pub(super) fn operator_words(line: &str) -> Vec<(String, bool)> {
    let mut words = Vec::new();
    let mut word = Vec::new();
    let mut program = true;
    let mut flush = |word: &mut Vec<u8>, program: &mut bool| {
        if word.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(word).into_owned();
        word.clear();
        let assignment = strip_assignment(&text);
        let value = assignment.unwrap_or(&text);
        let base = value.rsplit('/').next().unwrap_or(value).to_string();
        words.push((base, *program));
        if assignment.is_none() {
            *program = false;
        }
    };
    for &byte in line.as_bytes() {
        match byte {
            b'\'' | b'"' | b'\\' => {}
            b' ' | b'\t' | b'\n' | b'\r' => flush(&mut word, &mut program),
            _ if BREAKS.contains(&byte) => {
                flush(&mut word, &mut program);
                if COMMAND_STARTS.contains(&byte) {
                    program = true;
                }
            }
            _ => word.push(byte),
        }
    }
    flush(&mut word, &mut program);
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
}
