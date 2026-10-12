//! `tm hook`'s `SessionStart` delivery, and the hook-side channel-taint
//! re-apply when the daemon does not confirm it (#8433 P0, D09).
//!
//! Why (owner ruling D09): a failed taint re-apply at SessionStart must fail
//! the hook call AND leave the new session id reading tainted. A failed hook
//! does not stop a Claude session, and the egress guard reads only the taint
//! file, so the hook writes that file itself — mechanism (a) of the plan's D09
//! note. One writer and one reader path stay in force: the hook writes the
//! same `<session_id>.json` through `channels_taint::write_taint`, so S4's
//! reader needs no second marker.
//! What: [`post_lifecycle`] routes `SessionEnd` to the unchanged best-effort
//! socket post, and `SessionStart` to [`deliver_session_start`] (socket, then
//! HTTP, reporting the outcome) and [`settle_session_start`]. A daemon that
//! answers 2xx settles nothing. Otherwise, when no managed session's taint
//! covers the payload's cwd, there is nothing to re-apply and the hook
//! succeeds as before; when one does, or the marks cannot be listed, the hook
//! taints the new id (channel `reapply-failed`) for that managed session and
//! fails. If that write fails too, the
//! hook records the double failure for `tm doctor` and fails.
//! Test: `hook_session_taint_tests.rs`; end to end in
//! `tests/tm_hook_session_start_taint_8433.rs`.

use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use trusty_mpm::core::channels_taint::{self, Cover, TaintRecord};

/// What the daemon said to a `SessionStart`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// A 2xx answer: the daemon ingested the event and re-applied any taint.
    Accepted,
    /// A non-2xx answer, with its text.
    Refused(String),
    /// No answer: unreachable, timed out, or the exchange failed.
    NoAnswer(String),
}

/// Deliver one lifecycle event; `Err` fails the hook (exit 1).
///
/// What: `SessionEnd` takes [`super::hook_notify::post_lifecycle_via_socket`]
/// and always succeeds. `SessionStart` is delivered and settled under the
/// process home.
/// Test: `tests/tm_hook_session_start_taint_8433.rs`.
pub(crate) async fn post_lifecycle(
    client: &reqwest::Client,
    url: &str,
    event: &str,
    session_id: &str,
    body: &Value,
) -> anyhow::Result<()> {
    if event != "SessionStart" {
        super::hook_notify::post_lifecycle_via_socket(client, url, body).await;
        return Ok(());
    }
    let socket = super::managed_merged_prs::is_loopback_url(url)
        .then(trusty_mpm::client::http_client::resolve_daemon_socket)
        .and_then(Result::ok);
    let delivery = deliver_session_start(socket.as_deref(), url, body).await;
    let home = dirs::home_dir().filter(|home| home.is_absolute());
    let cwd = body["payload"]["cwd"]
        .as_str()
        .filter(|cwd| !cwd.is_empty())
        .map(Path::new);
    settle_session_start(home.as_deref(), session_id, cwd, &delivery).map_err(anyhow::Error::msg)
}

/// Deliver a `SessionStart` over the socket, else HTTP, and report the answer.
///
/// What: one 2 s socket attempt; only an unreachable socket falls back to
/// HTTP (500 ms connect, 2 s total), so the daemon never ingests the event
/// twice (#8531).
/// Test: `tests/tm_hook_session_start_8531.rs`,
/// `tests/tm_hook_session_start_taint_8433.rs`,
/// `socket_exchange_failure_is_no_answer_without_http_fallback`.
pub(crate) async fn deliver_session_start(
    socket: Option<&Path>,
    url: &str,
    body: &Value,
) -> Delivery {
    use trusty_mpm::client::{DaemonCallError, DaemonClient};
    if let Some(socket) = socket {
        let sent = DaemonClient::over_socket(socket)
            .post("/hooks")
            .json(body)
            .timeout(Duration::from_secs(2))
            .send()
            .await;
        match sent {
            Ok(response) => {
                return match response.error_for_status() {
                    Ok(_) => Delivery::Accepted,
                    Err(e) => Delivery::Refused(e.to_string()),
                };
            }
            Err(DaemonCallError::Unreachable { .. }) => {}
            Err(e) => return Delivery::NoAnswer(e.to_string()),
        }
    }
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(e) => return Delivery::NoAnswer(e.to_string()),
    };
    match client.post(format!("{url}/hooks")).json(body).send().await {
        Ok(response) if response.status().is_success() => Delivery::Accepted,
        Ok(response) => Delivery::Refused(response.status().to_string()),
        Err(e) => Delivery::NoAnswer(e.to_string()),
    }
}

/// Settle a `SessionStart` the daemon did not confirm (D09); `Err` fails it.
///
/// Why: the daemon is the re-apply's writer; when it did not confirm, this
/// side cannot tell whether the new id needed a taint. The hook runs in the
/// user tier, for every session on the machine, so it taints only where a
/// managed session's taint covers the cwd (critic HIGH 2): one taint must not
/// spread to every session an outage touches.
/// What: [`channels_taint::covering_mark`] for the cwd. No covering mark:
/// nothing to re-apply, the hook succeeds as before. A covering mark: the new
/// id is tainted for that managed session, so its retire removes the file.
/// Marks that cannot be listed: the new id is tainted with no owner, which
/// the doctor row reports. An unknown home fails closed.
/// Test: `accepted_delivery_settles_nothing`,
/// `daemon_unreachable_with_no_taint_anywhere_writes_nothing`,
/// `unconfirmed_delivery_with_taint_present_taints_the_new_id_and_fails`,
/// `outage_at_an_unrelated_cwd_taints_nothing`,
/// `outage_at_the_tainted_workspace_taints_the_new_id_for_its_managed_session`,
/// `a_hook_written_taint_retires_with_its_managed_session`,
/// `double_failure_is_recorded_and_fails`.
pub(crate) fn settle_session_start(
    home: Option<&Path>,
    session_id: &str,
    cwd: Option<&Path>,
    delivery: &Delivery,
) -> Result<(), String> {
    let why = match delivery {
        Delivery::Accepted => return Ok(()),
        Delivery::Refused(why) | Delivery::NoAnswer(why) => why,
    };
    let Some(home) = home else {
        return Err(format!(
            "trusty-mpm: the daemon did not confirm SessionStart ({why}) and the home directory \
             is unknown, so the channel taint cannot be checked (#8433)"
        ));
    };
    let dir = channels_taint::taint_dir(home);
    let owner = match channels_taint::covering_mark(&dir, cwd) {
        Cover::None => return Ok(()),
        Cover::Managed(managed) => managed,
        Cover::Unknown => String::new(),
    };
    if channels_taint::read_state(&dir, session_id).is_tainted() {
        return Err(format!(
            "trusty-mpm: the daemon did not confirm SessionStart ({why}); this session already \
             reads channel-tainted (#8433)"
        ));
    }
    let record = TaintRecord {
        v: channels_taint::FORMAT_VERSION,
        session_id: session_id.to_owned(),
        managed_session: owner,
        channel: channels_taint::REAPPLY_FAILED_CHANNEL.to_owned(),
        route: String::new(),
        question_id: String::new(),
        tainted_at: chrono::Utc::now(),
    };
    match channels_taint::write_taint(&dir, &record) {
        Ok(path) => Err(format!(
            "trusty-mpm: the daemon did not confirm the channel-taint re-apply for SessionStart \
             ({why}), so the hook tainted this session itself at {} (#8433 D09)",
            path.display()
        )),
        Err(e) => {
            record_double_failure(&dir, session_id, why, &e.to_string());
            Err(format!(
                "trusty-mpm: the daemon did not confirm the channel-taint re-apply ({why}) and \
                 the hook's own taint write failed ({e}); `tm doctor` reports it (#8433 D09)"
            ))
        }
    }
}

/// Append one line naming a double re-apply failure to the taint dir's
/// [`channels_taint::REAPPLY_FAILURES_FILE`].
///
/// What: best effort, mode 0600; a failed append is one stderr line. The
/// file sits under the taint-dir trust anchor, so no session can delete it,
/// and the `channel_taint` doctor row reads it.
fn record_double_failure(dir: &Path, session_id: &str, daemon: &str, hook: &str) {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = dir.join(channels_taint::REAPPLY_FAILURES_FILE);
    let line = serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "session_id": session_id,
        "daemon": daemon,
        "hook": hook,
    });
    let appended = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(&file)
        .and_then(|mut out| out.write_all(format!("{line}\n").as_bytes()));
    if let Err(e) = appended {
        eprintln!(
            "trusty-mpm: could not record the re-apply double failure in {}: {e}",
            file.display()
        );
    }
}

#[cfg(test)]
#[path = "hook_session_taint_tests.rs"]
mod tests;
