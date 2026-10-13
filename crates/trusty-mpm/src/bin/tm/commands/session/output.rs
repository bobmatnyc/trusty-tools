//! `tm sessions output` — capture a session's tmux pane.
//!
//! Why: the `Output` arm of the `session` dispatcher grew a managed-session
//! path (#9162) and `session.rs` sits at the 500-SLOC cap, so the verb lives
//! here.
//! What: [`session_output`] fetches the output body and prints it; the body is
//! fetched by [`fetch_output`].
//! Test: `output_tests.rs` (`managed_*_9162`, `project_*`).

use crate::formatters::session::{input_box_note, print_compression_stats};

/// `tm sessions output <id|name>`: print the pane text, then the stats and the
/// input-box note.
///
/// Why: stdout stays the pane text byte for byte; the notes go to stderr (#8407).
/// What: prints the body's `output` to stdout, compression stats, and the
/// `[input box: ...]` note to stderr.
/// Test: `output_tests.rs`.
pub(crate) async fn session_output(
    daemon: &trusty_mpm::client::DaemonClient,
    id_or_name: &str,
    lines: u32,
    summarize: bool,
) -> anyhow::Result<()> {
    let body = fetch_output(daemon, id_or_name, lines, summarize).await?;
    let output = body.get("output").and_then(|v| v.as_str()).unwrap_or("");
    print!("{output}");
    print_compression_stats(&body);
    // #8407: on stderr, so stdout stays the pane text byte for byte.
    eprintln!("{}", input_box_note(&body));
    Ok(())
}

/// Fetch the output body for a session, managed sessions first.
///
/// Why (#9162): `GET /sessions/{id}/output` resolves only project sessions, so
/// every managed session read "not found". Stop and Resume resolve managed
/// sessions first; Output now does too.
/// What: lists the managed sessions and classifies `id_or_name` against them
/// (id, exact name, unique prefix). A match reads
/// `GET /api/v1/sessions/managed/{id}/output`; no match falls through to the
/// project route. An ambiguous prefix tries the project route first (an exact
/// project session name wins, as for Stop and Resume) and is an error naming
/// the candidates only when that route answers 404; any other project-route
/// failure is returned as itself. A list failure is returned as that failure, never as "not found", and a refused
/// managed read is returned with the daemon's reason.
/// Test: `output_tests.rs`.
pub(crate) async fn fetch_output(
    daemon: &trusty_mpm::client::DaemonClient,
    id_or_name: &str,
    lines: u32,
    summarize: bool,
) -> anyhow::Result<serde_json::Value> {
    use trusty_mpm::client::{TargetMatch, classify_target};

    // #9162: not `resolve_managed_match`, which swallows a list error as `None`
    // and would send the request down the project path to a false "not found".
    let managed = daemon.list_managed_sessions().await.map_err(|e| {
        anyhow::anyhow!("could not list managed sessions to resolve '{id_or_name}': {e}")
    })?;
    // #9162: an ambiguous managed prefix may still be an exact project session
    // name, so it tries the project route and reports ambiguity only on a 404.
    let mut ambiguous: Option<String> = None;
    let path = match classify_target(&managed, id_or_name) {
        TargetMatch::Found(m) => format!("/api/v1/sessions/managed/{}/output", m.id),
        TargetMatch::Ambiguous(all) => {
            let names: Vec<&str> = all.iter().map(|m| m.name.as_str()).collect();
            ambiguous = Some(names.join(", "));
            format!("/sessions/{id_or_name}/output")
        }
        TargetMatch::NotFound => format!("/sessions/{id_or_name}/output"),
    };
    let managed_hit = path.starts_with("/api/");
    // #6288: `lines` typed, so the socket's params struct decodes it.
    let mut req = daemon.get(path).query(&[("lines", lines)]);
    if summarize {
        req = req.query(&[("compress", "summarise")]);
    }
    let resp = req.send().await?;
    let status = resp.status();
    if managed_hit && !status.is_success() {
        let reason = resp.text().await.unwrap_or_default();
        anyhow::bail!("session '{id_or_name}' output refused ({status}): {reason}");
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        if let Some(names) = ambiguous {
            anyhow::bail!(
                "session '{id_or_name}' is ambiguous: it matches {names}; use a full name or id"
            );
        }
        anyhow::bail!("session '{id_or_name}' not found");
    }
    let body: serde_json::Value = resp.error_for_status()?.json().await?;
    Ok(body)
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod output_tests;
