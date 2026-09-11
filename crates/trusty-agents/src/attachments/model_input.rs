//! What an attached turn says to the model, and how a reload reads it back (#7370).
//!
//! Why: the prompt assembly path builds a plain `.content(String)` for the user
//! turn (`ctrl::pm_task::dispatch::persona_prompt`), and this slice deliberately
//! does not change that — no provider vision payloads, no multipart content
//! arrays. So an attachment reaches the model the only way a string can carry
//! it: text and CSV as their own contents under a fenced block, everything else
//! as a one-line reference naming what was attached. A binary file described
//! rather than inlined is honest; base64 in the prompt would be neither
//! readable by the model nor affordable.
//!
//! One string serves both consumers. The turn that is persisted is the turn the
//! model saw — the same bytes, not a summary of them — because the prior
//! attempt on this issue was blocked precisely for letting the persisted
//! history and the delivered turn diverge. Rehydration then needs no second
//! source of truth: [`parse_markers`] reads the ids back out of the stored
//! content and the cards are rebuilt from the manifest.
//!
//! What: [`marker_for`] / [`parse_markers`] own the `[[attachment:<id>]]`
//! marker; [`render`] turns one row plus its bytes into the block that follows
//! its marker; [`augment_user_turn`] joins the user's own text to those blocks.
//!
//! Test: `super::tests::model_input_tests`.

use super::manifest::Attachment;
use super::store::is_attachment_id;

/// Opening delimiter of the in-content attachment marker.
pub const MARKER_OPEN: &str = "[[attachment:";
/// Closing delimiter of the in-content attachment marker.
pub const MARKER_CLOSE: &str = "]]";

/// Most bytes of a text or CSV attachment inlined into one turn (32 KiB).
///
/// Why: the turn is persisted and replayed as conversation history on EVERY
/// later turn in the session, so an inlined file is paid for repeatedly, not
/// once. 32 KiB is roughly 8k tokens — enough for a spreadsheet export or a log
/// excerpt to be answered about, small enough that a session carrying several
/// does not crowd out the conversation itself.
/// Test: `super::tests::model_input_tests::text_is_capped_with_a_truncation_note`.
pub const MAX_INLINE_TEXT_BYTES: usize = 32 * 1024;

/// The marker that refers to `id` from inside a turn's content.
///
/// Test: `super::tests::model_input_tests::markers_round_trip`.
pub fn marker_for(id: &str) -> String {
    format!("{MARKER_OPEN}{id}{MARKER_CLOSE}")
}

/// Every attachment id referenced by `content`, in order, without repeats.
///
/// Why: rehydration reads the persisted turn and has to know which cards to
/// rebuild. Ids are SHAPE-checked here rather than trusted, so ordinary prose
/// that happens to contain the delimiters cannot make a reload go looking for
/// an attachment that was never stored.
/// What: scans for `[[attachment:` … `]]` pairs and keeps the payloads that
/// pass [`is_attachment_id`], preserving first-seen order.
/// Test: `super::tests::model_input_tests::markers_round_trip`,
/// `super::tests::model_input_tests::parse_ignores_malformed_markers`.
pub fn parse_markers(content: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut rest = content;
    while let Some(open) = rest.find(MARKER_OPEN) {
        let after = &rest[open + MARKER_OPEN.len()..];
        let Some(close) = after.find(MARKER_CLOSE) else {
            break;
        };
        let candidate = &after[..close];
        if is_attachment_id(candidate) && !found.iter().any(|seen| seen == candidate) {
            found.push(candidate.to_string());
        }
        rest = &after[close + MARKER_CLOSE.len()..];
    }
    found
}

/// The block one attachment contributes to the user turn.
///
/// Why: the marker has to be present whatever the media type, because it is
/// what a reload resolves the card from. Only the BODY differs — inlined
/// contents for text, a description for everything else.
/// What: for a text media type, the marker line, a header naming the file, and
/// a fenced block holding up to [`MAX_INLINE_TEXT_BYTES`] of the decoded
/// contents with an explicit truncation note when it did not all fit. For any
/// other media type, the marker line and a single reference line — name, media
/// type, size — and nothing else.
/// Test: `super::tests::model_input_tests::text_is_inlined_under_a_fence`,
/// `super::tests::model_input_tests::binary_is_one_reference_line`,
/// `super::tests::model_input_tests::text_is_capped_with_a_truncation_note`.
pub fn render(attachment: &Attachment, bytes: &[u8]) -> String {
    let marker = marker_for(&attachment.id);
    let header = format!(
        "{marker} {} ({}, {} bytes)",
        attachment.file_name, attachment.media_type, attachment.size
    );
    if !is_inlinable_text(&attachment.media_type) {
        return format!("{header} — binary attachment; its contents are not included.");
    }
    let (text, truncated) = decode_capped(bytes, MAX_INLINE_TEXT_BYTES);
    let language = if attachment.media_type == "text/csv" {
        "csv"
    } else {
        "text"
    };
    let fence = fence_for(&text);
    let note = if truncated {
        format!(
            "\n[truncated: the first {} of {} bytes are shown]",
            text.len(),
            bytes.len()
        )
    } else {
        String::new()
    };
    format!("{header}\n{fence}{language}\n{text}{note}\n{fence}")
}

/// The fence that can hold `content` without it breaking out.
///
/// Why: an attachment is arbitrary text, and a Markdown file or a code listing
/// routinely contains a fence of its own. A fixed three-backtick fence ends at
/// the file's first one, and the rest of the file is then read as prose - the
/// model sees a truncated attachment and instructions that were never given to
/// it. CommonMark ends a fenced block only at a run of AT LEAST as many
/// backticks as opened it, so opening with one more than the longest run
/// inside makes the content unable to close it.
/// What: a run of `max(longest run in content, 2) + 1` backticks - three for
/// ordinary content, four for content holding a three-backtick run, and so on.
/// Test: `super::tests::model_input_tests::a_fence_inside_the_content_cannot_break_out`.
fn fence_for(content: &str) -> String {
    let mut run = 0usize;
    let mut longest = 0usize;
    for byte in content.bytes() {
        if byte == b'`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(longest.max(2) + 1)
}

/// Join the user's own text to the rendered attachment blocks.
///
/// Why: the user's words come FIRST so the instruction is not buried under a
/// spreadsheet, and the blocks are blank-line separated so a model reading the
/// turn sees them as distinct attachments rather than one run-on document.
/// What: `user_text` verbatim when `blocks` is empty — a turn with no
/// attachments is byte-identical to what it was before this feature existed.
/// Test: `super::tests::model_input_tests::no_attachments_leaves_the_turn_untouched`,
/// `super::tests::model_input_tests::blocks_follow_the_users_text`.
pub fn augment_user_turn(user_text: &str, blocks: &[String]) -> String {
    if blocks.is_empty() {
        return user_text.to_string();
    }
    let joined = blocks.join("\n\n");
    if user_text.trim().is_empty() {
        return joined;
    }
    format!("{}\n\n{joined}", user_text.trim_end())
}

/// Whether a media type's bytes are worth inlining as text.
///
/// Why: `text/*` covers plain text, CSV, Markdown and HTML. The three
/// structured formats below are textual in practice and are what an assistant
/// is most often handed next to a CSV, so they are named rather than left to
/// the binary path. Public because the caller assembling a turn asks this
/// BEFORE reading a file — a 10 MiB binary that will be rendered as one
/// reference line must not be loaded into memory to produce it.
/// Test: `super::tests::model_input_tests::binary_is_one_reference_line`.
pub fn is_inlinable_text(media_type: &str) -> bool {
    media_type.starts_with("text/")
        || matches!(
            media_type,
            "application/json" | "application/xml" | "application/x-ndjson"
        )
}

/// Decode `bytes` as UTF-8, truncated to at most `cap` bytes.
///
/// Why: the cap is in BYTES (it is a prompt-size budget) but the result must be
/// valid UTF-8, so the cut is walked back to a character boundary rather than
/// taken blind. `from_utf8_lossy` handles a file that was never UTF-8 at all.
/// What: `(text, truncated)`.
fn decode_capped(bytes: &[u8], cap: usize) -> (String, bool) {
    if bytes.len() <= cap {
        return (String::from_utf8_lossy(bytes).into_owned(), false);
    }
    let text = String::from_utf8_lossy(bytes).into_owned();
    let mut end = cap.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}
