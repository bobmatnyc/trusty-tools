//! trusty-search MCP stub + project-index registration/reindex helpers.
//!
//! Why: split out of `settings.rs` (issue #610 SLOC cap) — the trusty-search
//! index lifecycle (build the pinned MCP stub, find-or-create the index, then
//! best-effort trigger a reindex so it is actually populated) is a cohesive
//! concern distinct from the output-style/hook/trust-preseed helpers that
//! remain in `settings.rs`.
//! What: [`trusty_search_mcp_value`] / [`inject_trusty_search_mcp`] build and
//! write the `.mcp.json` stub (issue #1373); [`register_project_index`] +
//! [`best_effort_create_index`] + [`best_effort_trigger_reindex`] +
//! [`index_is_fresh`] find-or-create the daemon-side index and ensure it is
//! populated (issue #1908).
//! Test: each `pub(super)` function has a dedicated test in `tests.rs`.

use std::path::Path;

use super::PrepError;
use super::settings::inject_mcp_server;

/// Build the `trusty-search` MCP server definition injected into a project's
/// `.mcp.json`, optionally pinned to a project index (issue #1373).
///
/// Why (issue #1270 / step 4): trusty-mpm-spawned sessions need the code-search
/// tools (`search`, `grep`, `get_call_chain`, …). Issue #1373: without pinning,
/// the contextless `trusty-search serve` stub left index selection to the LLM,
/// which routinely queried the WRONG index (usually the persistent `claude-mpm`
/// one) instead of the session's own project. Passing `--index <derived-id>`
/// pins the session to its project index so a bare `search` always resolves
/// correctly and fan-out never sweeps every index. The canonical stdio MCP
/// invocation is bare `serve` (stdio is the default transport; HTTP is off
/// unless `--with-http`).
/// What: returns the JSON `Value` for a stdio MCP server running
/// `trusty-search serve` — with `["serve", "--index", "<id>"]` when `index_id`
/// is `Some` (non-empty), else the unpinned `["serve"]`. Index *creation* is
/// handled separately by [`register_project_index`].
/// Test: `trusty_search_mcp_value_pins_index`, `trusty_search_mcp_value_unpinned`.
pub(super) fn trusty_search_mcp_value(index_id: Option<&str>) -> serde_json::Value {
    let args: Vec<serde_json::Value> = match index_id {
        Some(id) if !id.trim().is_empty() => vec![
            serde_json::Value::String("serve".to_string()),
            serde_json::Value::String("--index".to_string()),
            serde_json::Value::String(id.to_string()),
        ],
        _ => vec![serde_json::Value::String("serve".to_string())],
    };
    serde_json::json!({
        "type": "stdio",
        "command": "trusty-search",
        "args": args,
    })
}

/// Inject the `trusty-search` MCP server into the project's `.mcp.json`,
/// pinned to the project's index when one is known (issue #1373).
///
/// Why (issue #1270 / step 4): spawned sessions must reach the code-search
/// tools, but `trusty-search` was never registered alongside `trusty-memory`.
/// Issue #1373: the stub must additionally PIN the session to the project's own
/// index (`--index <id>`) so queries never resolve to the wrong index. When
/// `index_id` is `None` (derivation failed) the unpinned stub is written so the
/// session still gets the tools — backward-compatible with the pre-#1373 stub.
/// What: builds the (optionally pinned) server value via
/// [`trusty_search_mcp_value`] and registers it under the key `trusty-search`.
/// Test: `inject_trusty_search_mcp_adds_server`,
/// `inject_trusty_search_mcp_preserves_existing`,
/// `inject_trusty_search_mcp_is_idempotent`,
/// `inject_trusty_search_mcp_pins_index`,
/// `inject_both_mcp_servers_coexist`.
pub(super) fn inject_trusty_search_mcp(
    project_path: &Path,
    index_id: Option<&str>,
) -> Result<(), PrepError> {
    inject_mcp_server(
        project_path,
        "trusty-search",
        trusty_search_mcp_value(index_id),
    )
}

/// Find-or-create the trusty-search index for `project_root`, best-effort
/// trigger a reindex so it is actually populated, and return its id (issues
/// #1373, #1908).
///
/// Why: pinning the serve stub to an index id is only useful if that index
/// actually exists in the daemon — otherwise a query against it returns nothing
/// and the LLM falls back to guessing (the very bug #1373 fixes). At session
/// launch we therefore derive the project's canonical index id (the same rule
/// trusty-search's `detect_project` uses, via the shared
/// `trusty_common::derive_index_id`) and best-effort register it with the
/// running daemon. The daemon's `POST /indexes` is idempotent (returns
/// `created: false` for an existing id), so a re-register is safe and cheap.
/// Issue #1908: `POST /indexes` alone only registers an EMPTY index and starts
/// a future-changes file watcher — it never walks the existing tree (see
/// `trusty-search/src/service/server/indexes.rs::create_index_handler`). Without
/// an explicit reindex trigger, a freshly pinned session index stays empty
/// until *something* changes on disk, so the very first `search` query in a
/// spawned session silently returns nothing. [`best_effort_trigger_reindex`] is
/// therefore called right after registration, in the same reachable-daemon
/// branch, so both calls share one "is the daemon up" check.
/// What: resolves the git-root for `project_root`, derives the index id, and —
/// when the id is non-empty AND the trusty-search daemon address is discoverable
/// — POSTs `{id, root_path}` to `/indexes` then best-effort triggers a reindex
/// (skipping it when the index is already fresh; see
/// [`best_effort_trigger_reindex`]). ALWAYS returns the derived id (`None` only
/// when derivation yields an empty string) so the caller can pin the stub even
/// if the daemon is unreachable; every failed/skipped step is logged at
/// warn/debug and never propagates (the session must still launch).
/// Test: `register_project_index_returns_derived_id` (derivation + daemon-down
/// graceful path) in `tests.rs`.
pub(super) fn register_project_index(project_root: &Path) -> Option<String> {
    let root = trusty_common::resolve_project_root(project_root);
    let index_id = trusty_common::derive_index_id(&root);
    if index_id.trim().is_empty() {
        tracing::warn!(
            "skipping trusty-search index registration: empty index id for {}",
            root.display()
        );
        return None;
    }

    // Discover the running daemon's address. Absent / unreadable file ⇒ daemon
    // not started: skip registration (best-effort) but still return the id so
    // the stub is pinned — the daemon will create the index on first reindex.
    match trusty_common::read_daemon_addr("trusty-search") {
        Ok(Some(addr)) if !addr.trim().is_empty() => {
            let base = if addr.starts_with("http://") || addr.starts_with("https://") {
                addr
            } else {
                format!("http://{addr}")
            };
            best_effort_create_index(&base, &index_id, &root);
            best_effort_trigger_reindex(&base, &index_id);
        }
        _ => {
            tracing::warn!(
                "trusty-search daemon address not found; pinning index '{index_id}' \
                 without pre-registering it (it will be created on first reindex)"
            );
        }
    }

    Some(index_id)
}

/// POST `/indexes` to find-or-create `index_id`; failures are logged, never
/// propagated (issue #1373).
///
/// Why: registration is best-effort — a daemon that is briefly unreachable, or
/// an HTTP hiccup, must NOT abort session launch. Isolating the blocking HTTP
/// call here keeps [`register_project_index`] readable and the error handling
/// in one place.
/// What: issues a short-timeout blocking `POST {base}/indexes` with body
/// `{id, root_path}` ON A DEDICATED OS THREAD. `prepare_session` is synchronous
/// but is frequently invoked from inside a tokio runtime (the daemon/TUI launch
/// paths); creating `reqwest::blocking`'s internal runtime directly there panics
/// with "Cannot drop a runtime in a context where blocking is not allowed".
/// Running the blocking client on a freshly-spawned `std::thread` (joined here)
/// keeps that nested runtime entirely off the async worker, so the call is safe
/// from both sync and async callers. A non-2xx response or transport error is
/// logged at warn/debug and swallowed; the daemon endpoint is idempotent so
/// re-creates are harmless. The client uses a tight ~1s overall timeout
/// (750 ms connect) so the joined thread returns quickly: this call sits on the
/// `prepare_session` hot path and must NOT stall a session launch when the
/// daemon is slow or unreachable. Registration is purely best-effort — the
/// index is also created on the first reindex — so a short cap is acceptable.
/// Test: exercised via `register_project_index_returns_derived_id` (daemon-down
/// path) and `launch_session_errors_when_daemon_unreachable` (async-context
/// safety); the live HTTP path is covered by integration use.
fn best_effort_create_index(base: &str, index_id: &str, root: &Path) {
    let url = format!("{base}/indexes");
    let body = serde_json::json!({ "id": index_id, "root_path": root.to_string_lossy() });
    let index_id = index_id.to_string();
    let root_display = root.display().to_string();

    let result = std::thread::spawn(move || {
        // 1s overall / 750ms connect cap: this runs synchronously on the
        // session-launch hot path, so the worst-case stall must stay small.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(1))
            .connect_timeout(std::time::Duration::from_millis(750))
            .build()?;
        let resp = client.post(&url).json(&body).send()?;
        Ok::<reqwest::StatusCode, reqwest::Error>(resp.status())
    })
    .join();

    match result {
        Ok(Ok(status)) if status.is_success() => {
            tracing::debug!("registered trusty-search index '{index_id}' (root={root_display})");
        }
        Ok(Ok(status)) => {
            tracing::warn!(
                "trusty-search index registration for '{index_id}' returned HTTP {status}"
            );
        }
        Ok(Err(e)) => {
            tracing::warn!("trusty-search index registration for '{index_id}' failed: {e}");
        }
        Err(_) => {
            tracing::warn!("trusty-search index registration thread for '{index_id}' panicked");
        }
    }
}

/// Best-effort, non-blocking trigger of a trusty-search reindex for `index_id`
/// (issue #1908).
///
/// Why: [`best_effort_create_index`] only find-or-creates an EMPTY index — the
/// daemon's `POST /indexes` handler registers the id and starts a
/// future-changes file watcher but never walks the existing tree. Without an
/// explicit reindex trigger, a freshly pinned session index stays empty until
/// *something* changes on disk, so the very first `search`/`grep` query in a
/// spawned session silently returns nothing. `POST /indexes/{id}/reindex` is
/// fire-and-forget server-side — it `tokio::spawn`s the walk and returns
/// `{"queued": true}` almost instantly — so triggering it here does not risk a
/// long stall; the short dedicated-thread timeout below guards against the
/// (much rarer) case where even the initial HTTP round trip itself is slow.
/// What: on a dedicated OS thread (mirroring [`best_effort_create_index`]) with
/// a ~2s overall / 750ms connect timeout: first does a cheap `GET
/// {base}/indexes/{id}/status` freshness probe (see [`index_is_fresh`]) and
/// skips the reindex entirely when the index already has chunks and was
/// indexed within the last hour; otherwise POSTs `{base}/indexes/{id}/reindex`.
/// A failed status probe is treated as "not fresh" (fail-open toward
/// reindexing). Every outcome — skipped, triggered, non-2xx, transport error,
/// panicked thread — is logged at warn/debug and swallowed; the daemon-side
/// reindex is itself idempotent, so calling it redundantly is harmless, and
/// session launch must never block or fail because trusty-search is
/// unreachable or slow.
/// Test: `index_is_fresh_true_when_recently_indexed_with_chunks`,
/// `index_is_fresh_false_when_no_chunks`, `index_is_fresh_false_when_stale`,
/// `index_is_fresh_false_when_last_indexed_missing_or_malformed` in `tests.rs`;
/// the live-HTTP trigger path is exercised the same way
/// `best_effort_create_index` is (daemon-down graceful path via
/// `register_project_index_returns_derived_id`).
fn best_effort_trigger_reindex(base: &str, index_id: &str) {
    let status_url = format!("{base}/indexes/{index_id}/status");
    let reindex_url = format!("{base}/indexes/{index_id}/reindex");
    let index_id = index_id.to_string();

    let result = std::thread::spawn(move || -> Result<&'static str, reqwest::Error> {
        // 2s overall / 750ms connect cap: this runs synchronously on the
        // session-launch hot path (after best_effort_create_index's own 1s
        // budget), so the worst-case added stall must stay small.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .connect_timeout(std::time::Duration::from_millis(750))
            .build()?;

        let already_fresh = client
            .get(&status_url)
            .send()
            .ok()
            .filter(|resp| resp.status().is_success())
            .and_then(|resp| resp.json::<serde_json::Value>().ok())
            .is_some_and(|body| index_is_fresh(&body));
        if already_fresh {
            return Ok("skipped: index already fresh");
        }

        let resp = client.post(&reindex_url).send()?;
        Ok(if resp.status().is_success() {
            "triggered"
        } else {
            "reindex request returned non-2xx"
        })
    })
    .join();

    match result {
        Ok(Ok(outcome)) => {
            tracing::debug!("trusty-search reindex for '{index_id}': {outcome}");
        }
        Ok(Err(e)) => {
            tracing::warn!("trusty-search reindex trigger for '{index_id}' failed: {e}");
        }
        Err(_) => {
            tracing::warn!("trusty-search reindex trigger thread for '{index_id}' panicked");
        }
    }
}

/// Whether a `GET /indexes/{id}/status` response body represents an index
/// fresh enough that [`best_effort_trigger_reindex`] should skip reindexing
/// (issue #1908).
///
/// Why: pure predicate over the JSON body so the freshness rule is unit
/// testable without a live daemon — [`best_effort_trigger_reindex`] is
/// otherwise pure I/O. Skipping redundant reindexes avoids reindex spam on
/// every session launch of an already-fresh workspace.
/// What: returns `true` when `chunk_count` is a positive integer AND
/// `last_indexed` parses as an RFC3339 timestamp no more than one hour in the
/// past (clock skew that makes it appear in the future is also treated as not
/// fresh, out of caution). Any missing/malformed/zero field returns `false`
/// (fail-open toward reindexing, never toward skipping).
/// Test: `index_is_fresh_true_when_recently_indexed_with_chunks`,
/// `index_is_fresh_false_when_no_chunks`, `index_is_fresh_false_when_stale`,
/// `index_is_fresh_false_when_last_indexed_missing_or_malformed`.
pub(super) fn index_is_fresh(status: &serde_json::Value) -> bool {
    let chunk_count = status
        .get("chunk_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if chunk_count == 0 {
        return false;
    }
    let Some(last_indexed) = status
        .get("last_indexed")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let Ok(indexed_at) = chrono::DateTime::parse_from_rfc3339(last_indexed) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(indexed_at.with_timezone(&chrono::Utc));
    age >= chrono::Duration::zero() && age <= chrono::Duration::hours(1)
}
