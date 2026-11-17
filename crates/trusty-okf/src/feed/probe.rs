//! The retrieval and withdrawal probes (SPEC-OKF-01 §4.9 "The retrieval
//! probe", §5.4 F6, F9, F12, F13).
//!
//! Why: an acknowledged put is not retrieval proof (§1.1 item 13), and text
//! the current and a prior revision share is never evidence (r6, row 4).
//! The probe is the only path to Searchable.
//! What: [`retrieval`] runs check 1 (a lexical `search.query` scoped to the
//! path returns a hit whose text occurs in the current bytes) and check 2
//! (with D1: `search.index.file.status` reports the current bytes' hash
//! under the identity the probe record carries;
//! without D1: the path's chunks each occur verbatim in the current file and
//! together cover every non-whitespace character of the body). [`withdrawal`]
//! passes only when the scoped query and the path's chunk list are both
//! empty. A call that fails is an `Err`: the probe could not run (F13).
//! #10746 S7.
//! Test: `c133_old_chunks_fail_the_probe_and_the_new_ones_pass`,
//! `c63_acknowledgement_alone_is_not_searchable_through_the_feed`,
//! `chunk_coverage_needs_every_body_character`,
//! `file_status_with_a_null_identity_is_never_searchable`,
//! `file_status_with_another_identity_is_never_searchable`.

use serde_json::{Value, json};

use super::wire::{CHUNKS_LIST, Chunk, FILE_STATUS, IndexStatus, QUERY, chunks_at, parse_identity};
use super::{IndexFeed, SearchError};
use crate::state::IndexIdentity;

/// Page size for `search.chunks.list` (the daemon caps it at 1000).
const PAGE: usize = 1000;

/// Words of the body the retrieval query carries (§4.9).
const QUERY_WORDS: usize = 12;

/// Every chunk under `prefix`, paged by offset.
pub(crate) async fn chunks_under(
    sink: &dyn IndexFeed,
    index_id: &str,
    prefix: &str,
) -> Result<Vec<Chunk>, SearchError> {
    let mut out = Vec::new();
    loop {
        let reply = sink
            .call(
                CHUNKS_LIST,
                json!({"index_id": index_id, "path_prefix": prefix, "limit": PAGE, "offset": out.len()}),
            )
            .await?;
        let page = chunks_at(&reply, "chunks");
        let total = reply.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
        let done = page.is_empty();
        out.extend(page);
        if done || out.len() >= total {
            return Ok(out);
        }
    }
}

/// Hits for `text` scoped to `prefix`. The probe asks the lexical lane only
/// (§4.9); `retrieve` (`lexical` false) takes the daemon's default lanes.
pub(crate) async fn query(
    sink: &dyn IndexFeed,
    index_id: &str,
    text: &str,
    prefix: Option<&str>,
    top_k: usize,
    lexical: bool,
) -> Result<Vec<Chunk>, SearchError> {
    let mut body = json!({
        "text": text, "top_k": top_k, "mode": "all", "compact": false, "expand_graph": false,
    });
    if lexical {
        body["stage"] = json!("lexical");
    }
    if let Some(p) = prefix.filter(|p| !p.is_empty()) {
        body["path_prefix"] = json!(p);
    }
    let reply = sink
        .call(QUERY, json!({"index_id": index_id, "body": body}))
        .await?;
    Ok(chunks_at(&reply, "results"))
}

/// The probe query: the first 12 words of the body, else the title.
fn query_text(body: &str, title: Option<&str>) -> Option<String> {
    let words: Vec<&str> = body.split_whitespace().take(QUERY_WORDS).collect();
    if !words.is_empty() {
        return Some(words.join(" "));
    }
    title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Whether the chunk texts cover every non-whitespace character of `body`,
/// each text occurring verbatim in `content` (§4.9 "Chunk coverage").
///
/// What: an empty chunk set, an empty chunk text, or a text absent from
/// `content` fails. `body` is the tail of `content` after the frontmatter;
/// when it is not, the whole of `content` must be covered.
pub(crate) fn covers(chunks: &[&str], content: &str, body: &str) -> bool {
    if chunks.is_empty() {
        return false;
    }
    let mut covered = vec![false; content.len()];
    for c in chunks {
        if c.trim().is_empty() || !content.contains(c) {
            return false;
        }
        for (i, m) in content.match_indices(c) {
            covered[i..i + m.len()].iter_mut().for_each(|b| *b = true);
        }
    }
    let start = if content.ends_with(body) {
        content.len() - body.len()
    } else {
        0
    };
    content[start..]
        .char_indices()
        .all(|(i, ch)| ch.is_whitespace() || covered[start + i])
}

/// The revision a probe checks: the exact text the put sent.
pub(crate) struct Sent<'a> {
    /// Index-relative path.
    pub(crate) wire: &'a str,
    /// The text sent.
    pub(crate) content: &'a str,
    /// Its body (after the frontmatter).
    pub(crate) body: &'a str,
    /// Its title, for an empty body.
    pub(crate) title: Option<&'a str>,
    /// Lower-case hex SHA-256 of `content`.
    pub(crate) hex: &'a str,
    /// The identity the probe record will carry; with D1 the `file.status`
    /// reply must name exactly this identity.
    pub(crate) identity: Option<&'a IndexIdentity>,
}

/// Check 1 and check 2 for one path's sent bytes (§4.9).
///
/// What: `Ok(true)` only when both checks pass for `sent`.
pub(crate) async fn retrieval(
    sink: &dyn IndexFeed,
    index_id: &str,
    status: &IndexStatus,
    sent: &Sent<'_>,
) -> Result<bool, SearchError> {
    let Sent {
        wire,
        content,
        body,
        title,
        hex: sent_hex,
        ..
    } = *sent;
    let Some(text) = query_text(body, title) else {
        return Ok(false);
    };
    let hits = query(sink, index_id, &text, Some(wire), 10, true).await?;
    let at_path = |c: &Chunk| c.wire_path(&status.root).as_deref() == Some(wire);
    let retrieved = hits
        .iter()
        .any(|h| at_path(h) && !h.content.trim().is_empty() && content.contains(&h.content));
    if !retrieved {
        return Ok(false);
    }
    if status.d1() {
        let reply = sink
            .call(
                FILE_STATUS,
                json!({"index_id": index_id, "body": {"path": wire}}),
            )
            .await?;
        let present = reply.get("present").and_then(Value::as_bool) == Some(true);
        let hash = reply.get("content_sha256").and_then(Value::as_str);
        // #10746: the hash counts only under the live identity the record
        // carries; a null, malformed or other identity is not evidence (§4.9).
        let observed = reply.get("identity").and_then(parse_identity);
        let same = observed.is_some() && observed.as_ref() == sent.identity;
        return Ok(same && present && hash.is_some_and(|h| h.eq_ignore_ascii_case(sent_hex)));
    }
    let chunks = chunks_under(sink, index_id, wire).await?;
    let texts: Vec<&str> = chunks
        .iter()
        .filter(|c| at_path(c))
        .map(|c| c.content.as_str())
        .collect();
    Ok(covers(&texts, content, body))
}

/// The withdrawal probe: no hit and no chunk for the path (§4.9, F9).
pub(crate) async fn withdrawal(
    sink: &dyn IndexFeed,
    index_id: &str,
    status: &IndexStatus,
    wire: &str,
) -> Result<bool, SearchError> {
    let at_path = |c: &Chunk| c.wire_path(&status.root).as_deref() == Some(wire);
    if chunks_under(sink, index_id, wire)
        .await?
        .iter()
        .any(at_path)
    {
        return Ok(false);
    }
    let stem = wire
        .rsplit('/')
        .next()
        .unwrap_or(wire)
        .trim_end_matches(".md");
    let hits = query(sink, index_id, stem, Some(wire), 10, true).await?;
    Ok(!hits.iter().any(at_path))
}
