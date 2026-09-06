//! The chunk-id grammar: the one place that builds, validates, and parses a
//! `RawChunk::id`.
//!
//! Why (#6581): the grammar used to be re-implemented at three sites that had
//! to agree and did not. `chunker::walk::make_chunk_id` built it,
//! `store::path_match::is_chunk_id_suffix` validated a suffix against a
//! hard-coded segment count, and `service::call_chain::location_from_chunk_id`
//! parsed it back with an `rsplitn(3, ':')` that mis-read the named shape. A
//! chunk id is the primary key in redb, the HNSW sidecar, the KG tables, and
//! the BM25 document map, so a builder and a parser that disagree corrupt the
//! corpus rather than just returning a wrong string.
//!
//! What: [`make`] is the only builder; [`parse`] and [`parse_suffix`] are the
//! only parsers. Two base shapes, plus two optional tails:
//!
//! | Shape | Grammar |
//! |---|---|
//! | Positional (no name) | `{file}:{start}:{end}` |
//! | Named | `{file}::{type}::{name}::{start}::{end}` |
//! | Legacy named (pre-#6581, read-only) | `{file}::{type}::{name}::{start}` |
//! | Duplicate-span tail | `{base}::dup::{n}` |
//! | Sub-chunk tail | `{base}::sub::{n}` |
//!
//! `end_line` is necessary but NOT sufficient on its own: a minified bundle is
//! one physical line, so every declaration in it has `start == end == 1` and two
//! `function e(…)` still land on one id. The `::dup::{n}` tail
//! `chunker::ast::disambiguate_chunk_ids` appends closes that last gap, and the
//! owner ruling of 2026-09-05 scopes it to exactly that case: an identical span
//! (same file, type, name, start AND end).
//!
//! Test: `core::chunk_id::tests` — every constructor/parser pair round-trips,
//! and `roundtrip_covers_every_shape_make_can_emit` ties [`parse`] and
//! [`parse_suffix`] to [`make`] so the sites can never drift apart again.

/// Marker for the identical-span disambiguator (#6581).
const DUP_MARKER: &str = "::dup::";
/// Marker segment introduced by `chunker::walk::split_oversized`.
const SUB_MARKER: &str = "::sub::";

/// Which named shapes a validator accepts for one index (#6581).
///
/// Why: the owner ruling of 2026-09-05 makes legacy acceptance per-index and
/// temporary — an index that has not run M005 still holds pre-#6581 named ids
/// and must match them, and one that has run it holds none, so accepting the
/// wider grammar there only widens the `#3401` lookalike-path surface for no
/// recall. A global constant cannot express "this index but not that one".
/// What: a `Copy` two-state policy threaded from `CodeIndexer::chunk_id_shapes`
/// into [`is_valid_suffix`]. `NewOnly` is the post-M005 state.
/// Test: `suffix_policy_gates_the_legacy_shape`; per-index behaviour in
/// `core::migration::m005::tests::the_suffix_policy_narrows_per_index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkIdShapes {
    /// Only the shapes [`make`] emits today. The state after M005.
    NewOnly,
    /// Also the pre-#6581 named shape, for an index that has not run M005.
    NewAndLegacy,
}

/// What a chunk id turned out to be.
///
/// Why: three consumers need three different answers from the same parse — the
/// call-chain renderer wants `file` and `start_line`, M005 wants to know whether
/// a corpus still holds pre-#6581 named ids, and `path_match` only wants a
/// yes/no. One enum serves all three without a second parser.
/// What: `Positional` and `Named` are what [`make`] emits today; `LegacyNamed`
/// is the pre-#6581 named shape, which only ever arrives from a corpus written
/// by an older binary or from an MCP client replaying a stale id.
/// Test: `parses_each_shape`, `legacy_named_is_distinguishable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkIdShape {
    /// `{file}:{start}:{end}` — emitted when the chunk has no name.
    Positional { start_line: usize, end_line: usize },
    /// `{file}::{type}::{name}::{start}::{end}` — the current named shape.
    Named {
        chunk_type: String,
        name: String,
        start_line: usize,
        end_line: usize,
    },
    /// `{file}::{type}::{name}::{start}` — the pre-#6581 named shape.
    LegacyNamed {
        chunk_type: String,
        name: String,
        start_line: usize,
    },
}

/// A parsed chunk id.
///
/// Why: `file` and the two tails are orthogonal to the base shape, so they sit
/// beside it rather than being repeated in every variant.
/// What: `file` is the head before the shape's first delimiter; `dup_index` and
/// `sub_index` are `Some(n)` when the id carried the corresponding tail.
/// Test: `parses_each_shape`, `parses_both_tails`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedChunkId {
    pub file: String,
    pub shape: ChunkIdShape,
    pub dup_index: Option<usize>,
    pub sub_index: Option<usize>,
}

impl ParsedChunkId {
    /// The 1-based source line this chunk starts at.
    pub fn start_line(&self) -> usize {
        match &self.shape {
            ChunkIdShape::Positional { start_line, .. }
            | ChunkIdShape::Named { start_line, .. }
            | ChunkIdShape::LegacyNamed { start_line, .. } => *start_line,
        }
    }

    /// `true` when this id still carries the pre-#6581 named shape.
    pub fn is_legacy_named(&self) -> bool {
        matches!(self.shape, ChunkIdShape::LegacyNamed { .. })
    }
}

/// Build a chunk id. The only builder — every id in the corpus comes from here.
///
/// Why (#6581): a named id used to omit `end_line`, so two declarations sharing
/// a name and a start line produced one id. A minified bundle puts every
/// declaration on line 1, so `assets/index.js` collapsed 2,299 declarations onto
/// 225 ids and the #6571 dedupe dropped the surplus — those symbols were simply
/// absent from search.
/// What: an empty `name` yields the positional shape (`{file}:{start}:{end}`);
/// otherwise the named shape carries both line numbers. Line numbers are
/// `usize`, so both tails are always a plain `\d+` run.
/// Test: `make_named_carries_end_line`, `make_unnamed_is_positional`,
/// `roundtrip_covers_every_shape_make_can_emit`.
pub fn make(
    file: &str,
    chunk_type: &str,
    name: &str,
    start_line: usize,
    end_line: usize,
) -> String {
    if name.is_empty() {
        format!("{file}:{start_line}:{end_line}")
    } else {
        format!("{file}::{chunk_type}::{name}::{start_line}::{end_line}")
    }
}

/// Append the identical-span disambiguator for the `n`-th repeat of a base id.
///
/// Why (#6581): line numbers cannot separate two declarations that begin AND end
/// on the same physical line, which is every declaration in a minified bundle.
/// The owner ruling scopes this tail to exactly that case.
/// What: `{base}::dup::{n}`, with `n` counting repeats after the first (so the
/// first occurrence keeps the bare base id and an unminified corpus never sees
/// this tail at all).
/// Test: `parses_both_tails`, and
/// `named_chunks_sharing_a_start_line_get_distinct_ids` in `chunker::tests`.
pub fn make_dup(base_id: &str, dup_index: usize) -> String {
    format!("{base_id}{DUP_MARKER}{dup_index}")
}

/// Append the sub-chunk tail `chunker::walk::split_oversized` uses.
pub fn make_sub(base_id: &str, sub_index: usize) -> String {
    format!("{base_id}{SUB_MARKER}{sub_index}")
}

/// Split a trailing `{marker}{digits}` off an id, if it has one.
fn split_tail<'a>(chunk_id: &'a str, marker: &str) -> (&'a str, Option<usize>) {
    let Some((base, idx)) = chunk_id.rsplit_once(marker) else {
        return (chunk_id, None);
    };
    if !all_digits(idx) {
        return (chunk_id, None);
    }
    match idx.parse::<usize>() {
        Ok(n) => (base, Some(n)),
        Err(_) => (chunk_id, None),
    }
}

/// Strip both optional tails, innermost last.
///
/// Why: a sub-chunk id embeds its parent's id verbatim and the disambiguator is
/// applied before splitting, so the tails always appear in the order
/// `…::dup::{n}::sub::{m}`. Every parser has to peel them in that order before
/// it can look at the base.
/// What: returns `(base, dup_index, sub_index)`.
/// Test: `parses_both_tails`, `tail_requires_digits`.
pub fn split_tails(chunk_id: &str) -> (&str, Option<usize>, Option<usize>) {
    let (after_sub, sub) = split_tail(chunk_id, SUB_MARKER);
    let (base, dup) = split_tail(after_sub, DUP_MARKER);
    (base, dup, sub)
}

/// Parse a whole chunk id.
///
/// Why: `service::call_chain` renders `file:line` from a graph-supplied id, and
/// M005 has to tell a pre-#6581 named id from a current one. Both need the same
/// parse, and the previous hand-rolled `rsplitn(3, ':')` returned
/// `"assets/index.js::Function::e:"` for a named id.
/// What: strips the tails, then tries the named shape, then the legacy named
/// shape, then the positional shape, in that order. `file` is the head up to the
/// first `::` (named shapes) or the head left by splitting the two trailing
/// `:{digits}` groups (positional). A file path containing `::` is not
/// representable in this grammar and never has been.
/// Test: `parses_each_shape`, `legacy_named_is_distinguishable`,
/// `named_wins_over_legacy_when_both_would_match`.
pub fn parse(chunk_id: &str) -> Option<ParsedChunkId> {
    let (base, dup_index, sub_index) = split_tails(chunk_id);
    match parse_base(base) {
        Some((file, shape)) => Some(ParsedChunkId {
            file,
            shape,
            dup_index,
            sub_index,
        }),
        // A base that will not parse means a tail split was a false positive
        // (a chunk literally named `sub` or `dup`); retry on the whole id.
        None if dup_index.is_some() || sub_index.is_some() => {
            let (file, shape) = parse_base(chunk_id)?;
            Some(ParsedChunkId {
                file,
                shape,
                dup_index: None,
                sub_index: None,
            })
        }
        None => None,
    }
}

/// Parse an id that has already had its tails stripped.
fn parse_base(base: &str) -> Option<(String, ChunkIdShape)> {
    if let Some((file, rest)) = base.split_once("::") {
        if file.is_empty() {
            return None;
        }
        return Some((file.to_string(), parse_named_body(rest)?));
    }
    // Positional: `{file}:{start}:{end}`.
    let (head, end) = base.rsplit_once(':')?;
    let (file, start) = head.rsplit_once(':')?;
    if !all_digits(start) || !all_digits(end) || file.is_empty() {
        return None;
    }
    Some((
        file.to_string(),
        ChunkIdShape::Positional {
            start_line: start.parse().ok()?,
            end_line: end.parse().ok()?,
        },
    ))
}

/// Parse the body of a named id — everything after `{file}::`.
///
/// Why: `chunker::walk`'s method qualifier puts `Type::method` in `name`, so
/// `name` is NOT colon-free and cannot be recovered by splitting on `::` alone.
/// The line numbers at the tail are digit runs, and `chunk_type` at the head is
/// a `ChunkType::as_str()` value with no colons, so both ends are anchored and
/// everything between them is the name.
/// What: takes `chunk_type` from the left, `end`/`start` from the right, and
/// treats the remainder as `name`. Prefers the current shape over the legacy one
/// whenever both would match, so a migrated id is never mistaken for a stale one.
/// Test: `named_with_qualified_method_name`,
/// `named_wins_over_legacy_when_both_would_match`.
fn parse_named_body(body: &str) -> Option<ChunkIdShape> {
    let (chunk_type, rest) = body.split_once("::")?;
    if chunk_type.is_empty() || chunk_type.contains(':') {
        return None;
    }
    // Current shape: `{name}::{start}::{end}`.
    if let Some((head, end)) = rest.rsplit_once("::") {
        if let Some((name, start)) = head.rsplit_once("::") {
            if all_digits(start) && all_digits(end) && !name.is_empty() {
                return Some(ChunkIdShape::Named {
                    chunk_type: chunk_type.to_string(),
                    name: name.to_string(),
                    start_line: start.parse().ok()?,
                    end_line: end.parse().ok()?,
                });
            }
        }
    }
    // Legacy shape: `{name}::{start}`.
    let (name, start) = rest.rsplit_once("::")?;
    if !all_digits(start) || name.is_empty() {
        return None;
    }
    Some(ChunkIdShape::LegacyNamed {
        chunk_type: chunk_type.to_string(),
        name: name.to_string(),
        start_line: start.parse().ok()?,
    })
}

/// `true` when `suffix` — everything in a chunk id after a matched file-path
/// prefix — is exactly one of the shapes `shapes` admits.
///
/// Why (#3401): `store::path_match` uses this to decide whether a `path_prefix`
/// landed on a real chunk-id boundary or in the middle of a coincidentally-named
/// sibling file. Accepting "any `:`" let a real directory `vendor/foo:evil/...`
/// match `path_prefix: "vendor/foo"`. Why it takes `shapes` (#6581): an index
/// that has not run M005 still holds pre-#6581 named ids and must match them,
/// and one that has run it holds none — see [`ChunkIdShapes`].
/// What: validates the WHOLE remainder — `:{start}:{end}`, or `::` followed by a
/// named body, each optionally carrying the `::dup::` / `::sub::` tails. Under
/// [`ChunkIdShapes::NewOnly`] a suffix that only parses as the legacy named
/// shape is rejected.
/// Test: `suffix_accepts_every_shape`, `suffix_rejects_a_bare_colon_path`,
/// `suffix_policy_gates_the_legacy_shape`.
pub fn is_valid_suffix(suffix: &str, shapes: ChunkIdShapes) -> bool {
    match parse_suffix(suffix) {
        Some(ChunkIdShape::LegacyNamed { .. }) => shapes == ChunkIdShapes::NewAndLegacy,
        Some(_) => true,
        None => false,
    }
}

/// Parse a chunk-id suffix (the file prefix already removed).
///
/// Test: `suffix_accepts_every_shape`, `suffix_rejects_a_bare_colon_path`.
pub fn parse_suffix(suffix: &str) -> Option<ChunkIdShape> {
    let (base, _dup, _sub) = split_tails(suffix);
    let attempt = |s: &str| -> Option<ChunkIdShape> {
        if let Some(body) = s.strip_prefix("::") {
            return parse_named_body(body);
        }
        let rest = s.strip_prefix(':')?;
        let (start, end) = rest.split_once(':')?;
        if !all_digits(start) || !all_digits(end) {
            return None;
        }
        Some(ChunkIdShape::Positional {
            start_line: start.parse().ok()?,
            end_line: end.parse().ok()?,
        })
    };
    attempt(base).or_else(|| attempt(suffix))
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_named_carries_end_line() {
        // #6581: the end line is what separates two declarations on one line.
        assert_eq!(
            make("assets/index.js", "Function", "e", 1, 40),
            "assets/index.js::Function::e::1::40"
        );
    }

    #[test]
    fn make_unnamed_is_positional() {
        assert_eq!(make("src/lib.rs", "Code", "", 10, 40), "src/lib.rs:10:40");
    }

    #[test]
    fn parses_each_shape() {
        let p = parse("src/lib.rs:10:40").expect("positional");
        assert_eq!(p.file, "src/lib.rs");
        assert_eq!(p.start_line(), 10);
        assert_eq!(
            p.shape,
            ChunkIdShape::Positional {
                start_line: 10,
                end_line: 40
            }
        );

        let n = parse("src/auth.rs::Function::authenticate::42::78").expect("named");
        assert_eq!(n.file, "src/auth.rs");
        assert_eq!(n.start_line(), 42);
        assert!(!n.is_legacy_named());
    }

    #[test]
    fn legacy_named_is_distinguishable() {
        let l = parse("assets/index.js::Function::e::1").expect("legacy");
        assert_eq!(l.file, "assets/index.js");
        assert_eq!(l.start_line(), 1);
        assert!(l.is_legacy_named());
    }

    #[test]
    fn named_wins_over_legacy_when_both_would_match() {
        // `::Function::e::1::74` parses as legacy too (name "e::1", start 74).
        // The current shape must win so a migrated id is never called stale.
        let p = parse("a.js::Function::e::1::74").expect("parsed");
        assert!(!p.is_legacy_named(), "got {:?}", p.shape);
        assert_eq!(p.start_line(), 1);
    }

    #[test]
    fn named_with_qualified_method_name() {
        // The method qualifier emits `Type::method` for Rust/Scala/PHP methods.
        let p = parse("src/corpus.rs::Method::CorpusStore::save_kg_graph::40::120")
            .expect("qualified method id");
        assert_eq!(p.file, "src/corpus.rs");
        match p.shape {
            ChunkIdShape::Named {
                ref chunk_type,
                ref name,
                start_line,
                end_line,
            } => {
                assert_eq!(chunk_type, "Method");
                assert_eq!(name, "CorpusStore::save_kg_graph");
                assert_eq!((start_line, end_line), (40, 120));
            }
            other => panic!("expected Named, got {other:?}"),
        }
    }

    #[test]
    fn parses_both_tails() {
        let p = parse("a.js::Function::e::1::1::dup::2::sub::3").expect("both tails");
        assert_eq!(p.file, "a.js");
        assert_eq!(p.dup_index, Some(2));
        assert_eq!(p.sub_index, Some(3));
        assert_eq!(p.start_line(), 1);
        assert!(!p.is_legacy_named());

        let d = parse("a.js::Function::e::1::1::dup::2").expect("dup only");
        assert_eq!((d.dup_index, d.sub_index), (Some(2), None));

        let s = parse("src/big.rs::Function::wide::10::900::sub::3").expect("sub only");
        assert_eq!((s.dup_index, s.sub_index), (None, Some(3)));
    }

    #[test]
    fn tail_requires_digits() {
        assert_eq!(
            split_tails("a.rs:1:2::sub::x"),
            ("a.rs:1:2::sub::x", None, None)
        );
        assert_eq!(split_tails("a.rs:1:2::sub::7"), ("a.rs:1:2", None, Some(7)));
        assert_eq!(
            split_tails("a.rs:1:2::dup::1::sub::7"),
            ("a.rs:1:2", Some(1), Some(7))
        );
    }

    #[test]
    fn suffix_accepts_every_shape() {
        let all = ChunkIdShapes::NewAndLegacy;
        assert!(is_valid_suffix(":10:40", all));
        assert!(is_valid_suffix("::Function::authenticate::42::78", all));
        assert!(is_valid_suffix("::Method::CorpusStore::save::40::120", all));
        assert!(is_valid_suffix("::Function::e::1", all)); // legacy, still readable
        assert!(is_valid_suffix("::Function::e::1::1::dup::2", all));
        assert!(is_valid_suffix("::Function::wide::10::900::sub::3", all));
    }

    /// The per-index gate the owner ruling calls for: an index that has not run
    /// M005 matches both named shapes, one that has run it matches only the new
    /// one. A wrong implementation that ignores the policy fails the second half.
    #[test]
    fn suffix_policy_gates_the_legacy_shape() {
        for shapes in [ChunkIdShapes::NewAndLegacy, ChunkIdShapes::NewOnly] {
            // The current shapes are accepted under either policy.
            assert!(is_valid_suffix(":10:40", shapes), "{shapes:?}");
            assert!(
                is_valid_suffix("::Function::authenticate::42::78", shapes),
                "{shapes:?}"
            );
        }
        assert!(is_valid_suffix(
            "::Function::e::1",
            ChunkIdShapes::NewAndLegacy
        ));
        assert!(!is_valid_suffix("::Function::e::1", ChunkIdShapes::NewOnly));
        // The tails do not smuggle a legacy base past the gate.
        assert!(!is_valid_suffix(
            "::Function::e::1::sub::2",
            ChunkIdShapes::NewOnly
        ));
    }

    #[test]
    fn suffix_rejects_a_bare_colon_path() {
        // The #3401 property: `:` is a legal POSIX path byte, so a real
        // directory must never look like a chunk-id boundary.
        let all = ChunkIdShapes::NewAndLegacy;
        assert!(!is_valid_suffix(":evil/src/lib.rs", all));
        assert!(!is_valid_suffix("::bar.rs:10:20", all));
        assert!(!is_valid_suffix(":9lives.rs:15:25", all));
        assert!(!is_valid_suffix("", all));
        assert!(!is_valid_suffix("/src/lib.rs", all));
    }

    /// Ties every parser to the builder: whatever [`make`] can emit, [`parse`]
    /// and [`parse_suffix`] must both accept, with or without either tail. This
    /// is the assertion that stops the grammar sites from drifting apart again.
    #[test]
    fn roundtrip_covers_every_shape_make_can_emit() {
        let cases = [
            ("src/lib.rs", "Code", "", 1usize, 1usize),
            ("src/lib.rs", "Code", "", 10, 40),
            ("assets/index.js", "Function", "e", 1, 1),
            ("src/auth.rs", "Function", "authenticate", 42, 78),
            ("src/c.rs", "Method", "CorpusStore::save_kg_graph", 40, 120),
        ];
        for (file, ty, name, start, end) in cases {
            let base = make(file, ty, name, start, end);
            for id in [
                base.clone(),
                make_dup(&base, 2),
                make_sub(&base, 4),
                make_sub(&make_dup(&base, 2), 4),
            ] {
                let parsed = parse(&id).unwrap_or_else(|| panic!("parse failed for {id}"));
                assert_eq!(parsed.file, file, "file mismatch for {id}");
                assert_eq!(parsed.start_line(), start, "start mismatch for {id}");
                assert!(!parsed.is_legacy_named(), "{id} must not read as legacy");
                assert!(
                    is_valid_suffix(&id[file.len()..], ChunkIdShapes::NewOnly),
                    "suffix of {id} must validate under the post-M005 policy"
                );
            }
        }
    }
}
