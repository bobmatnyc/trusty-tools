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
//! this, so existing chunks, the deferred catch-up pass and indexes with no
//! known window keep today's behaviour.
//! Test: `core::indexer::tests::window_split_10481`.

use std::collections::HashSet;
use std::ops::Range;

use anyhow::{Context, Result};
use trusty_common::embedder::{chunk_text, ChunkError, ChunkTokenizer};

use super::super::CodeIndexer;
use super::truncation::WindowProbe;
use crate::core::chunker::RawChunk;

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

/// The path/symbol header for `chunk`, ending in a newline (#10481).
///
/// Why: the code-search brief (recommendation 2) adds file and symbol context
/// to what the vector lane sees; a window past the first has no declaration
/// line of its own.
/// What: `// path: <file>` plus ` :: <Kind> <name>` for a named chunk. A path
/// over [`HEADER_PATH_MAX`] keeps its tail, from a `/` boundary when one
/// exists. Derived from the chunk alone, so every window of one parent gets
/// the same header and the text is reproducible.
/// Test: `the_header_names_the_path_and_symbol`.
pub(crate) fn embed_header(chunk: &RawChunk) -> String {
    let file = chunk.file.as_str();
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

/// The text the embedder receives for `chunk`: the header plus the content
/// when `headed`, else the stored content unchanged (#10481).
/// Test: `the_header_is_embedded_but_never_stored`.
pub(crate) fn embed_text(chunk: &RawChunk, headed: bool) -> String {
    if headed {
        format!("{}{}", embed_header(chunk), chunk.content)
    } else {
        chunk.content.clone()
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

/// Byte ranges of the windows of `chunk` when header plus content exceed
/// `window`; `None` when the chunk fits whole.
///
/// What: a byte prefilter first (a token is at least one byte), so a short
/// chunk is never tokenized. Otherwise the content is cut for
/// `window - header_tokens`, the header counting against the budget.
fn plan(
    tokenizer: &ChunkTokenizer,
    window: usize,
    chunk: &RawChunk,
) -> Result<Option<Vec<Range<usize>>>, ChunkError> {
    let header = embed_header(chunk);
    if header.len() + chunk.content.len() + tokenizer.special_overhead() <= window {
        return Ok(None);
    }
    let room = window.saturating_sub(tokenizer.count_tokens(&header)?);
    let pieces = chunk_text(tokenizer, &chunk.content, room)?;
    if pieces.len() <= 1 {
        return Ok(None);
    }
    Ok(Some(pieces.into_iter().map(|p| p.byte_range).collect()))
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
/// Test: `a_chunk_over_the_window_splits_into_windows_that_each_fit`,
/// `consecutive_windows_overlap_by_the_common_chunker_constant`,
/// `a_chunk_within_the_window_keeps_its_id_and_content`,
/// `a_window_that_cannot_be_split_keeps_the_chunk_whole`,
/// `windows_of_a_one_line_chunk_get_distinct_ids`.
pub(crate) fn split_at_window(
    tokenizer: &ChunkTokenizer,
    window: usize,
    chunks: Vec<RawChunk>,
) -> (Vec<RawChunk>, SplitReport) {
    let mut report = SplitReport::default();
    let mut planned: Vec<(RawChunk, Option<Vec<Range<usize>>>)> = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let ranges = match plan(tokenizer, window, &chunk) {
            Ok(ranges) => ranges,
            Err(e) => {
                report.kept_whole += 1;
                report.first_error.get_or_insert_with(|| e.to_string());
                None
            }
        };
        planned.push((chunk, ranges));
    }
    // Ids of chunks that stay: a window must not take one of them.
    let mut used: HashSet<String> = planned
        .iter()
        .filter(|(_, ranges)| ranges.is_none())
        .map(|(c, _)| c.id.clone())
        .collect();
    let mut out = Vec::with_capacity(planned.len());
    for (chunk, ranges) in planned {
        let Some(ranges) = ranges else {
            out.push(chunk);
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
        }
    }
    (out, report)
}

impl CodeIndexer {
    /// Split `chunks` at the embedder's window ahead of an inline embed (#10481).
    ///
    /// Why: both inline embed paths (`index_file` and the full-pipeline parse)
    /// hand new or changed chunks straight to the embedder, which cuts them.
    /// What: returns `(chunks, headed)`. With no embedder or store, a
    /// vector-skipping index, or a window or tokenizer the embedder does not
    /// report, the chunks come back as they went in and `headed` is false: no
    /// split, no header, today's behaviour. A lazy sidecar reports its window
    /// only after its first answer, so a write before that answer goes unsplit;
    /// this method never sends a probe of its own, since an embedder that never
    /// reports a window would then pay one extra call per write.
    /// [`CodeIndexer::warm_embedder`] runs at the start of a full reindex.
    /// `headed` is true only when the window and a tokenizer are known, the case
    /// the header budget was computed in. A task panic fails the call, as the
    /// batch parse does.
    /// Test: `an_embedder_with_no_window_does_not_split`,
    /// `an_unknown_window_does_not_split_until_the_embedder_reports_it`.
    pub(crate) async fn split_for_embed(
        &self,
        chunks: Vec<RawChunk>,
    ) -> Result<(Vec<RawChunk>, bool)> {
        let (Some(embedder), Some(_store)) = (&self.embedder, &self.store) else {
            return Ok((chunks, false));
        };
        if self.skip_vector || chunks.is_empty() {
            return Ok((chunks, false));
        }
        let probe = WindowProbe::from_embedder(embedder.as_ref());
        let WindowProbe::Counting { window, tokenizer } = probe else {
            return Ok((chunks, false));
        };
        let (split, report) =
            tokio::task::spawn_blocking(move || split_at_window(&tokenizer, window, chunks))
                .await
                .context("window split task panicked")?;
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
        Ok((split, true))
    }
}
