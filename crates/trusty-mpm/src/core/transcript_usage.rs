//! Fold a Claude Code session transcript into its tokens-in / tokens-out pair
//! (#7074).
//!
//! Why: the owner asked for the token counts a session spent to reach a commit,
//! and no `tm`-owned store holds them. The `statusLine` payload carries a cost
//! in dollars and a context-window size, never an output-token count, so the
//! session's own transcript is the only place the pair exists. Reading it at
//! commit time — rather than accumulating a third counter on the status bar's
//! hot render path — keeps this feature a pure reader, matching the savings
//! ledger's own read-time-fold rule.
//!
//! What: streams the JSON-Lines transcript Claude Code writes at
//! `<CLAUDE_CONFIG_DIR>/projects/<slug>/<session_id>.jsonl` and sums the
//! `message.usage` figures of every assistant message.
//!
//! Two properties a caller depends on:
//!
//! - **One message is counted once.** Claude Code writes one transcript LINE
//!   per content block, and every line of one assistant turn repeats that
//!   turn's identical `usage` object. Summing lines therefore triple-counts a
//!   three-block turn. The fold dedupes on `message.id`, so the count is per
//!   message, not per line.
//! - **Tokens-in is what was actually sent.** `input_tokens` alone excludes the
//!   cached prefix, which is the bulk of a long session's input; the sum here
//!   is `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`.
//!
//! The read is BOUNDED. `git` blocks on the `prepare-commit-msg` hook that
//! reaches this fold, and a long session's transcript grows into the hundreds
//! of megabytes, so an uncapped stream would slow every commit for the rest of
//! that session. The fold reads at most [`TRANSCRIPT_TAIL_BYTES`] from the end
//! of the file and reports [`TranscriptUsage::truncated`] when it did, because
//! the store behind it holds only the transcript's PATH — no running per-session
//! totals exist to make a tail read stand for the whole session.
//!
//! Everything fails soft: a missing, unreadable, or truncated transcript folds
//! to zero, and a line that does not parse is skipped with a `warn!` rather
//! than aborting the fold — a commit must never fail over a stats read.
//!
//! Test: the inline suite in `transcript_usage_tests.rs` —
//! `fold_sums_one_message_once_per_id`,
//! `fold_counts_cache_tokens_as_tokens_in`,
//! `fold_of_a_missing_transcript_is_empty`,
//! `fold_skips_a_malformed_line_and_keeps_the_valid_total`,
//! `fold_reads_only_the_tail_of_an_oversized_transcript`,
//! `fold_of_a_transcript_within_the_cap_is_not_truncated`.

use std::collections::HashSet;
use std::io::{BufRead as _, Seek as _};
use std::path::Path;

use serde::Deserialize;

/// How much of a transcript's tail [`fold_transcript`] reads, in bytes.
///
/// Why: `git` blocks on the `prepare-commit-msg` hook this fold runs inside, so
/// the read's cost is paid by every commit of the session. 8 MiB is read in
/// tens of milliseconds and still spans hundreds of assistant turns, so an
/// ordinary session is never truncated at all, while a transcript that has
/// grown to hundreds of megabytes costs the same bounded read as a small one.
/// What: when the file is larger than this, the fold starts this many bytes
/// before EOF and discards the partial line at that offset, so every line it
/// parses is whole.
/// Test: `fold_reads_only_the_tail_of_an_oversized_transcript`,
/// `fold_of_a_transcript_within_the_cap_is_not_truncated`.
pub const TRANSCRIPT_TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// The tokens one session sent and received, folded from its transcript.
///
/// Why: the two figures travel together into the commit footer, and `messages`
/// is what distinguishes "the transcript held no assistant turn yet" from "the
/// file could not be read at all" in a bug report.
/// What: sums over deduplicated assistant messages.
/// Test: `fold_sums_one_message_once_per_id`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TranscriptUsage {
    /// Input tokens actually sent, cache creation and cache reads included.
    pub tokens_in: u64,
    /// Output tokens the model produced.
    pub tokens_out: u64,
    /// How many distinct assistant messages were counted.
    pub messages: usize,
    /// Whether the byte cap stopped the fold short of the whole transcript.
    ///
    /// Why: when this is set the three figures above describe the tail window,
    /// not the session, and a caller rendering them owes the reader that
    /// distinction — the commit footer says so on its own `Tokens-Window` line.
    /// Test: `fold_reads_only_the_tail_of_an_oversized_transcript`.
    pub truncated: bool,
}

impl TranscriptUsage {
    /// Whether the fold found no assistant message to count.
    ///
    /// Test: `fold_of_a_missing_transcript_is_empty`.
    pub fn is_empty(&self) -> bool {
        self.messages == 0
    }
}

/// One transcript line, reduced to the two fields this fold reads.
///
/// Why: `#[serde(default)]` on every field and no `deny_unknown_fields` means a
/// schema addition on Claude Code's side costs nothing here — a user line, a
/// tool result, or a summary line simply carries no `message.usage` and is
/// skipped.
/// What: `message.id` (the dedup key) and `message.usage`.
/// Test: `fold_sums_one_message_once_per_id`.
#[derive(Debug, Deserialize)]
struct TranscriptLine {
    #[serde(default)]
    message: Option<TranscriptMessage>,
}

#[derive(Debug, Deserialize)]
struct TranscriptMessage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    usage: Option<MessageUsage>,
}

#[derive(Debug, Default, Deserialize)]
struct MessageUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

/// Cheap pre-filter: only a line mentioning `"usage"` can contribute.
///
/// Why: a long transcript is mostly tool results and user turns, and running
/// `serde_json` over all of them would make this fold cost far more than the
/// commit it runs inside. A substring test rejects those lines before any
/// parse.
/// What: a plain `contains`; a false positive costs one wasted parse, and a
/// false negative is impossible because the key is spelled this way in every
/// assistant line.
/// Test: covered by `fold_sums_one_message_once_per_id` (the fixture's user
/// lines carry no `usage` and must not be counted).
fn may_carry_usage(line: &str) -> bool {
    line.contains("\"usage\"")
}

/// Fold `transcript` into its tokens-in / tokens-out pair.
///
/// Why/What: see the module doc — reads at most [`TRANSCRIPT_TAIL_BYTES`] from
/// the end of the file, dedupes on `message.id`, and sums. A missing or
/// unreadable file, and a line that does not parse, each cost only what they
/// carried.
/// Test: `fold_sums_one_message_once_per_id`,
/// `fold_counts_cache_tokens_as_tokens_in`,
/// `fold_of_a_missing_transcript_is_empty`,
/// `fold_skips_a_malformed_line_and_keeps_the_valid_total`,
/// `fold_reads_only_the_tail_of_an_oversized_transcript`.
pub fn fold_transcript(transcript: &Path) -> TranscriptUsage {
    fold_transcript_tail(transcript, TRANSCRIPT_TAIL_BYTES)
}

/// [`fold_transcript`] with the byte cap as a parameter.
///
/// Why: a test proving the cap holds would otherwise have to generate a real
/// 8 MiB fixture on every run. Taking the limit as an argument lets the same
/// code path be proven against a few hundred bytes.
/// What: seeks to `limit` bytes before EOF when the file is larger, discards
/// the partial line at that offset, then folds forward to EOF.
/// Test: `fold_reads_only_the_tail_of_an_oversized_transcript`,
/// `fold_of_a_transcript_within_the_cap_is_not_truncated`.
fn fold_transcript_tail(transcript: &Path, limit: u64) -> TranscriptUsage {
    let Ok(file) = std::fs::File::open(transcript) else {
        // Absent is the ordinary state for a session with no transcript yet.
        return TranscriptUsage::default();
    };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return TranscriptUsage::default();
    };

    let mut total = TranscriptUsage::default();
    let mut reader = std::io::BufReader::new(file);
    if len > limit {
        total.truncated = true;
        // The seek lands mid-line; that partial line is read and thrown away so
        // every line the fold then parses is whole. A window holding no newline
        // at all consumes to EOF here and folds to zero, which is the same
        // fail-soft outcome as an unreadable file.
        if reader.seek(std::io::SeekFrom::Start(len - limit)).is_err() {
            return TranscriptUsage::default();
        }
        let mut partial = Vec::new();
        if reader.read_until(b'\n', &mut partial).is_err() {
            return TranscriptUsage::default();
        }
    }

    let mut seen: HashSet<String> = HashSet::new();
    // `index` counts lines within the window read, not from the file's start.
    for (index, line) in reader.lines().enumerate() {
        let Ok(line) = line else {
            tracing::warn!(
                transcript = %transcript.display(),
                line = index + 1,
                "stopping the transcript fold at an unreadable line"
            );
            break;
        };
        if line.trim().is_empty() || !may_carry_usage(&line) {
            continue;
        }
        let parsed: TranscriptLine = match serde_json::from_str(&line) {
            Ok(parsed) => parsed,
            Err(source) => {
                tracing::warn!(
                    transcript = %transcript.display(),
                    line = index + 1,
                    %source,
                    "skipping a malformed transcript line"
                );
                continue;
            }
        };
        let Some(message) = parsed.message else {
            continue;
        };
        let Some(usage) = message.usage else {
            continue;
        };
        // A message with no id cannot be deduped, so it is counted once on the
        // line that carries it — the alternative, dropping it, would undercount
        // a shape Claude Code has not written but might.
        if let Some(id) = message.id
            && !seen.insert(id)
        {
            continue;
        }
        total.tokens_in +=
            usage.input_tokens + usage.cache_creation_input_tokens + usage.cache_read_input_tokens;
        total.tokens_out += usage.output_tokens;
        total.messages += 1;
    }
    total
}

#[cfg(test)]
#[path = "transcript_usage_tests.rs"]
mod tests;
