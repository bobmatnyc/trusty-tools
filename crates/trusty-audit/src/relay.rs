//! Reading a child's output: credentials removed, everything else to the log,
//! the progress lines onward.
//!
//! Why: #5823. `crate::run` gave each `tga audit` child a log file as its
//! stdout and stderr and then awaited it, so the four-hour budget could elapse
//! with nothing on screen. The child does report its stages — nine of them, on
//! its own progress bus — but through a pipe that reached only the file.
//!
//! The log is not the thing to give up in exchange. A sweep is unattended and
//! the log is how a failure is diagnosed afterwards, so this module TEES: every
//! byte still lands in the log, and the lines that are progress events are
//! additionally decoded and forwarded to the display.
//!
//! What: [`tee_and_relay`], one call per stream, filtered by a [`Scrubber`].
//!
//! ## Why the tee scrubs (#5869)
//!
//! The child is handed the OpenRouter credential in its environment, and it
//! spawns `trusty-review` with the same. Any process in that chain can echo a
//! credential — a provider's non-2xx body quoted verbatim, a debug line, a
//! `git` remote URL carrying a token in a clone failure — and until #5869 those
//! bytes went straight to disk. A human opening the log to diagnose a failure
//! read the key in plaintext, and the planned guided-help path would send
//! bounded excerpts of that same log to OpenRouter inside a prompt body.
//!
//! **What this cannot do.** [`scrub_secrets`] removes only values this process
//! already holds. A secret the child derived, fetched over the network, or read
//! from its own config under a name no registry entry knows passes through
//! untouched. The log is therefore LOWER-RISK, NOT PROVEN CLEAN — treat it as
//! sensitive still.
//!
//! ## Why bytes rather than lines of text
//!
//! A child's output is not guaranteed to be UTF-8 — one `git` message in an
//! unexpected encoding is enough. Reading `String` lines would end the pump at
//! the first such byte and truncate the log from there, turning a cosmetic
//! feature into evidence loss. So the pump is byte-oriented throughout.
//!
//! ## One search space, no invented boundaries
//!
//! A needle is removed only if the scrubber sees it whole, so every boundary
//! this module invents is somewhere a credential can hide in two halves. Three
//! such boundaries existed and all three leaked (#5869 review rounds):
//!
//! - The pump used to scrub only the part of its buffer it was about to write,
//!   so a needle straddling that cut was split, matched neither half, and both
//!   halves reached the log verbatim. The pump now scrubs the WHOLE buffer and
//!   cuts the result.
//! - The mixed-encoding path used to scrub each valid-UTF-8 run separately, so
//!   one invalid byte injected between a credential's characters split it the
//!   same way. A needle cannot CONTAIN an invalid byte, but nothing stops one
//!   being injected INTO it. [`Scrubber::scrub`] now searches the runs
//!   concatenated — invalid bytes elided — and maps each match back to the
//!   segment's own byte offsets.
//! - The fix for the second one invented the third. The hold-back is counted in
//!   TEXT bytes and was then clamped in RAW bytes, and the two units disagree
//!   exactly when invalid padding sits inside an arrived prefix: the raw clamp
//!   moved the cut PAST the text walk's boundary and wrote the prefix out. Any
//!   bound stated in one unit and applied in the other is this defect again.
//!
//! What survives is a hold-back: bytes at the tail of the buffer that the pump
//! keeps rather than writes, because a needle that has only PARTLY ARRIVED
//! cannot be matched yet. That is a bound on the stream, not a boundary in the
//! search space — and the bound is now enforced without moving the cut. The
//! hold-back's raw size is capped by dropping invalid padding OUT of it, which
//! the pump then writes; the text the walk kept is never given up
//! ([`Scrubber::cut_for`]).
//!
//! Test: `super::relay_tests`.

use std::borrow::Cow;
use std::sync::Arc;

use tokio::io::{
    AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWriteExt as _, BufReader,
};
use trusty_common::credentials::scrub_secrets;
use trusty_progress::relay::StageEvent;

use crate::progress::Progress;

/// Most bytes the pump will accumulate from one read before forcing them out.
///
/// Why: the child's output is a stream and a line is only a line once a newline
/// arrives. `read_until` alone will grow its buffer until one does, so a child
/// that prints a megabyte-per-second single line with no newline — a stuck
/// progress spinner, a base64 blob — is an unbounded allocation in a process
/// that must survive four hours. Bounding the read turns that into a flush.
/// Test: `super::relay_tests::a_line_that_never_ends_does_not_grow_without_bound`.
const SEGMENT_LIMIT: usize = 256 * 1024;

/// Most bytes held back across a mid-line flush, whatever the needles say.
///
/// Why: the hold-back exists so a credential that has only PARTLY ARRIVED is
/// still matched next round, so it wants to be as long as the longest needle.
/// It must also stay far below [`SEGMENT_LIMIT`] or the pump stops making
/// progress — it would re-hold everything it just read. A pathological "secret"
/// longer than this is bounded here and loses only the partly-arrived case,
/// never the ordinary one.
///
/// It bounds the hold-back twice, in the two units the hold-back is counted in.
/// [`Scrubber::held_back`] is a count of TEXT bytes and is capped here at
/// construction; the RAW span those text bytes occupy is capped here again,
/// because invalid bytes injected among them make the span arbitrarily longer.
///
/// **The raw cap must never be applied by moving the cut.** It was, until the
/// #5869 re-verify round: the cut was computed by walking text bytes and then
/// clamped in raw bytes, so ~4KB of invalid padding inside an arrived prefix
/// pushed the cut PAST the walk's boundary and wrote that prefix to the log in
/// the clear — the remainder then arrived without it and matched nothing. The
/// cap is now honoured by dropping invalid padding out of the hold
/// ([`Scrubber::cut_for`]), which is bytes the pump writes rather than bytes it
/// evicts.
/// Test: `super::relay_tests::an_absurd_needle_cannot_stall_the_pump`,
/// `super::relay_tests::a_padded_partial_credential_survives_a_four_kilobyte_pad`.
const MAX_HELD_BACK: usize = 4096;

/// The token [`scrub_secrets`] leaves in a needle's place.
///
/// Why: [`Scrubber::redact_spans`] splices the replacement into a byte buffer
/// rather than a `str`, which is a thing `scrub_secrets` — `&str` in, `String`
/// out — cannot express, so this module writes the token itself.
/// `trusty_common` does not export it, so this restates it.
/// Test: `super::relay_tests::the_replacement_token_matches_the_redactor`,
/// which fails the moment the two drift.
const REDACTED: &[u8] = b"[REDACTED]";

/// The credential values to strip from a child's output.
///
/// Why: #5869. The needles must be materialized once per sweep, not once per
/// line — [`trusty_common::credentials::resolved_secret_values`] reads
/// `.env.local` and opens the secure store, which is not a per-line cost. The
/// two pumps of one child share this by `Arc`, and every child of one sweep
/// shares the same set.
/// What: an immutable needle set plus the hold-back it implies. Cloning is an
/// `Arc` bump. An empty set makes [`Scrubber::scrub`] a borrow and the pump's
/// hold-back zero, so a build with no resolvable credential pays nothing.
/// Test: `super::relay_tests::a_credential_never_reaches_the_log`,
/// `super::relay_tests::a_credential_straddling_the_flush_cut_is_still_caught`,
/// `super::relay_tests::a_credential_interrupted_by_an_invalid_byte_is_removed`.
#[derive(Clone, Debug)]
pub(crate) struct Scrubber {
    secrets: Arc<[String]>,
    held_back: usize,
}

impl Scrubber {
    /// Build a scrubber over `secrets`, deriving the hold-back from them.
    ///
    /// What: drops the values [`scrub_secrets`] would decline (it applies a
    /// minimum length, which it does not export — [`Self::qualifies`] asks it
    /// rather than restating the rule), then takes the hold-back one byte short
    /// of the longest survivor, capped at [`MAX_HELD_BACK`]. Filtering here is
    /// what keeps [`Self::redact_spans`], which locates needles itself, from
    /// removing a value the redactor would have left alone.
    pub(crate) fn over(secrets: Vec<String>) -> Self {
        let secrets: Vec<String> = secrets.into_iter().filter(|s| Self::qualifies(s)).collect();
        let held_back = secrets
            .iter()
            .map(String::len)
            .max()
            .unwrap_or(0)
            .saturating_sub(1)
            .min(MAX_HELD_BACK);
        Self {
            secrets: secrets.into(),
            held_back,
        }
    }

    /// Whether [`scrub_secrets`] would actually remove `needle`.
    ///
    /// Why: `trusty_common` owns the rule for which values are worth redacting
    /// — too short a needle turns prose into `[REDACTED]` confetti — and keeps
    /// the threshold private. Asking the redactor what it does to a needle in
    /// isolation reads the rule without copying it: a value it declines comes
    /// back unchanged.
    /// Test: `super::relay_tests::a_needle_the_redactor_declines_is_dropped`.
    fn qualifies(needle: &str) -> bool {
        scrub_secrets(needle, std::slice::from_ref(&needle)) != needle
    }

    /// A scrubber that removes nothing, for a caller with no credential to hide.
    #[cfg(test)]
    pub(crate) fn none() -> Self {
        Self::over(Vec::new())
    }

    /// `bytes` with every known credential replaced by `[REDACTED]`.
    ///
    /// # Postconditions
    /// On return, no needle of `self` occurs in the result — neither
    /// contiguously nor with invalid bytes injected between its characters.
    /// Every byte of `bytes` that no needle covers is present unchanged and in
    /// order; a needle's own bytes, and any invalid bytes injected among them,
    /// are gone. This holds for the WHOLE of `bytes`: the caller owes it a
    /// contiguous buffer, because a needle split across two calls is a needle
    /// neither call can see.
    ///
    /// Why: the hot path is a log line with no credential in it, run once per
    /// line for hours. It must not allocate, so a segment that matches nothing
    /// is returned borrowed — including one holding invalid bytes, which the
    /// pre-review code always copied.
    /// What: whole-UTF-8 segments — nearly all of them — take one validation,
    /// one search, and [`scrub_secrets`], the workspace's one redactor. A
    /// segment carrying invalid bytes is searched over its valid runs
    /// CONCATENATED, so a needle those bytes interrupt is still found, and a
    /// match is spliced back by byte offset rather than lossily converted,
    /// because the log is a verbatim record where nothing was removed.
    /// Test: `super::relay_tests::a_credential_never_reaches_the_log`,
    /// `super::relay_tests::a_credential_beside_non_utf8_bytes_is_still_removed`,
    /// `super::relay_tests::a_credential_interrupted_by_an_invalid_byte_is_removed`.
    fn scrub<'a>(&self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        if self.secrets.is_empty() {
            return Cow::Borrowed(bytes);
        }
        if let Ok(text) = std::str::from_utf8(bytes) {
            if !self.occurs_in(text) {
                return Cow::Borrowed(bytes);
            }
            return Cow::Owned(scrub_secrets(text, &self.secrets).into_bytes());
        }
        let (text, runs) = utf8_runs(bytes);
        if !self.occurs_in(&text) {
            return Cow::Borrowed(bytes);
        }
        Cow::Owned(self.redact_spans(bytes, &text, &runs))
    }

    /// Whether any needle occurs in `text`.
    ///
    /// `str::contains` is a two-way search rather than the naive scan a
    /// `windows()` comparison over raw bytes would be, which is why this asks
    /// the question in `str` rather than in `[u8]`.
    fn occurs_in(&self, text: &str) -> bool {
        self.secrets.iter().any(|s| text.contains(s.as_str()))
    }

    /// `bytes` with every needle found in `text` cut out by byte offset.
    ///
    /// Why: `text` is the segment's valid-UTF-8 runs concatenated, so a match in
    /// it can span a gap of invalid bytes — the case that reached the log in the
    /// clear before the #5869 review. Splicing by offset is what keeps the
    /// invalid bytes OUTSIDE a match, which are ordinary evidence, while
    /// dropping the ones INSIDE one, which are part of the credential's span.
    /// What: matches longest needle first and drops any overlap, mirroring
    /// [`scrub_secrets`]' own longest-first ordering so a secret that is a
    /// prefix of another cannot leave the longer one's tail behind. Each match
    /// is mapped from `text` offsets back to `bytes` offsets through `runs`, and
    /// the surviving stretches are copied unchanged.
    /// Test: `super::relay_tests::a_credential_interrupted_by_an_invalid_byte_is_removed`,
    /// `super::relay_tests::a_credential_beside_non_utf8_bytes_is_still_removed`.
    fn redact_spans(&self, bytes: &[u8], text: &str, runs: &[Run]) -> Vec<u8> {
        let mut needles: Vec<&str> = self.secrets.iter().map(String::as_str).collect();
        needles.sort_unstable_by_key(|s| std::cmp::Reverse(s.len()));

        let mut spans: Vec<(usize, usize)> = Vec::new();
        for needle in needles {
            for (at, _) in text.match_indices(needle) {
                let (start, end) = (at, at + needle.len());
                if !spans.iter().any(|&(a, b)| a < end && start < b) {
                    spans.push((start, end));
                }
            }
        }
        spans.sort_unstable();

        let mut out = Vec::with_capacity(bytes.len());
        let mut cursor = 0usize;
        for (start, end) in spans {
            let from = Run::origin_at(runs, start);
            let to = Run::origin_after(runs, end);
            out.extend_from_slice(&bytes[cursor..from]);
            out.extend_from_slice(REDACTED);
            cursor = to;
        }
        out.extend_from_slice(&bytes[cursor..]);
        out
    }

    /// How to divide `bytes` so the tail keeps every byte of a needle that has
    /// only partly arrived.
    ///
    /// # Postconditions
    /// Every TEXT byte the walk chose to keep is in [`Cut::carry`], in order.
    /// No raw-byte bound moves the division past that boundary — a bound that
    /// binds takes it out of [`Cut::padding`], which the pump WRITES, never out
    /// of the text the walk kept. `carry` is at most [`MAX_HELD_BACK`] bytes in
    /// every case, and `head`, `padding` and `carry` together are `bytes`.
    ///
    /// Why: the pump writes what it cuts off, so anything it cuts through is
    /// written in the clear. A partly-arrived needle occupies at most
    /// [`Self::held_back`] bytes of TEXT at the very end of the buffer — but
    /// invalid bytes injected between its characters make its RAW span longer
    /// than that, and a raw-byte hold-back would slice its head off. So the walk
    /// counts text bytes and lets the invalid bytes among them ride along.
    /// What: walks the valid-UTF-8 runs backwards until the tail holds
    /// [`Self::held_back`] bytes of text. If the raw span of that tail is within
    /// [`MAX_HELD_BACK`] the tail is carried whole, invalid bytes and all —
    /// which is what keeps [`Self::redact_spans`] able to remove the ones INSIDE
    /// the eventual match. Past that span the padding is lifted out and written
    /// instead: the pump's memory bound is honoured by writing meaningless
    /// bytes early rather than by evicting credential bytes, and the only cost
    /// is that in that case those invalid bytes reach the log ahead of the text
    /// they interrupted rather than being removed with it.
    /// Test: `super::relay_tests::a_credential_straddling_the_flush_cut_is_still_caught`,
    /// `super::relay_tests::an_interrupted_credential_straddling_a_cut_is_removed`,
    /// `super::relay_tests::a_padded_partial_credential_survives_a_four_kilobyte_pad`,
    /// `super::relay_tests::the_half_buffer_clamp_cannot_evict_a_partly_arrived_credential`,
    /// `super::relay_tests::a_credential_at_the_head_of_an_almost_wholly_invalid_segment_is_held`,
    /// `super::relay_tests::a_line_that_never_ends_does_not_grow_without_bound`.
    fn cut_for<'a>(&self, bytes: &'a [u8]) -> Cut<'a> {
        if self.held_back == 0 {
            return Cut::written_whole(bytes);
        }
        let (_, runs) = utf8_runs(bytes);
        let cut = Self::text_cut(&runs, self.held_back);
        if bytes.len() - cut <= MAX_HELD_BACK {
            return Cut {
                head: &bytes[..cut],
                padding: Vec::new(),
                carry: Cow::Borrowed(&bytes[cut..]),
            };
        }
        let (padding, carry) = lift_padding(bytes, &runs, cut);
        Cut {
            head: &bytes[..cut],
            padding,
            carry: Cow::Owned(carry),
        }
    }

    /// The offset the last `want` TEXT bytes of a segment start at.
    ///
    /// Zero when the segment holds fewer than `want` of them: the whole segment
    /// is then a candidate arrived prefix, so none of it may be written.
    fn text_cut(runs: &[Run], want: usize) -> usize {
        let mut want = want;
        for run in runs.iter().rev() {
            if run.len >= want {
                return run.orig + run.len - want;
            }
            want -= run.len;
        }
        0
    }
}

/// How one flush divides a scrubbed buffer.
///
/// `head`, then `padding`, then `carry` is `bytes` reordered only to the extent
/// [`Scrubber::cut_for`] documents. The pump writes `head` and `padding` and
/// keeps `carry` for the next segment.
struct Cut<'a> {
    /// Written first, unchanged: the buffer up to the hold-back's start.
    head: &'a [u8],
    /// Written after `head`: invalid bytes lifted out of an over-long hold.
    /// Empty on the ordinary path, where the hold is carried whole.
    padding: Vec<u8>,
    /// Kept for the next segment.
    carry: Cow<'a, [u8]>,
}

impl<'a> Cut<'a> {
    /// The whole buffer written, nothing held — a scrubber with no needles.
    fn written_whole(bytes: &'a [u8]) -> Self {
        Self {
            head: bytes,
            padding: Vec::new(),
            carry: Cow::Borrowed(&[]),
        }
    }
}

/// Split `bytes[from..]` into its invalid bytes and its text bytes, each in
/// stream order.
///
/// Why: a hold whose raw span outgrew [`MAX_HELD_BACK`] must shrink, and the
/// invalid bytes in it are the part that can be given up — they are not
/// credential characters, so writing them leaks nothing, whereas writing the
/// text among them is exactly the #5869 re-verify CRITICAL.
/// Test: `super::relay_tests::a_padded_partial_credential_survives_a_four_kilobyte_pad`.
fn lift_padding(bytes: &[u8], runs: &[Run], from: usize) -> (Vec<u8>, Vec<u8>) {
    let mut padding = Vec::new();
    let mut text = Vec::new();
    let mut at = from;
    for run in runs.iter().filter(|r| r.orig + r.len > from) {
        let start = run.orig.max(from);
        padding.extend_from_slice(&bytes[at..start]);
        text.extend_from_slice(&bytes[start..run.orig + run.len]);
        at = run.orig + run.len;
    }
    padding.extend_from_slice(&bytes[at..]);
    (padding, text)
}

/// One maximal run of valid UTF-8 inside a segment.
///
/// `text` is the run's offset in the segment's runs CONCATENATED — the space a
/// needle is searched in; `orig` is its offset in the segment itself.
#[derive(Clone, Copy, Debug)]
struct Run {
    text: usize,
    orig: usize,
    len: usize,
}

impl Run {
    /// The segment offset of the text byte at `text` offset `at`.
    fn origin_at(runs: &[Run], at: usize) -> usize {
        let i = runs.partition_point(|r| r.text <= at).saturating_sub(1);
        runs[i].orig + (at - runs[i].text)
    }

    /// The segment offset one past the text byte ending at `text` offset `end`.
    ///
    /// Distinct from [`Run::origin_at`] because a match ending exactly at a
    /// run's end must resolve to that run, not to the next one across the gap —
    /// resolving forwards would swallow invalid bytes that are not part of it.
    fn origin_after(runs: &[Run], end: usize) -> usize {
        let i = runs.partition_point(|r| r.text + r.len < end);
        runs[i].orig + (end - runs[i].text)
    }
}

/// Split `bytes` into its valid-UTF-8 runs, and those runs concatenated.
///
/// Why: a needle is valid UTF-8, so it can never CONTAIN an invalid byte — but
/// nothing stops one being INJECTED INTO it, and the pre-review code split at
/// every such byte and searched each side alone. The concatenation is the one
/// contiguous search space a needle is guaranteed to lie inside; the runs are
/// how a match in it maps back to the segment's own offsets.
/// What: walks the segment, splitting at each [`std::str::Utf8Error`]. An
/// `error_len` of `None` is an incomplete trailing sequence, skipped whole.
/// Runs are non-empty and in stream order, so their `text` offsets increase.
/// Test: `super::relay_tests::a_credential_interrupted_by_an_invalid_byte_is_removed`.
fn utf8_runs(bytes: &[u8]) -> (String, Vec<Run>) {
    let mut text = String::with_capacity(bytes.len());
    let mut runs: Vec<Run> = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        let rest = &bytes[at..];
        let (valid_len, skip) = match std::str::from_utf8(rest) {
            Ok(_) => (rest.len(), 0),
            Err(e) => (
                e.valid_up_to(),
                e.error_len().unwrap_or(rest.len() - e.valid_up_to()).max(1),
            ),
        };
        if valid_len > 0 {
            runs.push(Run {
                text: text.len(),
                orig: at,
                len: valid_len,
            });
            // `valid_up_to` guarantees this borrows rather than substituting;
            // it is the no-unwrap spelling of the cast.
            text.push_str(&String::from_utf8_lossy(&rest[..valid_len]));
        }
        at += valid_len + skip;
        if skip == 0 {
            break;
        }
    }
    (text, runs)
}

/// Copy `reader` into `log` with credentials removed, forwarding progress lines.
///
/// # Postconditions
/// On `Ok`, every byte read reached `log` except the bytes of a needle in
/// `scrubber`, each of which was replaced by `[REDACTED]`, and `log` was
/// flushed; every well-formed relay line was delivered as a
/// [`crate::progress::ProgressUpdate::UnitStage`] for `target`. On `Err`, the
/// log is incomplete — the caller must treat that as a failure of the unit
/// rather than a cosmetic one, because the log is the evidence.
///
/// What: accumulates up to each `\n`, bounded by [`SEGMENT_LIMIT`], scrubs the
/// segment, writes it, and decodes it only when it starts with the relay marker.
/// A relay line is kept in the log too: the log is a record of what the child
/// said, not a filtered view of it. The relay side reads the SCRUBBED bytes, so
/// a credential quoted inside a stage detail never reaches the display either;
/// `[REDACTED]` carries no tab, so the wire format's field count survives.
///
/// A segment that reaches [`SEGMENT_LIMIT`] with no newline is written out
/// early. The buffer is scrubbed WHOLE and the CUT IS TAKEN FROM THE SCRUBBED
/// RESULT, so a credential lying across the cut is already gone before there is
/// a cut to lie across; what the tail holds back is only a needle that has not
/// finished arriving ([`Scrubber::cut_for`]). Scrubbing the emitted part alone,
/// as this did before the #5869 review, wrote both halves of a straddling
/// credential verbatim. Early writing is also what keeps a child printing one
/// endless line from growing this buffer without bound: [`Scrubber::cut_for`]
/// carries at most [`MAX_HELD_BACK`] bytes forward whatever the encoding, so
/// the buffer peaks at [`SEGMENT_LIMIT`] plus [`MAX_HELD_BACK`], with one
/// scrubbed copy of that alive at a time.
/// Test: `super::relay_tests::every_non_secret_byte_reaches_the_log_and_events_reach_the_sink`,
/// `super::relay_tests::a_credential_straddling_the_flush_cut_is_still_caught`,
/// `super::relay_tests::an_interrupted_credential_straddling_a_cut_is_removed`,
/// `super::relay_tests::a_line_that_never_ends_does_not_grow_without_bound`.
///
/// # Errors
///
/// Any read from `reader` or write to `log`.
pub(crate) async fn tee_and_relay<R>(
    reader: R,
    mut log: tokio::fs::File,
    progress: Progress,
    target: String,
    scrubber: Scrubber,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let read = (&mut reader)
            .take(SEGMENT_LIMIT as u64)
            .read_until(b'\n', &mut pending)
            .await?;
        if read == 0 {
            // EOF. The child was killed mid-line, or simply ended without one;
            // either way what is held is the last thing it managed to say.
            if !pending.is_empty() {
                let clean = scrubber.scrub(&pending);
                emit(&mut log, &progress, &target, &clean).await?;
            }
            break;
        }
        if pending.ends_with(b"\n") {
            {
                let clean = scrubber.scrub(&pending);
                emit(&mut log, &progress, &target, &clean).await?;
            }
            pending.clear();
        } else if pending.len() >= SEGMENT_LIMIT {
            // #5869: scrub the WHOLE buffer, THEN cut it. Cutting first split a
            // straddling credential into two unmatched halves.
            let clean = scrubber.scrub(&pending).into_owned();
            let cut = scrubber.cut_for(&clean);
            emit(&mut log, &progress, &target, cut.head).await?;
            if !cut.padding.is_empty() {
                emit(&mut log, &progress, &target, &cut.padding).await?;
            }
            pending.clear();
            pending.extend_from_slice(&cut.carry);
        }
    }
    log.flush().await
}

/// Write one ALREADY-SCRUBBED segment, and relay it if it is an event.
///
/// Why: every caller scrubs a whole buffer before cutting anything out of it,
/// so this cannot scrub for them without re-introducing the boundary the
/// #5869 review found. The contract is therefore on the caller: `clean` is
/// what [`Scrubber::scrub`] returned, or a prefix of it.
async fn emit(
    log: &mut tokio::fs::File,
    progress: &Progress,
    target: &str,
    clean: &[u8],
) -> std::io::Result<()> {
    log.write_all(clean).await?;
    // `is_active` first: the common segment is ordinary logging, and with no
    // sink attached there is nothing to decode it for.
    if progress.is_active()
        && let Some(event) = decode(clean)
    {
        progress.unit_stage(target, event);
    }
    Ok(())
}

/// The event a line carries, or `None` for ordinary output.
///
/// Why: the overwhelming majority of a child's lines are its own logging, so
/// the common path must not allocate. The marker check is a byte comparison;
/// only a line that passes it is even considered as UTF-8.
/// Test: `super::relay_tests::ordinary_output_is_not_mistaken_for_an_event`.
fn decode(line: &[u8]) -> Option<StageEvent> {
    if !line.starts_with(trusty_progress::relay::LINE_PREFIX.as_bytes()) {
        return None;
    }
    StageEvent::decode(std::str::from_utf8(line).ok()?)
}

#[cfg(test)]
mod relay_tests {
    use super::*;
    use crate::progress::{Recorder, StageState};
    use trusty_progress::relay::StageEvent;

    /// A needle long enough for `scrub_secrets` to apply (its floor is 8 chars).
    const KEY: &str = "sk-or-v1-0123456789abcdef0123456789abcdef";

    async fn run(input: &str) -> (String, Vec<StageEvent>) {
        let (log, stages) = run_bytes(input.as_bytes(), Scrubber::none()).await;
        (String::from_utf8(log).expect("text"), stages)
    }

    async fn run_bytes(input: &[u8], scrubber: Scrubber) -> (Vec<u8>, Vec<StageEvent>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("child.log");
        let file = tokio::fs::File::create(&path).await.expect("create log");
        let (recorder, progress) = Recorder::new();

        tee_and_relay(input, file, progress, "acme/api".into(), scrubber)
            .await
            .expect("the pump completes");

        (std::fs::read(&path).expect("read log"), recorder.stages())
    }

    /// Why: the two obligations at once, and they pull in opposite directions —
    /// a filter that forwarded events would be simpler and would silently drop
    /// them from the log a failure is diagnosed from.
    /// What: a child stream mixing ordinary logging with relay lines leaves the
    /// log byte-identical to the input AND delivers every event. #5869 renamed
    /// this from `every_byte_reaches_the_log_…`: with scrubbing in the path the
    /// unqualified claim is no longer true, and the assertion it makes — over a
    /// stream holding no credential — is the qualified one.
    /// Test: this is the test.
    #[tokio::test]
    async fn every_non_secret_byte_reaches_the_log_and_events_reach_the_sink() {
        let started = StageEvent::new("Audit", "collect", StageState::Started)
            .with_counts(0, Some(9))
            .with_detail("stage 1 of 9");
        let failed = StageEvent::new("Audit", "jira sync", StageState::Failed)
            .with_detail("no JIRA project configured");
        let input = format!(
            "INFO starting\n{}\nWARN slow remote\n{}\ndone\n",
            started.encode(),
            failed.encode()
        );

        let (log, stages) = run(&input).await;
        assert_eq!(log, input, "the log must be the whole stream");
        assert_eq!(stages, vec![started, failed]);
    }

    /// Why: a child writes far more logging than events, and a line that merely
    /// mentions a stage must not become one.
    /// What: log-shaped lines produce no events, and the log still holds them.
    /// Test: this is the test.
    #[tokio::test]
    async fn ordinary_output_is_not_mistaken_for_an_event() {
        let input = "collect: ok\n@trusty-progress/1 start Audit collect\nAudit\tcollect\tok\n";
        let (log, stages) = run(input).await;
        assert_eq!(log, input);
        assert!(stages.is_empty(), "{stages:?}");
    }

    /// Why: the child is killed on timeout and dies wherever it was, so the
    /// last line can be a partial write with no newline. Losing it from the log
    /// would lose the last thing the child managed to say.
    /// What: a stream whose final line has no newline still reaches the log
    /// verbatim, and a truncated event yields nothing rather than a half-event.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_stream_cut_mid_line_keeps_what_arrived() {
        let whole = StageEvent::new("Audit", "classify", StageState::Started).encode();
        let input = format!("INFO working\n{}", &whole[..whole.len() / 2]);
        let (log, stages) = run(&input).await;
        assert_eq!(log, input);
        assert!(stages.is_empty(), "{stages:?}");
    }

    /// Why: a child's output is not guaranteed to be UTF-8, and a pump that
    /// decoded every line as text would stop at the first byte that is not —
    /// truncating the log, which is evidence, over a display, which is not.
    /// What: invalid UTF-8 passes through to the log and the pump keeps going.
    /// Test: this is the test.
    #[tokio::test]
    async fn invalid_utf8_does_not_truncate_the_log() {
        let event = StageEvent::new("Audit", "report", StageState::Completed);
        let mut input: Vec<u8> = b"before\n\xff\xfe not text\n".to_vec();
        input.extend_from_slice(event.encode().as_bytes());
        input.push(b'\n');

        let (log, stages) = run_bytes(&input, Scrubber::none()).await;
        assert_eq!(log, input);
        assert_eq!(stages, vec![event]);
    }

    /// Why: #5869 — the whole point. A child handed the credential can echo it,
    /// and until this the bytes went to disk verbatim.
    /// What: a key on the child's stream is replaced in the log, and the
    /// surrounding text is untouched.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_credential_never_reaches_the_log() {
        let input = format!("ERROR 401: the key {KEY} is not valid\nnext line\n");
        let (log, _) = run_bytes(input.as_bytes(), Scrubber::over(vec![KEY.to_owned()])).await;
        let log = String::from_utf8(log).expect("text");

        assert!(!log.contains(KEY), "{log}");
        assert_eq!(
            log,
            "ERROR 401: the key [REDACTED] is not valid\nnext line\n"
        );
    }

    /// Why: #5869 — a credential quoted in a stage detail would otherwise reach
    /// the operator's terminal, a second sink with the same exposure.
    /// What: the relay path decodes the SCRUBBED bytes, so the event's detail
    /// arrives masked and the wire format still parses.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_credential_inside_a_relay_line_is_masked_before_the_sink() {
        let leaky = StageEvent::new("Audit", "review", StageState::Failed)
            .with_detail(format!("provider rejected {KEY}"));
        let input = format!("{}\n", leaky.encode());

        let (log, stages) = run_bytes(input.as_bytes(), Scrubber::over(vec![KEY.to_owned()])).await;

        assert!(!String::from_utf8_lossy(&log).contains(KEY));
        assert_eq!(stages.len(), 1, "the event must still decode: {stages:?}");
        assert_eq!(
            stages[0].detail.as_deref(),
            Some("provider rejected [REDACTED]")
        );
    }

    /// Why: the child's output is a stream, so a credential can land either side
    /// of the point where the pump gives up waiting for a newline. Naive
    /// per-segment scrubbing misses exactly that case.
    /// What: a key placed deliberately across the mid-line flush boundary is
    /// still removed, because the pump holds back a tail as long as the longest
    /// needle before writing.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_credential_split_across_a_flush_is_still_caught() {
        // One line with no newline until well past the limit, with the key
        // straddling the first flush point.
        let head = "x".repeat(SEGMENT_LIMIT - KEY.len() / 2);
        let input = format!("{head}{KEY} trailing\n");

        let (log, _) = run_bytes(input.as_bytes(), Scrubber::over(vec![KEY.to_owned()])).await;
        let log = String::from_utf8(log).expect("text");

        assert!(!log.contains(KEY), "the key survived the flush boundary");
        assert!(
            log.contains("[REDACTED] trailing"),
            "tail: {:?}",
            &log[log.len() - 40..]
        );
    }

    /// Why: #5869 review round, CRITICAL 1. The pump used to scrub only the
    /// part of its buffer it was about to write, so a needle whose start lay in
    /// `[cut - len + 1, cut - 1]` was split by the cut, matched neither half,
    /// and BOTH HALVES reached the log verbatim. The reproduction measured a log
    /// byte-identical in length to its input — not one byte redacted anywhere.
    /// `a_credential_split_across_a_flush_is_still_caught` passed throughout
    /// because it starts its key AFTER the cut rather than across it.
    /// What: the key starts exactly `held_back` bytes before the first cut — the
    /// adversarial position — and is still removed, because the buffer is
    /// scrubbed whole before there is a cut to straddle.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_credential_straddling_the_flush_cut_is_still_caught() {
        let scrubber = Scrubber::over(vec![KEY.to_owned()]);
        // The first cut lands `held_back` from the end of a full segment, so a
        // key starting `held_back` before that cut lies across it.
        let cut = SEGMENT_LIMIT - scrubber.held_back;
        let head = "x".repeat(cut - scrubber.held_back);
        // Long enough that the newline arrives well after SEGMENT_LIMIT, so the
        // pump reaches the mid-line flush at all.
        let tail = format!(" trailing{}\n", "y".repeat(200));
        let input = format!("{head}{KEY}{tail}");
        assert!(
            input.len() > SEGMENT_LIMIT,
            "the flush path must be reached"
        );

        let (log, _) = run_bytes(input.as_bytes(), scrubber).await;
        let log = String::from_utf8(log).expect("text");

        assert_no_fragment_of_the_key(log.as_bytes());
        assert!(
            log.contains("[REDACTED] trailing"),
            "the key was not replaced"
        );
        assert_eq!(log.len(), input.len() - KEY.len() + REDACTED.len());
    }

    /// Why: #5869 review round, CRITICAL 2. The mixed-encoding path used to
    /// scrub each valid-UTF-8 run alone. A needle cannot CONTAIN an invalid
    /// byte, which is what the old reasoning said — but nothing stops one being
    /// INJECTED INTO it, and after the split neither run held the whole key. The
    /// reproduction recovered the entire key from two fragments flanking one
    /// meaningless byte. It needs no long line and no cut alignment.
    /// What: a key cut in two by a single stray non-UTF-8 byte is removed whole,
    /// the injected byte goes with it, and the text either side survives.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_credential_interrupted_by_an_invalid_byte_is_removed() {
        let (head, rest) = KEY.split_at(20);
        let mut input: Vec<u8> = format!("before {head}").into_bytes();
        input.push(0xff);
        input.extend_from_slice(format!("{rest} after\n").as_bytes());

        let (log, _) = run_bytes(&input, Scrubber::over(vec![KEY.to_owned()])).await;

        assert_no_fragment_of_the_key(&log);
        assert_eq!(log, b"before [REDACTED] after\n".to_vec());
    }

    /// Why: #5869 review round — the case that catches a partial fix. Removing
    /// one of the two boundaries leaves the other: a needle both interrupted by
    /// an invalid byte AND cut in half by the flush is missed by a fix to either
    /// alone. It is also why the hold-back counts TEXT bytes, not raw ones — the
    /// arrived prefix here is 50 raw bytes against a 40-byte hold-back, so a
    /// raw-byte hold-back would slice nine characters of the key off and write
    /// them.
    /// What: the segment ends mid-key with garbage injected into the arrived
    /// prefix; nothing of the key reaches the log, and it is removed whole once
    /// the rest arrives.
    /// Test: this is the test.
    #[tokio::test]
    async fn an_interrupted_credential_straddling_a_cut_is_removed() {
        let scrubber = Scrubber::over(vec![KEY.to_owned()]);
        const GARBAGE: usize = 20;
        let (head, rest) = KEY.split_at(20);
        let (middle, last) = rest.split_at(10);
        // Land the segment boundary exactly at the end of `middle`, so the key
        // is half-arrived when the pump flushes.
        let filler = "x".repeat(SEGMENT_LIMIT - head.len() - GARBAGE - middle.len());

        let mut input: Vec<u8> = format!("{filler}{head}").into_bytes();
        input.extend(std::iter::repeat_n(0xff_u8, GARBAGE));
        input.extend_from_slice(format!("{middle}{last} trailing\n").as_bytes());
        assert!(
            input.len() > SEGMENT_LIMIT,
            "the flush path must be reached"
        );
        assert!(
            head.len() + GARBAGE + middle.len() > scrubber.held_back,
            "the arrived prefix must outrun a raw-byte hold-back"
        );

        let (log, _) = run_bytes(&input, scrubber).await;
        let log = String::from_utf8(log).expect("only the garbage was invalid, and it went");

        assert_no_fragment_of_the_key(log.as_bytes());
        assert_eq!(log, format!("{filler}[REDACTED] trailing\n"));
    }

    /// No run of the key's characters, at any length worth recovering, is in
    /// `log`. A whole-key check alone passes on a leak of all but one character.
    ///
    /// Searches RAW bytes: a log that carries invalid UTF-8 has no `str` form to
    /// search, and converting it lossily first is a second unit system in a file
    /// whose every leak so far came from exactly that.
    fn assert_no_fragment_of_the_key(log: &[u8]) {
        for len in [8, 16, 24, 32, KEY.len()] {
            let fragment = &KEY.as_bytes()[..len];
            assert!(
                !log.windows(len).any(|w| w == fragment),
                "a {len}-character fragment of the key reached the log: {}",
                &KEY[..len]
            );
        }
    }

    /// Whether `needle` occurs anywhere in `log`, byte for byte.
    fn contains_bytes(log: &[u8], needle: &[u8]) -> bool {
        log.windows(needle.len()).any(|w| w == needle)
    }

    /// The reproduction behind the third #5869 CRITICAL, parameterised by how
    /// much invalid padding sits inside the arrived prefix.
    ///
    /// The key straddles the flush cut with `garbage` invalid bytes injected
    /// into its arrived prefix, so the prefix's RAW span is
    /// `garbage + 40` while its TEXT span is 40. Any clamp counted in raw bytes
    /// moves the cut past the prefix's first text byte and writes it out.
    async fn assert_a_padded_prefix_survives_the_cut(garbage: usize) {
        let scrubber = Scrubber::over(vec![KEY.to_owned()]);
        let (head, rest) = KEY.split_at(20);
        let (middle, last) = rest.split_at(10);
        let filler = "x".repeat(SEGMENT_LIMIT - head.len() - garbage - middle.len());

        let mut input: Vec<u8> = format!("{filler}{head}").into_bytes();
        input.extend(std::iter::repeat_n(0xff_u8, garbage));
        input.extend_from_slice(format!("{middle}{last} trailing\n").as_bytes());
        assert!(
            input.len() > SEGMENT_LIMIT,
            "the flush path must be reached"
        );
        assert!(
            garbage + head.len() + middle.len() > MAX_HELD_BACK,
            "the padding must outrun the raw clamp, or this proves nothing"
        );

        let (log, _) = run_bytes(&input, scrubber).await;

        assert_no_fragment_of_the_key(&log);
        assert!(
            contains_bytes(&log, b"[REDACTED] trailing"),
            "the key was not replaced"
        );
    }

    /// Why: #5869 re-verify round, CRITICAL 3. [`Scrubber::cut_for`] walked TEXT
    /// bytes to find the cut and then clamped the result in RAW bytes. When
    /// invalid padding inside the arrived prefix pushed its raw span past
    /// [`MAX_HELD_BACK`], the clamp moved the cut PAST the walk's own boundary
    /// and wrote the prefix out in the clear; the remainder arrived in a later
    /// segment without it and matched nothing, so both halves reached the log.
    /// The trigger is ~4KB of non-UTF-8 near a flush boundary — a binary diff or
    /// a corrupted pack object from a `git` child, no alignment needed.
    /// What: 4200 bytes of padding inside the prefix, and nothing of the key
    /// reaches the log.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_padded_partial_credential_survives_a_four_kilobyte_pad() {
        assert_a_padded_prefix_survives_the_cut(4200).await;
    }

    /// Why: the same defect at a different split point — the re-verifier's two
    /// reproductions cut the key in different places, so one size is one data
    /// point rather than the class.
    /// What: 10000 bytes of padding inside the prefix, same outcome.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_padded_partial_credential_survives_a_ten_kilobyte_pad() {
        assert_a_padded_prefix_survives_the_cut(10_000).await;
    }

    /// Why: [`MAX_HELD_BACK`] was not the only raw-byte clamp over a text-byte
    /// walk — `.min(bytes.len() / 2)` was the second, and it evicts the same
    /// bytes once the padding is large enough to reach it. A fix aimed only at
    /// the constant named in the finding leaves this one live.
    /// What: 200000 bytes of padding, over half a full segment, and the key is
    /// still removed whole.
    /// Test: this is the test.
    #[tokio::test]
    async fn the_half_buffer_clamp_cannot_evict_a_partly_arrived_credential() {
        assert_a_padded_prefix_survives_the_cut(200_000).await;
    }

    /// Why: the third place the two units disagreed. When a segment holds FEWER
    /// text bytes than the hold-back wants, the walk gives up and cuts at zero —
    /// meaning "keep all of it" — and the raw clamp then turned that into "keep
    /// the last 4096 bytes", writing a key sitting at the head of a segment that
    /// is otherwise invalid bytes.
    /// What: 20 characters of the key, then a segment's worth of invalid bytes,
    /// then the rest. The head is held across every flush and the key is removed
    /// whole when its remainder arrives.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_credential_at_the_head_of_an_almost_wholly_invalid_segment_is_held() {
        let scrubber = Scrubber::over(vec![KEY.to_owned()]);
        let (head, rest) = KEY.split_at(20);
        let garbage = SEGMENT_LIMIT + MAX_HELD_BACK;
        assert!(
            head.len() < scrubber.held_back,
            "the walk must run out of text before it is satisfied"
        );

        let mut input: Vec<u8> = head.as_bytes().to_vec();
        input.extend(std::iter::repeat_n(0xff_u8, garbage));
        input.extend_from_slice(format!("{rest} trailing\n").as_bytes());

        let (log, _) = run_bytes(&input, scrubber).await;

        assert_no_fragment_of_the_key(&log);
        assert!(
            contains_bytes(&log, b"[REDACTED] trailing"),
            "the key was not replaced"
        );
    }

    /// Why: [`Scrubber::redact_spans`] writes [`REDACTED`] itself, because
    /// splicing into bytes is a thing `scrub_secrets` cannot express. The two
    /// tokens must not drift.
    /// What: the redactor's own replacement for a qualifying needle is byte-for
    /// byte what this module writes.
    /// Test: this is the test.
    #[test]
    fn the_replacement_token_matches_the_redactor() {
        let probe = "0123456789abcdef";
        assert_eq!(scrub_secrets(probe, &[probe]).as_bytes(), REDACTED);
    }

    /// Why: this module locates needles itself, so a value it would remove but
    /// `scrub_secrets` would decline is a value redacted on one path and not the
    /// other — divergence in a credential filter.
    /// What: a needle under the redactor's minimum is dropped at construction,
    /// so it can never reach either path, and the hold-back follows the
    /// survivors.
    /// Test: this is the test.
    #[test]
    fn a_needle_the_redactor_declines_is_dropped() {
        assert!(!Scrubber::qualifies(""));
        assert!(!Scrubber::qualifies("abc"));
        assert!(Scrubber::qualifies(KEY));

        let mixed = Scrubber::over(vec!["abc".to_owned(), KEY.to_owned()]);
        assert_eq!(&*mixed.secrets, &[KEY.to_owned()]);
        assert_eq!(mixed.held_back, KEY.len() - 1);
    }

    /// Why: a needle is valid UTF-8, so it cannot span an invalid byte — but a
    /// pump that gave up on a non-text segment would leave the key beside it in
    /// the clear.
    /// What: a key in the same segment as invalid bytes is removed, and the
    /// invalid bytes still reach the log unchanged.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_credential_beside_non_utf8_bytes_is_still_removed() {
        let mut input: Vec<u8> = format!("before {KEY} ").into_bytes();
        input.extend_from_slice(b"\xff\xfe after\n");

        let (log, _) = run_bytes(&input, Scrubber::over(vec![KEY.to_owned()])).await;

        assert!(!log.windows(KEY.len()).any(|w| w == KEY.as_bytes()));
        assert_eq!(log, b"before [REDACTED] \xff\xfe after\n".to_vec());
    }

    /// Why: `read_until` grows its buffer until a newline arrives, so a child
    /// printing one endless line is an unbounded allocation in a process that
    /// runs for hours.
    /// What: the assertion is that bytes reach the log while the stream is
    /// STILL OPEN and has carried no newline. A pump that buffered the line
    /// would have written nothing yet, so an empty log here is the unbounded
    /// behaviour and a non-empty one is the bound. The log then still holds the
    /// whole stream once it closes — flushing early costs no bytes.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_line_that_never_ends_does_not_grow_without_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("child.log");
        let file = tokio::fs::File::create(&path).await.expect("create log");
        let (_recorder, progress) = Recorder::new();
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);

        let pump = tokio::spawn(tee_and_relay(
            reader,
            file,
            progress,
            "acme/api".into(),
            Scrubber::over(vec![KEY.to_owned()]),
        ));

        let burst = vec![b'z'; SEGMENT_LIMIT * 3 + 17];
        let feeder = tokio::spawn(async move {
            writer.write_all(&burst).await.expect("feed the pump");
            // Hold the stream open: the point is what the log holds BEFORE EOF.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            writer
        });

        // Poll rather than sleep-and-hope: the condition is "the pump wrote
        // something", and it is either reached or the test fails on the bound.
        let mut on_disk = 0;
        for _ in 0..100 {
            on_disk = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if on_disk > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            on_disk > 0,
            "nothing reached the log while the endless line was still open — \
             the pump is buffering it whole"
        );

        drop(feeder.await.expect("the feeder finishes"));
        pump.await.expect("join").expect("the pump completes");
        assert_eq!(
            std::fs::read(&path).expect("read log"),
            vec![b'z'; SEGMENT_LIMIT * 3 + 17],
            "flushing early must not cost a byte"
        );
    }

    /// Why: the hold-back is derived from the needles, and a set with none must
    /// not make the pump hold bytes it will never need.
    /// What: an empty scrubber holds back nothing and returns its input borrowed.
    /// Test: this is the test.
    #[test]
    fn an_empty_scrubber_holds_nothing_back_and_never_allocates() {
        let empty = Scrubber::none();
        assert_eq!(empty.held_back, 0);
        assert!(matches!(empty.scrub(b"anything at all"), Cow::Borrowed(_)));

        let one = Scrubber::over(vec![KEY.to_owned()]);
        assert_eq!(one.held_back, KEY.len() - 1);
        assert!(matches!(one.scrub(b"nothing to remove"), Cow::Borrowed(_)));
    }

    /// Why: the hold-back must stay far below [`SEGMENT_LIMIT`] or the pump
    /// re-holds everything it read and stops making progress. Two things can
    /// inflate it, and the #5869 re-verify round closed the second one without
    /// reopening the first: an absurdly long NEEDLE, capped here at
    /// construction; and invalid PADDING among a needle's characters, which no
    /// cap on the needle bounds because it is not part of the needle.
    /// [`Scrubber::cut_for`] now bounds the padding by writing it out rather
    /// than by moving the cut, so both vectors stay closed and the carried tail
    /// is at most [`MAX_HELD_BACK`] bytes however the segment is encoded — the
    /// claim `the_hold_back_stays_within_its_cap_whatever_the_padding` checks
    /// directly.
    /// What: an absurdly long needle is capped at [`MAX_HELD_BACK`].
    /// Test: this is the test.
    #[test]
    fn an_absurd_needle_cannot_stall_the_pump() {
        let huge = Scrubber::over(vec!["q".repeat(SEGMENT_LIMIT * 2)]);
        assert_eq!(huge.held_back, MAX_HELD_BACK);
        assert!(huge.held_back < SEGMENT_LIMIT / 2);
    }

    /// Why: the pump's memory bound is `SEGMENT_LIMIT + MAX_HELD_BACK`, and it
    /// rests entirely on what [`Scrubber::cut_for`] carries. The tests above
    /// reach that through the pump; this asserts the bound itself, over the
    /// segment shapes that inflate a raw hold — padding inside the tail,
    /// padding at the very end, and a segment with almost no text in it at all.
    /// What: whatever the encoding, the carry is within the cap, its text is
    /// within the needle-derived hold-back, and the three pieces reassemble the
    /// segment's bytes.
    /// Test: this is the test.
    #[test]
    fn the_hold_back_stays_within_its_cap_whatever_the_padding() {
        let scrubber = Scrubber::over(vec![KEY.to_owned()]);
        let text = "t".repeat(64);
        let shapes: Vec<Vec<u8>> = vec![
            // Padding inside the held tail: the walk keeps text either side.
            [text.as_bytes(), &vec![0xff; 9000], &text.as_bytes()[..20]].concat(),
            // Padding after the last text byte.
            [text.as_bytes(), &vec![0xff; 9000]].concat(),
            // Almost no text at all: the walk runs out and cuts at zero.
            [&b"ab"[..], &vec![0xff; 9000]].concat(),
            // No text at all.
            vec![0xff; 9000],
        ];

        for bytes in shapes {
            let cut = scrubber.cut_for(&bytes);
            assert!(
                !cut.padding.is_empty(),
                "every shape here outruns the cap, so every one must lift padding"
            );
            assert!(
                cut.carry.len() <= MAX_HELD_BACK,
                "carried {} bytes, cap is {MAX_HELD_BACK}",
                cut.carry.len()
            );
            let carried_text: usize = utf8_runs(&cut.carry).1.iter().map(|r| r.len).sum();
            assert!(
                carried_text <= scrubber.held_back,
                "carried {carried_text} text bytes against a hold-back of {}",
                scrubber.held_back
            );
            let mut whole = cut.head.to_vec();
            whole.extend_from_slice(&cut.padding);
            whole.extend_from_slice(&cut.carry);
            assert_eq!(whole.len(), bytes.len(), "no byte was dropped");
        }
    }
}
