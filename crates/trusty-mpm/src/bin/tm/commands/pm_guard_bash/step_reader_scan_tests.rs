//! Source-scan guard: every rule that reads the shell-group walk fails closed
//! on a command the walk cannot place (#9127).
//!
//! Why: `shell_groups::grouped_steps` hands back steps even for a command it
//! marks `parsed: false`. Critic round 4 (HIGH) found four rules — the HEAD
//! switch, the linked-worktree HEAD move, the worktree removal and the
//! main-checkout HEAD move — reading those steps without checking `parsed`, so
//! `command -p eval "cd <wt>"; git …` was placed as if its `cd` ran. A new
//! step-reading rule is one call away from the same hole; this scan makes a
//! forgotten refusal a CI failure instead of a review finding.
//! What: each production `.rs` file in `pm_guard_bash/` is lexed by the lexer
//! the #9121 scan uses — comments, literals and `#[cfg(test)]` modules out —
//! and cut into `fn` bodies. A body naming a walker entry ([`STEP_SOURCES`])
//! as an identifier, however the call is spaced or wrapped, makes that
//! function a step reader. Every path from a rule entry point down to a reader
//! must pass a function naming a refusal ([`REFUSALS`]). An entry point is a
//! function no scanned body names, or one visible outside `pm_guard_bash`,
//! whose callers there go unscanned; a call cycle counts as unrefused. A walker
//! entry named outside any function (an `as` alias, a stored pointer) fails
//! outright. [`EXEMPT`] lists the walker and the refusal themselves; an entry
//! that no longer reads steps fails too, so the list only shrinks. The walker
//! is `pub(super)` in `shell_groups`, so no file outside this directory can
//! call it.
//! Test: `every_step_reader_fails_closed_on_an_unplaced_command`,
//! `the_step_scan_flags_an_unrefused_reader`.

use std::collections::HashSet;
use std::ops::Range;
use std::path::Path;

#[path = "../../../../source_scan_lex.rs"]
mod lex;
use lex::{Lexed, code_only, is_ident, lex, matching, skip_ws};

/// The walker's entry points: `grouped_steps`, and `steps_into`, the private
/// recursion it runs.
const STEP_SOURCES: &[&str] = &["grouped_steps", "steps_into"];

/// A body naming one of these refuses an unplaced command: the shared refusal,
/// and the constant the commit rule's older equivalent denies with.
const REFUSALS: &[&str] = &["unplaced_git_verb_reason", "UNPARSED_GROUP_COMMIT_REASON"];

/// `(file, function, reason)` — step readers that need no refusal.
const EXEMPT: &[(&str, &str, &str)] = &[
    (
        "shell_groups.rs",
        "grouped_steps",
        "the walker itself; it runs `steps_into`",
    ),
    (
        "shell_groups.rs",
        "unplaced_git_verb_reason",
        "the refusal itself; it reads `parsed` to decide",
    ),
];

/// One `fn` item in production code.
struct Function {
    file: String,
    name: String,
    /// Visible outside `pm_guard_bash`, where callers go unscanned.
    exported: bool,
    /// Every identifier its body names.
    idents: HashSet<String>,
}

/// What one scan found.
#[derive(Debug, Default)]
struct Scan {
    /// `file::function` of every step reader, exempt ones included.
    readers: Vec<String>,
    /// One line per unrefused path or out-of-function walker name.
    findings: Vec<String>,
}

/// Whether the code at `at` is the whole identifier `word`.
fn word_at(src: &[Lexed], at: usize, word: &str) -> bool {
    let len = word.chars().count();
    let code_ident = |i: usize| src.get(i).is_some_and(|&(c, code)| code && is_ident(c));
    at + len <= src.len()
        && src[at..at + len]
            .iter()
            .zip(word.chars())
            .all(|(&(c, code), w)| code && c == w)
        && !(at > 0 && code_ident(at - 1))
        && !code_ident(at + len)
}

/// The `{` opening the body of the `fn` whose signature starts at `from`, or
/// `None` for a body-less declaration.
fn body_open(src: &[Lexed], from: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &(c, code)) in src.iter().enumerate().skip(from) {
        match (c, code) {
            ('(' | '[', true) => depth += 1,
            (')' | ']', true) => depth = depth.saturating_sub(1),
            ('{', true) if depth == 0 => return Some(i),
            (';', true) if depth == 0 => return None,
            _ => {}
        }
    }
    None
}

/// Whether the qualifiers before a `fn` make it visible outside
/// `pm_guard_bash`: `pub`, `pub(crate)`, `pub(in …)`, or `pub(super)` in
/// `mod.rs`.
fn exported(before: &[Lexed], rel: &str) -> bool {
    let text: String = before
        .iter()
        .map(|&(c, code)| if code { c } else { ' ' })
        .collect();
    let ends_with_word = |s: &str, w: &str| {
        s.strip_suffix(w)
            .is_some_and(|rest| !rest.ends_with(is_ident))
    };
    let mut rest = text.trim_end();
    while let Some(q) = ["async", "const", "unsafe", "extern"]
        .into_iter()
        .find(|q| ends_with_word(rest, q))
    {
        rest = rest[..rest.len() - q.len()].trim_end();
    }
    if ends_with_word(rest, "pub") {
        return true;
    }
    let Some(inner) = rest.strip_suffix(')') else {
        return false;
    };
    let Some(open) = inner.rfind('(') else {
        return false;
    };
    ends_with_word(inner[..open].trim_end(), "pub")
        && match inner[open + 1..].trim() {
            "self" => false,
            "super" => rel == "mod.rs",
            _ => true,
        }
}

/// Every identifier in `src`.
fn idents(src: &[Lexed]) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut word = String::new();
    for &(c, code) in src {
        if code && is_ident(c) {
            word.push(c);
        } else if !word.is_empty() {
            out.insert(std::mem::take(&mut word));
        }
    }
    if !word.is_empty() {
        out.insert(word);
    }
    out
}

/// One file's functions, and a finding for each walker entry it names outside
/// a function body other than at its own definition.
fn scan_file(rel: &str, text: &str) -> (Vec<Function>, Vec<String>) {
    let src = lex(&code_only(text));
    let mut functions = Vec::new();
    let mut bodies = Vec::new();
    for at in 0..src.len() {
        if !word_at(&src, at, "fn") {
            continue;
        }
        let name_at = skip_ws(&src, at + 2);
        let name: String = src[name_at..]
            .iter()
            .take_while(|&&(c, code)| code && is_ident(c))
            .map(|&(c, _)| c)
            .collect();
        if name_at == at + 2 || name.is_empty() {
            continue; // `fn(…)`, a pointer type
        }
        let Some(open) = body_open(&src, name_at + name.chars().count()) else {
            continue;
        };
        let Some(close) = matching(&src, open, '{', '}') else {
            continue;
        };
        functions.push(Function {
            file: rel.to_string(),
            exported: exported(&src[at.saturating_sub(200)..at], rel),
            idents: idents(&src[open..close]),
            name,
        });
        bodies.push(open..close);
    }
    let loose = loose_names(rel, &src, &bodies);
    (functions, loose)
}

/// A finding for each walker entry `src` names outside every function body,
/// other than at its definition or in a `use` that keeps its name.
///
/// Why (#9127): `use … grouped_steps as walk;`, or a `const` holding it, hides
/// every reader behind a name this scan does not follow.
fn loose_names(rel: &str, src: &[Lexed], bodies: &[Range<usize>]) -> Vec<String> {
    let in_body = |at: usize| bodies.iter().any(|body| body.contains(&at));
    let uses: Vec<Range<usize>> = (0..src.len())
        .filter(|&at| word_at(src, at, "use") && !in_body(at))
        .filter_map(|at| {
            let end = src[at..].iter().position(|&(c, code)| code && c == ';')?;
            Some(at..at + end)
        })
        .collect();
    STEP_SOURCES
        .iter()
        .filter(|source| {
            (0..src.len()).any(|at| {
                let imported = || {
                    uses.iter().any(|item| item.contains(&at))
                        && !word_at(src, skip_ws(src, at + source.len()), "as")
                };
                word_at(src, at, source)
                    && !in_body(at)
                    && !word_at(src, skip_ws_back(src, at), "fn")
                    && !imported()
            })
        })
        .map(|source| {
            format!(
                "{rel}: `{source}` named outside a function body, where an alias or a \
                 stored pointer hides its readers"
            )
        })
        .collect()
}

/// The start of the word ending just before the whitespace preceding `at`.
fn skip_ws_back(src: &[Lexed], at: usize) -> usize {
    let mut end = at;
    while end > 0 && src[end - 1].1 && src[end - 1].0.is_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && src[start - 1].1 && is_ident(src[start - 1].0) {
        start -= 1;
    }
    start
}

/// A path from an entry point down to `at` on which no function refuses,
/// entry point first; `None` when every path refuses.
fn unrefused_path(
    at: usize,
    all: &[Function],
    callers: &[Vec<usize>],
    visiting: &mut Vec<usize>,
) -> Option<Vec<usize>> {
    let function = &all[at];
    if REFUSALS.iter().any(|r| function.idents.contains(*r)) {
        return None;
    }
    if visiting.contains(&at) || function.exported || callers[at].is_empty() {
        return Some(vec![at]);
    }
    visiting.push(at);
    let found = callers[at]
        .iter()
        .find_map(|&caller| unrefused_path(caller, all, callers, visiting));
    visiting.pop();
    found.map(|mut path| {
        path.push(at);
        path
    })
}

/// Every step reader in `sources` (`(file, text)` pairs), and every path to
/// one that no refusal guards.
fn scan(sources: &[(String, String)]) -> Scan {
    let mut out = Scan::default();
    let mut all = Vec::new();
    for (rel, text) in sources {
        let (functions, loose) = scan_file(rel, text);
        all.extend(functions);
        out.findings.extend(loose);
    }
    // A caller is any other function whose body names this one; a same-named
    // function elsewhere is its own recursion, not a caller.
    let callers: Vec<Vec<usize>> = all
        .iter()
        .map(|callee| {
            (0..all.len())
                .filter(|&j| all[j].name != callee.name && all[j].idents.contains(&callee.name))
                .collect()
        })
        .collect();
    let label = |f: &Function| format!("{}::{}", f.file, f.name);
    for (at, function) in all.iter().enumerate() {
        let reads = STEP_SOURCES
            .iter()
            .any(|s| *s != function.name && function.idents.contains(*s));
        if !reads {
            continue;
        }
        out.readers.push(label(function));
        if EXEMPT
            .iter()
            .any(|(file, name, _)| *file == function.file && *name == function.name)
        {
            continue;
        }
        if let Some(path) = unrefused_path(at, &all, &callers, &mut Vec::new()) {
            let path: Vec<String> = path.iter().map(|&k| label(&all[k])).collect();
            out.findings.push(path.join(" -> "));
        }
    }
    out
}

/// Every production `.rs` file directly in `dir`, as `(file name, text)`.
fn production_sources(dir: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.expect("read a directory entry").path())
        .filter(|path| path.extension().is_some_and(|e| e == "rs"))
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            let test =
                name == "tests.rs" || name.ends_with("_tests.rs") || name.ends_with("_test.rs");
            (!test).then(|| {
                let text = std::fs::read_to_string(&path).expect("read source");
                (name, text)
            })
        })
        .collect();
    out.sort();
    out
}

/// Why/What: see the module docs.
/// Test: this test.
#[test]
fn every_step_reader_fails_closed_on_an_unplaced_command() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/tm/commands/pm_guard_bash");
    let sources = production_sources(&dir);
    assert!(
        sources.len() > 40,
        "the scan found only {} files",
        sources.len()
    );
    let scan = scan(&sources);
    assert!(
        scan.findings.is_empty(),
        "#9127: a rule reads the shell-group walk without refusing a command it cannot \
         place. Call `shell_groups::unplaced_git_verb_reason(command, <verb predicate>)` and \
         return its refusal before reading the steps, on every path below (entry point \
         first):\n  {}",
        scan.findings.join("\n  ")
    );
    let stale: Vec<String> = EXEMPT
        .iter()
        .map(|(file, name, _)| format!("{file}::{name}"))
        .filter(|entry| !scan.readers.contains(entry))
        .collect();
    assert!(
        stale.is_empty(),
        "these EXEMPT entries no longer read steps; delete them: {stale:?}"
    );
}

/// Why: a scan that cannot fail proves nothing. Each fixture pins one rule
/// of the scan: an unrefused reader, however spaced, is flagged; a refusal in
/// the reader or in every caller clears it; an exported or cyclic reader
/// cannot lean on its scanned callers; comments, literals and test modules
/// name nothing; an alias is flagged where it is declared.
/// Test: this test.
#[test]
fn the_step_scan_flags_an_unrefused_reader() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "an unrefused reader",
            "fn rule(c: &str) { for s in grouped_steps(c).steps {} }",
            &["x.rs::rule"],
        ),
        (
            "a spaced, wrapped call",
            "fn rule(c: &str) { let g = shell_groups::grouped_steps\n    (c); }",
            &["x.rs::rule"],
        ),
        (
            "a refusal in the reader",
            "fn rule(c: &str) { if unplaced_git_verb_reason(c, p).is_some() { return; } \
             grouped_steps(c); }",
            &[],
        ),
        (
            "the commit rule's equivalent",
            "fn rule(c: &str) -> &str { if !grouped_steps(c).parsed { \
             UNPARSED_GROUP_COMMIT_REASON } else { \"\" } }",
            &[],
        ),
        (
            "a refusal in the only caller",
            "fn walk(c: &str) { grouped_steps(c); }\n\
             fn rule(c: &str) { if unplaced_git_verb_reason(c, p).is_some() { return; } \
             walk(c); }",
            &[],
        ),
        (
            "a second caller without one",
            "fn walk(c: &str) { grouped_steps(c); }\n\
             fn rule(c: &str) { unplaced_git_verb_reason(c, p); walk(c); }\n\
             fn other(c: &str) { walk(c); }",
            &["x.rs::other -> x.rs::walk"],
        ),
        (
            "an exported reader",
            "pub(crate) fn walk(c: &str) { grouped_steps(c); }\n\
             fn rule(c: &str) { unplaced_git_verb_reason(c, p); walk(c); }",
            &["x.rs::walk"],
        ),
        (
            "a call cycle",
            "fn a(c: &str) { b(c); grouped_steps(c); }\nfn b(c: &str) { a(c); }",
            &["x.rs::a -> x.rs::b -> x.rs::a"],
        ),
        (
            "comments, literals and a test module",
            "// grouped_steps(c)\nfn f() { let s = \"grouped_steps(c)\"; /* grouped_steps */ }\n\
             #[cfg(test)]\nmod tests { fn t() { grouped_steps(\"x\"); } }",
            &[],
        ),
        (
            "a plain import",
            "use super::shell_groups::{Step, grouped_steps};\n\
             fn rule(c: &str) { unplaced_git_verb_reason(c, p); grouped_steps(c); }",
            &[],
        ),
        (
            "an aliased import",
            "use super::shell_groups::grouped_steps as walk;\nfn rule(c: &str) { walk(c); }",
            &[
                "x.rs: `grouped_steps` named outside a function body, where an alias or a \
               stored pointer hides its readers",
            ],
        ),
    ];
    let wrong: Vec<String> = cases
        .iter()
        .filter_map(|(label, fixture, want)| {
            let got = scan(&[("x.rs".to_string(), (*fixture).to_string())]).findings;
            (got != *want).then(|| format!("{label}: got {got:?}"))
        })
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
}
