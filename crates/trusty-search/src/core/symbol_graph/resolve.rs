//! Symbol identity and grounded name resolution for `SymbolGraph` (#6167).
//!
//! Why: node identity used to be the bare `function_name`, first-write-wins, so
//! every `write` in the workspace collapsed onto one node and every call to
//! `write` became an edge to whichever crate registered first. On an 85k-chunk
//! index that produced 74% cross-crate callee edges — confident and wrong.
//! What: identity is now `<file>::<symbol>`, so every definition gets its own
//! node, and a callee only becomes an edge when the resolver has GROUNDS for it
//! — same file, then same directory, then same package, then a workspace-unique
//! name. Anything below that bar resolves to nothing and no edge is emitted.
//! Test: `bare_name_collision_does_not_create_a_cross_crate_edge`,
//! `each_definition_of_a_shared_name_gets_its_own_node`,
//! `unique_workspace_name_still_resolves_across_files`.

use std::collections::HashMap;

use petgraph::graph::NodeIndex;

/// Qualified node identity: `<file>::<symbol>`.
///
/// Why: `file` and `symbol` are individually ambiguous across a workspace;
/// their pair is not. Contributed nodes (ADR-0009) carry an extractor-minted
/// id instead and are keyed by that id verbatim.
/// What: joins the two with `::`, the same separator the symbol names already
/// use, so the key reads naturally in logs and in the persisted corpus.
/// Test: `qualified_key_round_trips_through_persistence`.
pub(crate) fn qualified_key(file: &str, symbol: &str) -> String {
    format!("{file}::{symbol}")
}

/// Trailing identifier of a possibly-qualified symbol (`Foo::bar` → `bar`).
pub(crate) fn bare_name(symbol: &str) -> &str {
    symbol.rsplit("::").next().unwrap_or(symbol)
}

/// Directory containing `file`, or `""` for a bare filename.
fn parent_dir(file: &str) -> &str {
    match file.rsplit_once('/') {
        Some((dir, _)) => dir,
        None => "",
    }
}

/// Best available stand-in for "the package this file belongs to".
///
/// Why: cross-file call resolution needs a scope narrower than the workspace,
/// and the graph is built from chunk paths alone — no manifest is in reach at
/// build time, so the package boundary has to be inferred from the path.
/// What: the first two path segments (`crates/trusty-search`, `packages/ui`,
/// `src/core`), falling back to the first segment for shallower paths. A
/// heuristic, deliberately: it only ever NARROWS which edges are allowed, so a
/// wrong guess drops a real edge rather than inventing a false one.
/// Test: `unique_workspace_name_still_resolves_across_files`.
fn package_root(file: &str) -> &str {
    let mut end = 0;
    for (seen, (idx, _)) in file.match_indices('/').enumerate() {
        end = idx;
        if seen == 1 {
            return &file[..end];
        }
    }
    if end == 0 {
        file
    } else {
        &file[..end]
    }
}

/// File extension, used to keep a Rust call from binding to a TypeScript method.
fn extension(file: &str) -> &str {
    let base = file.rsplit('/').next().unwrap_or(file);
    base.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("")
}

/// Everything the resolver needs about one candidate definition.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Candidate {
    pub idx: NodeIndex,
    /// `false` for containers (module, class, struct, impl) — not call targets.
    pub callable: bool,
}

/// Per-graph lookup tables backing [`resolve_callee`] and [`resolve_name`].
///
/// Why: resolution runs once per (caller, callee) pair — 850k times on the
/// dogfood index — so every step has to be a hash lookup or a walk over a
/// candidate list that is short in practice.
/// What: `by_key` is the qualified-identity map, `file_bare` answers "does this
/// file define exactly one thing whose trailing identifier is X", and `by_name`
/// holds every definition under both its full symbol and its trailing
/// identifier.
/// Test: exercised by every test in `resolve_tests`.
#[derive(Debug, Default, Clone)]
pub(crate) struct NameIndex {
    /// `<file>::<symbol>` → node. One entry per definition.
    pub by_key: HashMap<String, NodeIndex>,
    /// `<file>::<bare>` → the node, or `None` when that file defines several
    /// symbols sharing the trailing identifier (ambiguous — no grounds).
    pub file_bare: HashMap<String, Option<NodeIndex>>,
    /// Full symbol AND trailing identifier → every definition under that name.
    pub by_name: HashMap<String, Vec<Candidate>>,
    /// Node → the file that defines it, for scope comparisons.
    pub file_of: HashMap<NodeIndex, String>,
}

impl NameIndex {
    /// Record one definition under every name it can be reached by.
    pub fn insert(&mut self, file: &str, symbol: &str, idx: NodeIndex, callable: bool) {
        let key = qualified_key(file, symbol);
        self.by_key.insert(key, idx);
        self.file_of.insert(idx, file.to_string());

        let bare = bare_name(symbol);
        let fb = qualified_key(file, bare);
        // Second symbol in the same file with this trailing identifier makes
        // the file-scoped shortcut ambiguous, so poison the entry.
        self.file_bare
            .entry(fb)
            .and_modify(|slot| {
                if *slot != Some(idx) {
                    *slot = None;
                }
            })
            .or_insert(Some(idx));

        let cand = Candidate { idx, callable };
        self.by_name
            .entry(symbol.to_string())
            .or_default()
            .push(cand);
        if bare != symbol {
            self.by_name.entry(bare.to_string()).or_default().push(cand);
        }
    }

    /// Every definition reachable by `name`, in registration order.
    pub fn candidates(&self, name: &str) -> &[Candidate] {
        self.by_name.get(name).map_or(&[], |v| v.as_slice())
    }
}

/// How much evidence a callee resolution rests on, widest scope last.
///
/// Why: the consumer-visible contract is that an edge means the resolver had
/// grounds. Naming the grounds keeps that auditable rather than implicit.
/// What: ordered from strongest to weakest; `resolve_callee` returns the first
/// scope that yields exactly one candidate.
/// Test: `same_file_callee_still_resolves`,
/// `unique_workspace_name_still_resolves_across_files`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Grounds {
    /// The callee is defined in the caller's own file.
    SameFile,
    /// Exactly one definition in the caller's directory.
    SameDirectory,
    /// Exactly one definition in the caller's package.
    SamePackage,
    /// Exactly one definition anywhere in the corpus.
    WorkspaceUnique,
}

/// Resolve a callee name to a node, or to nothing when no scope gives grounds.
///
/// Why: the defect this replaces resolved any bare name to an arbitrary node,
/// so `self.index.write()` bound to whichever `write` registered first (#6167).
/// What: tries the caller's own file first (exact symbol, then trailing
/// identifier), then widens to directory, package, and finally the whole
/// corpus — accepting a scope only when it yields exactly ONE candidate.
/// Cross-file resolution additionally requires a matching file extension, so a
/// Rust call never binds to a `.ts` method. `require_callable` drops container
/// symbols (modules, structs) for `CallsFunction` edges.
/// Test: `bare_name_collision_does_not_create_a_cross_crate_edge`,
/// `cross_language_name_collision_is_not_an_edge`,
/// `unique_workspace_name_still_resolves_across_files`.
pub(crate) fn resolve_callee(
    index: &NameIndex,
    caller_file: &str,
    callee: &str,
    require_callable: bool,
) -> Option<(NodeIndex, Grounds)> {
    // 1. Same file, exact symbol.
    if let Some(&idx) = index.by_key.get(&qualified_key(caller_file, callee)) {
        return Some((idx, Grounds::SameFile));
    }
    // 2. Same file, trailing identifier (a call to `bar` reaching `Foo::bar`).
    if let Some(Some(idx)) = index
        .file_bare
        .get(&qualified_key(caller_file, bare_name(callee)))
    {
        return Some((*idx, Grounds::SameFile));
    }

    let cands: Vec<&Candidate> = index
        .candidates(callee)
        .iter()
        .filter(|c| !require_callable || c.callable)
        .filter(|c| {
            index
                .file_of
                .get(&c.idx)
                .is_some_and(|f| extension(f) == extension(caller_file))
        })
        .collect();
    if cands.is_empty() {
        return None;
    }

    // 3/4. Widen one scope at a time; accept only an unambiguous answer.
    for (scope, grounds) in [
        (parent_dir(caller_file), Grounds::SameDirectory),
        (package_root(caller_file), Grounds::SamePackage),
    ] {
        let mut hit = None;
        let mut n = 0usize;
        for c in &cands {
            let in_scope = index
                .file_of
                .get(&c.idx)
                .is_some_and(|f| f.starts_with(scope) && !scope.is_empty());
            if in_scope {
                n += 1;
                hit = Some(c.idx);
            }
        }
        if n == 1 {
            return hit.map(|i| (i, grounds));
        }
        if n > 1 {
            // Ambiguous at the narrowest scope that matched — widening can only
            // add more candidates, so stop rather than pick one.
            return None;
        }
    }

    // 5. Unique across the whole corpus is grounds in itself.
    if cands.len() == 1 {
        return Some((cands[0].idx, Grounds::WorkspaceUnique));
    }
    None
}

/// What a caller-supplied entry-point name resolved to.
///
/// Why: `get_call_chain` must never silently anchor to one of several
/// same-named definitions — that is how a trace lands in the wrong file 43% of
/// the time (#6167). Ambiguity is reported, not hidden.
/// What: `Unique` when one definition matched, `Ambiguous` when several did
/// (carrying every candidate so the caller can say so), `NotFound` otherwise.
/// Test: `bare_name_lookup_reports_every_candidate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NameResolution {
    Unique(NodeIndex),
    Ambiguous {
        chosen: NodeIndex,
        candidates: Vec<NodeIndex>,
    },
    NotFound,
}

/// Resolve a user-supplied symbol reference against the graph's name index.
///
/// Why: entry anchoring accepts three spellings — `Type::method`, a bare name,
/// and `<path>::<symbol>` — and the last one used to 404 outright (#6167).
/// What: tries the qualified key first (which is exactly the `<path>::<symbol>`
/// form), then a path-suffix match so a partial path anchors, then the name
/// index. `rank` orders candidates when several match; the caller supplies node
/// degree so the most-connected definition is chosen and the rest reported.
/// Test: `path_qualified_entry_point_anchors_instead_of_404`.
pub(crate) fn resolve_name(
    index: &NameIndex,
    name: &str,
    rank: impl Fn(NodeIndex) -> usize,
) -> NameResolution {
    if let Some(&idx) = index.by_key.get(name) {
        return NameResolution::Unique(idx);
    }
    // `<path>::<symbol>` where the path is a suffix of the real file path.
    if let Some((path, symbol)) = name.rsplit_once("::") {
        if path.contains('/') || path.contains('.') {
            let mut hits: Vec<NodeIndex> = index
                .by_key
                .iter()
                .filter(|(k, _)| {
                    k.rsplit_once("::").is_some_and(|(f, s)| {
                        (s == symbol || bare_name(s) == symbol) && f.ends_with(path)
                    })
                })
                .map(|(_, &i)| i)
                .collect();
            if !hits.is_empty() {
                hits.sort_by_key(|&i| (std::cmp::Reverse(rank(i)), i.index()));
                return finish(hits);
            }
        }
    }
    let mut hits: Vec<NodeIndex> = index.candidates(name).iter().map(|c| c.idx).collect();
    if hits.is_empty() {
        // Last resort: the case-insensitive substring match this surface has
        // always offered. Ranked by degree like every other multi-hit path, so
        // a fuzzy anchor is reported as ambiguous rather than silently chosen.
        let needle = name.to_ascii_lowercase();
        hits = index
            .by_name
            .iter()
            .filter(|(n, _)| n.to_ascii_lowercase().contains(&needle))
            .flat_map(|(_, cands)| cands.iter().map(|c| c.idx))
            .collect();
    }
    hits.sort_by_key(|&i| (std::cmp::Reverse(rank(i)), i.index()));
    hits.dedup();
    finish(hits)
}

fn finish(hits: Vec<NodeIndex>) -> NameResolution {
    match hits.len() {
        0 => NameResolution::NotFound,
        1 => NameResolution::Unique(hits[0]),
        _ => NameResolution::Ambiguous {
            chosen: hits[0],
            candidates: hits,
        },
    }
}
