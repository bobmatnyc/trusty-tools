//! Structured diagnostics for the `search_health` MCP tool (#5264).
//!
//! Why: the arm used to forward the daemon's health body verbatim, so a caller
//! got one of two useless answers. When nothing was listening it got a raw
//! transport string with no remedy in it; when a daemon DID answer
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
//! `search.health` fields pass through verbatim; this module adds no field the
//! daemon did not send, following the same rule [`super::unavailable`] states.
//! #9168: the probes go over the daemon's socket, so the report names the
//! `socket` that answered where it used to name a `base_url`.
//!
//! Test: `mcp/tools/tests_health.rs`, and end to end over a real stdio session
//! in the crate's `mcp_stdio_e2e_5264` integration test.

use std::path::Path;

use serde_json::{Map, Value};

use super::health_verdicts::{
    embedder_unavailable, failed_migration_stages, held_reason, unknown_count_cause,
};
use super::McpServer;
use crate::mcp::cwd_scope::{
    confirm_candidate, derive_cwd_candidate, parse_index_entries, Confirmation, CwdCandidate,
    DaemonIndex,
};
use crate::service::rpc::reads::{METHOD_INDEXES_LIST, METHOD_INDEX_STATUS};

/// The daemon answered and the resolved project index holds chunks.
pub const HEALTH_OK: &str = "ok";
/// Nothing is serving the daemon socket this session resolved.
pub const HEALTH_DAEMON_UNREACHABLE: &str = "daemon_unreachable";
/// Something answered the health probe, but not with a health object.
pub const HEALTH_DAEMON_ERROR: &str = "daemon_error";
/// The daemon is healthy; it has no index registered for this project.
pub const HEALTH_INDEX_NOT_REGISTERED: &str = "index_not_registered";
/// The index is registered on the daemon but holds zero chunks.
pub const HEALTH_INDEX_EMPTY: &str = "index_empty";
/// The project-level check produced no verdict.
///
/// Why: this is not `ok`. Reporting the daemon healthy when the caller's own
/// project was never checked is the same unverified-green this tool exists to
/// stop, moved one layer down from the daemon to the index.
///
/// Covers two shapes, distinguishable by the report's `index` block. No index
/// could be NAMED, so nothing was probed at all (`index` is null); or an index
/// was probed and the daemon declined to state its chunk count (#5633 — `index`
/// carries `chunk_count: null`, plus `corpus_open_failure` when that is why).
/// Both are a check that never established an answer, which is why they share a
/// status; the remediation differs and is set per shape.
pub const HEALTH_INDEX_UNKNOWN: &str = "index_unknown";

/// A migration this index needs failed, so its contents are not what they
/// should be (#7979).
///
/// Why: this is not `ok` and it is not `index_empty`. A failed
/// `chunks.json` → `index.redb` migration leaves the index serving 0 chunks
/// that a reindex CANNOT repair — the source of truth is a snapshot the daemon
/// refuses to parse, and reindexing overwrites nothing useful while destroying
/// the operator's chance to look at it. The empty-index verdict prescribes
/// exactly that reindex, so the migration fault has to be checked first.
pub const HEALTH_INDEX_MIGRATION_FAILED: &str = "index_migration_failed";

/// The index is fine, but the daemon's embedder is not answering (#8348).
///
/// Why: searches still return lexical rows, flagged `meta.vector_unavailable`,
/// so every other check passes — and `ok` hid a daemon serving no semantic
/// results at all.
pub const HEALTH_EMBEDDER_UNAVAILABLE: &str = "embedder_unavailable";

/// The index serves reads but indexes nothing new: an exclude glob does not
/// parse, so the daemon holds it until a PATCH fixes the glob (#9059).
pub const HEALTH_INDEX_HELD: &str = "index_held";

/// Longest response-body excerpt echoed back in a diagnostic.
const BODY_EXCERPT_CHARS: usize = 400;

/// Fields of the daemon's `search.health` body forwarded into the report.
///
/// Together with `socket` these are what let a caller tell "healthy" from
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
/// What: probes `search.health`, and on success probes the resolved
/// project's `search.index.status`, then folds both into one report. Never returns
/// `Err`: a daemon that cannot be reached is itself the health verdict, so it
/// belongs in the response body rather than in an error envelope the caller
/// would have to parse prose out of. The report's `healthy` boolean is the
/// field to branch on, because a successful tool call no longer implies a
/// healthy daemon.
/// Test: `search_health_reports_daemon_unreachable_with_remediation`,
/// `search_health_reports_a_daemon_that_answers_badly`,
/// `search_health_reports_an_unregistered_project_index`,
/// `search_health_names_the_answering_daemon`,
/// `search_health_does_not_report_ok_when_no_index_could_be_resolved`,
/// `search_health_does_not_report_an_unreadable_chunk_count_as_empty`.
pub(super) async fn handle_search_health(server: &McpServer, args: &Value) -> Value {
    let cwd = std::env::current_dir().ok();
    report_health(server, resolve_scope(server, args, cwd.as_deref())).await
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
pub(super) async fn report_health(server: &McpServer, scope: Option<Scope>) -> Value {
    let base = server.daemon.socket().display().to_string();

    let health = match probe_daemon(server).await {
        DaemonProbe::Unreachable { detail } => {
            let daemon = serde_json::json!({
                "socket": base,
                "reachable": false,
                "error": detail,
            });
            return report(
                HEALTH_DAEMON_UNREACHABLE,
                daemon,
                Value::Null,
                format!("No trusty-search daemon is serving socket {base} ({detail})."),
                "Start it with `trusty-search start`, then retry. If a daemon IS \
                 running elsewhere, the socket was resolved from this process's \
                 TRUSTY_DATA_DIR (or TRUSTY_SEARCH_SOCKET) — check that it points at \
                 the intended instance.",
            );
        }
        DaemonProbe::Error { rpc_code, detail } => {
            let daemon = serde_json::json!({
                "socket": base,
                "reachable": true,
                "rpc_code": rpc_code,
                "error": detail,
            });
            return report(
                HEALTH_DAEMON_ERROR,
                daemon,
                Value::Null,
                format!(
                    "Something is serving socket {base} but did not answer search.health \
                     with a health object: {detail}."
                ),
                "Check `trusty-search status` and the daemon log. A refusal from a \
                 process that is not trusty-search means another service holds this \
                 socket — stop it, or point this session at the right daemon with \
                 TRUSTY_DATA_DIR.",
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
    let Some(scope) = scope else {
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
    let (index_id, source) = match scope {
        Scope::Named { index_id, source } => (index_id, source),
        Scope::Cwd(candidate) => {
            match confirm_cwd_scope(server, &candidate, &daemon, &answered).await {
                Ok(named) => named,
                Err(refusal) => return refusal,
            }
        }
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
            let mut detail = Map::new();
            detail.insert("registered".into(), Value::Bool(true));
            for key in [
                "chunk_count",
                "root_path",
                "watcher",
                "semantic_coverage",
                // #5633: the daemon sends this beside a null `chunk_count`. It
                // is the REASON the count is unknown, and it was arriving in
                // the response body and being dropped here.
                "corpus_open_failure",
                // #7979: a migration that failed at boot is why an index can
                // read as empty; dropping it here left `search_health` silent
                // about the one fact that explains the count.
                "migration_error",
            ] {
                if let Some(v) = body.get(key) {
                    detail.insert(key.to_string(), v.clone());
                }
            }
            let index = index_scope(&index_id, source, Value::Object(detail));
            // #7979: checked BEFORE the chunk-count arms. A migration fault is
            // why the count is what it is, and the `index_empty` arm below
            // prescribes the one action — reindex — that cannot fix it.
            if let Some(stages) = failed_migration_stages(&body) {
                return report(
                    HEALTH_INDEX_MIGRATION_FAILED,
                    daemon,
                    index,
                    format!(
                        "{answered} Index '{index_id}' ({source}) has a FAILED migration \
                         ({stages}), so whatever it reports holding is not what it should \
                         hold. This is NOT an empty index and NOT a healthy one."
                    ),
                    "Read `migration_error` on `index_status` for the failure text. Do NOT \
                     reindex on this verdict — a reindex cannot repair a corrupt legacy \
                     snapshot and destroys the evidence. Fix or move aside the file the \
                     error names, then restart the daemon so the migration retries.",
                );
            }
            // #9059: a held index answers searches, so the count arms read it `ok`.
            if let Some(why) = held_reason(&body) {
                return report(
                    HEALTH_INDEX_HELD,
                    daemon,
                    index,
                    format!("{answered} Index '{index_id}' ({source}) is HELD: {why}"),
                    "Fix `exclude_globs` with `PATCH /indexes/<id>/config`; the hold lifts \
                     without a restart, and the PATCH starts a catch-up reindex for the \
                     changes refused while held (`catch_up_reindex` in its response). \
                     A reindex before the fix is refused.",
                );
            }
            // #5633: the daemon's `chunk_count` is `Option<usize>` and a null
            // one rides a 200, so `unwrap_or(0)` turned "I could not read this"
            // into "it holds nothing" — and prescribed the reindex that is
            // exactly wrong against a write-quarantined corpus.
            match body.get("chunk_count").and_then(Value::as_u64) {
                Some(0) => report(
                    HEALTH_INDEX_EMPTY,
                    daemon,
                    index,
                    format!("{answered} Index '{index_id}' ({source}) is registered but holds 0 chunks, so every search against it returns nothing."),
                    "Populate it with `trusty-search index <path>`, or \
                     `trusty-search doctor --fix`, which reindexes every \
                     zero-chunk index.",
                ),
                // #8348: checked only once the index itself is healthy.
                Some(chunks) if embedder_unavailable(&health) => report(
                    HEALTH_EMBEDDER_UNAVAILABLE,
                    daemon,
                    index,
                    format!("{answered} Index '{index_id}' ({source}) holds {chunks} chunks, but the embedder is not answering, so searches return lexical results only (`meta.vector_unavailable: true`)."),
                    "Check that `trusty-embedderd` is installed and starts (`trusty-search doctor`), \
                     then read `embedder_error` on `/health` and the daemon log.",
                ),
                Some(chunks) => report(
                    HEALTH_OK,
                    daemon,
                    index,
                    format!("{answered} Index '{index_id}' ({source}) holds {chunks} chunks."),
                    "None needed.",
                ),
                // Absent, null, or non-numeric: the count was never
                // established. That is not a count of zero.
                None => {
                    let (because, remediation) = unknown_count_cause(&body);
                    report(
                        HEALTH_INDEX_UNKNOWN,
                        daemon,
                        index,
                        format!(
                            "{answered} Index '{index_id}' ({source}) is registered, but \
                             {because}, so this reports NOTHING about how many chunks it \
                             holds or whether searches against it will find anything. This \
                             is NOT a report of an empty index."
                        ),
                        remediation,
                    )
                }
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
fn summarize_daemon(socket: &str, daemon: &Value) -> String {
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
        "The trusty-search daemon on socket {socket} answered: version {version}, \
         {indexes} index(es), {chunks} chunks."
    )
}

/// Forward the daemon's own identifying health fields, plus the socket it
/// answered on.
fn daemon_identity(socket: &str, health: &Value) -> Value {
    let mut out = Map::new();
    out.insert("socket".into(), Value::from(socket));
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
/// then a candidate derived from `cwd`. The cwd tier is returned UNCONFIRMED —
/// see [`Scope::Cwd`]. `cwd` is a parameter so tests need not change the
/// process-global working directory.
/// Test: `search_health_reports_which_source_named_the_index`,
/// `cwd_fallback_resolves_same_basename_checkouts_to_their_own_indexes`.
pub(super) fn resolve_scope(server: &McpServer, args: &Value, cwd: Option<&Path>) -> Option<Scope> {
    if let Some(id) = args.get("index_id").and_then(Value::as_str) {
        if !id.trim().is_empty() {
            return Some(Scope::Named {
                index_id: id.to_string(),
                source: "argument",
            });
        }
    }
    if let Some(id) = server.pinned_index.clone() {
        return Some(Scope::Named {
            index_id: id,
            source: "session_pin",
        });
    }
    derive_cwd_candidate(cwd?).map(Scope::Cwd)
}

/// Which index a `search_health` call is about, before the daemon is asked.
pub(super) enum Scope {
    /// Named outright — an explicit argument or the session pin.
    Named {
        index_id: String,
        source: &'static str,
    },
    /// #8229: derived from the working directory. The id is a bare basename, so
    /// two checkouts named alike share it; it is confirmed against the daemon's
    /// `root_path` list by [`confirm_cwd_scope`] before anything is probed.
    Cwd(CwdCandidate),
}

/// Confirm a working-directory candidate against the daemon's roots (#8229).
///
/// Why: probing the bare-basename id reported a different clone's index —
/// 50,731 chunks, 0 vectors, rooted elsewhere — as this project's, with
/// `healthy: true`, while the index actually rooted at the cwd sat unused on
/// the same daemon. `serve`'s startup pin already refuses that collision
/// (#5264, #6864); this applies the same verdict.
/// What: reads `search.indexes.list` (details) and runs
/// [`confirm_candidate`]. A matching root probes the derived id (`cwd`); an
/// index registered under another id at this root probes that one
/// (`cwd_root_match`); an unserved id probes the derived id so the usual
/// `index_not_registered` verdict follows. A root mismatch is
/// `index_not_registered` naming the other tree, an unconfirmable root is
/// `index_unknown`, and an unreadable list is `daemon_error` — the `Err` arm
/// carries the finished report. Never `ok` for a tree the index does not serve.
/// Test: `cwd_fallback_resolves_same_basename_checkouts_to_their_own_indexes`,
/// `cwd_fallback_refuses_an_id_served_from_another_tree`.
async fn confirm_cwd_scope(
    server: &McpServer,
    candidate: &CwdCandidate,
    daemon: &Value,
    answered: &str,
) -> Result<(String, &'static str), Value> {
    let id = candidate.index_id.as_str();
    let root = candidate.project_root.display().to_string();
    let entries = match fetch_index_entries(server).await {
        Ok(entries) => entries,
        Err(detail) => {
            let index = index_scope(
                id,
                "cwd",
                serde_json::json!({ "registered": null, "project_root": root, "error": detail }),
            );
            return Err(report(
                HEALTH_DAEMON_ERROR,
                daemon.clone(),
                index,
                format!(
                    "{answered} Its index list could not be read to confirm which index \
                     serves {root}: {detail}."
                ),
                "Retry once. If it persists, check the daemon log, or re-run \
                 `search_health` with an explicit `index_id`.",
            ));
        }
    };
    match confirm_candidate(candidate, &entries) {
        Confirmation::Confirmed | Confirmation::NotServed => Ok((id.to_string(), "cwd")),
        Confirmation::ServedByAnotherId { index_id } => Ok((index_id, "cwd_root_match")),
        Confirmation::RootMismatch { serving_root } => {
            let serving = serving_root.display().to_string();
            let index = index_scope(
                id,
                "cwd",
                serde_json::json!({
                    "registered": false,
                    "project_root": root,
                    "serving_root": serving,
                }),
            );
            Err(report(
                HEALTH_INDEX_NOT_REGISTERED,
                daemon.clone(),
                index,
                format!(
                    "{answered} Its index '{id}' is rooted at {serving}, a DIFFERENT tree \
                     that shares this project's directory name, and no index is rooted at \
                     {root} — so searches for this project will not find its code."
                ),
                "Index this project with `trusty-search index <path>`, or re-run \
                 `search_health` with the `index_id` `list_indexes` shows for this \
                 root. Do not treat the same-named index as this project's.",
            ))
        }
        Confirmation::RootUnknown => {
            let index = index_scope(
                id,
                "cwd",
                serde_json::json!({ "registered": true, "project_root": root, "root_path": null }),
            );
            Err(report(
                HEALTH_INDEX_UNKNOWN,
                daemon.clone(),
                index,
                format!(
                    "{answered} It serves index '{id}' but reports no root for it, so \
                     whether that index is this project ({root}) could not be confirmed."
                ),
                "Re-run `search_health` with an explicit `index_id`.",
            ))
        }
    }
}

/// The details listing, parsed into id/root pairs (#8229).
async fn fetch_index_entries(server: &McpServer) -> Result<Vec<DaemonIndex>, String> {
    let params = serde_json::json!({ "details": true });
    let body = server
        .daemon
        .call(METHOD_INDEXES_LIST, params)
        .await
        .map_err(|e| server.describe(&e))?;
    Ok(parse_index_entries(&body))
}

/// What `search.health` did.
enum DaemonProbe {
    /// Nothing is serving the socket.
    Unreachable { detail: String },
    /// Something answered, but not with a health object.
    Error {
        rpc_code: Option<i64>,
        detail: String,
    },
    /// A JSON health object.
    Ok(Value),
}

/// Probe `search.health` while preserving WHY it failed.
///
/// Why (#5264): a refused dial and a refusal from whatever answered need
/// different remediation — start the daemon, versus find out what else holds
/// the socket. #9168: the client's unreachable kind is that split on the
/// socket (`DaemonCallError::is_unreachable`), so no error text is parsed.
/// What: three verdicts. A success that is not a JSON object counts as an
/// error, because a process answering the health method with something else
/// is not this daemon and saying "healthy" about it would be the exact failure
/// #5264 filed.
/// Test: `search_health_reports_daemon_unreachable_with_remediation`,
/// `search_health_reports_a_daemon_that_answers_badly`,
/// `search_health_rejects_a_2xx_body_that_is_not_a_health_object`.
async fn probe_daemon(server: &McpServer) -> DaemonProbe {
    match server.daemon.health().await {
        Ok(v) if v.is_object() => DaemonProbe::Ok(v),
        Ok(v) => DaemonProbe::Error {
            rpc_code: None,
            detail: format!(
                "the answer is not a JSON health object: {}",
                excerpt(&v.to_string())
            ),
        },
        Err(e) if e.is_unreachable() => DaemonProbe::Unreachable {
            detail: e.to_string(),
        },
        Err(e) => DaemonProbe::Error {
            rpc_code: e.code(),
            detail: excerpt(&server.describe(&e)),
        },
    }
}

/// What `search.index.status` said.
enum IndexProbe {
    /// The daemon answered `not found`: this project has no index here.
    Missing,
    /// The daemon answered, but with neither a status nor `not found`.
    Unknown { detail: String },
    /// A status body.
    Present { body: Value },
}

/// Probe one index's status, keeping `not found` distinct from every other
/// failure.
///
/// Why: `not found` is the "this project is unindexed" signal the health
/// report owes its caller; folding it in with an unavailable refusal or a
/// transport failure would tell an agent to reindex when it should have
/// retried.
/// Test: `search_health_reports_an_unregistered_project_index`.
async fn probe_index(server: &McpServer, index_id: &str) -> IndexProbe {
    let params = serde_json::json!({ "index_id": index_id });
    match server.daemon.call(METHOD_INDEX_STATUS, params).await {
        Ok(body) => IndexProbe::Present { body },
        Err(e) if e.is_not_found() => IndexProbe::Missing,
        Err(e) => IndexProbe::Unknown {
            detail: excerpt(&server.describe(&e)),
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
