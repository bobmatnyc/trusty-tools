//! BM25 lexical index — zero-dependency port from `trusty-search`.
//!
//! Why: trusty-memory needs a lexical-search lane to complement its vector
//! index, and several other trusty-* crates already speak BM25. Hoisting the
//! implementation into `trusty-common` (gated behind the `bm25` feature) lets
//! every consumer share one tokenizer + scorer rather than re-implementing the
//! algorithm. Originally ported from open-mpm `src/context/bm25.rs`; no
//! external crate deps remain.
//!
//! What: a code-aware tokenizer (`tokenize`, with camelCase / PascalCase /
//! alpha↔digit splits) plus an incremental BM25 index (`BM25Index`) keyed by
//! opaque string ids. Insert / update / remove are O(d) in the document's
//! token count; `score_query_all` is O(sum(df_i)) over the query's terms.
//!
//! Test: `cargo test -p trusty-common --features bm25` covers tokenisation,
//! ranking, incremental updates, and corpus-cap behaviour.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

/// Default cap on the number of live documents BM25 will accept.
///
/// Why: posting lists, `doc_terms`, and per-term frequency maps scale linearly
/// with corpus size. On a 200 000-chunk index BM25 alone can consume several
/// hundred MB of RAM; bounding the corpus keeps per-query latency low and
/// memory predictable. Lexical recall above this cap typically falls off — a
/// companion vector index can still cover the long tail. Override via
/// `TRUSTY_BM25_CORPUS_CAP`.
/// What: a module-level constant used by `bm25_corpus_cap()`.
/// Test: covered indirectly by `bm25_corpus_cap_env_override`.
const DEFAULT_BM25_CORPUS_CAP: usize = 50_000;

/// Resolve the active corpus cap, honouring `TRUSTY_BM25_CORPUS_CAP`.
///
/// Why: operators in memory-constrained deployments need a runtime knob to
/// shrink (or grow) the cap without recompiling. Reading the env var on every
/// upsert is cheap; the call site only fires on inserts that would otherwise
/// allocate a new slot.
/// What: parses `TRUSTY_BM25_CORPUS_CAP` as `usize`. Zero / unset / unparsable
/// falls back to [`DEFAULT_BM25_CORPUS_CAP`] so a bogus env value can never
/// silently disable the cap.
/// Test: covered by `bm25_corpus_cap_env_override`.
fn bm25_corpus_cap() -> usize {
    std::env::var("TRUSTY_BM25_CORPUS_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(DEFAULT_BM25_CORPUS_CAP)
}

/// Latch so we only log "BM25 corpus cap reached" once per process — repeated
/// indexing of a large repo would otherwise drown the daemon log in warnings.
static BM25_CAP_LOGGED: AtomicBool = AtomicBool::new(false);

/// Three-pass tokenizer for code-aware BM25.
///
/// Why: source-code identifiers carry meaning at multiple granularities. A
/// single split on non-alphanumeric characters loses too much signal —
/// `CodeIndexer` should match `indexer`, `HTTP2Client` should match `http`,
/// `2`, and `client`. Three passes layered over a non-alphanumeric outer
/// split capture each shape without bespoke per-language rules.
/// What: emits, for every alphanumeric run in the input,
///   1. the raw token lowercased (exact identifier match still wins),
///   2. its camelCase / PascalCase parts (`CodeIndexer` → `code`, `indexer`),
///   3. its alpha↔digit parts (`HTTP2Client` → `http`, `2`, `client`).
///
/// Tokens are deduped and sorted at the end so the inverted index sees a
/// stable per-doc list.
///
/// Test: `tokenize_camel_case_pascal`, `tokenize_alpha_digit_split`,
/// `tokenize_acronym_then_word`, `tokenize_dedups_and_sorts`.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }

        // Pass 1: raw token lowercased.
        tokens.push(raw.to_lowercase());

        // Pass 2: camelCase / PascalCase split.
        let camel_parts = split_camel_case(raw);
        if camel_parts.len() > 1 {
            tokens.extend(camel_parts.iter().map(|s| s.to_lowercase()));
        }

        // Pass 3: alpha↔digit split.
        let digit_parts = split_on_digits(raw);
        if digit_parts.len() > 1 {
            tokens.extend(digit_parts.iter().map(|s| s.to_lowercase()));
        }
    }
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

/// Split an identifier at camelCase / PascalCase / acronym boundaries.
///
/// Why: keeps the camel/Pascal logic in one place so `tokenize` stays focused.
/// What: applies two boundary rules:
///   - lowercase / digit → uppercase (`codeIndexer` → `code`, `Indexer`),
///   - uppercase run → uppercase + lowercase (`HTTPSClient` → `HTTPS`, `Client`).
///
/// Returns the whole input verbatim when it has fewer than two characters.
///
/// Test: covered transitively by `tokenize_*` tests in this module.
fn split_camel_case(s: &str) -> Vec<&str> {
    let bytes_len = s.len();
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    if chars.len() < 2 {
        return vec![s];
    }
    let mut bounds: Vec<usize> = vec![0];
    for i in 1..chars.len() {
        let (idx, c) = chars[i];
        let (_, prev) = chars[i - 1];
        // lowercase/digit → uppercase
        let lower_to_upper = (prev.is_lowercase() || prev.is_ascii_digit()) && c.is_uppercase();
        // uppercase run → uppercase + lowercase: split before the trailing
        // uppercase that begins a new word (e.g. "HTTPSClient" → split at 'C').
        let acronym_to_word = prev.is_uppercase()
            && c.is_uppercase()
            && i + 1 < chars.len()
            && chars[i + 1].1.is_lowercase();
        if lower_to_upper || acronym_to_word {
            bounds.push(idx);
        }
    }
    bounds.push(bytes_len);
    bounds
        .windows(2)
        .map(|w| &s[w[0]..w[1]])
        .filter(|p| !p.is_empty())
        .collect()
}

/// Split at alpha↔digit transitions.
///
/// Why: keeps the alpha↔digit logic in one place so `tokenize` stays focused.
/// What: `HTTP2` → [`HTTP`, `2`]; `v3alpha` → [`v`, `3`, `alpha`]. Returns the
/// input verbatim when it has fewer than two characters.
/// Test: covered transitively by `tokenize_alpha_digit_split`.
fn split_on_digits(s: &str) -> Vec<&str> {
    let bytes_len = s.len();
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    if chars.len() < 2 {
        return vec![s];
    }
    let mut bounds: Vec<usize> = vec![0];
    for i in 1..chars.len() {
        let (idx, c) = chars[i];
        let (_, prev) = chars[i - 1];
        let alpha_to_digit = prev.is_alphabetic() && c.is_ascii_digit();
        let digit_to_alpha = prev.is_ascii_digit() && c.is_alphabetic();
        if alpha_to_digit || digit_to_alpha {
            bounds.push(idx);
        }
    }
    bounds.push(bytes_len);
    bounds
        .windows(2)
        .map(|w| &s[w[0]..w[1]])
        .filter(|p| !p.is_empty())
        .collect()
}

/// Incremental BM25 index keyed by opaque string `doc_id`.
///
/// Why: rebuilding the index over the entire corpus on every query is O(n) and
/// dominates p50 latency on large indexes (115 k chunks → ~9.5 s in
/// trusty-search). We keep the inverted lists hot and mutate them as documents
/// are added / removed.
///
/// What: each document is identified by an opaque `doc_id`. Internally we
/// allocate a stable `usize` slot per id and reuse it on update; removed slots
/// are tracked in a free-list so the per-term `Vec` stays compact.
/// Per-document term frequencies are kept in `doc_terms` so `remove_document`
/// can decrement `doc_freqs` accurately without re-tokenizing.
///
/// `score_query_all` walks only the posting lists for the query terms, so a
/// k-term query touches `O(sum(df_i))` postings instead of N docs.
///
/// Test: see the `#[cfg(test)] mod tests` block at the bottom of this file.
pub struct BM25Index {
    k1: f32,
    b: f32,
    /// Per-term document frequency.
    doc_freqs: HashMap<String, usize>,
    /// Per-slot doc length (token count). `None` for free / removed slots.
    doc_lengths: Vec<Option<usize>>,
    /// Per-term posting list: `(slot, term_count_in_doc)`.
    inverted: HashMap<String, Vec<(usize, usize)>>,
    /// Running total of live-slot doc lengths. Maintained incrementally on
    /// every upsert/remove so `avg_doc_len()` is O(1) instead of O(slots).
    ///
    /// Why: bulk-ingest paths call `upsert_document` in tight loops. The
    /// original implementation refreshed `avg_doc_len` by scanning every
    /// slot inside `remove_document` (and again at the tail of
    /// `upsert_document`), so a 640-doc batch on a 50 k-doc index performed
    /// ~32 M iterations of pure bookkeeping. Tracking the total length
    /// incrementally turns that into a constant-time `u64` add/subtract per
    /// document.
    total_doc_length: u64,
    /// doc_id → slot. Used by upsert/remove to find an existing slot.
    id_to_slot: HashMap<String, usize>,
    /// slot → doc_id. Used by `score_query_all` to materialize results.
    slot_to_id: Vec<Option<String>>,
    /// Free slots returned by `remove_document`, reused on next `add_document`.
    free_slots: Vec<usize>,
    /// Per-slot term list, retained so `remove_document` can update postings
    /// without re-tokenizing the original text.
    doc_terms: Vec<Option<Vec<String>>>,
    /// Number of live (non-free) slots.
    live_docs: usize,
}

impl BM25Index {
    /// Construct an empty index with the canonical BM25 parameters
    /// (`k1 = 1.5`, `b = 0.75`).
    ///
    /// Why: these defaults match the classical Robertson/Spärck-Jones tuning
    /// and are what trusty-search has used in production since launch.
    /// What: zero-initialised maps + vectors; no I/O.
    /// Test: every other test in this module starts here.
    pub fn new() -> Self {
        Self {
            k1: 1.5,
            b: 0.75,
            doc_freqs: HashMap::new(),
            doc_lengths: Vec::new(),
            inverted: HashMap::new(),
            total_doc_length: 0,
            id_to_slot: HashMap::new(),
            slot_to_id: Vec::new(),
            free_slots: Vec::new(),
            doc_terms: Vec::new(),
            live_docs: 0,
        }
    }

    /// Number of live documents currently indexed.
    pub fn len(&self) -> usize {
        self.live_docs
    }

    /// Returns `true` when no documents are indexed.
    pub fn is_empty(&self) -> bool {
        self.live_docs == 0
    }

    /// Average doc length over live slots. O(1) — read from the running
    /// `total_doc_length` sum maintained by `upsert_document` and
    /// `remove_document`.
    ///
    /// Why: BM25 scoring needs avg doc length per query, and the bulk
    /// ingest path does N upsert/remove operations per batch. Computing
    /// the average from the running total avoids the full-corpus scan
    /// that previously dominated batch commits.
    /// What: returns `total_doc_length / live_docs`, or `0.0` when the
    /// index is empty (avoids division by zero).
    /// Test: every BM25 ranking test in this module depends on this value.
    fn avg_doc_len(&self) -> f32 {
        if self.live_docs == 0 {
            0.0
        } else {
            self.total_doc_length as f32 / self.live_docs as f32
        }
    }

    /// Allocate a slot for a new doc_id, reusing a freed slot when possible.
    fn allocate_slot(&mut self, doc_id: &str) -> usize {
        if let Some(slot) = self.free_slots.pop() {
            self.slot_to_id[slot] = Some(doc_id.to_string());
            self.doc_lengths[slot] = Some(0);
            self.doc_terms[slot] = Some(Vec::new());
            slot
        } else {
            let slot = self.slot_to_id.len();
            self.slot_to_id.push(Some(doc_id.to_string()));
            self.doc_lengths.push(Some(0));
            self.doc_terms.push(Some(Vec::new()));
            slot
        }
    }

    /// Insert a document. If `doc_id` already exists, the previous postings
    /// are removed first so updates are idempotent.
    ///
    /// Memory cap: when the live-doc count is already at or above
    /// `TRUSTY_BM25_CORPUS_CAP` (default 50 000) **and** this is a brand-new
    /// `doc_id`, the upsert is dropped. Updates to existing documents are
    /// always honoured — they don't grow the corpus. A single tracing warn
    /// is emitted the first time the cap is hit (per process); subsequent
    /// drops are silent to avoid log spam during full reindexes of oversized
    /// corpora. A bulk caller that wants a per-rebuild dropped-count instead
    /// of this one-time latch (e.g. a rehydrate rebuild — issue #3684) should
    /// use [`Self::upsert_document_reporting`] directly and own its own
    /// logging.
    pub fn upsert_document(&mut self, doc_id: &str, text: &str) {
        if !self.upsert_document_reporting(doc_id, text)
            && !BM25_CAP_LOGGED.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                cap = bm25_corpus_cap(),
                live_docs = self.live_docs,
                "BM25 corpus cap reached — dropping further new documents \
                 (override with TRUSTY_BM25_CORPUS_CAP)"
            );
        }
    }

    /// Insert-or-update `doc_id`, reporting whether it was accepted rather
    /// than logging a process-wide latch (issue #3684).
    ///
    /// Why: [`Self::upsert_document`]'s cap enforcement logs via a
    /// process-wide "log once ever" latch, which is right for the steady
    /// incremental-ingest path (one call per changed chunk, spread over the
    /// process lifetime) but wrong for a bulk rebuild — idle-evict rehydrate
    /// (`trusty-search::core::indexer::idle_evict`) or warm-boot restore
    /// (`trusty-search::core::indexer::persist::load_chunks_from_redb`) — which
    /// re-runs the entire cap-truncation decision on every rebuild and wants
    /// a fresh per-rebuild dropped-count, not a single one-time warning that
    /// went stale after the first rebuild.
    /// What: identical cap/tokenize/insert logic to `upsert_document`, minus
    /// the static latch/log — returns `true` when the document was inserted
    /// or updated, `false` when a brand-new `doc_id` was dropped because
    /// `live_docs >= TRUSTY_BM25_CORPUS_CAP`. The caller owns counting drops
    /// and logging a summary.
    /// Test: `upsert_document_reporting_returns_false_when_capped`,
    /// `upsert_document_delegates_to_reporting_and_still_logs_once`.
    pub fn upsert_document_reporting(&mut self, doc_id: &str, text: &str) -> bool {
        if self.id_to_slot.contains_key(doc_id) {
            self.remove_document(doc_id);
        } else {
            let cap = bm25_corpus_cap();
            if self.live_docs >= cap {
                return false;
            }
        }
        let slot = self.allocate_slot(doc_id);
        self.id_to_slot.insert(doc_id.to_string(), slot);
        self.live_docs += 1;

        let tokens = tokenize(text);
        let doc_len = tokens.len();
        self.doc_lengths[slot] = Some(doc_len);
        // Maintain the running sum so `avg_doc_len()` stays O(1).
        self.total_doc_length = self.total_doc_length.saturating_add(doc_len as u64);

        // Per-doc term counts.
        let mut term_counts: HashMap<&str, usize> = HashMap::new();
        for t in &tokens {
            *term_counts.entry(t.as_str()).or_default() += 1;
        }
        for (term, count) in term_counts {
            *self.doc_freqs.entry(term.to_string()).or_default() += 1;
            self.inverted
                .entry(term.to_string())
                .or_default()
                .push((slot, count));
        }
        self.doc_terms[slot] = Some(tokens);
        true
    }

    /// Legacy slot-based add. Retained so the in-tree `score(query, doc_id)`
    /// API keeps working for existing tests; new callers should prefer
    /// [`Self::upsert_document`].
    pub fn add_document(&mut self, doc_id: usize, text: &str) {
        // Map the slot-style doc_id onto a synthetic string id that won't
        // collide with real ids. This keeps the legacy test surface intact
        // while the production path uses `upsert_document` exclusively.
        let synthetic = format!("__legacy:{doc_id}");
        self.upsert_document(&synthetic, text);
    }

    /// Remove a document by id. No-op when the id isn't present.
    pub fn remove_document(&mut self, doc_id: &str) {
        let Some(slot) = self.id_to_slot.remove(doc_id) else {
            return;
        };
        // Decrement doc_freqs and prune postings using the cached term list.
        if let Some(terms) = self.doc_terms[slot].take() {
            // Unique terms only — doc_freqs counts documents, not occurrences.
            let mut unique = terms.clone();
            unique.sort_unstable();
            unique.dedup();
            for term in &unique {
                if let Some(df) = self.doc_freqs.get_mut(term) {
                    *df = df.saturating_sub(1);
                    if *df == 0 {
                        self.doc_freqs.remove(term);
                    }
                }
                if let Some(postings) = self.inverted.get_mut(term) {
                    postings.retain(|(s, _)| *s != slot);
                    if postings.is_empty() {
                        self.inverted.remove(term);
                    }
                }
            }
        }
        // Subtract this slot's length from the running sum before clearing it
        // so `avg_doc_len()` stays consistent with the live-slot population.
        if let Some(old_len) = self.doc_lengths[slot] {
            self.total_doc_length = self.total_doc_length.saturating_sub(old_len as u64);
        }
        self.doc_lengths[slot] = None;
        self.slot_to_id[slot] = None;
        self.free_slots.push(slot);
        self.live_docs = self.live_docs.saturating_sub(1);
    }

    /// Score every document that contains at least one query term, returning
    /// `(doc_id, score)` pairs sorted by score descending. Up to `top_k`
    /// pairs are returned. Documents with score zero are filtered out.
    ///
    /// This is the production search entry point. Cost is `O(sum(df_i))`
    /// over query terms — independent of total corpus size.
    pub fn score_query_all(&self, query: &str, top_k: usize) -> Vec<(String, f32)> {
        self.score_query_all_filtered(query, top_k, None)
    }

    /// Same as [`Self::score_query_all`], but `filter` is evaluated on each
    /// candidate `doc_id` BEFORE the `top_k` truncation (issue #3401 —
    /// trusty-tools).
    ///
    /// Why: a caller-supplied scope filter (e.g. a repo/path prefix) must not
    /// lose recall to `top_k` — a document that genuinely matches the scope
    /// AND has real lexical overlap with the query can rank outside the
    /// unscoped top `top_k` and still need to surface once scoped. Applying
    /// `filter` naively AFTER this function returns cannot recover such a
    /// document, because it was already discarded by the internal
    /// `truncate(top_k)`. Evaluating it in the same pass, before truncation,
    /// fixes that — `filter` only has to reject candidates, so the touched-set
    /// cost model (`O(sum(df_i))` over query terms) is unchanged.
    /// What: identical BM25 scoring pass as `score_query_all`; the one
    /// difference is a `filter(|(id, _)| filter(id))` step inserted between
    /// resolving `doc_id`s and truncating.
    /// Test: `trusty-search::core::indexer::tests::path_filter_search::
    /// test_path_prefix_filter_recovers_bm25_match_beyond_want` exercises this
    /// through the full search pipeline with a target chunk that DOES share
    /// query tokens and is one of more than `want` matching documents.
    pub fn score_query_all_with_filter(
        &self,
        query: &str,
        top_k: usize,
        filter: &dyn Fn(&str) -> bool,
    ) -> Vec<(String, f32)> {
        self.score_query_all_filtered(query, top_k, Some(filter))
    }

    /// Shared scoring pass behind [`Self::score_query_all`] and
    /// [`Self::score_query_all_with_filter`]. `filter`, when present, is
    /// applied before the `top_k` truncation — see the doc comment on
    /// `score_query_all_with_filter` for why that ordering matters.
    fn score_query_all_filtered(
        &self,
        query: &str,
        top_k: usize,
        filter: Option<&dyn Fn(&str) -> bool>,
    ) -> Vec<(String, f32)> {
        if self.live_docs == 0 || top_k == 0 {
            return Vec::new();
        }
        let n = self.live_docs as f32;
        let avg = self.avg_doc_len().max(1.0);

        // Accumulate score per slot. HashMap is fine — the touched-slot set is
        // bounded by the union of postings for the query terms, not by N.
        let mut acc: HashMap<usize, f32> = HashMap::new();
        for term in tokenize(query) {
            let df = match self.doc_freqs.get(&term) {
                Some(d) if *d > 0 => *d as f32,
                _ => continue,
            };
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
            let Some(postings) = self.inverted.get(&term) else {
                continue;
            };
            for (slot, count) in postings {
                let dl = match self.doc_lengths.get(*slot).and_then(|x| *x) {
                    Some(l) => l as f32,
                    None => continue,
                };
                let tf = *count as f32;
                let tf_norm =
                    tf * (self.k1 + 1.0) / (tf + self.k1 * (1.0 - self.b + self.b * dl / avg));
                *acc.entry(*slot).or_insert(0.0) += idf * tf_norm;
            }
        }

        let mut scored: Vec<(String, f32)> = acc
            .into_iter()
            .filter(|(_, s)| *s > 0.0)
            .filter_map(|(slot, score)| {
                self.slot_to_id
                    .get(slot)
                    .and_then(|o| o.clone())
                    .map(|id| (id, score))
            })
            // Issue #3401: scope filter applied BEFORE truncation — see
            // `score_query_all_with_filter`'s doc comment.
            .filter(|(id, _)| filter.is_none_or(|f| f(id.as_str())))
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(top_k);
        scored
    }

    /// Score a single document by its legacy slot-style `doc_id`.
    ///
    /// Why: preserved for backward compatibility with tests that constructed
    /// the index via `add_document(usize, ..)`. New code should use
    /// [`Self::score_query_all`].
    /// What: walks the query's tokens, looks up the term's posting for the
    /// requested slot, and accumulates BM25 contribution. Returns `0.0` for
    /// removed / out-of-range slots.
    /// Test: `bm25_scores_relevant_doc_higher`.
    pub fn score(&self, query: &str, doc_id: usize) -> f32 {
        let n = self.live_docs as f32;
        let dl = match self.doc_lengths.get(doc_id).and_then(|x| *x) {
            Some(l) => l as f32,
            None => return 0.0,
        };
        let mut score = 0.0f32;

        for term in tokenize(query) {
            let df = *self.doc_freqs.get(&term).unwrap_or(&0) as f32;
            if df == 0.0 {
                continue;
            }
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();

            let tf = self
                .inverted
                .get(&term)
                .and_then(|v| v.iter().find(|(id, _)| *id == doc_id))
                .map(|(_, c)| *c as f32)
                .unwrap_or(0.0);

            let tf_norm = tf * (self.k1 + 1.0)
                / (tf + self.k1 * (1.0 - self.b + self.b * dl / self.avg_doc_len().max(1.0)));

            score += idf * tf_norm;
        }
        score
    }
}

impl Default for BM25Index {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
