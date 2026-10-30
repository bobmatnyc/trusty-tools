//! Split new chunks at the embedder's input window, and head what is embedded (#10481).
//!
//! Why: an embedder encodes at most its window of tokens and drops the rest, so
//! 29.9% of chunks had a vector describing only their head (#9284 counts it).
//! A window-sized chunk is fully visible to the vector lane.
//! What: [`CodeIndexer::split_for_embed`] replaces each over-window chunk with
//! windows cut by `trusty_common::embedder::chunk_text`; each window is an
//! ordinary chunk with its own line-span id, vector, BM25 document and corpus
//! row. [`embed_header`] is a deterministic `// path: <file> :: <Kind> <name>`
//! line, prepended by [`embed_text`] to the text EMBEDDED only: stored chunk
//! text and BM25 documents never carry it. Only the two inline embed paths call
//! this, so existing chunks and indexes with no known window keep today's
//! behaviour.
//!
//! #10504: the deferred parse (`parse_files_deferred`) splits the same way, so a
//! default index no longer stores one over-window chunk per long function, and the
//! catch-up heads each owed chunk that fits (`catch_up_headers`). Kill switch
//! `TRUSTY_DEFERRED_WINDOW_SPLIT=0` (`deferred_split_enabled`).
//! Test: `core::indexer::tests::window_split_10481`,
//! `core::indexer::tests::window_split_10504`.

use std::collections::HashSet;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use trusty_common::embedder::{chunk_text, ChunkError, ChunkTokenizer};

use super::super::CodeIndexer;
use crate::core::chunker::RawChunk;
use crate::core::embed::Embedder;
use crate::core::index_roots::IndexRoots;
use crate::core::indexer::ParsedBatch;

/// Longest path, in bytes, a header carries; a longer path keeps its tail.
const HEADER_PATH_MAX: usize = 96;
/// Longest symbol name, in bytes, a header carries.
const HEADER_NAME_MAX: usize = 80;

/// Keep at most `max` bytes of `s`, cut on a character boundary.
fn clip(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The path a header names for `file`: root-relative, never absolute (#10481).
///
/// Why: `index_file` may be handed an absolute path, which would put a home
/// directory into the embedded text and give one chunk two different headers.
/// What: a relative path is kept as stored. An absolute one is made relative to
/// the root that owns it ([`crate::core::index_roots::IndexRoots::relative_path`]);
/// under no root, only the file name is kept.
/// Test: `an_absolute_path_is_made_root_relative_in_the_header`,
/// `an_absolute_path_under_no_root_keeps_only_its_file_name`.
pub(crate) fn header_path(file: &str, roots: &IndexRoots) -> String {
    let path = Path::new(file);
    if !path.is_absolute() {
        return file.to_string();
    }
    let rel = roots.relative_path(path);
    if Path::new(&rel).is_absolute() {
        return path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    }
    rel
}

/// The path/symbol header for `chunk` at `file`, ending in a newline (#10481).
///
/// Why: the code-search brief (recommendation 2) adds file and symbol context
/// to what the vector lane sees; a window past the first has no declaration
/// line of its own.
/// What: `// path: <file>` plus ` :: <Kind> <name>` for a named chunk. A path
/// over [`HEADER_PATH_MAX`] keeps its tail, from a `/` boundary when one
/// exists. `file` comes from [`header_path`]. Every window of one parent gets
/// the same header, and the text is reproducible.
/// Test: `the_header_names_the_path_and_symbol`.
pub(crate) fn embed_header(chunk: &RawChunk, file: &str) -> String {
    let path = if file.len() > HEADER_PATH_MAX {
        let mut start = file.len() - HEADER_PATH_MAX;
        while !file.is_char_boundary(start) {
            start += 1;
        }
        match file[start..].find('/') {
            Some(i) => &file[start + i + 1..],
            None => &file[start..],
        }
    } else {
        file
    };
    match chunk.function_name.as_deref().filter(|n| !n.is_empty()) {
        Some(name) => format!(
            "// path: {path} :: {} {}\n",
            chunk.chunk_type.as_str(),
            clip(name, HEADER_NAME_MAX)
        ),
        None => format!("// path: {path}\n"),
    }
}

/// The text the embedder receives for `chunk`: `header` plus the content, or
/// the stored content alone when there is no header (#10481).
/// Test: `the_header_is_embedded_but_never_stored`.
pub(crate) fn embed_text(chunk: &RawChunk, header: Option<&str>) -> String {
    match header {
        Some(h) if !h.is_empty() => format!("{h}{}", chunk.content),
        _ => chunk.content.clone(),
    }
}

/// What one split call did.
#[derive(Debug, Default)]
pub(crate) struct SplitReport {
    /// Chunks replaced by windows.
    pub parents_split: usize,
    /// Chunks left whole because the tokenizer could not split them.
    pub kept_whole: usize,
    /// The first such failure.
    pub first_error: Option<String>,
}

/// How one chunk goes to the embedder: its header (empty: none) and its
/// window ranges (`None`: the chunk fits whole).
struct Plan {
    header: String,
    ranges: Option<Vec<Range<usize>>>,
}

/// Plan `chunk`: header, and windows when header plus content exceed `window`.
///
/// What: a byte prefilter first (a token is at least one byte), so a short
/// chunk is never tokenized. Otherwise the content is cut for
/// `window - header_tokens`, the header counting against the budget. A header
/// that leaves less than half the window drops out for this chunk, which is cut
/// at the full window, rather than costing the content most of its room.
fn plan(
    tokenizer: &ChunkTokenizer,
    window: usize,
    chunk: &RawChunk,
    file: &str,
) -> Result<Plan, ChunkError> {
    let mut header = embed_header(chunk, file);
    if header.len() + chunk.content.len() + tokenizer.special_overhead() <= window {
        return Ok(Plan {
            header,
            ranges: None,
        });
    }
    let mut room = window.saturating_sub(tokenizer.count_tokens(&header)?);
    if room < window / 2 {
        header.clear();
        room = window;
    }
    let pieces = chunk_text(tokenizer, &chunk.content, room)?;
    let ranges = (pieces.len() > 1).then(|| pieces.into_iter().map(|p| p.byte_range).collect());
    Ok(Plan { header, ranges })
}

/// Build window `n` of `src`: ranges are byte ranges of `src.content`.
///
/// What: line span from the newlines before the range; `calls`, `inherits_from`,
/// NLP fields and `virtual_terms` go to window 0 only, so the symbol graph keeps
/// one set of edges per symbol. `parent_chunk_id` is carried unchanged.
fn window_chunk(src: &RawChunk, newlines: &[usize], range: Range<usize>, first: bool) -> RawChunk {
    let line_of = |byte: usize| src.start_line + newlines.partition_point(|&p| p < byte);
    let start_line = line_of(range.start).min(src.end_line);
    let last_byte = range.end.saturating_sub(1).max(range.start);
    let end_line = line_of(last_byte).clamp(start_line, src.end_line.max(start_line));
    RawChunk {
        id: String::new(),
        file: src.file.clone(),
        start_line,
        end_line,
        content: src.content[range].to_string(),
        function_name: src.function_name.clone(),
        language: src.language.clone(),
        chunk_type: src.chunk_type.clone(),
        calls: if first { src.calls.clone() } else { Vec::new() },
        inherits_from: if first {
            src.inherits_from.clone()
        } else {
            Vec::new()
        },
        chunk_depth: src.chunk_depth,
        parent_chunk_id: src.parent_chunk_id.clone(),
        child_chunk_ids: Vec::new(),
        nlp_keywords: if first {
            src.nlp_keywords.clone()
        } else {
            Vec::new()
        },
        nlp_code_refs: if first {
            src.nlp_code_refs.clone()
        } else {
            Vec::new()
        },
        virtual_terms: if first {
            src.virtual_terms.clone()
        } else {
            Vec::new()
        },
    }
}

/// Replace every chunk that exceeds `window` with window chunks (#10481).
///
/// Why: see the module doc. Pure and blocking: the caller runs it on a
/// blocking thread.
/// What: order is kept, windows take the parent's place. A window id is
/// [`crate::core::chunk_id::make`] over its own line span; an id already held by
/// a kept chunk or an earlier window gets the existing `::dup::{n}` tail, so no
/// new id shape exists. A chunk the tokenizer cannot split stays whole and is
/// counted in the report. A chunk within the window is returned untouched.
/// The returned headers are 1:1 with the chunks; an empty one means none.
/// Test: `a_chunk_over_the_window_splits_into_windows_that_each_fit`,
/// `consecutive_windows_overlap_by_the_common_chunker_constant`,
/// `a_chunk_within_the_window_keeps_its_id_and_content`,
/// `a_window_that_cannot_be_split_keeps_the_chunk_whole`,
/// `windows_of_a_one_line_chunk_get_distinct_ids`,
/// `a_header_that_leaves_too_little_room_is_dropped`.
pub(crate) fn split_at_window(
    tokenizer: &ChunkTokenizer,
    window: usize,
    roots: &IndexRoots,
    chunks: Vec<RawChunk>,
) -> (Vec<RawChunk>, Vec<String>, SplitReport) {
    let mut report = SplitReport::default();
    let mut planned: Vec<(RawChunk, Plan)> = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let file = header_path(&chunk.file, roots);
        let plan = plan(tokenizer, window, &chunk, &file).unwrap_or_else(|e| {
            report.kept_whole += 1;
            report.first_error.get_or_insert_with(|| e.to_string());
            // Unsplit and unheaded: the embed is cut as it was before this change.
            Plan {
                header: String::new(),
                ranges: None,
            }
        });
        planned.push((chunk, plan));
    }
    // Ids of chunks that stay: a window must not take one of them.
    let mut used: HashSet<String> = planned
        .iter()
        .filter(|(_, p)| p.ranges.is_none())
        .map(|(c, _)| c.id.clone())
        .collect();
    let mut out = Vec::with_capacity(planned.len());
    let mut headers = Vec::with_capacity(planned.len());
    for (chunk, Plan { header, ranges }) in planned {
        let Some(ranges) = ranges else {
            out.push(chunk);
            headers.push(header);
            continue;
        };
        report.parents_split += 1;
        let newlines: Vec<usize> = chunk.content.match_indices('\n').map(|(i, _)| i).collect();
        let name = chunk.function_name.as_deref().unwrap_or("");
        for (n, range) in ranges.into_iter().enumerate() {
            let mut w = window_chunk(&chunk, &newlines, range, n == 0);
            let base = crate::core::chunk_id::make(
                &w.file,
                w.chunk_type.as_str(),
                name,
                w.start_line,
                w.end_line,
            );
            let mut id = base.clone();
            let mut dup = 0usize;
            while !used.insert(id.clone()) {
                dup += 1;
                id = crate::core::chunk_id::make_dup(&base, dup);
            }
            w.id = id;
            out.push(w);
            headers.push(header.clone());
        }
    }
    (out, headers, report)
}

/// Env var that turns the deferred-path split off (#10504).
const DEFERRED_SPLIT_ENV: &str = "TRUSTY_DEFERRED_WINDOW_SPLIT";

/// Read the kill switch value: on unless it is `0`, `false` or `off` (#10504).
///
/// Why: the split changes how many chunks every default index stores, and the
/// effect on the chunk cap and BM25 is not measured; an operator needs a way back.
/// What: pure, so a test needs no process-env change. Case and padding ignored.
/// Test: `a_kill_switch_off_parses_deferred_unsplit`.
pub(crate) fn parse_switch(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "false" | "off")
    )
}

/// Whether #10504's behaviour is on, read from `TRUSTY_DEFERRED_WINDOW_SPLIT`.
///
/// What: off turns off the deferred split, the catch-up headers and the assumed
/// (MiniLM) window fallback, so every path behaves as #10481 did.
/// Test: `a_kill_switch_off_parses_deferred_unsplit`.
pub fn deferred_split_enabled() -> bool {
    parse_switch(std::env::var(DEFERRED_SPLIT_ENV).ok().as_deref())
}

/// The header for `chunk` when header plus content fit `window`, else empty (#10504).
///
/// Why: the catch-up embeds a stored chunk as it is; a header that pushed it past
/// the window would cut its tail, the loss the split exists to remove. A chunk cut
/// without a header (room under window/2) must not gain one here.
/// What: a byte prefilter first (a token is at least one byte), then a token
/// count of the headed text plus the special tokens. A count error is no header.
/// Test: `a_short_owed_chunk_gets_a_header_and_an_oversize_old_one_does_not`,
/// `a_window_cut_without_a_header_is_embedded_without_one`.
fn header_if_fits(
    tokenizer: &ChunkTokenizer,
    window: usize,
    chunk: &RawChunk,
    file: &str,
) -> String {
    let header = embed_header(chunk, file);
    let overhead = tokenizer.special_overhead();
    if header.len() + chunk.content.len() + overhead <= window {
        return header;
    }
    let headed = embed_text(chunk, Some(&header));
    match tokenizer.count_tokens(&headed) {
        Ok(n) if n + overhead <= window => header,
        _ => String::new(),
    }
}

/// Headers for `chunks`, 1:1, each empty when it would not fit `window` (#10504).
/// Test: `the_catch_up_embeds_each_window_with_the_header_and_stores_none`.
fn catch_up_headers(
    tokenizer: &ChunkTokenizer,
    window: usize,
    roots: &IndexRoots,
    chunks: &[RawChunk],
) -> Vec<String> {
    chunks
        .iter()
        .map(|c| header_if_fits(tokenizer, window, c, &header_path(&c.file, roots)))
        .collect()
}

/// What a blocking split attempt found.
enum SplitOutcome {
    Split {
        window: usize,
        chunks: Vec<RawChunk>,
        headers: Vec<String>,
        report: SplitReport,
    },
    /// No window or tokenizer: the chunks come back whole; `Some` names a load failure.
    Whole(Vec<RawChunk>, Option<String>),
}

/// Ask `embedder` for its split window and split `chunks` (blocking).
fn split_blocking(
    embedder: &Arc<dyn Embedder>,
    roots: &IndexRoots,
    chunks: Vec<RawChunk>,
) -> SplitOutcome {
    match embedder.split_window() {
        Ok(Some((window, tokenizer))) => {
            let (chunks, headers, report) = split_at_window(&tokenizer, window, roots, chunks);
            SplitOutcome::Split {
                window,
                chunks,
                headers,
                report,
            }
        }
        Ok(None) => SplitOutcome::Whole(chunks, None),
        Err(e) => SplitOutcome::Whole(chunks, Some(e.to_string())),
    }
}

impl CodeIndexer {
    /// Split `chunks` at the embedder's window ahead of an inline embed (#10481).
    ///
    /// Why: both inline embed paths (`index_file` and the full-pipeline parse)
    /// hand new or changed chunks straight to the embedder, which cuts them.
    /// What: returns `(chunks, headers)`, `headers` 1:1 with the chunks (an
    /// empty string means no header for that chunk). With no embedder or store, a
    /// vector-skipping index, or a window or tokenizer the embedder does not
    /// report, the chunks come back as they went in and `headers` is `None`: no
    /// split, no header, today's behaviour. A lazy sidecar reports its window
    /// only after its first answer, so a write before that answer goes unsplit;
    /// this method never sends a probe of its own, since an embedder that never
    /// reports a window would then pay one extra call per write.
    /// [`CodeIndexer::warm_embedder`] runs at the start of a full reindex.
    /// `headers` is `Some` only when the window and a tokenizer are known, the
    /// case the header budget was computed in. A task panic fails the call, as the
    /// batch parse does.
    /// Test: `an_embedder_with_no_window_does_not_split`,
    /// `an_unknown_window_does_not_split_until_the_embedder_reports_it`.
    pub(crate) async fn split_for_embed(
        &self,
        chunks: Vec<RawChunk>,
    ) -> Result<(Vec<RawChunk>, Option<Vec<String>>)> {
        let (Some(embedder), Some(_store)) = (&self.embedder, &self.store) else {
            return Ok((chunks, None));
        };
        if self.skip_vector || chunks.is_empty() {
            return Ok((chunks, None));
        }
        let embedder = Arc::clone(embedder);
        let roots = self.chunk_roots();
        let outcome =
            tokio::task::spawn_blocking(move || split_blocking(&embedder, &roots, chunks))
                .await
                .context("window split task panicked")?;
        let (window, split, headers, report) = match outcome {
            SplitOutcome::Split {
                window,
                chunks,
                headers,
                report,
            } => (window, chunks, headers, report),
            SplitOutcome::Whole(whole, failure) => {
                if let Some(reason) = failure {
                    // #10504: say why a split did not run; the chunks embed cut, as before.
                    tracing::warn!(
                        index_id = %self.index_id,
                        %reason,
                        "no tokenizer to split chunks with; they stay whole (#10504)",
                    );
                }
                return Ok((whole, None));
            }
        };
        self.embed_window.add_split(report.parents_split as u64);
        if report.kept_whole > 0 {
            tracing::warn!(
                index_id = %self.index_id,
                kept_whole = report.kept_whole,
                reason = %report.first_error.as_deref().unwrap_or("unknown"),
                window,
                "chunks the tokenizer could not split stay whole and embed cut (#10481)",
            );
        }
        Ok((split, Some(headers)))
    }

    /// Parse `files` for a deferred index, splitting chunks at the embedder window (#10504).
    ///
    /// Why: the fast pass stored one chunk per function, so a long function was
    /// embedded cut by the catch-up. Splitting here puts the windows in BM25, the
    /// corpus and redb from the first commit, and no stored chunk is ever rewritten.
    /// What: `parse_files_only`, then, when `split`, [`CodeIndexer::split_for_embed`]
    /// on the chunks. Embeddings stay `None` and the headers are dropped (the
    /// catch-up recomputes them). `split` false, no embedder, no window or no
    /// tokenizer: the chunks come back as `parse_files_only` made them.
    /// Test: `a_deferred_index_splits_a_new_long_chunk_once_the_window_is_known`,
    /// `with_no_window_and_no_tokenizer_a_deferred_parse_does_not_split`,
    /// `a_kill_switch_off_parses_deferred_unsplit`.
    pub(crate) async fn parse_files_deferred(
        &self,
        files: Vec<(String, String)>,
        split: bool,
    ) -> Result<ParsedBatch> {
        let mut batch = self.parse_files_only(files).await?;
        if !split || batch.chunks.is_empty() {
            return Ok(batch);
        }
        let (chunks, _headers) = self
            .split_for_embed(std::mem::take(&mut batch.chunks))
            .await?;
        batch.embeddings = vec![None; chunks.len()];
        batch.chunks = chunks;
        Ok(batch)
    }

    /// Headers for one wave of owed chunks, 1:1, or `None` when none can be built (#10504).
    ///
    /// Why: the catch-up embedded stored text bare, so a window past the first of a
    /// long function carried no file or symbol. The header goes to the embedded
    /// text only; stored text and BM25 never carry it.
    /// What: `None` with the kill switch off, no embedder, a vector-skipping index,
    /// or no split window. Otherwise [`header_if_fits`] per chunk, on a blocking
    /// thread.
    /// Test: `the_catch_up_embeds_each_window_with_the_header_and_stores_none`,
    /// `with_the_switch_off_the_catch_up_adds_no_header`.
    pub(crate) async fn catch_up_headers(&self, chunks: &[RawChunk]) -> Option<Vec<String>> {
        self.catch_up_headers_if(deferred_split_enabled(), chunks)
            .await
    }

    /// [`Self::catch_up_headers`] with the switch value given, so a test sets none.
    pub(crate) async fn catch_up_headers_if(
        &self,
        enabled: bool,
        chunks: &[RawChunk],
    ) -> Option<Vec<String>> {
        if !enabled {
            return None;
        }
        let embedder = Arc::clone(self.embedder.as_ref()?);
        if self.skip_vector || chunks.is_empty() {
            return None;
        }
        let roots = self.chunk_roots();
        let owned = chunks.to_vec();
        let built = tokio::task::spawn_blocking(move || {
            let (window, tokenizer) = embedder.split_window().ok().flatten()?;
            Some(catch_up_headers(&tokenizer, window, &roots, &owned))
        })
        .await;
        built.ok().flatten()
    }
}
