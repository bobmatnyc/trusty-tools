//! Sibling credential calls written as an argv list, for the credential-print
//! trigger (#10734).
//!
//! Why: `names_cli_trigger` reads words split on whitespace and shell
//! punctuation, so a script body's `subprocess.run(["op", "read", ref])` gave
//! the words `["op",` and `"read",` and the body was allowed. Widening that
//! split instead let every quoted `op` (a dict key, `e.get("op")`) and every
//! prose `op,` pair with any later `read`, which refused seeded scripts.
//! What: [`names_argv_trigger`] reads only quoted literals, each in the bracket
//! group that holds it, and matches the trigger table against one group's
//! literals at a time ([`names_trigger_words`]), so the words must sit in the
//! same argv list, not merely in the same file.
//! Test: `credential_print_tests::denies_a_credential_call_written_as_an_argv_list_10734`,
//! `credential_print_tests::allows_the_10734_neighbours`.

use super::{basename, names_trigger_words};

/// Nesting past which the text is read as naming a call (fail closed), which
/// also bounds the merge work per literal.
const MAX_GROUP_DEPTH: usize = 64;

/// What a bracket group is, by its opener and the byte before it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The top level of one line (`system "op", "read", ref` in Perl/Ruby).
    Line,
    /// `[` not glued to a value: a list literal.
    List,
    /// `(` not glued to a value: a tuple or a grouping.
    Tuple,
    /// `(` glued to a name or a closer: a call's arguments.
    Call,
    /// A subscript `x["op"]` or a `{` body: never an argv list.
    Other,
}

/// One open bracket group and the literals read in it.
struct Group {
    kind: Kind,
    words: Vec<String>,
    /// The current element holds code that is not a literal.
    other: bool,
    /// The current element holds a literal.
    literal: bool,
}

impl Group {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            words: Vec::new(),
            other: false,
            literal: false,
        }
    }

    /// Close the current element; in a list, an element with no literal is a
    /// run-time word (`["op", sub, ref]`), read as `$`.
    fn end_element(&mut self) {
        if self.kind == Kind::List && self.other && !self.literal {
            self.words.push("$".to_string());
        }
        (self.other, self.literal) = (false, false);
    }
}

/// Whether `text` names a sibling credential call as quoted literals in one
/// argv list, tuple, call or top-level line (#10734).
///
/// What: lowercases `text` and walks it once. A quoted literal joins the
/// innermost open group as one word (its basename, or `$` in a list when it
/// interpolates); one a newline or the end cuts off joins nothing. A literal
/// holding a quote or a bracket is also read as code itself (`python3 -c
/// '…'`). A triple quote is read as no delimiter at all, so its contents are
/// code: the guard blanks `#` lines before judging a body, which can drop a
/// docstring's closing line (#10734). A list or tuple's words also join its
/// parent, so `execFileSync("op", ["read", ref])` reads as one call. A
/// subscript and a `{` body are never read. A line or `;` at the top level
/// ends that line's group. Fail closed: a group left open at the end is still
/// read, and nesting past [`MAX_GROUP_DEPTH`], of brackets or of literals
/// read as code, counts as a match.
/// Test: `credential_print_tests::denies_a_credential_call_written_as_an_argv_list_10734`,
/// `credential_print_tests::allows_the_10734_neighbours`,
/// `credential_print_tests::deep_bracket_nesting_fails_closed_10734`.
pub(super) fn names_argv_trigger(text: &str) -> bool {
    names_argv_trigger_at(text, 0)
}

/// [`names_argv_trigger`] for a literal read as code `depth` levels down.
fn names_argv_trigger_at(text: &str, depth: usize) -> bool {
    if depth > MAX_GROUP_DEPTH {
        return true;
    }
    let chars: Vec<char> = text.to_ascii_lowercase().chars().collect();
    let mut stack = vec![Group::new(Kind::Line)];
    // The byte before the current one, `' '` after whitespace, so `x[` is a
    // subscript and `return [` a list.
    let mut prev = ' ';
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        i += 1;
        let top = stack.len() - 1;
        match c {
            '\'' | '"' => {
                let (word, next) = read_literal(&chars, i - 1);
                i = next;
                if let Some(word) = word {
                    if word.contains(['\'', '"', '[', '('])
                        && names_argv_trigger_at(&word, depth + 1)
                    {
                        return true;
                    }
                    let group = &mut stack[top];
                    let dynamic = group.kind == Kind::List && word.contains(['$', '{', '`']);
                    group.words.push(if dynamic {
                        "$".to_string()
                    } else {
                        basename(word.trim())
                    });
                    group.literal = true;
                }
            }
            '[' | '(' | '{' => {
                let glued = prev.is_alphanumeric() || "_)]'\"".contains(prev);
                let kind = match (c, glued) {
                    ('[', false) => Kind::List,
                    ('(', false) => Kind::Tuple,
                    ('(', true) => Kind::Call,
                    _ => Kind::Other,
                };
                stack[top].other = true;
                if stack.len() > MAX_GROUP_DEPTH {
                    return true;
                }
                stack.push(Group::new(kind));
            }
            ']' | ')' | '}' if top > 0 => {
                if let Some(group) = stack.pop()
                    && finish(group, stack.last_mut())
                {
                    return true;
                }
            }
            ',' => stack[top].end_element(),
            '\n' | ';' if top == 0 => {
                let line = std::mem::replace(&mut stack[0], Group::new(Kind::Line));
                if finish(line, None) {
                    return true;
                }
            }
            c if c.is_whitespace() => {}
            _ => stack[top].other = true,
        }
        prev = if c.is_whitespace() { ' ' } else { c };
    }
    while let Some(group) = stack.pop() {
        if finish(group, stack.last_mut()) {
            return true;
        }
    }
    false
}

/// Close `group`: whether its words name a call; a list's or tuple's words
/// then join `parent`.
fn finish(mut group: Group, parent: Option<&mut Group>) -> bool {
    group.end_element();
    if group.kind == Kind::Other {
        return false;
    }
    if names_trigger_words(&group.words) {
        return true;
    }
    if let (Kind::List | Kind::Tuple, Some(parent)) = (group.kind, parent) {
        parent.words.append(&mut group.words);
    }
    false
}

/// The literal opening at `start`, escapes resolved, and the index after it:
/// `None` past a triple quote, which is skipped, and for a one-line string a
/// newline or the end cuts off, which resumes at that newline.
fn read_literal(chars: &[char], start: usize) -> (Option<String>, usize) {
    let quote = chars[start];
    if chars.get(start + 1) == Some(&quote) && chars.get(start + 2) == Some(&quote) {
        return (None, start + 3);
    }
    let mut word = String::new();
    let mut j = start + 1;
    while let Some(&c) = chars.get(j) {
        match c {
            '\\' => {
                word.extend(chars.get(j + 1));
                j += 2;
            }
            '\n' => return (None, j),
            c if c == quote => return (Some(word), j + 1),
            c => {
                word.push(c);
                j += 1;
            }
        }
    }
    (None, chars.len())
}
