//! A `Grep` tool glob read by ripgrep's own glob rules.
//!
//! Why: #10761 follow-up — the `Grep` glob arm read the glob with SHELL rules,
//! but ripgrep compiles it with `globset`, which differs in three ways the
//! guard missed. Inside a class `\` is a plain member, so `.e[!\]v` reaches
//! `.env`; outside one `\x` is `x`, so `.e\nv` is `.env`; and `{a,b}` is an
//! alternation whose branches may hold `/`, so the basename cut split
//! `{src/x,.env}` at the `/` and lost `.env`.
//! What: [`rg_glob_readings`] parses the glob the way `globset` 0.4.21's
//! `Parser` does and returns every name it can reach. The caller takes each
//! reading's basename afterwards.
//! A glob over [`MAX_RG_GLOB_BYTES`] is refused before either reader runs.
//! Test: `a_grep_glob_is_read_by_ripgrep_rules`,
//! `a_malformed_or_oversized_grep_glob_fails_closed`,
//! `a_deeply_nested_grep_glob_fails_closed_without_overflow`,
//! `a_grep_glob_over_the_cap_is_refused_in_bounded_time`.

use super::pm_guard_secret_bracket_parse::Item;
use super::pm_guard_secret_brackets::{MAX_READINGS, reachable, representative};

/// The deepest `{…}` nesting read; deeper fails closed. Older `globset`
/// releases rejected any nested alternation (`ErrorKind::NestedAlternates`),
/// so a routine glob nests once or twice.
const MAX_NESTING: usize = 32;

/// The longest `Grep` glob read; a longer one is refused.
// #10761: a glob is read by two readers, each up to 256 readings x length.
// A routine glob is under 100 bytes; 4 KiB leaves a path-heavy one room.
pub(crate) const MAX_RG_GLOB_BYTES: usize = 4 * 1024;

/// One parsed piece of a glob.
enum Piece {
    /// Text kept as written: a literal's stand-in, or a `?`/`*` wildcard.
    Text(String),
    /// A class: every reading character it reaches.
    Class(Vec<char>),
    /// A `{…}` alternation: each branch's pieces.
    Alternates(Vec<Vec<Piece>>),
}

/// Every name a ripgrep glob can reach, `*` and `?` kept as wildcards, or
/// `None` when the glob is malformed or has too many readings to judge.
///
/// Why: see the module doc. A glob `globset` rejects never runs, but the guard
/// cannot prove which version of ripgrep the harness ships, so a malformed
/// glob fails closed rather than being judged by a guess.
/// What: parses the whole glob (see [`parse`]) and enumerates up to
/// [`MAX_READINGS`] names. Over the cap each class is read as `?`; still over,
/// the answer is `None`.
/// Test: `a_grep_glob_is_read_by_ripgrep_rules`,
/// `a_malformed_or_oversized_grep_glob_fails_closed`,
/// `a_deeply_nested_grep_glob_fails_closed_without_overflow`.
pub(crate) fn rg_glob_readings(glob: &str) -> Option<Vec<String>> {
    let pieces = parse(glob)?;
    [false, true]
        .into_iter()
        .find_map(|widen| expand(&pieces, widen))
}

/// Parse `glob` with `globset`'s rules; `None` on any error it reports.
///
/// What: `?` and `*` stay wildcards; `[` opens a class (see [`class`]); `{`,
/// `,` and `}` build nested alternations, a `,` outside one being literal;
/// `\` makes the next character literal. A literal character becomes its
/// [`representative`] with `/` kept. An unclosed class or alternation, an
/// unopened `}`, a dangling `\` and a reversed range are errors, and so,
/// for this guard only, is nesting deeper than [`MAX_NESTING`].
fn parse(glob: &str) -> Option<Vec<Piece>> {
    let mut chars = glob.chars().peekable();
    // The open alternations: each holds its finished branches and the current one.
    let mut stack: Vec<(Vec<Vec<Piece>>, Vec<Piece>)> = Vec::new();
    let mut top: Vec<Piece> = Vec::new();
    while let Some(c) = chars.next() {
        let piece = match c {
            '?' | '*' => Piece::Text(c.to_string()),
            '[' => class(&mut chars)?,
            '\\' => literal(chars.next()?),
            '{' => {
                stack.push((Vec::new(), Vec::new()));
                // #10761: `expand` and Drop recurse once per level; fail closed.
                if stack.len() > MAX_NESTING {
                    return None;
                }
                continue;
            }
            '}' => {
                let (mut branches, branch) = stack.pop()?;
                branches.push(branch);
                Piece::Alternates(branches)
            }
            ',' => match stack.last_mut() {
                Some((branches, branch)) => {
                    branches.push(std::mem::take(branch));
                    continue;
                }
                None => literal(','),
            },
            c => literal(c),
        };
        match stack.last_mut() {
            Some((_, branch)) => branch.push(piece),
            None => top.push(piece),
        }
    }
    stack.is_empty().then_some(top)
}

fn literal(c: char) -> Piece {
    Piece::Text(representative(c, true).to_string())
}

/// The class after a `[`, read as `globset`'s `parse_class` reads it: an
/// optional `!`/`^`, a `]` or `-` first is a member, `-` between two members
/// is a range, a trailing `-` is a member, and every other character —
/// `\` included — is itself.
fn class(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<Piece> {
    let negated = chars.next_if(|c| matches!(c, '!' | '^')).is_some();
    let mut items: Vec<Item> = Vec::new();
    let mut first = true;
    let mut in_range = false;
    loop {
        let c = chars.next()?;
        match c {
            ']' if !first => break,
            '-' if !first && !in_range => in_range = true,
            c if in_range => {
                // `globset` extends the last member, a range included (`a-c-e`).
                let lo = match items.pop() {
                    Some(Item::Char(lo) | Item::Range(lo, _)) => lo,
                    _ => return None,
                };
                if c < lo {
                    return None;
                }
                items.push(Item::Range(lo, c));
                in_range = false;
            }
            c => items.push(Item::Char(c)),
        }
        first = false;
    }
    if in_range {
        items.push(Item::Char('-'));
    }
    Some(Piece::Class(reachable(negated, &items, true)))
}

/// Every reading of `pieces`, or `None` past [`MAX_READINGS`]; `widen` reads
/// each class as `?`.
fn expand(pieces: &[Piece], widen: bool) -> Option<Vec<String>> {
    let mut out = vec![String::new()];
    for piece in pieces {
        let options: Vec<String> = match piece {
            Piece::Text(text) => vec![text.clone()],
            Piece::Class(_) if widen => vec!["?".to_string()],
            Piece::Class(reach) => reach.iter().map(char::to_string).collect(),
            Piece::Alternates(branches) => {
                let mut all = Vec::new();
                for branch in branches {
                    all.extend(expand(branch, widen)?);
                }
                all
            }
        };
        if out.len().saturating_mul(options.len()) > MAX_READINGS {
            return None;
        }
        // #10761: a one-option piece is appended in place, so the work is
        // readings x length, not a rebuild of every reading per piece.
        if let [only] = options.as_slice() {
            out.iter_mut().for_each(|reading| reading.push_str(only));
            continue;
        }
        out = out
            .iter()
            .flat_map(|prefix| options.iter().map(move |o| format!("{prefix}{o}")))
            .collect();
    }
    Some(out)
}

/// The refusal for a `Grep` glob over [`MAX_RG_GLOB_BYTES`], else `None`.
/// Test: `a_grep_glob_over_the_cap_is_refused_in_bounded_time`.
pub(crate) fn rg_glob_too_long(glob: &str) -> Option<String> {
    (glob.len() > MAX_RG_GLOB_BYTES).then(|| {
        format!(
            "this `Grep` glob is refused (issue #10761) — it is {} bytes, over the \
             {MAX_RG_GLOB_BYTES}-byte limit up to which this guard reads a glob, so it is too \
             long to judge. Use a shorter glob, or several `Grep` calls.",
            glob.len()
        )
    })
}
