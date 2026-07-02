//! Client-side SSE watcher for real-time provisioning-stage progress (#1904).
//!
//! Why: [`super::guided_launch::launch_new_session_and_attach`] wraps the
//! blocking managed-spawn POST in a spinner; this module makes that spinner
//! show REAL steps ("Cloning repository…", "Deploying agents…", …) instead of
//! one static message, by subscribing to the daemon's existing `GET /events`
//! SSE stream (`crate::daemon::api::stream_events`) and watching for the
//! `provisioning_stage` envelopes `crate::core::provisioning_stage::emit`
//! publishes. The client cannot subscribe to the per-session
//! `GET /sessions/{id}/events` stream because it does not learn the session id
//! until the wrapped POST returns — that is the entire problem this feature
//! solves — so it subscribes to the GLOBAL stream and correlates by
//! `repo_url` instead (a value the client already has before issuing the
//! POST). Known limitation: two concurrent `tm` launches for the SAME
//! `repo_url` against the same daemon would each see the other's stage
//! events; this is purely cosmetic (the actual outcome is still driven by the
//! POST response), so it is an accepted trade-off for staying minimal.
//! What: [`watch_provisioning_stages`] is the background task
//! [`launch_new_session_and_attach`](super::guided_launch::launch_new_session_and_attach)
//! spawns and aborts once the POST returns; [`parse_stage_frame`] is the pure
//! byte-buffer-free frame parser it uses, extracted so the parsing logic is
//! unit-testable without a live daemon or network connection.
//! Test: `parse_stage_frame_extracts_matching_repo_url`,
//! `parse_stage_frame_ignores_other_repo_url`,
//! `parse_stage_frame_ignores_keep_alive_comment`,
//! `parse_stage_frame_ignores_non_stage_events`.

use tokio_stream::StreamExt;

use trusty_mpm::core::provisioning_stage::ProvisioningStage;

/// One parsed `provisioning_stage` SSE envelope (see
/// `crate::core::provisioning_stage::emit`'s JSON shape).
struct StageEnvelope {
    repo_url: String,
    stage: String,
    label: String,
}

/// Parse one SSE frame (the bytes between two `\n\n` delimiters) into a
/// [`StageEnvelope`], if it is a `provisioning_stage` event.
///
/// Why: isolating the parsing from the network I/O loop lets it be tested
/// with plain string fixtures — no daemon, no tokio runtime, no bytes stream.
/// What: joins every `data:`-prefixed line in the frame (trimming the
/// optional leading space SSE writers add), JSON-decodes the result, and
/// extracts `repo_url`/`stage`/`label` when `"kind" == "provisioning_stage"`.
/// Returns `None` for keep-alive comment frames (`: ping`), frames with no
/// `data:` line, malformed JSON, or any other event `kind`.
/// Test: `parse_stage_frame_extracts_matching_repo_url`,
/// `parse_stage_frame_ignores_keep_alive_comment`,
/// `parse_stage_frame_ignores_non_stage_events`.
fn parse_stage_frame(frame: &str) -> Option<StageEnvelope> {
    let data: String = frame
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&data).ok()?;
    if value.get("kind").and_then(serde_json::Value::as_str) != Some("provisioning_stage") {
        return None;
    }
    Some(StageEnvelope {
        repo_url: value.get("repo_url")?.as_str()?.to_string(),
        stage: value.get("stage")?.as_str()?.to_string(),
        label: value.get("label")?.as_str()?.to_string(),
    })
}

/// Subscribe to `GET {events_url}` and update `spinner`'s message as
/// `provisioning_stage` events matching `repo_url` arrive.
///
/// Why: this is the background half of the #1904 stretch goal — real
/// step-by-step progress instead of a static spinner message. It is spawned
/// as a detached task by the caller and aborted once the wrapped POST
/// returns, so it never outlives the launch it is reporting on.
/// What: opens the SSE connection; on ANY failure to connect (older daemon
/// without the route, network error, non-2xx) returns immediately and
/// silently — the caller's spinner keeps its original static message, which
/// is the exact pre-#1904-stretch-goal behaviour, so a daemon that cannot
/// stream events never regresses the already-shipped minimum-bar spinner.
/// On success, reads the byte stream, splits it into SSE frames on `\n\n`,
/// and for each [`parse_stage_frame`] result whose `repo_url` matches, calls
/// `spinner.set_message("tm: <label>…")`. Returns as soon as a `Complete`
/// stage for the matching `repo_url` arrives, or when the stream ends/errors.
/// Test: the frame-parsing logic is unit-tested via [`parse_stage_frame`]
/// directly; the network loop itself is exercised end-to-end by manual
/// verification (see PR description) since it requires a live daemon.
pub(crate) async fn watch_provisioning_stages(
    client: reqwest::Client,
    events_url: String,
    repo_url: String,
    spinner: trusty_progress::ProgressHandle,
) {
    let Ok(resp) = client.get(&events_url).send().await else {
        return;
    };
    if !resp.status().is_success() {
        return;
    }
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    while let Some(Ok(chunk)) = stream.next().await {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = buf.find("\n\n") {
            let frame: String = buf.drain(..idx + 2).collect();
            let Some(envelope) = parse_stage_frame(&frame) else {
                continue;
            };
            if envelope.repo_url != repo_url {
                continue;
            }
            spinner.set_message(format!("tm: {}…", envelope.label));
            if envelope.stage == ProvisioningStage::Complete.wire_name() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the happy path — a well-formed envelope for OUR repo_url must be
    /// extracted so the caller can update the spinner.
    /// What: builds a single-line SSE `data:` frame and asserts every field
    /// round-trips.
    /// Test: this is the test.
    #[test]
    fn parse_stage_frame_extracts_matching_repo_url() {
        let frame = "data: {\"kind\":\"provisioning_stage\",\"session\":\"s1\",\"repo_url\":\"https://github.com/acme/widgets\",\"stage\":\"CloningRepo\",\"label\":\"Cloning repository\",\"at\":\"2026-01-01T00:00:00Z\"}\n\n";
        let env = parse_stage_frame(frame).expect("parses");
        assert_eq!(env.repo_url, "https://github.com/acme/widgets");
        assert_eq!(env.stage, "CloningRepo");
        assert_eq!(env.label, "Cloning repository");
    }

    /// Why: `watch_provisioning_stages` filters by `repo_url` after parsing;
    /// this test only asserts the PARSE succeeds regardless of whose repo it
    /// is — the filtering itself is the caller's job, exercised in the loop
    /// (not unit-testable without a live stream), so parsing must not itself
    /// discard a non-matching repo_url.
    /// What: parses a frame for a DIFFERENT repo and asserts the fields are
    /// still extracted correctly (the caller does the comparison).
    /// Test: this is the test.
    #[test]
    fn parse_stage_frame_ignores_other_repo_url() {
        let frame = "data: {\"kind\":\"provisioning_stage\",\"session\":\"s1\",\"repo_url\":\"https://github.com/other/repo\",\"stage\":\"CloningRepo\",\"label\":\"Cloning repository\",\"at\":\"2026-01-01T00:00:00Z\"}\n\n";
        let env = parse_stage_frame(frame).expect("parses");
        assert_eq!(env.repo_url, "https://github.com/other/repo");
    }

    /// Why: the daemon's SSE keep-alive is a `: ping` comment line (no
    /// `data:` prefix) sent every 15s; it must never be mistaken for a stage
    /// event.
    /// What: asserts a comment-only frame parses to `None`.
    /// Test: this is the test.
    #[test]
    fn parse_stage_frame_ignores_keep_alive_comment() {
        assert!(parse_stage_frame(": ping\n\n").is_none());
    }

    /// Why: the global `/events` stream also carries ordinary `HookEventRecord`
    /// frames (issue #1270's existing hook relay); those must be ignored, not
    /// mistaken for provisioning-stage events.
    /// What: asserts a `data:` frame with `"kind"` absent (a plain hook event
    /// envelope has no `"kind"` field at all) parses to `None`.
    /// Test: this is the test.
    #[test]
    fn parse_stage_frame_ignores_non_stage_events() {
        let frame = "data: {\"session\":\"s1\",\"event\":\"PreToolUse\",\"at\":\"2026-01-01T00:00:00Z\",\"payload\":{}}\n\n";
        assert!(parse_stage_frame(frame).is_none());
        assert!(parse_stage_frame("data: not json at all\n\n").is_none());
        assert!(parse_stage_frame("\n\n").is_none());
    }
}
