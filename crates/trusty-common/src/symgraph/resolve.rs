//! Grounded name resolution for `SymbolGraph` (#6170; ports #6169's semantics).
//!
//! Why: `SymbolGraph` kept one global bare-name → node map, first write wins, so
//! a call to `write` became an edge to whichever `write` the registry happened
//! to insert first — a different file, and often a different crate. That is the
//! defect PR #6169 fixed in trusty-search's parallel `SymbolGraph`; this module
//! is the same contract for trusty-common's.
//! What: every definition is keyed `<file>::<symbol>`, and a callee becomes an
//! edge only when one scope around the CALLER yields exactly one candidate —
//! the caller's own file first, then each ancestor directory, then the whole
//! corpus. Ambiguity at the narrowest scope that matched resolves to nothing
//! rather than to a guess.
//! Test: `same_file_callee_wins_over_an_earlier_registered_twin`,
//! `bare_name_collision_across_crates_creates_no_edge`,
//! `corpus_unique_name_still_resolves_across_files`.

use std::collections::HashMap;
use std::path::Path;

use petgraph::stable_graph::NodeIndex;

use crate::symgraph::symbol::detect_language;

/// Qualified node identity: `<file>::<symbol>`.
pub(crate) fn qualified_key(file: &str, symbol: &str) -> String {
    format!("{file}::{symbol}")
}

/// Trailing identifier of a possibly-qualified symbol (`api::Foo::bar` → `bar`).
pub(crate) fn bare_name(symbol: &str) -> &str {
    symbol.rsplit("::").next().unwrap_or(symbol)
}

/// File extension, the fallback identity for a file no grammar recognises.
fn extension(file: &str) -> &str {
    let base = file.rsplit('/').next().unwrap_or(file);
    base.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("")
}

/// Language family of `file` — `.ts` and `.tsx` are both `typescript`.
///
/// Why: keeping a Rust call from binding to a TypeScript method needs a
/// language comparison, and comparing raw extensions is not one: `.ts` and
/// `.tsx` twins looked like different languages, so the filter dropped one and
/// left the other falsely unique. A heuristic that narrows must never narrow to
/// exactly one.
/// What: `symgraph::symbol::detect_language`'s tag — the same extension table
/// the parser uses — falling back to the raw extension when no grammar claims
/// the file.
/// Test: `sibling_extensions_of_one_language_stay_ambiguous`.
fn language_of(file: &str) -> &str {
    detect_language(Path::new(file))
        .map(|(_, tag)| tag)
        .unwrap_or_else(|| extension(file))
}

/// Does `file` end with `path` at a path-segment boundary?
fn path_suffix_matches(file: &str, path: &str) -> bool {
    file == path || (file.ends_with(path) && file[..file.len() - path.len()].ends_with('/'))
}

/// Scopes to try around `file`, narrowest first: the file itself, each ancestor
/// directory, then `""` — the whole corpus.
///
/// Why: #6169 widens file → directory → package → corpus. "Package" there is a
/// two-segment path guess, which is wrong for the absolute paths this crate's
/// registry carries. Walking every ancestor is the same ladder without the
/// guess: it stops at the nearest scope holding any candidate, so a directory
/// hit still beats a crate-wide one.
/// What: returns `[file, parent, grandparent, …, ""]`, or just `[""]` when the
/// caller's file is unknown — an unknown file grounds nothing but uniqueness.
/// Test: `directory_scope_beats_a_distant_twin`.
fn scopes(file: &str) -> Vec<&str> {
    if file.is_empty() {
        return vec![""];
    }
    let mut out = vec![file];
    let mut cur = file;
    while let Some((parent, _)) = cur.rsplit_once('/') {
        if parent.is_empty() {
            break;
        }
        out.push(parent);
        cur = parent;
    }
    out.push("");
    out
}

/// Is `candidate` inside `scope`? `""` is the whole corpus.
fn in_scope(candidate: &str, scope: &str) -> bool {
    if scope.is_empty() {
        return true;
    }
    candidate == scope
        || (candidate.starts_with(scope) && candidate[scope.len()..].starts_with('/'))
}

/// Everything the resolver needs about one candidate definition.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Candidate {
    pub idx: NodeIndex,
    /// `false` for containers (struct, trait, import) — not call targets.
    pub callable: bool,
}

/// How much evidence a resolution rests on, widest scope last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Grounds {
    /// The callee is defined in the caller's own file.
    SameFile,
    /// Exactly one definition inside an ancestor directory of the caller.
    SharedScope,
    /// Exactly one definition anywhere in the graph.
    CorpusUnique,
}

/// Per-graph lookup tables backing [`resolve_callee`] and [`rank_matches`].
#[derive(Debug, Default, Clone)]
pub(crate) struct NameIndex {
    /// `<file>::<symbol>` → the definition, or `None` when two symbols in one
    /// file share that exact key (a `fn write` beside an import named `write`)
    /// — ambiguous, so the exact-key shortcut declines rather than picking the
    /// last one inserted.
    pub by_key: HashMap<String, Option<Candidate>>,
    /// Full symbol AND trailing identifier → every definition under that name.
    pub by_name: HashMap<String, Vec<Candidate>>,
    /// Node → the file that defines it, for scope comparisons.
    pub file_of: HashMap<NodeIndex, String>,
}

impl NameIndex {
    /// Record one definition under every name it can be reached by.
    pub fn insert(&mut self, file: &str, symbol: &str, idx: NodeIndex, callable: bool) {
        self.file_of.insert(idx, file.to_string());

        let cand = Candidate { idx, callable };
        // #6170: a second symbol under the same exact key poisons the entry —
        // last write wins is how a call landed on an import.
        self.by_key
            .entry(qualified_key(file, symbol))
            .and_modify(|slot| {
                if slot.map(|c| c.idx) != Some(idx) {
                    *slot = None;
                }
            })
            .or_insert(Some(cand));
        self.by_name
            .entry(symbol.to_string())
            .or_default()
            .push(cand);
        let bare = bare_name(symbol);
        if bare != symbol {
            self.by_name.entry(bare.to_string()).or_default().push(cand);
        }
    }

    /// Every definition reachable by `name`, in registration order.
    pub fn candidates(&self, name: &str) -> &[Candidate] {
        self.by_name.get(name).map_or(&[], |v| v.as_slice())
    }

    fn file_of(&self, idx: NodeIndex) -> &str {
        self.file_of.get(&idx).map_or("", |f| f.as_str())
    }
}

/// Resolve a callee name to a node, or to nothing when no scope gives grounds.
///
/// Why: the map this replaces answered every `write` with the same node, so
/// `trace_execution_flow` reported callees in crates the caller cannot even see
/// (#6170). An edge now means the resolver had grounds for it.
/// What: an unambiguous, callable exact `<caller file>::<callee>` first, then
/// the narrowest scope around the caller that holds any candidate at all —
/// accepting it only when it holds exactly ONE. Widening from an ambiguous
/// scope can only add candidates, so an ambiguous scope ends the search.
/// Cross-file resolution additionally requires the same language family, and
/// `require_callable` drops containers for `Calls` edges.
/// Test: `bare_name_collision_across_crates_creates_no_edge`,
/// `cross_language_twin_is_not_an_edge`,
/// `an_exact_key_shared_by_two_symbols_is_not_grounds`.
pub(crate) fn resolve_callee(
    index: &NameIndex,
    caller_file: &str,
    callee: &str,
    require_callable: bool,
) -> Option<(NodeIndex, Grounds)> {
    // The shortcut answers only when the key names ONE definition and that
    // definition can actually be called; otherwise fall through to the scope
    // walk, which applies both rules itself.
    if let Some(Some(c)) = index.by_key.get(&qualified_key(caller_file, callee))
        && (!require_callable || c.callable)
    {
        return Some((c.idx, Grounds::SameFile));
    }

    // A dependency is raw call text (`Foo::bar`), a definition is registered
    // under its full id and its trailing identifier; try the written form
    // before falling back to the identifier.
    let mut named = index.candidates(callee);
    if named.is_empty() {
        named = index.candidates(bare_name(callee));
    }
    let caller_lang = language_of(caller_file);
    let cands: Vec<&Candidate> = named
        .iter()
        .filter(|c| !require_callable || c.callable)
        .filter(|c| caller_file.is_empty() || language_of(index.file_of(c.idx)) == caller_lang)
        .collect();
    if cands.is_empty() {
        return None;
    }

    for scope in scopes(caller_file) {
        let mut hit = None;
        let mut n = 0usize;
        for c in &cands {
            if in_scope(index.file_of(c.idx), scope) {
                n += 1;
                hit = Some(c.idx);
            }
        }
        if n > 1 {
            return None;
        }
        if n == 1 {
            let idx = hit?;
            let grounds = if index.file_of(idx) == caller_file && !caller_file.is_empty() {
                Grounds::SameFile
            } else if scope.is_empty() {
                Grounds::CorpusUnique
            } else {
                Grounds::SharedScope
            };
            return Some((idx, grounds));
        }
    }
    None
}

/// Rank every definition a caller-supplied name can mean, best first.
///
/// Why: a bare name that several definitions answer to must not silently anchor
/// to whichever was registered first — the caller has to be able to say which
/// one it took and what else matched (#6170).
/// What: the qualified key first, then `<path>::<symbol>` — matched against the
/// FILE each candidate was defined in, never by splitting the key, because a
/// registry key is `<file>::<module>::<name>` and splitting it at the last
/// `::` leaves the module path where the file should be. Then the name index,
/// falling back to the trailing identifier. Multi-hit results are ordered by
/// `rank` — the graph passes node degree, so the most-connected definition
/// leads.
/// Test: `path_qualified_name_anchors_on_the_file_it_names`,
/// `ambiguous_bare_name_reports_every_candidate`.
pub(crate) fn rank_matches(
    index: &NameIndex,
    name: &str,
    rank: impl Fn(NodeIndex) -> usize,
) -> Vec<NodeIndex> {
    if let Some(Some(c)) = index.by_key.get(name) {
        return vec![c.idx];
    }

    let mut hits: Vec<NodeIndex> = Vec::new();
    if let Some((path, symbol)) = name.rsplit_once("::")
        && (path.contains('/') || path.contains('.'))
    {
        hits = index
            .candidates(symbol)
            .iter()
            .filter(|c| path_suffix_matches(index.file_of(c.idx), path))
            .map(|c| c.idx)
            .collect();
    }
    if hits.is_empty() {
        hits = index.candidates(name).iter().map(|c| c.idx).collect();
    }
    if hits.is_empty() {
        hits = index
            .candidates(bare_name(name))
            .iter()
            .map(|c| c.idx)
            .collect();
    }
    hits.sort_by_key(|&i| (std::cmp::Reverse(rank(i)), i.index()));
    hits.dedup();
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exact_key_shared_by_two_symbols_is_not_grounds() {
        // Why: one file can hold `fn write` and an import named `write`. Both
        // key to `<file>::write`, and the shortcut used to return whichever was
        // inserted last, ignoring `require_callable` (#6170).
        // What: registers the import last, then asks for a callable `write`.
        // Asserts the function is what resolves, by way of the scope walk.
        let mut index = NameIndex::default();
        let func = NodeIndex::new(0);
        let import = NodeIndex::new(1);
        index.insert("src/io.rs", "write", func, true);
        index.insert("src/io.rs", "write", import, false);

        let (idx, grounds) =
            resolve_callee(&index, "src/io.rs", "write", true).expect("callable twin resolves");
        assert_eq!(idx, func);
        assert_eq!(grounds, Grounds::SameFile);

        // With no callable requirement the key names two symbols and neither is
        // grounds, so nothing resolves.
        assert!(resolve_callee(&index, "src/io.rs", "write", false).is_none());
    }

    #[test]
    fn sibling_extensions_of_one_language_stay_ambiguous() {
        // Why: `.ts` and `.tsx` are one language. Comparing raw extensions
        // dropped the `.tsx` twin and left the `.ts` one falsely unique.
        let mut index = NameIndex::default();
        index.insert("ui/lib/a.ts", "get", NodeIndex::new(0), true);
        index.insert("ui/widgets/b.tsx", "get", NodeIndex::new(1), true);
        assert!(resolve_callee(&index, "ui/app/main.ts", "get", true).is_none());

        // A Rust caller still sees neither of them.
        assert!(resolve_callee(&index, "crates/a/src/lib.rs", "get", true).is_none());
    }
}
