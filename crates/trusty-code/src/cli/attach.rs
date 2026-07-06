//! `tcode attach <session-id>` (#2060).
//!
//! Why: the standalone streaming subcommand — attach to a session and
//! narrate its live event stream to the terminal until it reaches a
//! terminal state or the user presses Ctrl-C. Shares its polling-loop shape
//! with `cli::run_task::run` (which attaches to the session IT just
//! created); factored differently because `run_task` also needs to issue
//! the initial `task.run` call and print a final summary, while this
//! handler's whole job starts at `session.attach`.
//! What: `session.attach` for the replay burst, then
//! `StdioRpcClient::next_notification` in a loop (bounded polling interval,
//! so Ctrl-C is never more than [`POLL_INTERVAL`] late), printing each via
//! `cli_client::render::render_event_line`. Same in-memory-per-daemon
//! caveat as `cli::session`/`cli::cancel`/`cli::transcript` — see their
//! module docs.
//! Test: `tests/cli_e2e.rs::attach_unknown_session_errors_cleanly`; the
//! meaningful "real live events" case is exercised via
//! `run_task_streams_live_events_and_reports_final_status`, which attaches
//! to a session created within the SAME invocation.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::json;

use trusty_code::cli_client::StdioRpcClient;
use trusty_code::cli_client::render::render_event_line;
use trusty_code::events::Event;

use super::tcode_exe;

/// How often the attach loop wakes up to check for Ctrl-C between
/// notification polls.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// `tcode attach <SESSION_ID> [--project P]`.
///
/// Why/What: see module docs.
pub async fn run(project: &Path, session_id: &str) -> Result<()> {
    let exe = tcode_exe::resolve()?;
    let mut client = StdioRpcClient::spawn(&exe, project)?;

    let attach_result = client
        .call("session.attach", json!({"session_id": session_id}))
        .await?;
    let replay = attach_result
        .get("events")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut done = false;
    for raw in &replay {
        if let Ok(envelope) =
            serde_json::from_value::<trusty_code::events::SessionEventEnvelope>(raw.clone())
        {
            println!("{}", render_event_line(&envelope));
            done = done || matches!(envelope.event, Event::SessionDone { .. });
        }
    }

    while !done {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("tcode attach: detaching (Ctrl-C)");
                break;
            }
            notification = client.next_notification(POLL_INTERVAL) => {
                match notification? {
                    Some(envelope) => {
                        println!("{}", render_event_line(&envelope));
                        done = matches!(envelope.event, Event::SessionDone { .. });
                    }
                    None => continue,
                }
            }
        }
    }

    client.shutdown(Duration::from_secs(5)).await?;
    Ok(())
}
