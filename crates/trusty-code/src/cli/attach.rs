//! `tcode attach <session-id>` (#2060).
//!
//! Why: the standalone streaming subcommand — attach to a session and
//! narrate its event stream to the terminal until it reaches a terminal
//! state or the user presses Ctrl-C.
//! What: dials the daemon serving `--project` on its Unix socket
//! ([`super::daemon_client::connect`], #10169) and tails `session.events`:
//! the session's ring-buffer replay, then its live events, on one
//! connection, each printed via `cli_client::render::render_event_line`.
//! `session.attach` is refused on the socket, which cannot push
//! notifications (`crate::serve::uds`). The command ends on `SessionDone` or
//! `SessionCancelled`. A stream error, or a stream the daemon ends before
//! either event, is an error and a nonzero exit — never a clean detach.
//! Test: `thin_clients_uds_10169::attach_replays_a_session_created_on_the_daemon`,
//! `thin_clients_uds_10169::attach_unknown_session_errors_cleanly`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::json;

use trusty_code::cli_client::render::render_event_line;
use trusty_code::events::{Event, SessionEventEnvelope};

use super::daemon_client;

/// `tcode attach <SESSION_ID> [--project P]`.
///
/// Why/What: see module docs.
pub async fn run(project: &Path, session_id: &str) -> Result<()> {
    // #10169: tail the running daemon's session, not a fresh stdio child's.
    let client = daemon_client::connect(project).await?;
    let mut stream = client
        .open_stream::<SessionEventEnvelope>("session.events", json!({"session_id": session_id}))
        .await?;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("tcode attach: detaching (Ctrl-C)");
                return Ok(());
            }
            frame = stream.next_frame() => match frame {
                Some(Ok(envelope)) => {
                    println!("{}", render_event_line(&envelope));
                    if matches!(
                        envelope.event,
                        Event::SessionDone { .. } | Event::SessionCancelled { .. }
                    ) {
                        return Ok(());
                    }
                }
                Some(Err(e)) => return Err(e).context("session.events stream failed"),
                None => bail!("the daemon ended the session.events stream before the session finished"),
            }
        }
    }
}
