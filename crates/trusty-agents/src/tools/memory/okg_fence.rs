//! Fencing search hits drawn from the agent's OKG store (#4533, DOC-63 §6.3
//! `S-4.4`/`S-4.5`/`S-4.6`).
//!
//! Why: `vector_search` is how the OKG store reaches a model turn, and until
//! this module it returned a bare JSON array — path, score, and a raw snippet
//! of ingested content — with no provenance and no fence. Memory drawers going
//! into the same model's SYSTEM prompt had been delimited and preambled since
//! #3928. That asymmetry is DOC-63 `S-4.2`: an assistant's drawers were
//! fenced; the knowledge store this epic fills from Gmail, Drive, Slack,
//! Notion and Granola was not.
//!
//! What: [`normalize`] turns the daemon's response into [`Hit`]s that retain
//! the ABSOLUTE file path alongside the display path; [`to_json`] reproduces
//! the pre-#4533 envelope byte-for-byte for every non-OKG corpus; and
//! [`render_fenced`] is the OKG path — it resolves each hit's trust label and
//! renders untrusted hits inside [`crate::untrusted::KNOWLEDGE_FENCE`], the
//! same fence memory drawers get.
//!
//! ## Where the label comes from — and why not from the chunk payload
//!
//! DOC-63 `S-4.4` prescribes carrying the label "into the trusty-search chunk
//! payload by the index feed". **That mechanism does not exist and cannot be
//! used as written.** Verified against `origin/main`:
//!
//! - `POST /indexes/{id}/index-file` accepts exactly `{path, content}`
//!   (`trusty-search/src/service/server/router.rs:232-236`). There is no
//!   metadata channel, and the struct is not `deny_unknown_fields`, so an
//!   extra key would be silently dropped rather than rejected — a carrier that
//!   fails invisibly.
//! - A returned hit is a `CodeChunk`
//!   (`trusty-search/src/core/indexer/types.rs:19-69`) with a fixed, typed
//!   field set and no tag list, attribute map, or free-form payload. Neither
//!   does the stored `RawChunk`.
//! - The markdown chunker has no frontmatter handling at all
//!   (`trusty-search/src/core/chunker/document.rs:107-183`): a `--- … ---`
//!   header survives verbatim in the FIRST chunk only. A file-level label
//!   would therefore reach exactly one chunk out of N — the "label stops at
//!   the file" failure `S-4.4` exists to forbid, in a subtler shape.
//!
//! So the label is resolved HERE, per hit, from the hit's own absolute path,
//! before the content reaches the model. That satisfies what `S-4.4` is
//! actually for — every chunk carries a label at the point of use, not just
//! the one that happened to contain the frontmatter — without a trusty-search
//! schema change this ticket does not own. The cost is one small local file
//! read per hit, bounded by the caller's `limit` (≤ 50), on a path that has
//! just completed a network round trip.
//!
//! ## Fail closed
//!
//! `S-4.6`: a hit from an OKG store whose label cannot be established is
//! fenced as untrusted. Every failure mode collapses to that in
//! [`trusty_kb::okg::trust::TrustLabel::of_entity_file`] — missing file,
//! unreadable file, no frontmatter, no `trust` key, unrecognised value. Labels
//! arrive incrementally over a corpus that already exists, so the entire
//! pre-#4532 corpus takes this path and must be safe on it.
//!
//! ## Not a guarantee
//!
//! Fencing is a mitigation. No delimiter reliably survives an adversarial
//! instruction (DOC-63 §6.5). The load-bearing control remains capability
//! reduction, pinned by `bundled_personas_pin_git_reach`.
//!
//! Test: `super::tests` — `okg_hits_are_fenced`,
//! `unlabelled_okg_hit_is_fenced`, `user_authored_okg_hit_is_not_fenced`,
//! `okg_fence_is_the_same_fence_memory_drawers_get`,
//! `okg_hit_cannot_escape_the_envelope`,
//! `non_okg_index_output_is_unchanged`.

use std::path::Path;

use serde_json::{Value, json};
use trusty_kb::okg::trust::TrustLabel;

use crate::untrusted::KNOWLEDGE_FENCE;

use super::recall::HIT_MAX_CHARS;

/// One normalized search hit.
///
/// Why: the pre-#4533 code collapsed the daemon's response straight to JSON
/// and discarded everything it did not print — including `file`, the absolute
/// path, which is the ONLY handle on the hit's trust label (see the module
/// doc). Keeping it is what makes the label resolvable at all.
/// What: `path` is what the model is shown (root-relative when the daemon
/// reports one); `file` is the absolute path used for label resolution and is
/// never displayed on its own.
/// Test: `normalize_daemon_hits_handles_wrapped_and_bare_arrays`,
/// `normalize_keeps_the_absolute_file_path`.
#[derive(Debug, Clone)]
pub(super) struct Hit {
    /// Display path, as the pre-#4533 envelope emitted it.
    pub(super) path: String,
    /// Absolute on-disk path, when the daemon reported one.
    pub(super) file: Option<String>,
    pub(super) score: Value,
    pub(super) snippet: String,
}

impl Hit {
    /// Resolve this hit's trust label, FAIL-CLOSED.
    ///
    /// Why/What: see the module doc. A hit with no absolute path, or one that
    /// is not absolute, cannot be resolved and is therefore untrusted — the
    /// same answer every other failure mode gives.
    /// Test: `unlabelled_okg_hit_is_fenced`, `hit_without_a_file_path_is_fenced`.
    fn trust(&self) -> TrustLabel {
        let Some(file) = self.file.as_deref() else {
            return TrustLabel::UntrustedExternal;
        };
        let path = Path::new(file);
        if !path.is_absolute() {
            return TrustLabel::UntrustedExternal;
        }
        TrustLabel::of_entity_file(path)
    }

    /// One fence entry: a header line naming the hit, then its snippet.
    ///
    /// The header is rendered through the same neutralizer as the body because
    /// `path` is attacker-influenceable too — a filename is content.
    fn as_entry(&self, label: TrustLabel) -> String {
        format!(
            "{} (score {}, trust: {})\n{}",
            self.path,
            self.score,
            label.as_str(),
            self.snippet
        )
    }
}

/// Reshape the daemon's search response into [`Hit`]s.
///
/// Why: the daemon wraps hits under `results`/`hits` depending on the route
/// version and names the text field `content`/`snippet`/`text`; collapsing
/// that here means one hit shape regardless of which backend answered.
/// What: accepts either a bare array or an object with a `results`/`hits`
/// array; truncates snippets to [`HIT_MAX_CHARS`]; caps at `limit`. The
/// display-path precedence (`path`, then `file_path`, then `file`) is the
/// pre-#4533 order with `file` appended, so no existing corpus's rendering
/// changes; `file` is captured separately and always.
/// Test: `normalize_daemon_hits_handles_wrapped_and_bare_arrays`,
/// `normalize_keeps_the_absolute_file_path`.
pub(super) fn normalize(body: &Value, limit: usize) -> Vec<Hit> {
    let arr = body
        .as_array()
        .or_else(|| body.get("results").and_then(Value::as_array))
        .or_else(|| body.get("hits").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();
    arr.into_iter()
        .take(limit)
        .map(|h| {
            let path = h
                .get("path")
                .or_else(|| h.get("file_path"))
                .or_else(|| h.get("file"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let file = h
                .get("file")
                .or_else(|| h.get("file_path"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let snippet_raw = h
                .get("content")
                .or_else(|| h.get("snippet"))
                .or_else(|| h.get("text"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| h.to_string());
            Hit {
                path,
                file,
                score: h.get("score").cloned().unwrap_or(Value::Null),
                snippet: snippet_raw.chars().take(HIT_MAX_CHARS).collect(),
            }
        })
        .collect()
}

/// The pre-#4533 JSON envelope, unchanged.
///
/// Why: only the agent's OWN OKG store is in scope for #4533. Every other
/// corpus — an attached tier-2 index, the embedded local code index — keeps
/// its exact prior output, because changing what those return is a prompt
/// change this ticket did not justify and did not test.
/// What: `[{path, score, snippet}]`, the same keys in the same order.
/// Test: `non_okg_index_output_is_unchanged`.
pub(super) fn to_json(hits: &[Hit]) -> String {
    let out: Vec<Value> = hits
        .iter()
        .map(|h| json!({ "path": h.path, "score": h.score, "snippet": h.snippet }))
        .collect();
    serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string())
}

/// Render OKG-store hits with untrusted content fenced.
///
/// Why/What/Test: see the module doc. Ordering is deliberate: any
/// user-authored hits come FIRST, then the fenced block, so the closing
/// delimiter is the last thing in the result and no trailing text can be read
/// as fenced content.
pub(super) fn render_fenced(hits: &[Hit], index_id: &str) -> String {
    if hits.is_empty() {
        return format!("No results in your knowledge store `{index_id}`.");
    }

    let labelled: Vec<(&Hit, TrustLabel)> = hits.iter().map(|h| (h, h.trust())).collect();
    let (untrusted, trusted): (Vec<_>, Vec<_>) =
        labelled.iter().partition(|(_, label)| label.is_untrusted());

    let mut out = format!(
        "{} result(s) from your knowledge store `{index_id}`.\n\n",
        hits.len()
    );

    if !trusted.is_empty() {
        out.push_str(
            "### Knowledge from a source you designated user-authored\n\
             These entries came from a local directory the operator marked as authored by the \
             user, so they are ordinary reference material.\n\n",
        );
        for (hit, label) in &trusted {
            out.push_str(&KNOWLEDGE_FENCE.render_entry(&hit.as_entry(*label)));
        }
        out.push('\n');
    }

    if !untrusted.is_empty() {
        out.push_str(
            &KNOWLEDGE_FENCE.wrap(
                untrusted
                    .iter()
                    .map(|(hit, label)| hit.as_entry(*label))
                    .collect::<Vec<_>>()
                    .iter()
                    .map(String::as_str),
            ),
        );
    }

    out
}
