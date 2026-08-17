//! Reading a child's output: everything to the log, the progress lines onward.
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
//! What: [`tee_and_relay`], one call per stream.
//!
//! ## Why bytes rather than lines of text
//!
//! A child's output is not guaranteed to be UTF-8 — one `git` message in an
//! unexpected encoding is enough. Reading `String` lines would end the pump at
//! the first such byte and truncate the log from there, turning a cosmetic
//! feature into evidence loss. So the log write is bytes, verbatim, and only a
//! line that already carries the relay marker is decoded at all.
//!
//! Test: `super::relay_tests`.

use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWriteExt as _, BufReader};
use trusty_progress::relay::StageEvent;

use crate::progress::Progress;

/// Copy `reader` into `log`, forwarding progress lines to `progress`.
///
/// # Postconditions
/// On `Ok`, every byte read was written to `log` and `log` was flushed, and
/// every well-formed relay line was delivered as a
/// [`crate::progress::ProgressUpdate::UnitStage`] for `target`. On `Err`, the
/// log is incomplete — the caller must treat that as a failure of the unit
/// rather than a cosmetic one, because the log is the evidence.
///
/// What: reads up to each `\n`, writes the bytes through unchanged, and decodes
/// the line only when it starts with the relay marker. A relay line is kept in
/// the log too: the log is a record of what the child said, not a filtered view
/// of it.
/// Test: `super::relay_tests::every_byte_reaches_the_log_and_events_reach_the_sink`.
///
/// # Errors
///
/// Any read from `reader` or write to `log`.
pub(crate) async fn tee_and_relay<R>(
    reader: R,
    mut log: tokio::fs::File,
    progress: Progress,
    target: String,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).await? == 0 {
            break;
        }
        log.write_all(&line).await?;
        if progress.is_active() {
            if let Some(event) = decode(&line) {
                progress.unit_stage(target.as_str(), event);
            }
        }
    }
    log.flush().await
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

    async fn run(input: &str) -> (String, Vec<StageEvent>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("child.log");
        let file = tokio::fs::File::create(&path).await.expect("create log");
        let (recorder, progress) = Recorder::new();

        tee_and_relay(input.as_bytes(), file, progress, "acme/api".into())
            .await
            .expect("the pump completes");

        (
            std::fs::read_to_string(&path).expect("read log"),
            recorder.stages(),
        )
    }

    /// Why: the two obligations at once, and they pull in opposite directions —
    /// a filter that forwarded events would be simpler and would silently drop
    /// them from the log a failure is diagnosed from.
    /// What: a child stream mixing ordinary logging with relay lines leaves the
    /// log byte-identical to the input AND delivers every event.
    /// Test: this is the test.
    #[tokio::test]
    async fn every_byte_reaches_the_log_and_events_reach_the_sink() {
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
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("child.log");
        let file = tokio::fs::File::create(&path).await.expect("create log");
        let (recorder, progress) = Recorder::new();
        let event = StageEvent::new("Audit", "report", StageState::Completed);

        let mut input: Vec<u8> = b"before\n\xff\xfe not text\n".to_vec();
        input.extend_from_slice(event.encode().as_bytes());
        input.push(b'\n');

        tee_and_relay(input.as_slice(), file, progress, "acme/api".into())
            .await
            .expect("the pump survives non-text output");

        assert_eq!(std::fs::read(&path).expect("read log"), input);
        assert_eq!(recorder.stages(), vec![event]);
    }
}
