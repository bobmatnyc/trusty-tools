//! The comment reading for a shell body's nested-run scan (#10598, #10548 P2).
//!
//! Why: the scan reads the raw body, so comment prose is split into segments
//! (`ack/progress/fyi` after a `;` in `pm-notify.sh`) and an apostrophe in a
//! comment unbalances the quoting for every line after it. Blanking any `#`
//! line naively refused nothing it should: `#` inside an open quote or after a
//! backslash-newline is code (the #10548 rows `quote1.sh` and `cont1.sh`).
//! What: [`blank_run_scan_comments`] walks the body with quotes, here-documents
//! and `$( … )` tracked, and blanks only a `#` that opens a real comment.
//! Fail closed: a shape it cannot follow answers the raw body, the reading
//! this scan had before, which only ever over-reads.
//! Test: `a_comment_cannot_start_a_nested_run_10598`,
//! `a_comment_cannot_unbalance_the_run_scan_10548`,
//! `every_review_bypass_row_refuses_10548`,
//! `blank_run_scan_comments_keeps_every_code_hash_10598`.

/// `body` with each real comment blanked to spaces, or `body` unchanged when
/// the walk loses track.
pub(super) fn blank_run_scan_comments(body: &str) -> String {
    walk(body).unwrap_or_else(|| body.to_string())
}

/// What the walker is inside.
#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    /// A `$( … )`, with its own unmatched `(` count.
    Sub(usize),
    /// A double-quoted string.
    Double,
}

/// One pass of [`blank_run_scan_comments`]; `None` means it lost track.
fn walk(body: &str) -> Option<String> {
    let bytes = body.as_bytes();
    let mut out = bytes.to_vec();
    let mut stack: Vec<Ctx> = Vec::new();
    let mut pending: Vec<(Vec<u8>, bool)> = Vec::new();
    let (mut i, mut paren, mut after_escape) = (0, 0usize, false);
    while let Some(&c) = bytes.get(i) {
        let next = bytes.get(i + 1).copied();
        let escaped = std::mem::take(&mut after_escape);
        if c == b'\\' {
            after_escape = true;
            i += 2;
            continue;
        }
        if stack.last() == Some(&Ctx::Double) {
            match (c, next) {
                (b'"', _) => {
                    stack.pop();
                    i += 1;
                }
                (b'`', _) => return None,
                (b'$', Some(b'(')) => {
                    stack.push(Ctx::Sub(0));
                    i += 2;
                }
                (b'$', Some(b'{')) => i = skip_parameter(bytes, i)?,
                _ => i += 1,
            }
            continue;
        }
        match (c, next) {
            (b'\'', _) => i = bytes[i + 1..].iter().position(|b| *b == b'\'')? + i + 2,
            (b'$', Some(b'\'')) => i = skip_ansi_c(bytes, i + 2)?,
            (b'$', Some(b'$')) => i += 2,
            (b'"', _) => {
                stack.push(Ctx::Double);
                i += 1;
            }
            (b'`', _) => return None,
            (b'$', Some(b'(')) => {
                stack.push(Ctx::Sub(0));
                i += 2;
            }
            (b'$', Some(b'{')) => i = skip_parameter(bytes, i)?,
            (b'(', _) => {
                match stack.last_mut() {
                    Some(Ctx::Sub(open)) => *open += 1,
                    _ => paren += 1,
                }
                i += 1;
            }
            (b')', _) => {
                if stack.last() == Some(&Ctx::Sub(0)) {
                    stack.pop();
                } else if let Some(Ctx::Sub(open)) = stack.last_mut() {
                    *open -= 1;
                } else {
                    paren = paren.saturating_sub(1);
                }
                i += 1;
            }
            (b'<', Some(b'<')) if bytes.get(i + 2) == Some(&b'<') => i += 3,
            (b'<', Some(b'<')) => i = read_heredoc_word(bytes, i + 2, &mut pending)?,
            (b'\n', _) if !pending.is_empty() => {
                i = skip_heredoc_bodies(bytes, i + 1, &pending);
                pending.clear();
            }
            (b'#', _) if !escaped && word_start(bytes, i) => {
                // A comment inside `$( … )` can hide its closing paren.
                if !stack.is_empty() {
                    return None;
                }
                while out.get(i).is_some_and(|b| *b != b'\n') {
                    out[i] = b' ';
                    i += 1;
                }
            }
            // #10598: a `case` arm's `)` would close the substitution early.
            (b'c', _)
                if !stack.is_empty() && bytes[i..].starts_with(b"case") && word_start(bytes, i) =>
            {
                return None;
            }
            _ => i += 1,
        }
    }
    (stack.is_empty() && paren == 0).then_some(())?;
    String::from_utf8(out).ok()
}

/// Whether byte `i` starts a shell word.
fn word_start(bytes: &[u8], i: usize) -> bool {
    i == 0
        || matches!(
            bytes[i - 1],
            b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'('
        )
}

/// The index after the `${ … }` opening at `i`; `None` when it holds a quote
/// or a substitution, whose reading the walker does not follow.
fn skip_parameter(bytes: &[u8], i: usize) -> Option<usize> {
    let close = bytes[i..].iter().position(|b| *b == b'}')? + i;
    let inner = &bytes[i..close];
    let plain = !inner.iter().any(|b| matches!(b, b'\'' | b'"' | b'`'))
        && !inner.windows(2).any(|w| w == b"$(");
    plain.then_some(close + 1)
}

/// The index after a `$'…'` body starting at `from`, where `\'` does not close.
fn skip_ansi_c(bytes: &[u8], from: usize) -> Option<usize> {
    let mut k = from;
    loop {
        match *bytes.get(k)? {
            b'\\' => k += 2,
            b'\'' => return Some(k + 1),
            _ => k += 1,
        }
    }
}

/// Record the delimiter of the `<<` whose word starts near `at`; the index
/// after it. `None` for a computed delimiter.
fn read_heredoc_word(
    bytes: &[u8],
    mut at: usize,
    pending: &mut Vec<(Vec<u8>, bool)>,
) -> Option<usize> {
    let dash = bytes.get(at) == Some(&b'-');
    at += usize::from(dash);
    while matches!(bytes.get(at), Some(b' ' | b'\t')) {
        at += 1;
    }
    let mut end = at;
    while bytes.get(end).is_some_and(|b| !b" \t\n;&|()<>".contains(b)) {
        end += 1;
    }
    let word: Vec<u8> = bytes[at..end]
        .iter()
        .copied()
        .filter(|b| !matches!(b, b'\'' | b'"' | b'\\'))
        .collect();
    if word.is_empty() || word.iter().any(|b| matches!(b, b'$' | b'`')) {
        return None;
    }
    pending.push((word, dash));
    Some(end)
}

/// The index after every pending here-document body starting at `from`.
fn skip_heredoc_bodies(bytes: &[u8], mut from: usize, pending: &[(Vec<u8>, bool)]) -> usize {
    for (word, dash) in pending {
        while from < bytes.len() {
            let end = bytes[from..]
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |n| n + from);
            let mut line = &bytes[from..end];
            if *dash {
                while let [b'\t', rest @ ..] = line {
                    line = rest;
                }
            }
            from = (end + 1).min(bytes.len());
            if line == word.as_slice() {
                break;
            }
        }
    }
    from
}
