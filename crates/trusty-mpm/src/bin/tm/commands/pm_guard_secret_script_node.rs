//! The JavaScript reading of a node script body, for `pm_guard_secret_script`
//! (#10548).
//!
//! Why: the #8879 judge hands a node body to the shell scanners as a
//! here-document, and the #7266 file rule reads every `word.ext` in it as a
//! file. A JS member access (`x.key`, `cfg.env`, `lic.license`) then reads as
//! the `*.key`, `.env` or `*.license` file class, which refused a user's
//! comparison and adapter scripts. In JS, code names a file only through a
//! string, template or regex literal.
//! What: [`js_reading`] lexes the body as JS and returns it with each member
//! access dot in CODE joined (`x.key` → `x_key`) and each comment blanked.
//! Every literal, including a template's text, stays byte for byte, so the
//! shell judges still see `"server.key"`, `` `${home}/.env` `` and
//! `"gh auth token"`. Joining with `_` makes no new word for the credential
//! rules (`gh.auth.token` does not become `gh auth token`).
//!
//! Fail closed: a body the lexer cannot read in full answers `None` and is
//! judged raw, as before: an unterminated string, template, regex or block
//! comment, a newline in a quoted string, or an unbalanced brace. A body that
//! can run its own text (`eval`, `Function`, `vm` beside `__filename`,
//! `import.meta` or `process.argv`) keeps its comments.
//!
//! Residuals: a regex literal written after `)` or `]` is read as division
//! (`if (x) /re/`), which can misread the rest; a misread almost always ends
//! in an unterminated literal and answers `None`. A file name built at run
//! time (`'.e' + 'nv'`) is open, as it is in the shell judges.
//! Test: `pm_guard_secret_script_node_tests.rs`.

/// One lexer frame: code with its open-brace depth, or a template's text.
enum Frame {
    Code(u32),
    Template,
}

/// Keywords after which a `/` starts a regex literal, not a division.
const REGEX_AFTER: &[&str] = &[
    "return",
    "typeof",
    "case",
    "in",
    "of",
    "new",
    "delete",
    "void",
    "throw",
    "instanceof",
    "yield",
    "await",
    "else",
    "do",
];

/// Words that let a body run its own source text.
const SELF_EVAL: &[&str] = &["eval", "Function", "vm."];

/// Words that locate a body's own source.
const SELF_PATH: &[&str] = &["__filename", "import.meta", "process.argv"];

/// Read `body` as JS: member-access dots in code joined, comments blanked;
/// `None` for a body the lexer cannot read in full. See the module doc.
///
/// Why: a JS member access names no file (#10548).
/// What: one pass over `body` with a frame stack for template substitutions;
/// literals are copied verbatim, code dots between an operand and a property
/// name become `_`, comments become spaces (kept when the body can evaluate
/// itself).
/// Test: `a_node_run_of_member_accesses_allows_10548`,
/// `a_node_body_naming_a_secret_literal_refuses_10548`,
/// `a_self_evaluating_body_keeps_its_comments_judged_10548`,
/// `an_unreadable_js_body_is_judged_raw_10548`.
pub(super) fn js_reading(body: &str) -> Option<String> {
    let keep_comments =
        SELF_EVAL.iter().any(|w| body.contains(w)) && SELF_PATH.iter().any(|w| body.contains(w));
    let c: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    let mut stack = vec![Frame::Code(0)];
    let (mut i, mut regex_ok, mut word) = (0, true, String::new());
    while let Some(&ch) = c.get(i) {
        let next = c.get(i + 1).copied();
        if let Some(Frame::Template) = stack.last() {
            match ch {
                '\\' => {
                    out.push(ch);
                    out.push(next?);
                    i += 2;
                }
                '`' => {
                    out.push(ch);
                    stack.pop();
                    (i, regex_ok) = (i + 1, false);
                }
                '$' if next == Some('{') => {
                    out.push_str("${");
                    stack.push(Frame::Code(0));
                    (i, regex_ok) = (i + 2, true);
                }
                _ => {
                    out.push(ch);
                    i += 1;
                }
            }
            continue;
        }
        if is_ident(ch) {
            word.push(ch);
            out.push(ch);
            i += 1;
            continue;
        }
        if !word.is_empty() {
            regex_ok = REGEX_AFTER.contains(&word.as_str());
            word.clear();
        }
        let end = match ch {
            '/' if next == Some('/') => {
                let end = (i..c.len()).find(|&j| c[j] == '\n').unwrap_or(c.len());
                comment(&mut out, &c[i..end], keep_comments);
                end
            }
            '/' if next == Some('*') => {
                let end = (i + 2..c.len().saturating_sub(1))
                    .find(|&j| c[j] == '*' && c[j + 1] == '/')?
                    + 2;
                comment(&mut out, &c[i..end], keep_comments);
                end
            }
            '/' if regex_ok => {
                let end = regex_end(&c, i)?;
                regex_ok = false;
                out.extend(&c[i..end]);
                end
            }
            '\'' | '"' => {
                let end = string_end(&c, i)?;
                regex_ok = false;
                out.extend(&c[i..end]);
                end
            }
            '`' => {
                out.push(ch);
                stack.push(Frame::Template);
                i + 1
            }
            '.' if is_member_access(&out, next) => {
                // #10548: `x.key` is a property, never the `*.key` file class.
                out.push('_');
                i + 1
            }
            _ => {
                let nested = stack.len() > 1;
                match (ch, stack.last_mut()) {
                    ('{', Some(Frame::Code(depth))) => *depth += 1,
                    // A `}` at depth 0 closes a template substitution.
                    ('}', Some(Frame::Code(0))) if nested => {
                        stack.pop();
                    }
                    ('}', Some(Frame::Code(0))) => return None,
                    ('}', Some(Frame::Code(depth))) => *depth -= 1,
                    _ => {}
                }
                if !ch.is_whitespace() {
                    regex_ok = !matches!(ch, ')' | ']');
                }
                out.push(ch);
                i + 1
            }
        };
        i = end;
    }
    matches!(stack.as_slice(), [Frame::Code(0)]).then_some(out)
}

/// Whether `ch` continues a JS identifier, a number or a private name.
fn is_ident(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '$')
}

/// Whether the `.` about to be written after `out`, followed by `next`, is a
/// member access: an operand (a name, `)`, `]`, or `?.`) before it and a
/// property name after it. A number's decimal point is followed by a digit.
fn is_member_access(out: &str, next: Option<char>) -> bool {
    let starts_name = next.is_some_and(|n| n.is_alphabetic() || matches!(n, '_' | '$' | '#'));
    let mut before = out.chars().rev().skip_while(|c| c.is_whitespace());
    let operand = match before.next() {
        Some('?') => before.next().is_some_and(|p| p != '?'),
        Some(p) => is_ident(p) || matches!(p, ')' | ']'),
        None => false,
    };
    starts_name && operand
}

/// Write a comment: verbatim when `keep`, else as spaces with its newlines.
fn comment(out: &mut String, text: &[char], keep: bool) {
    out.extend(
        text.iter()
            .map(|&c| if keep || c == '\n' { c } else { ' ' }),
    );
}

/// The index just past the quoted string opening at `start`; `None` when it
/// is unterminated or crosses an unescaped newline.
fn string_end(c: &[char], start: usize) -> Option<usize> {
    let quote = c[start];
    let mut j = start + 1;
    loop {
        match c.get(j)? {
            '\\' => j += 2,
            '\n' => return None,
            &q if q == quote => return Some(j + 1),
            _ => j += 1,
        }
    }
}

/// The index just past the regex literal opening at `start`, flags included;
/// `None` when it is unterminated on its line.
fn regex_end(c: &[char], start: usize) -> Option<usize> {
    let (mut j, mut class) = (start + 1, false);
    loop {
        match c.get(j)? {
            '\\' => j += 2,
            '\n' => return None,
            '[' => (j, class) = (j + 1, true),
            ']' => (j, class) = (j + 1, false),
            '/' if !class => break,
            _ => j += 1,
        }
    }
    j += 1;
    while c.get(j).is_some_and(|f| f.is_ascii_alphabetic()) {
        j += 1;
    }
    Some(j)
}

/// Whether node's arguments `args` only syntax-check a script file.
///
/// Why: `node --check <file>` (`-c`) parses the file and never runs it, so its
/// body cannot read a secret (#10548).
/// What: true when every option before the first operand is `--check` or
/// `-c`, at least one is, and an operand follows. Any other option (`-r`,
/// `--import`, `--inspect`, `-e`, `--`), an input redirect or a here-document
/// answers false, and the file is judged as a run. Output redirects pass.
/// Test: `node_check_of_a_member_access_body_allows_10548`,
/// `a_node_run_beside_a_check_is_judged_10548`.
pub(super) fn check_only(args: &[String]) -> bool {
    use crate::commands::pm_guard_bash::{RedirectRole, input_redirect_operand, redirect_role};
    let (mut check, mut i) = (false, 0);
    while let Some(token) = args.get(i) {
        i += 1;
        if token.starts_with("<<") || input_redirect_operand(token).is_some() {
            return false;
        }
        match redirect_role(token) {
            RedirectRole::None => {}
            RedirectRole::TargetFollows => {
                i += 1;
                continue;
            }
            _ => continue,
        }
        match token.as_str() {
            "--check" | "-c" => check = true,
            t if t.starts_with('-') => return false,
            _ => return check,
        }
    }
    false
}

/// Whether `path` is a JavaScript file: a `.js`, `.mjs` or `.cjs` name, or a
/// body whose `#!` line runs node (`shebang`). A shell file node is told to
/// load (`node -r ./cat.sh`) keeps the shell judges' raw reading (#10548).
/// Test: `a_non_js_file_node_loads_is_judged_raw_10548`.
pub(super) fn is_js_file(path: &std::path::Path, shebang: &str) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    matches!(ext, Some("js" | "mjs" | "cjs")) || matches!(shebang, "node" | "nodejs")
}
