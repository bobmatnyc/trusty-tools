//! The symbols a diff changes, found from the diff alone (#9196).
//!
//! Why: AC1 asks for callers, callees and tests "for each changed symbol",
//! and the existing top-5-term search query is not symbol-derived (the AC's
//! shortcut to fail). Architect ruling Q3: diff-only, recall measured in the
//! live check.
//! What: [`changed_symbols`] reads one file's patch: a changed line that
//! declares a callable is `Declared`; a hunk whose `@@` suffix declares one,
//! or whose context above its first change does, `Touched`. A Rust method
//! takes the `impl` type the hunk shows (`Type::name`, the graph's own key
//! form). A hunk that names no symbol is counted, never guessed.
//! [`priority`] orders symbols for the caps.
//! Test: `symbols_tests.rs`.

use std::{cmp::Reverse, collections::HashMap};

/// How a symbol is tied to the diff; the declaration order is the priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    /// A changed line declares it.
    Declared,
    /// A hunk changes its body.
    Touched,
}

/// One changed symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangedSymbol {
    /// The file, as the diff names it (normalised).
    pub(crate) path: String,
    /// `name`, or `Type::name` for a Rust method whose `impl` the hunk shows.
    pub(crate) name: String,
    /// How the diff ties it.
    pub(crate) kind: Kind,
    /// Changed lines in the hunks that name it: the size proxy.
    pub(crate) diff_lines: usize,
}

impl ChangedSymbol {
    /// `path::name`: the ledger id and the call-chain entry point.
    pub(crate) fn id(&self) -> String {
        format!("{}::{}", self.path, self.name)
    }
}

/// What one patch gave.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Found {
    /// The symbols, one per name, in first-seen order.
    pub(crate) symbols: Vec<ChangedSymbol>,
    /// Hunks with a changed line and no symbol.
    pub(crate) unresolved: usize,
}

/// Longest symbol name queried (B4 amendment 4: diff text is untrusted).
const MAX_NAME_CHARS: usize = 200;

/// The changed symbols of `patch`, a file's hunks with their `@@` headers.
///
/// Why: plan §4 steps 1-3.
/// What: per hunk, every changed (`+`/`-`) line that declares a callable is
/// `Declared`; otherwise the `@@` suffix, else the nearest declaration in
/// the context and removed lines above the first change, is `Touched`; else
/// the hunk is `unresolved`. An indented Rust declaration takes the `impl`
/// that still encloses it, in the hunk or the `@@` suffix ([`qualify`]).
/// One entry per name: `Declared` wins and `diff_lines` add up.
/// Test: `a_rust_method_takes_its_impl_type`, `a_free_fn_after_a_closed_impl_is_not_a_method`,
/// `each_language_declaration_is_found`,
/// `a_body_edit_takes_the_hunk_header`, `a_body_edit_scans_back_through_context`,
/// `a_hunk_with_no_symbol_is_counted`.
pub(crate) fn changed_symbols(path: &str, patch: &str) -> Found {
    let rust = path.ends_with(".rs");
    let mut found = Found::default();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (suffix, hunk) in hunks(patch) {
        let changed = hunk.iter().filter(|l| is_change(l)).count();
        if changed == 0 {
            continue;
        }
        let mut named = Vec::new();
        for (i, line) in hunk.iter().enumerate() {
            if is_change(line)
                && let Some(name) = decl_name(&line[1..])
            {
                let name = qualify(rust, name, &line[1..], &hunk[..i], suffix);
                named.push((name, Kind::Declared));
            }
        }
        if named.is_empty() {
            let first = hunk.iter().position(|l| is_change(l)).unwrap_or(0);
            // The `@@` suffix is a declaration line itself: nothing above it.
            let touched = decl_name(suffix)
                .map(|n| qualify(rust, n, suffix, &[], ""))
                .or_else(|| {
                    (0..first).rev().find_map(|j| {
                        let body = &hunk[j][1..];
                        (!hunk[j].starts_with('+'))
                            .then(|| decl_name(body))
                            .flatten()
                            .map(|n| qualify(rust, n, body, &hunk[..j], suffix))
                    })
                });
            if let Some(name) = touched {
                named.push((name, Kind::Touched));
            }
        }
        if named.is_empty() {
            found.unresolved += 1;
        }
        for (name, kind) in named {
            let slot = *index.entry(name.clone()).or_insert_with(|| {
                found.symbols.push(ChangedSymbol {
                    path: path.to_string(),
                    name,
                    kind,
                    diff_lines: 0,
                });
                found.symbols.len() - 1
            });
            let symbol = &mut found.symbols[slot];
            symbol.kind = symbol.kind.min(kind);
            symbol.diff_lines += changed;
        }
    }
    found
}

/// `symbols` in priority order: `Declared` first, then more diff lines, then
/// path and name (plan AC2).
///
/// Test: `priority_is_declared_then_size_then_path`.
pub(crate) fn priority(mut symbols: Vec<ChangedSymbol>) -> Vec<ChangedSymbol> {
    symbols.sort_by(|a, b| {
        (a.kind, Reverse(a.diff_lines), &a.path, &a.name).cmp(&(
            b.kind,
            Reverse(b.diff_lines),
            &b.path,
            &b.name,
        ))
    });
    symbols
}

/// `(@@ suffix, body lines)` per hunk.
fn hunks(patch: &str) -> Vec<(&str, Vec<&str>)> {
    let mut out: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            // Keep the suffix line's own indentation; git adds one space.
            let suffix = rest
                .split_once("@@")
                .map_or("", |(_, s)| s.strip_prefix(' ').unwrap_or(s).trim_end());
            out.push((suffix, Vec::new()));
        } else if let Some((_, body)) = out.last_mut()
            && matches!(line.chars().next(), Some('+' | '-' | ' '))
            && !line.starts_with("+++")
            && !line.starts_with("---")
        {
            body.push(line);
        }
    }
    out
}

/// Whether a hunk line is an added or removed line.
fn is_change(line: &str) -> bool {
    line.starts_with('+') || line.starts_with('-')
}

/// `name`, as `Type::name` when `rust` and an `impl` still encloses `decl`.
///
/// Why: #9196 critic finding 2: a free fn edited after `impl Foo { }` came
/// back as `Foo::helper` from the `@@ ... @@ impl Foo {` suffix.
/// What: only an indented `decl` can be a method. Scanning up from it
/// through `above`, then the `@@` suffix (a column-0 line), an `impl` line
/// encloses it when `decl` is indented deeper and no `}` line at or left of
/// the `impl`'s indentation lies between them; otherwise `name` is returned
/// unqualified.
/// Test: `a_free_fn_after_a_closed_impl_is_not_a_method`,
/// `a_rust_method_takes_its_impl_type`, `a_body_edit_scans_back_through_context`.
fn qualify(rust: bool, name: String, decl: &str, above: &[&str], suffix: &str) -> String {
    let depth = indent(decl);
    if !rust || depth == 0 {
        return name;
    }
    let mut closed_at = usize::MAX; // the leftmost `}` seen so far
    let lines = above
        .iter()
        .rev()
        .map(|l| &l[1..])
        .chain(std::iter::once(suffix));
    for line in lines {
        let at = indent(line);
        if line.trim_start().starts_with('}') {
            closed_at = closed_at.min(at);
        } else if let Some(ty) = impl_type(line) {
            if closed_at > at && depth > at {
                return format!("{ty}::{name}");
            }
            return name;
        }
    }
    name
}

/// Leading whitespace characters of `line`.
fn indent(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

/// The callable `line` declares, if any: `fn` (Rust), `def` (Python),
/// `function` (TS/JS), `func` (Go, a method's receiver skipped).
fn decl_name(line: &str) -> Option<String> {
    const MODIFIERS: [&str; 10] = [
        "pub", "const", "async", "unsafe", "extern", "export", "default", "static", "\"C\"",
        "override",
    ];
    let mut words = line.split_whitespace();
    let keyword = loop {
        let word = words.next()?;
        if !(MODIFIERS.contains(&word) || word.starts_with("pub(")) {
            break word;
        }
    };
    let rest = match keyword {
        "fn" | "def" | "function" | "function*" | "func" => words.collect::<Vec<_>>().join(" "),
        _ => return None,
    };
    let rest = if keyword == "func" && rest.starts_with('(') {
        rest.split_once(')')
            .map(|(_, r)| r.trim_start().to_string())?
    } else {
        rest
    };
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let name = &rest[..end];
    let valid = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.len() <= MAX_NAME_CHARS;
    valid.then(|| name.to_string())
}

/// The self type a Rust `impl` line names, verbatim as trusty-search keys it.
///
/// Why: #9196 critic finding 1: trusty-search keys a method
/// `<file>::<Type>::<name>` with `Type` the source text of the `impl`'s
/// `type` field (`rust_impl_type_name`, trusty-search
/// `core/chunker/classify.rs:221`), so `impl<'a> FilesCall<'a>` is
/// `FilesCall<'a>`; `FilesCall::new` is not found.
/// What: past `impl` and its own `<...>` parameters, the text after a
/// top-level ` for ` (a trait impl) or else the whole remainder, up to the
/// first top-level space or `{`. Generic arguments keep their text exactly,
/// spaces included. `None` unless the type is followed by `{`, `where` or
/// nothing, holds only path, generic, reference, slice, tuple and pointer
/// characters, has its brackets balanced, and is at most 200 characters, so
/// a rejected type leaves the method unqualified and no diff text reaches a
/// heading unchecked.
/// Test: `a_rust_method_takes_its_impl_type`, `an_odd_impl_type_is_not_used`.
fn impl_type(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("impl")?;
    if !rest.starts_with([' ', '<']) {
        return None;
    }
    let rest = skip_generics(rest.trim_start()).trim_start();
    let rest = top_level_for(rest).unwrap_or(rest);
    let mut depth = 0usize;
    let mut prev = ' ';
    let mut end = rest.len();
    for (i, c) in rest.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' if prev == '-' => {}
            '>' | ')' | ']' => depth = depth.checked_sub(1)?,
            '{' if depth == 0 => {
                end = i;
                break;
            }
            c if c.is_whitespace() && depth == 0 => {
                end = i;
                break;
            }
            c if c.is_ascii_alphanumeric() || " _:',&;*-".contains(c) => {}
            _ => return None,
        }
        prev = c;
    }
    let (ty, after) = (&rest[..end], rest[end..].trim_start());
    let tail_ok = after.is_empty() || after.starts_with('{') || after.starts_with("where");
    let valid = depth == 0
        && tail_ok
        && ty.starts_with(|c: char| {
            c.is_ascii_alphabetic() || c == '_' || c == '&' || c == '(' || c == '['
        })
        && ty.chars().count() <= MAX_NAME_CHARS;
    valid.then_some(ty)
}

/// The text after a ` for ` outside any brackets: a trait impl's self type.
fn top_level_for(text: &str) -> Option<&str> {
    let mut depth = 0usize;
    for (i, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            '{' if depth == 0 => return None,
            ' ' if depth == 0 && text[i..].starts_with(" for ") => {
                return Some(text[i + 5..].trim_start());
            }
            _ => {}
        }
    }
    None
}

/// `text` past a leading `<...>` generic list.
fn skip_generics(text: &str) -> &str {
    if !text.starts_with('<') {
        return text;
    }
    let mut depth = 0usize;
    for (i, c) in text.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return &text[i + 1..];
                }
            }
            _ => {}
        }
    }
    ""
}

#[cfg(test)]
#[path = "symbols_tests.rs"]
mod tests;
