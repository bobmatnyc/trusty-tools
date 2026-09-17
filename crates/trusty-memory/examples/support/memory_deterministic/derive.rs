//! Pure source-preserving representations; no generated facts or summaries.
use super::*;
use trusty_common::bm25::tokenize;

/// Fixed source-byte safety limit, independent of the tuned occurrence budget.
pub const MAX_CHUNK_BYTES: usize = 4096;

/// Count repeated whitespace-delimited runs repeatedly; the BM25 tokenizer
/// deduplicates its returned vocabulary and is not a document-length measure.
pub fn token_occurrences(text: &str) -> usize {
    text.split_whitespace().map(|run| tokenize(run).len()).sum()
}

fn fits(text: &str, max_tokens: usize) -> bool {
    text.len() <= MAX_CHUNK_BYTES && token_occurrences(text) <= max_tokens
}

pub fn context(source: &Source, treatment: Treatment, policy: &Policy) -> String {
    if treatment < Treatment::Context {
        return String::new();
    }
    let d = &source.drawer;
    let fields = std::iter::once(d.room.as_str())
        .chain(d.tags.iter().map(String::as_str))
        .chain(d.fact_key.iter().map(String::as_str))
        .chain(d.aliases.iter().map(String::as_str));
    let mut terms = std::collections::BTreeSet::new();
    for field in fields {
        terms.extend(tokenize(field));
    }
    let mut context = String::new();
    for term in terms {
        let trial = format!("{context} {term}");
        if tokenize(&trial).len() <= policy.context_tokens {
            context = trial;
        }
    }
    context.trim().into()
}

pub fn ranges(body: &str, max_tokens: usize, split: bool) -> Vec<(usize, usize)> {
    if !split || fits(body, max_tokens) {
        return vec![(0, body.len())];
    }
    // Prefer paragraph boundaries; oversized paragraphs fall back to whitespace
    // boundaries, then scalar boundaries for a single run that exceeds either
    // lexical expansion occurrences or the fixed UTF-8 source-byte cap.
    let mut units = Vec::new();
    let mut start = 0;
    for (offset, _) in body.match_indices("\n\n") {
        units.push((start, offset + 2));
        start = offset + 2;
    }
    if start < body.len() {
        units.push((start, body.len()));
    }
    let mut fine = Vec::new();
    for (start, end) in units {
        if fits(&body[start..end], max_tokens) {
            fine.push((start, end));
            continue;
        }
        let mut cursor = start;
        for (offset, ch) in body[start..end].char_indices() {
            if ch.is_whitespace() {
                let next = start + offset + ch.len_utf8();
                if next > cursor {
                    fine.push((cursor, next));
                    cursor = next;
                }
            }
        }
        if cursor < end {
            fine.push((cursor, end));
        }
    }
    let mut output = Vec::new();
    let mut chunk_start = 0;
    let mut chunk_end = 0;
    for (start, end) in fine {
        if fits(&body[chunk_start..end], max_tokens) {
            chunk_end = end;
            continue;
        }
        if chunk_end > chunk_start {
            output.push((chunk_start, chunk_end));
        }
        chunk_start = start;
        chunk_end = start;
        if fits(&body[start..end], max_tokens) {
            chunk_end = end;
            continue;
        }
        for (offset, ch) in body[start..end].char_indices() {
            let next = start + offset + ch.len_utf8();
            if !fits(&body[chunk_start..next], max_tokens) {
                output.push((chunk_start, chunk_end));
                chunk_start = chunk_end;
            }
            chunk_end = next;
        }
    }
    if chunk_end > chunk_start {
        output.push((chunk_start, chunk_end));
    }
    output
}

pub fn rows(
    source: &Source,
    treatment: Treatment,
    policy: &Policy,
    fingerprint: &str,
) -> (Vec<Document>, Vec<Derived>, usize) {
    let d = &source.drawer;
    let context = context(source, treatment, policy);
    let mut documents = Vec::new();
    let mut derived = Vec::new();
    let mut bytes = 0;
    for (child, (start, end)) in
        ranges(&d.body, policy.chunk_tokens, treatment >= Treatment::Chunks)
            .into_iter()
            .enumerate()
    {
        let id = doc_id(&d.scope, &d.id, child);
        let mut text = d.body[start..end].to_owned();
        if !context.is_empty() {
            text.push('\n');
            text.push_str(&context);
        }
        bytes += text.len();
        documents.push(Document {
            doc_id: id.clone(),
            text,
        });
        derived.push(Derived {
            doc_id: id,
            scope: d.scope.clone(),
            id: d.id.clone(),
            revision: source.revision,
            fingerprint: fingerprint.into(),
            body_digest: digest(d.body.as_bytes()),
            byte_start: start,
            byte_end: end,
            line_start: 1 + d.body[..start].bytes().filter(|b| *b == b'\n').count(),
            line_end: 1 + d.body.as_bytes()[..end.saturating_sub(1)]
                .iter()
                .filter(|b| **b == b'\n')
                .count(),
        });
    }
    (documents, derived, bytes)
}
