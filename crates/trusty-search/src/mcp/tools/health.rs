//! Structured diagnostics for the `search_health` MCP tool (#5264).
//!
//! Why: the arm used to forward `GET /health` verbatim, so a caller got one of
//! two useless answers. When nothing was listening it got a raw reqwest string
//! (`GET http://127.0.0.1:7878/health: error trying to connect: tcp connect
//! error: Connection refused`) with no remedy in it; when a daemon DID answer
//! it got a bare `200` that named neither which daemon replied nor whether the
//! caller's own project was indexed on it. A probe from an isolated project
//! silently attached to this machine's production daemon — 42 indexes, 430k
//! chunks — and reported healthy, which is the failure this module exists to
//! make impossible to miss.
//!
//! What: [`handle_search_health`] returns a report that always names the
//! answering daemon and separates three states a caller must act on
//! differently — nothing listening, something answering badly, and a healthy
//! daemon that has no index for this project. Every state carries a
//! `remediation` string naming the command that fixes it. The daemon's own
//! `/health` fields pass through verbatim; this module adds no field the daemon
//! did not send, following the same rule [`super::unavailable`] states.
//!
//! Test: `mcp/tools/tests_health.rs`, and end to end over a real stdio session
//! in the crate's `mcp_stdio_e2e_5264` integration test.

use serde_json::{Map, Value};

use super::McpServer;

/// The daemon answered and the resolved project index holds chunks.
pub const HEALTH_OK: &str = "ok";
/// Nothing is listening at the daemon address this session resolved.
pub const HEALTH_DAEMON_UNREACHABLE: &str = "daemon_unreachable";
/// Something answered the health probe, but not with a 2xx health object.
pub const HEALTH_DAEMON_ERROR: &str = "daemon_error";
/// The daemon is healthy; it has no index registered for this project.
pub const HEALTH_INDEX_NOT_REGISTERED: &str = "index_not_registered";
/// The index is registered on the daemon but holds zero chunks.
pub const HEALTH_INDEX_EMPTY: &str = "index_empty";
/// No index could be named, so the project-level check never ran.
///
/// Why: this is not `ok`. Reporting the daemon healthy when the caller's own
/// project was never checked is the same unverified-green this tool exists to
/// stop, moved one layer down from the daemon to the index.
pub const HEALTH_INDEX_UNKNOWN: &str = "index_unknown";

/// Longest response-body excerpt echoed back in a diagnostic.
const BODY_EXCERPT_CHARS: usize = 400;

/// Fields of the daemon's `/health` body forwarded into the report.
///
/// Together with `base_url` these are what let a caller tell "healthy" from
/// "healthy, but not the daemon I meant": an isolated instance reports one or
/// two indexes and a few thousand chunks where the machine-wide daemon reports
/// dozens and hundreds of thousands.
const DAEMON_IDENTITY_FIELDS: &[&str] = &[
    "status",
    "version",
    "indexes",
    "indexes_populated",
    "total_chunks",
    "uptime_secs",
    "embedder",
];

/// Answer the `search_health` tool call.
///
/// Why (#5264): a health probe whose only failure mode is a transport string
/// cannot tell an agent what to do next, and one that reports `200` without
/// naming the responder cannot tell it whether the right daemon replied.
/// What: probes `GET /health`, and on success probes the resolved project's
/// `GET /indexes/{id}/status`, then folds both into one report. Never returns
/// `Err`: a daemon that cannot be reached is itself the health verdict, so it
/// belongs in the response body rather than in an error envelope the caller
/// would have to parse prose out of. The report's `healthy` boolean is the
/// field to branch on, because a successful tool call no longer implies a
/// healthy daemon.
/// Test: `search_health_reports_daemon_unreachable_with_remediation`,
/// `search_health_reports_a_non_2xx_daemon`,
/// `search_health_reports_an_unregistered_project_index`,
/// `search_health_names_the_answering_daemon`,
/// `search_health_does_not_report_ok_when_no_index_could_be_resolved`.
pub(super) async fn handle_search_health(server: &McpServer, args: &Value) -> Value {
    report_health(server, resolve_scope(server, args)).await
}

/// Pure-ish core of [`handle_search_health`], taking the resolved index scope
/// as a parameter instead of deriving it from process state.
///
/// Why: `resolve_scope`'s last fallback reads `std::env::current_dir()`, so the
/// "no index could be named" branch is unreachable from an in-process test —
/// the working directory always yields an id. Injecting the scope mirrors
/// `doctor_data_dir_from`'s split in `commands::doctor_checks` and makes that
/// branch directly assertable.
/// What: identical to the wrapper except that `scope` is supplied.
/// Test: `search_health_does_not_report_ok_when_no_index_could_be_resolved`.
pub(super) async fn report_health(
    server: &McpServer,
    scope: Option<(String, &'static str)>,
) -> Value {
    let base = server.base_url.clone();

    let health = match probe_daemon(server).await {
        DaemonProbe::Unreachable { detail } => {
            let daemon = serde_json::json!({
                "base_url": base,
                "reachable": false,
                "error": detail,
            });
            return report(
                HEALTH_DAEMON_UNREACHABLE,
                daemon,
                Value::Null,
                format!("No trusty-search daemon is listening at {base} ({detail})."),
                "Start it with `trusty-search start`, then retry. If a daemon IS \
                 running elsewhere, the address was resolved from this process's \
                 TRUSTY_DATA_DIR — check that it points at the intended instance.",
            );
        }
        DaemonProbe::HttpError { status, body } => {
            let daemon = serde_json::json!({
                "base_url": base,
                "reachable": true,
                "http_status": status,
                "body": body,
            });
            return report(
                HEALTH_DAEMON_ERROR,
                daemon,
                Value::Null,
                format!(
                    "Something is listening at {base} but answered /health with HTTP {status}."
                ),
                "Check `trusty-search status` and the daemon log. An HTTP status \
                 from a process that is not trusty-search means another service \
                 holds this port — stop it, or point this session at the right \
                 daemon with TRUSTY_DATA_DIR.",
            );
        }
        DaemonProbe::Ok(body) => body,
    };

    let daemon = daemon_identity(&base, &health);
    let answered = summarize_daemon(&base, &daemon);

    // #5264: the daemon is fine, but nothing about the CALLER's project was
    // verified. Reporting `ok`/`healthy: true` here would be a green verdict on
    // a check that never ran — the same defect this tool fixes at the daemon
    // layer, one level down.
    let Some((index_id, source)) = scope else {
        return report(
            HEALTH_INDEX_UNKNOWN,
            daemon,
            Value::Null,
            format!(
                "{answered} No index could be named — this session has no pin, no \
                 `index_id` was given, and none could be derived from the working \
                 directory — so NO project-level check ran and this reports nothing \
                 about whether your searches will find anything."
            ),
            "Call `list_indexes` to see what this daemon serves, then re-run \
             `search_health` with an explicit `index_id`.",
        );
    };

    match probe_index(server, &index_id).await {
        IndexProbe::Missing => {
            let index = index_scope(
                &index_id,
                source,
                serde_json::json!({ "registered": false }),
            );
            report(
                HEALTH_INDEX_NOT_REGISTERED,
                daemon,
                index,
                format!("{answered} It has no index '{index_id}' ({source}), so searches for this project will find nothing."),
                "Index the project with `trusty-search index <path>`. If the id \
                 looks wrong, `list_indexes` shows the ids this daemon actually \
                 serves.",
            )
        }
        IndexProbe::Unknown { detail } => {
            let index = index_scope(
                &index_id,
                source,
                serde_json::json!({ "registered": null, "error": detail }),
            );
            report(
                HEALTH_DAEMON_ERROR,
                daemon,
                index,
                format!(
                    "{answered} Its status for index '{index_id}' could not be read: {detail}."
                ),
                "Retry once; a 503 here is usually a load or restore in flight. \
                 If it persists, check the daemon log.",
            )
        }
        IndexProbe::Present { body } => {
            let chunks = body.get("chunk_count").and_then(Value::as_u64).unwrap_or(0);
            let mut detail = Map::new();
            detail.insert("registered".into(), Value::Bool(true));
            for key in ["chunk_count", "root_path", "watcher", "semantic_coverage"] {
                if let Some(v) = body.get(key) {
                    detail.insert(key.to_string(), v.clone());
                }
            }
            let index = index_scope(&index_id, source, Value::Object(detail));
            if chunks == 0 {
                report(
                    HEALTH_INDEX_EMPTY,
                    daemon,
                    index,
                    format!("{answered} Index '{index_id}' ({source}) is registered but holds 0 chunks, so every search against it returns nothing."),
                    "Populate it with `trusty-search index <path>`, or \
                     `trusty-search doctor --fix`, which reindexes every \
                     zero-chunk index.",
                )
            } else {
                report(
                    HEALTH_OK,
                    daemon,
                    index,
                    format!("{answered} Index '{index_id}' ({source}) holds {chunks} chunks."),
                    "None needed.",
                )
            }
        }
    }
}

/// Assemble one report body. `status` decides `healthy`.
fn report(status: &str, daemon: Value, index: Value, message: String, remediation: &str) -> Value {
    serde_json::json!({
        "status": status,
        "healthy": status == HEALTH_OK,
        "message": message,
        "remediation": remediation,
        "daemon": daemon,
        "index": index,
    })
}

/// The `index` block: which id was checked, where the id came from, and what
/// the daemon said about it.
fn index_scope(index_id: &str, source: &'static str, detail: Value) -> Value {
    let mut out = match detail {
        Value::Object(m) => m,
        other => {
            let mut m = Map::new();
            m.insert("detail".into(), other);
            m
        }
    };
    out.insert("index_id".into(), Value::from(index_id));
    out.insert("resolved_from".into(), Value::from(source));
    Value::Object(out)
}

/// Which daemon answered, in one sentence the model reads before the payload.
fn summarize_daemon(base: &str, daemon: &Value) -> String {
    let field = |k: &str| {
        daemon
            .get(k)
            .filter(|v| !v.is_null())
            .map(|v| v.to_string().trim_matches('"').to_string())
    };
    let version = field("version").unwrap_or_else(|| "unknown".into());
    let indexes = field("indexes").unwrap_or_else(|| "?".into());
    let chunks = field("total_chunks").unwrap_or_else(|| "?".into());
    format!(
        "The trusty-search daemon at {base} answered: version {version}, \
         {indexes} index(es), {chunks} chunks."
    )
}

/// Forward the daemon's own identifying `/health` fields, plus the address it
/// answered on.
fn daemon_identity(base: &str, health: &Value) -> Value {
    let mut out = Map::new();
    out.insert("base_url".into(), Value::from(base));
    out.insert("reachable".into(), Value::Bool(true));
    for key in DAEMON_IDENTITY_FIELDS {
        if let Some(v) = health.get(*key) {
            out.insert((*key).to_string(), v.clone());
        }
    }
    Value::Object(out)
}

/// Which index this probe is about, and how that was decided.
///
/// Why: "is this project indexed?" is only answerable once an id is fixed, and
/// an agent that has to guess one reintroduces the wrong-index defect #1373
/// closed. Reporting the SOURCE alongside the id is what lets a reader see that
/// a `cwd`-derived answer says nothing about the index a pinned session uses.
/// What: an explicit `index_id` argument wins, then the session pin (#1373),
/// then the id the CLI would derive from the working directory.
/// Test: `search_health_reports_which_source_named_the_index`.
fn resolve_scope(server: &McpServer, args: &Value) -> Option<(String, &'static str)> {
    if let Some(id) = args.get("index_id").and_then(Value::as_str) {
        if !id.trim().is_empty() {
            return Some((id.to_string(), "argument"));
        }
    }
    if let Some(id) = server.pinned_index.clone() {
        return Some((id, "session_pin"));
    }
    let cwd = std::env::current_dir().ok()?;
    let root = trusty_common::resolve_project_root(&cwd);
    let id = trusty_common::derive_index_id(&root);
    (!id.trim().is_empty()).then_some((id, "cwd"))
}

/// What `GET /health` did.
enum DaemonProbe {
    /// The TCP connection never produced an HTTP response.
    Unreachable { detail: String },
    /// An HTTP response arrived that was not a 2xx health object.
    HttpError { status: u16, body: String },
    /// A 2xx JSON object.
    Ok(Value),
}

/// Probe `GET /health` while preserving WHY it failed.
///
/// Why (#5264): [`McpServer::get`] flattens a refused connection and a 500 into
/// the same `DispatchError::Transport(String)`. The two need different
/// remediation — start the daemon, versus find out what else holds the port —
/// so this is deliberately the one place in the crate that reads
/// `reqwest::Error`'s classifiers instead of its `Display` text.
/// What: three verdicts. A 2xx body that is not a JSON object counts as an HTTP
/// error, because a process answering `/health` with something else is not this
/// daemon and saying "healthy" about it would be the exact failure #5264 filed.
/// Test: `search_health_reports_daemon_unreachable_with_remediation`,
/// `search_health_reports_a_non_2xx_daemon`,
/// `search_health_rejects_a_2xx_body_that_is_not_a_health_object`.
async fn probe_daemon(server: &McpServer) -> DaemonProbe {
    let url = format!("{}/health", server.base_url);
    let resp = match server.http.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            let kind = if e.is_connect() {
                "connection refused"
            } else if e.is_timeout() {
                "timed out"
            } else {
                "request failed"
            };
            return DaemonProbe::Unreachable {
                detail: format!("{kind}: {e}"),
            };
        }
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return DaemonProbe::HttpError {
            status: status.as_u16(),
            body: excerpt(&text),
        };
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(v) if v.is_object() => DaemonProbe::Ok(v),
        _ => DaemonProbe::HttpError {
            status: status.as_u16(),
            body: format!("2xx body is not a JSON health object: {}", excerpt(&text)),
        },
    }
}

/// What `GET /indexes/{id}/status` said.
enum IndexProbe {
    /// The daemon 404'd: this project has no index here.
    Missing,
    /// The daemon answered, but with neither 200 nor 404.
    Unknown { detail: String },
    /// A 2xx status body.
    Present { body: Value },
}

/// Probe one index's status, keeping `404` distinct from every other failure.
///
/// Why: `404` is the "this project is unindexed" signal the health report owes
/// its caller; folding it in with a 503 or a transport failure would tell an
/// agent to reindex when it should have retried.
/// Test: `search_health_reports_an_unregistered_project_index`.
async fn probe_index(server: &McpServer, index_id: &str) -> IndexProbe {
    let url = format!("{}/indexes/{}/status", server.base_url, index_id);
    let resp = match server.http.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            return IndexProbe::Unknown {
                detail: format!("request failed: {e}"),
            };
        }
    };
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return IndexProbe::Missing;
    }
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return IndexProbe::Unknown {
            detail: format!("HTTP {}: {}", status.as_u16(), excerpt(&text)),
        };
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(body) => IndexProbe::Present { body },
        Err(e) => IndexProbe::Unknown {
            detail: format!("undecodable 2xx status body: {e}"),
        },
    }
}

/// Bound an echoed response body so a large error page cannot flood the model's
/// context.
fn excerpt(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= BODY_EXCERPT_CHARS {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(BODY_EXCERPT_CHARS).collect();
    format!("{head}… (truncated)")
}
