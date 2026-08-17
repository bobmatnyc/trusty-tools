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
//! feature into evidence loss. So the pump is byte-oriented throughout, and a
//! segment that is not valid UTF-8 is scrubbed run-by-run rather than lost.
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
/// Why: the hold-back exists so a credential straddling a flush is still
/// matched next round, so it wants to be the longest needle. It must also stay
/// far below [`SEGMENT_LIMIT`] or the pump stops making progress — it would
/// re-hold everything it just read. A pathological "secret" longer than this is
/// bounded here and loses only the straddling case, never the ordinary one.
const MAX_HELD_BACK: usize = 4096;

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
/// `super::relay_tests::a_credential_split_across_a_flush_is_still_caught`.
#[derive(Clone, Debug)]
pub(crate) struct Scrubber {
    secrets: Arc<[String]>,
    held_back: usize,
}

impl Scrubber {
    /// Build a scrubber over `secrets`, deriving the hold-back from them.
    ///
    /// What: the hold-back is one byte short of the longest needle — enough
    /// that any needle straddling a mid-line flush still lies whole inside the
    /// next segment — capped at [`MAX_HELD_BACK`].
    pub(crate) fn over(secrets: Vec<String>) -> Self {
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

    /// A scrubber that removes nothing, for a caller with no credential to hide.
    #[cfg(test)]
    pub(crate) fn none() -> Self {
        Self::over(Vec::new())
    }

    /// `bytes` with every known credential replaced by `[REDACTED]`.
    ///
    /// Why: the hot path is a log line with no credential in it, run once per
    /// line for hours. It must not allocate, so a segment that is whole UTF-8
    /// and matches nothing is returned borrowed.
    /// What: validates once, then asks each needle whether it occurs; only a hit
    /// reaches [`scrub_secrets`], the workspace's one redactor. A segment that
    /// is not valid UTF-8 falls to [`Self::scrub_mixed`] rather than being
    /// lossily converted, because the log is a verbatim record.
    /// Test: `super::relay_tests::a_credential_never_reaches_the_log`,
    /// `super::relay_tests::a_credential_beside_non_utf8_bytes_is_still_removed`.
    fn scrub<'a>(&self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        if self.secrets.is_empty() {
            return Cow::Borrowed(bytes);
        }
        match std::str::from_utf8(bytes) {
            Ok(text) => {
                if !self.occurs_in(text) {
                    return Cow::Borrowed(bytes);
                }
                Cow::Owned(scrub_secrets(text, &self.secrets).into_bytes())
            }
            Err(_) => Cow::Owned(self.scrub_mixed(bytes)),
        }
    }

    /// Whether any needle occurs in `text`.
    ///
    /// `str::contains` is a two-way search rather than the naive scan a
    /// `windows()` comparison over raw bytes would be, which is why this asks
    /// the question in `str` rather than in `[u8]`.
    fn occurs_in(&self, text: &str) -> bool {
        self.secrets.iter().any(|s| text.contains(s.as_str()))
    }

    /// Scrub the valid-UTF-8 runs of `bytes`, passing the invalid bytes through.
    ///
    /// Why: a credential is by construction valid UTF-8, so it can never span an
    /// invalid byte — scrubbing each valid run and copying the invalid bytes
    /// verbatim removes exactly as much as scrubbing the whole would, while
    /// keeping the log byte-faithful where the child was not text. Converting
    /// lossily instead would substitute replacement characters into the one
    /// artifact a failure is diagnosed from.
    /// What: walks the segment, splitting at each [`std::str::Utf8Error`], and
    /// scrubs only the runs on the valid side of the split. An `error_len` of
    /// `None` is an incomplete trailing sequence, which is copied whole.
    /// Test: `super::relay_tests::a_credential_beside_non_utf8_bytes_is_still_removed`.
    fn scrub_mixed(&self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        let mut rest = bytes;
        loop {
            let (valid, tail, skip) = match std::str::from_utf8(rest) {
                Ok(_) => (rest, &[][..], 0),
                Err(e) => {
                    let (valid, tail) = rest.split_at(e.valid_up_to());
                    (valid, tail, e.error_len().unwrap_or(tail.len()).max(1))
                }
            };
            // `valid` is UTF-8 by `valid_up_to`, so this conversion never
            // substitutes anything; it is the no-unwrap spelling of the cast.
            let text = String::from_utf8_lossy(valid);
            if self.occurs_in(&text) {
                out.extend_from_slice(scrub_secrets(&text, &self.secrets).as_bytes());
            } else {
                out.extend_from_slice(valid);
            }
            let skip = skip.min(tail.len());
            out.extend_from_slice(&tail[..skip]);
            rest = &tail[skip..];
            if rest.is_empty() {
                return out;
            }
        }
    }
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
/// early, minus a hold-back tail long enough that a credential straddling the
/// cut is matched on the next segment instead. That is what keeps a child
/// printing one endless line from growing this buffer without bound.
/// Test: `super::relay_tests::every_non_secret_byte_reaches_the_log_and_events_reach_the_sink`,
/// `super::relay_tests::a_credential_split_across_a_flush_is_still_caught`,
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
                emit(&scrubber, &mut log, &progress, &target, &pending).await?;
            }
            break;
        }
        if pending.ends_with(b"\n") {
            emit(&scrubber, &mut log, &progress, &target, &pending).await?;
            pending.clear();
        } else if pending.len() >= SEGMENT_LIMIT {
            let cut = pending.len() - scrubber.held_back.min(pending.len());
            emit(&scrubber, &mut log, &progress, &target, &pending[..cut]).await?;
            pending.drain(..cut);
        }
    }
    log.flush().await
}

/// Scrub one segment, write it, and relay it if it is an event.
async fn emit(
    scrubber: &Scrubber,
    log: &mut tokio::fs::File,
    progress: &Progress,
    target: &str,
    segment: &[u8],
) -> std::io::Result<()> {
    let clean = scrubber.scrub(segment);
    log.write_all(&clean).await?;
    // `is_active` first: the common segment is ordinary logging, and with no
    // sink attached there is nothing to decode it for.
    if progress.is_active()
        && let Some(event) = decode(&clean)
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
    /// re-holds everything it read and stops making progress.
    /// What: an absurdly long needle is capped at [`MAX_HELD_BACK`].
    /// Test: this is the test.
    #[test]
    fn an_absurd_needle_cannot_stall_the_pump() {
        let huge = Scrubber::over(vec!["q".repeat(SEGMENT_LIMIT * 2)]);
        assert_eq!(huge.held_back, MAX_HELD_BACK);
        assert!(huge.held_back < SEGMENT_LIMIT / 2);
    }
}
