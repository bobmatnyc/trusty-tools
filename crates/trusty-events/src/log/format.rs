//! NDJSON line encoding and tolerant, whole-file decoding.
//!
//! Why: [`super::recovery`] (seq high-water mark) and [`super::replay`]
//! (replay-on-reconnect) both need "every event a day file holds, in order",
//! and both must survive the one on-disk defect a crash can actually leave
//! behind: a final line cut off mid-write. Sharing one read path here is what
//! keeps that tolerance rule from being implemented — and drifting — twice.
//! What: [`encode_line`] is the write side: one `HarnessEvent` as JSON plus a
//! trailing `\n`. [`read_events`] is the read side: reads the whole file,
//! parses each non-empty line, and treats a parse failure on the LAST
//! non-empty line as a truncated write (skipped, logged at debug, not an
//! error) — a parse failure on any earlier line is still skipped so one
//! corrupt record cannot make the rest of a day's history unreadable, but it
//! logs at `warn` because it is not the expected shape of a crash.
//! Test: `super::tests::read_events_skips_a_truncated_final_line`,
//! `super::tests::read_events_skips_a_corrupt_interior_line_and_keeps_reading`,
//! `super::tests::read_events_returns_empty_for_an_empty_file`.

use std::path::Path;

use trusty_common::control_bus::HarnessEvent;

use super::error::LogError;

/// Serialize `event` as one NDJSON line, trailing newline included.
///
/// Why infallible: `HarnessEvent` derives `Serialize` over plain, always-
/// representable field types (no `f64::NAN`-style JSON landmine in this
/// envelope), so a real serialization failure here would be a bug in the type
/// itself, not a runtime condition callers should have to handle. `expect` is
/// the intentional narrow use per this crate's "no `unwrap()`, reserve
/// `expect()` for invariants that can never occur at runtime" rule.
pub(crate) fn encode_line(event: &HarnessEvent) -> Vec<u8> {
    let mut line = serde_json::to_vec(event).expect("HarnessEvent always serializes to valid JSON");
    line.push(b'\n');
    line
}

/// Read every event `path` holds, tolerating a truncated final line.
///
/// What: reads the whole file, splits on `\n`, drops empty/whitespace-only
/// lines, and parses each remaining one as a `HarnessEvent`. A parse failure
/// on the last non-empty line is assumed to be a write the process died
/// mid-way through and is silently skipped (this is the contract callers
/// rely on: "a truncated final line is skipped, not fatal"). A parse failure
/// anywhere else is also skipped, so one corrupt line cannot cost every event
/// after it, but is logged at `warn` since it is not the truncated-tail case.
///
/// # Errors
///
/// [`LogError::Io`] only for the read itself failing (permissions, the file
/// vanishing between listing and reading). A missing file is NOT an error —
/// callers that list files before reading them should not normally hit this,
/// but a `NotFound` here returns an empty `Vec` rather than erroring, so a
/// file rotated away between listing and reading degrades to "nothing found"
/// rather than aborting a whole replay.
///
/// Test: `super::tests::read_events_skips_a_truncated_final_line`,
/// `super::tests::read_events_skips_a_corrupt_interior_line_and_keeps_reading`,
/// `super::tests::read_events_returns_empty_for_an_empty_file`.
pub(crate) async fn read_events(path: &Path) -> Result<Vec<HarnessEvent>, LogError> {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(LogError::Io {
                op: "read",
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let last_index = lines.len().saturating_sub(1);

    let mut events = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        match serde_json::from_str::<HarnessEvent>(line) {
            Ok(event) => events.push(event),
            Err(e) if index == last_index => {
                tracing::debug!(
                    path = %path.display(),
                    error = %e,
                    "event log: final line did not parse; treating as a truncated \
                     write and skipping it"
                );
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    line = index,
                    error = %e,
                    "event log: interior line did not parse; skipping it and \
                     continuing"
                );
            }
        }
    }
    Ok(events)
}
