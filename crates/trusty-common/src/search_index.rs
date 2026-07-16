//! Shared best-effort "ensure this project is indexed by trusty-search" entry
//! point, hoisted out of trusty-mpm so a second crate (trusty-code) can reuse
//! the ONE implementation instead of duplicating it.
//!
//! Why: the register-and-populate logic (derive the canonical index id, then
//! find-or-create the daemon-side index and best-effort trigger a
//! freshness-gated reindex) originally lived only in trusty-mpm's
//! `core::session_launch::search_index::register_project_index` (issues #1373 /
//! #1908). trusty-code now wants the same behaviour at task start so a tcode
//! run's working project is discoverable via trusty-search while the agent
//! loop proceeds. Per the workspace's common-entry-point rule (CLAUDE.md), a
//! capability used by two crates must be one shared function in trusty-common —
//! not copy-pasted — so the two call sites can never silently diverge.
//!
//! What: [`ensure_project_indexed`] resolves the git-root, derives the index id
//! via [`crate::resolve_project_root`] / [`crate::derive_index_id`], and — when
//! the daemon is discoverable — best-effort registers the index (`POST
//! /indexes`, ~1s cap) then best-effort triggers a freshness-gated reindex
//! (`POST /indexes/{id}/reindex`, ~2s cap, skipped when the index already holds
//! chunks indexed within the last hour). Every step is fail-open: failures are
//! logged at warn/debug and swallowed, never propagated, so the caller (a
//! session launch or a task run) is never blocked or aborted by an
//! unreachable/slow search daemon. The blocking HTTP calls run on dedicated OS
//! threads so the function is safe to call from inside a tokio runtime.
//!
//! Mid-task incremental re-indexing: [`ensure_project_indexed`] runs once, at
//! task start — for a greenfield project that starts EMPTY, that means
//! `search_code` finds nothing the engineer writes DURING the task.
//! [`index_files_best_effort`] complements it: called after each successful
//! file write/edit, it POSTs just that file's fresh content to the daemon's
//! cheap per-file `POST /indexes/{id}/index-file` endpoint (never a full
//! reindex walk), so the growing codebase stays searchable within the same
//! task. Same fail-open contract, and non-blocking by construction (spawns its
//! own detached thread rather than relying on the caller to wrap it, since
//! its call sites are tcode's tool executors, not a one-shot task-start hook).
//!
//! Test: `ensure_project_indexed_returns_derived_id_when_daemon_down`,
//! `ensure_project_indexed_none_for_root`, the `index_is_fresh_*` predicate
//! tests, the `index_files_inner_*` / `relative_index_path_*` /
//! `index_file_request_body_*` tests, and the incremental-hardening tests
//! `retry_backoff_is_bounded_and_increasing` /
//! `post_index_file_retries_transient_send_failure` in the `tests` module below.

use std::path::Path;

/// Find-or-create the trusty-search index for `project_root`, best-effort
/// trigger a reindex so it is actually populated, and return its id (issues
/// #1373, #1908).
///
/// Why: pinning a session/task to an index id is only useful if that index
/// actually exists in the daemon — otherwise a query against it returns nothing
/// and the LLM falls back to guessing (the very bug #1373 fixes). Callers
/// therefore derive the project's canonical index id (the same rule
/// trusty-search's `detect_project` uses, via [`crate::derive_index_id`]) and
/// best-effort register it with the running daemon. The daemon's `POST
/// /indexes` is idempotent (returns `created: false` for an existing id), so a
/// re-register is safe and cheap. Issue #1908: `POST /indexes` alone only
/// registers an EMPTY index and starts a future-changes file watcher — it never
/// walks the existing tree — so a reindex is triggered right after, in the same
/// reachable-daemon branch, sharing one "is the daemon up" check.
/// What: resolves the git-root for `project_root`, derives the index id, and —
/// when the id is non-empty AND the trusty-search daemon address is discoverable
/// — POSTs `{id, root_path}` to `/indexes` then best-effort triggers a reindex
/// (skipping it when the index is already fresh; see
/// [`best_effort_trigger_reindex`]). ALWAYS returns the derived id (`None` only
/// when derivation yields an empty string) so the caller can still pin the id
/// even if the daemon is unreachable; every failed/skipped step is logged at
/// warn/debug and never propagates (the caller must still make progress).
/// Test: `ensure_project_indexed_returns_derived_id_when_daemon_down`,
/// `ensure_project_indexed_none_for_root`.
pub fn ensure_project_indexed(project_root: &Path) -> Option<String> {
    let root = crate::resolve_project_root(project_root);
    let index_id = crate::derive_index_id(&root);
    if index_id.trim().is_empty() {
        tracing::warn!(
            "skipping trusty-search index registration: empty index id for {}",
            root.display()
        );
        return None;
    }

    // Discover the running daemon's address (issue #2033: via the shared
    // `resolve_daemon_base_url` helper — never a hardcoded port). Absent /
    // unreadable file ⇒ daemon not started: skip registration (best-effort) but
    // still return the id so the caller can pin it — the daemon will create the
    // index on first reindex.
    match crate::resolve_daemon_base_url("trusty-search") {
        Some(base) => {
            best_effort_create_index(&base, &index_id, &root);
            best_effort_trigger_reindex(&base, &index_id);
        }
        None => {
            tracing::warn!(
                "trusty-search daemon address not found; pinning index '{index_id}' \
                 without pre-registering it (it will be created on first reindex)"
            );
        }
    }

    Some(index_id)
}

/// Best-effort, non-blocking incremental re-index of specific files into an
/// ALREADY-REGISTERED trusty-search index (mid-task incremental re-indexing).
///
/// Why: [`ensure_project_indexed`] runs once at task start, when a greenfield
/// project is often EMPTY — so `search_code` finds nothing the engineer goes
/// on to write during the task. Re-registering (or fully reindexing) the
/// whole project after every write would mean a full-tree walk per file
/// (expensive); the daemon's per-file `POST /indexes/{id}/index-file`
/// endpoint lets a caller add or update ONE file's chunks cheaply, so the
/// growing codebase stays searchable within the same task.
/// What: spawns ONE detached OS thread and returns immediately — the caller
/// (a tool executor mid-turn) must never block or fail because trusty-search
/// is unreachable or slow. Inside the thread, [`index_files_inner`] derives
/// the same `(root, index_id)` [`ensure_project_indexed`] would (so this
/// always targets the same index a task-start call already created) and
/// POSTs each of `paths` to the daemon. A no-op with zero thread spawn when
/// `paths` is empty.
///
/// Sensitive-path note (issue #2747): unlike `POST /indexes`, the per-file
/// `index-file` endpoint does NOT re-run the sensitive-path denylist — it
/// looks the index up by id in the daemon's in-memory registry
/// (`crates/trusty-search/src/service/server/files.rs`'s `index_file_handler`
/// calls `state.registry.get(&index_id)`, never `allowlist::is_denied`), so
/// an index created under the #2747 `allow_sensitive_path` bypass (a tempdir
/// root) accepts incremental updates unconditionally. No bypass flag is
/// threaded through here because none is needed.
/// Test: this function is a thin spawn wrapper (side-effect only, no return
/// to assert); its logic is [`index_files_inner`], which the
/// `index_files_inner_*` tests below exercise directly (synchronously, off
/// the spawned thread) for determinism.
pub fn index_files_best_effort(project_root: &Path, paths: &[std::path::PathBuf]) {
    if paths.is_empty() {
        return;
    }
    let project_root = project_root.to_path_buf();
    let paths = paths.to_vec();
    std::thread::spawn(move || {
        index_files_inner(&project_root, &paths);
    });
}

/// Synchronous body of [`index_files_best_effort`], run on its detached
/// thread (or called directly by tests for determinism).
///
/// Why: split out so tests can exercise the fail-open branches (empty index
/// id, undiscoverable daemon) synchronously, without waiting on — or racing
/// — a spawned thread.
/// What: derives `(root, index_id)` via [`crate::resolve_project_root`] /
/// [`crate::derive_index_id`]; returns early (logged at debug) when the id is
/// empty or [`crate::resolve_daemon_base_url`] finds no running daemon;
/// otherwise builds ONE pooled HTTP client for the whole batch (issue #2785:
/// so multiple files in a `write_files` batch reuse keep-alive connections
/// instead of a fresh TCP connect per file) and, for each path, resolves it
/// against `root`, reads its current content from disk (an unreadable file —
/// e.g. deleted since the write — is logged at debug and skipped, not fatal to
/// the batch), and POSTs it via [`best_effort_index_one_file`] (which itself
/// retries transient send failures with backoff). Every step fails open.
/// Test: `index_files_inner_is_noop_for_empty_paths`,
/// `index_files_inner_skips_when_index_id_empty`,
/// `index_files_inner_skips_gracefully_when_daemon_down`.
fn index_files_inner(project_root: &Path, paths: &[std::path::PathBuf]) {
    if paths.is_empty() {
        return;
    }
    let root = crate::resolve_project_root(project_root);
    let index_id = crate::derive_index_id(&root);
    if index_id.trim().is_empty() {
        tracing::debug!(
            "skipping incremental trusty-search index update: empty index id for {}",
            root.display()
        );
        return;
    }
    let Some(base) = crate::resolve_daemon_base_url("trusty-search") else {
        tracing::debug!(
            "trusty-search daemon address not found; skipping incremental index \
             update for '{index_id}' ({} file(s))",
            paths.len()
        );
        return;
    };

    // One client per batch (#2785): reqwest keeps a connection pool per client,
    // so reusing it across the batch's files lets rapid successive writes ride
    // existing keep-alive connections instead of paying a fresh TCP connect
    // (and its transient-failure risk) per file. Fail open if it cannot build.
    let client = match build_index_client() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("skipping incremental index update: could not build HTTP client: {e}");
            return;
        }
    };

    for path in paths {
        let abs = if path.is_absolute() {
            path.clone()
        } else {
            root.join(path)
        };
        let rel = relative_index_path(&root, &abs);
        let content = match std::fs::read_to_string(&abs) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    "skipping incremental index update for {}: {e}",
                    abs.display()
                );
                continue;
            }
        };
        best_effort_index_one_file(&client, &base, &index_id, &rel, &content);
    }
}

/// Resolve `abs` to the path string the corpus stores for a file under `root`.
///
/// Why: the reindex walker stores every chunk's `file` field relative to the
/// index root (`crates/trusty-search/src/service/walker.rs` strips the
/// canonical root prefix); posting an absolute path here would create a
/// duplicate, differently-keyed corpus entry for the same file instead of
/// updating the one the walker already produced.
/// What: strips `root` as a prefix and forward-slash-normalises the
/// remainder; falls back to `abs` itself (lossy) when it does not live under
/// `root` — should not happen for a working-directory-scoped tool write, but
/// fails safe rather than panicking or silently dropping the update.
/// Test: `relative_index_path_strips_root_prefix`,
/// `relative_index_path_falls_back_for_paths_outside_root`.
fn relative_index_path(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root)
        .unwrap_or(abs)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Build the pooled blocking HTTP client used for incremental index updates.
///
/// Why: extracted so [`index_files_inner`] builds exactly ONE client per batch
/// (issue #2785 connection reuse) and so the retry test can construct an
/// identically-configured client.
/// What: a `reqwest::blocking::Client` with a 2s overall / 750ms connect
/// timeout — tight caps because this runs on a mid-task detached thread and
/// must never stall a long task when the daemon is slow. reqwest maintains an
/// idle-connection pool per client, so reusing the returned client across a
/// batch's files amortises TCP/handshake setup.
/// Test: covered indirectly by `post_index_file_retries_transient_send_failure`
/// (which builds and drives one), and by the daemon-down fail-open path in
/// `index_files_inner_skips_gracefully_when_daemon_down`.
fn build_index_client() -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .connect_timeout(std::time::Duration::from_millis(750))
        .build()
}

/// Max attempts (initial try + retries) for a single per-file index POST.
///
/// Why: issue #2785 — under sustained mid-task load the per-file HTTP call sees
/// transient send failures (connection resets / connect races under rapid
/// repeated writes). A tiny bounded retry recovers the vast majority without
/// ever blocking the task meaningfully (worst-case added stall is the sum of
/// [`retry_backoff`] over the retries, ~200ms for 3 attempts).
/// What: 3 total attempts.
/// Test: `retry_backoff_is_bounded_and_increasing`.
const MAX_INDEX_ATTEMPTS: u32 = 3;

/// Backoff to sleep BEFORE retry `attempt` (1-based) of a per-file index POST.
///
/// Why: a transient send failure under load often clears within tens of
/// milliseconds once the daemon drains the burst; a short exponential backoff
/// spaces retries without materially slowing the task. Kept as a pure function
/// so the schedule is unit-testable without any I/O.
/// What: `50ms * 3^(attempt-1)`, capped at 1s — i.e. 50ms before the 2nd try,
/// 150ms before the 3rd. Saturating arithmetic keeps it panic-free for any
/// `attempt`.
/// Test: `retry_backoff_is_bounded_and_increasing`.
fn retry_backoff(attempt: u32) -> std::time::Duration {
    let factor = 3u64.saturating_pow(attempt.saturating_sub(1));
    let millis = 50u64.saturating_mul(factor).min(1000);
    std::time::Duration::from_millis(millis)
}

/// Outcome of a per-file index POST, surfaced so tests can assert the
/// retry-then-succeed path without scraping logs.
///
/// Why: [`post_index_file_with_retries`] is otherwise pure I/O; returning a
/// small enum lets `post_index_file_retries_transient_send_failure` prove a
/// transient send failure is retried and ultimately succeeds.
/// What: `Indexed` (2xx), `HttpStatus` (non-2xx — not retried; a 4xx/404 for an
/// unknown index won't fix itself), or `SendFailed` (transport error on the
/// final attempt).
/// Test: `post_index_file_retries_transient_send_failure`.
#[derive(Debug, PartialEq, Eq)]
enum IndexOutcome {
    Indexed,
    HttpStatus(u16),
    SendFailed,
}

/// POST a single file's `{path, content}` to `url`, retrying transient send
/// failures with [`retry_backoff`] up to [`MAX_INDEX_ATTEMPTS`] times.
///
/// Why: issue #2785 — a single transport-level `send()` failure (connection
/// reset/connect race under rapid concurrent writes) previously dropped the
/// update entirely. Retrying transport errors (but NOT HTTP non-2xx, which
/// will not self-heal) recovers those transient failures.
/// What: reuses the caller-supplied pooled `client`; on a transport `Err` it
/// sleeps [`retry_backoff`] and retries (until attempts are exhausted → returns
/// `SendFailed`); a 2xx returns `Indexed` immediately; any other status returns
/// `HttpStatus` immediately (no retry). Never panics, never propagates.
/// Test: `post_index_file_retries_transient_send_failure`.
fn post_index_file_with_retries(
    client: &reqwest::blocking::Client,
    url: &str,
    body: &serde_json::Value,
) -> IndexOutcome {
    let mut last_err: Option<reqwest::Error> = None;
    for attempt in 0..MAX_INDEX_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(retry_backoff(attempt));
        }
        match client.post(url).json(body).send() {
            Ok(resp) if resp.status().is_success() => return IndexOutcome::Indexed,
            Ok(resp) => return IndexOutcome::HttpStatus(resp.status().as_u16()),
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(e) = &last_err {
        tracing::debug!(
            "per-file index POST to {url} failed after {MAX_INDEX_ATTEMPTS} attempts: {e}"
        );
    }
    IndexOutcome::SendFailed
}

/// POST `/indexes/{id}/index-file` for a single file; failures are logged,
/// never propagated.
///
/// Why: mirrors [`best_effort_create_index`]'s fail-open contract for the
/// per-file endpoint, hardened for issue #2785 (retry + connection reuse).
/// What: delegates to [`post_index_file_with_retries`] using the pooled
/// `client` [`index_files_inner`] built once for the batch (so rapid writes
/// reuse keep-alive connections). Unlike [`best_effort_create_index`], this
/// does NOT spawn-and-join its own nested OS thread: it is only ever reached
/// from inside [`index_files_inner`]'s own detached thread (spawned by
/// [`index_files_best_effort`]), which is already off any tokio runtime, so a
/// direct blocking call here cannot trigger the "cannot drop a runtime in a
/// context where blocking is not allowed" panic. A non-2xx response (including
/// 404 for an unregistered/unknown index — e.g. the daemon restarted since task
/// start) is logged at warn; a transport error surviving all retries is logged
/// at warn. Both are swallowed.
/// Test: exercised via `index_files_inner_skips_gracefully_when_daemon_down`
/// (daemon-down path, never reaches this function) and
/// `post_index_file_retries_transient_send_failure` (retry path); the live HTTP
/// success path is covered by integration use.
fn best_effort_index_one_file(
    client: &reqwest::blocking::Client,
    base: &str,
    index_id: &str,
    rel_path: &str,
    content: &str,
) {
    let url = format!("{base}/indexes/{index_id}/index-file");
    let body = index_file_request_body(rel_path, content);

    match post_index_file_with_retries(client, &url, &body) {
        IndexOutcome::Indexed => {
            tracing::debug!("incrementally indexed '{rel_path}' into '{index_id}'");
        }
        IndexOutcome::HttpStatus(status) => {
            tracing::warn!(
                "incremental index update for '{rel_path}' in '{index_id}' returned HTTP {status}"
            );
        }
        IndexOutcome::SendFailed => {
            tracing::warn!(
                "incremental index update for '{rel_path}' in '{index_id}' failed after \
                 {MAX_INDEX_ATTEMPTS} attempts"
            );
        }
    }
}

/// Build the JSON body for the `POST /indexes/{id}/index-file` call.
///
/// Why: extracted so the request shape is unit-testable without a live
/// daemon or a spawned thread — mirrors [`create_index_request_body`].
/// What: `{path, content}` — the exact shape the per-file endpoint's
/// `IndexFileRequest` expects (`crates/trusty-search/src/service/server/router.rs`).
/// No `allow_sensitive_path` field: see [`index_files_best_effort`]'s doc
/// comment for why the per-file endpoint needs no such opt-in.
/// Test: `index_file_request_body_targets_relative_path_and_content`.
fn index_file_request_body(rel_path: &str, content: &str) -> serde_json::Value {
    serde_json::json!({
        "path": rel_path,
        "content": content,
    })
}

/// Build the JSON body for the `POST /indexes` find-or-create call.
///
/// Why: extracted from `best_effort_create_index` so the request shape —
/// specifically, that `allow_sensitive_path` is always `true` here — is
/// unit-testable without a live daemon or a spawned thread.
/// What: `allow_sensitive_path: true` (explicit-index-sensitive-path-bypass):
/// this is ONLY reached from `ensure_project_indexed`, which is ONLY ever
/// called with one specific project root a client explicitly wants indexed
/// (tcode's/trusty-mpm's own working project) — never from trusty-search's
/// auto/broad-discovery paths, which keep the full denylist. That makes this
/// the "explicit request" case the flag exists for: it lets trusty-search
/// index a bake-off scratch project living under an OS-temp prefix (e.g.
/// `/var/folders/…`) instead of hard-rejecting it with 400. Harmless for
/// ordinary project roots (trusty-mpm worktrees, checked-out repos): none of
/// those live under `SENSITIVE_PATH_PREFIXES`, so the flag is a no-op for
/// them, and it never bypasses the OTHER denylist checks (credential dirs,
/// sensitive file names, top-level home dirs) — see
/// `trusty-search::allowlist::is_denied_allowing_sensitive_path`'s doc comment
/// for exactly what stays enforced.
/// Test: `create_index_request_body_sets_allow_sensitive_path`.
fn create_index_request_body(index_id: &str, root: &Path) -> serde_json::Value {
    serde_json::json!({
        "id": index_id,
        "root_path": root.to_string_lossy(),
        "allow_sensitive_path": true,
    })
}

/// POST `/indexes` to find-or-create `index_id`; failures are logged, never
/// propagated (issue #1373).
///
/// Why: registration is best-effort — a daemon that is briefly unreachable, or
/// an HTTP hiccup, must NOT abort the caller. Isolating the blocking HTTP call
/// here keeps [`ensure_project_indexed`] readable and the error handling in one
/// place.
/// What: issues a short-timeout blocking `POST {base}/indexes` with body
/// `{id, root_path, allow_sensitive_path}` (built by
/// [`create_index_request_body`]) ON A DEDICATED OS THREAD. Callers are
/// frequently inside a tokio runtime; creating `reqwest::blocking`'s internal
/// runtime directly there panics with "Cannot drop a runtime in a context
/// where blocking is not allowed". Running the blocking client on a
/// freshly-spawned `std::thread` (joined here) keeps that nested runtime
/// entirely off the async worker, so the call is safe from both sync and
/// async callers. A non-2xx response or transport error is logged at
/// warn/debug and swallowed; the daemon endpoint is idempotent so re-creates
/// are harmless. The client uses a tight ~1s overall timeout (750 ms connect)
/// so the joined thread returns quickly: this call sits on a hot path and
/// must NOT stall when the daemon is slow or unreachable.
/// Test: exercised via `ensure_project_indexed_returns_derived_id_when_daemon_down`
/// (daemon-down path); the live HTTP path is covered by integration use.
fn best_effort_create_index(base: &str, index_id: &str, root: &Path) {
    let url = format!("{base}/indexes");
    let body = create_index_request_body(index_id, root);
    let index_id = index_id.to_string();
    let root_display = root.display().to_string();

    let result = std::thread::spawn(move || {
        // 1s overall / 750ms connect cap: this runs synchronously on a hot
        // path, so the worst-case stall must stay small.
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
/// explicit reindex trigger, a freshly registered index stays empty until
/// *something* changes on disk, so the very first `search`/`grep` query silently
/// returns nothing. `POST /indexes/{id}/reindex` is fire-and-forget server-side
/// — it `tokio::spawn`s the walk and returns almost instantly — so triggering it
/// here does not risk a long stall; the short dedicated-thread timeout guards
/// the (much rarer) case where even the initial HTTP round trip is slow.
/// What: on a dedicated OS thread (mirroring [`best_effort_create_index`]) with
/// a ~2s overall / 750ms connect timeout: first does a cheap `GET
/// {base}/indexes/{id}/status` freshness probe (see [`index_is_fresh`]) and
/// skips the reindex entirely when the index already has chunks and was indexed
/// within the last hour; otherwise POSTs `{base}/indexes/{id}/reindex`. A failed
/// status probe is treated as "not fresh" (fail-open toward reindexing). Every
/// outcome — skipped, triggered, non-2xx, transport error, panicked thread — is
/// logged at warn/debug and swallowed; the daemon-side reindex is itself
/// idempotent, so calling it redundantly is harmless, and the caller must never
/// block or fail because trusty-search is unreachable or slow.
/// Test: `index_is_fresh_true_when_recently_indexed_with_chunks`,
/// `index_is_fresh_false_when_no_chunks`, `index_is_fresh_false_when_stale`,
/// `index_is_fresh_false_when_last_indexed_missing_or_malformed`; the live-HTTP
/// trigger path is exercised the same way `best_effort_create_index` is
/// (daemon-down graceful path via
/// `ensure_project_indexed_returns_derived_id_when_daemon_down`).
fn best_effort_trigger_reindex(base: &str, index_id: &str) {
    let status_url = format!("{base}/indexes/{index_id}/status");
    let reindex_url = format!("{base}/indexes/{index_id}/reindex");
    let index_id = index_id.to_string();

    let result = std::thread::spawn(move || -> Result<&'static str, reqwest::Error> {
        // 2s overall / 750ms connect cap: this runs synchronously on a hot path
        // (after best_effort_create_index's own 1s budget), so the worst-case
        // added stall must stay small.
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
/// every launch/run of an already-fresh workspace.
/// What: returns `true` when `chunk_count` is a positive integer AND
/// `last_indexed` parses as an RFC3339 timestamp no more than one hour in the
/// past (clock skew that makes it appear in the future is also treated as not
/// fresh, out of caution). Any missing/malformed/zero field returns `false`
/// (fail-open toward reindexing, never toward skipping).
/// Test: `index_is_fresh_true_when_recently_indexed_with_chunks`,
/// `index_is_fresh_false_when_no_chunks`, `index_is_fresh_false_when_stale`,
/// `index_is_fresh_false_when_last_indexed_missing_or_malformed`.
pub fn index_is_fresh(status: &serde_json::Value) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn scratch_dir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("trusty-search-index-{tag}-{pid}-{nanos}"));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn ensure_project_indexed_returns_derived_id_when_daemon_down() {
        // Why (#1373): the helper must derive the project's index id (git-root
        // basename, via `derive_index_id`) AND stay graceful when the
        // trusty-search daemon is unreachable — it still returns the id so the
        // caller can pin it. We force the daemon-down path by pointing the data
        // dir at an empty temp dir so `resolve_daemon_base_url` finds no address
        // file (and thus issues no HTTP POST). `ENV_LOCK` serialises the
        // process-global override against sibling env-mutating tests (the same
        // guard `daemon_addr`/`data_dir` tests use).
        let _guard = crate::data_dir::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let data_dir = scratch_dir("data");
        fs::create_dir_all(&data_dir).unwrap();
        // SAFETY: guarded by ENV_LOCK; removed below before returning.
        unsafe {
            std::env::set_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV, &data_dir);
        }

        // A git-rooted project: id == the git-root basename, even from a nested dir.
        let project = scratch_dir("git");
        fs::create_dir_all(project.join(".git")).unwrap();
        let nested = project.join("crates/inner");
        fs::create_dir_all(&nested).unwrap();

        let id = ensure_project_indexed(&nested);
        let expected = crate::derive_index_id(&project);

        unsafe {
            std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
        }
        let _ = fs::remove_dir_all(&project);
        let _ = fs::remove_dir_all(&data_dir);

        assert_eq!(id, Some(expected), "id is the git-root basename");
    }

    #[test]
    fn ensure_project_indexed_none_for_root() {
        // Derivation yields an empty id for the filesystem root, so the helper
        // returns None without touching the daemon.
        assert_eq!(ensure_project_indexed(Path::new("/")), None);
    }

    /// `index_files_inner` is a true no-op — no filesystem or network I/O —
    /// when handed an empty path list.
    ///
    /// Why: [`index_files_best_effort`] is called from every successful write
    /// tool executor; a batch write with zero files (should not normally
    /// happen, but must not misbehave if it does) must not derive an index id
    /// or attempt any I/O.
    /// What: calls `index_files_inner` with `project_root = "/"` (which would
    /// otherwise short-circuit on the empty-id path anyway) and an empty
    /// `paths` slice; asserts it returns immediately without panicking.
    /// Test: this test.
    #[test]
    fn index_files_inner_is_noop_for_empty_paths() {
        index_files_inner(Path::new("/"), &[]);
    }

    /// `index_files_inner` skips cleanly when `derive_index_id` yields an
    /// empty id (mirrors `ensure_project_indexed_none_for_root`'s "no index to
    /// target" case for the incremental path).
    ///
    /// Why: the filesystem root has no meaningful basename to derive an id
    /// from; posting to a daemon under an empty id would be meaningless. This
    /// must be detected and skipped before any daemon lookup or file read.
    /// What: calls `index_files_inner` with `project_root = "/"` and a
    /// non-empty `paths` slice; asserts it returns without panicking (no
    /// index id to target, so no I/O is attempted).
    /// Test: this test.
    #[test]
    fn index_files_inner_skips_when_index_id_empty() {
        index_files_inner(Path::new("/"), &[PathBuf::from("some/file.rs")]);
    }

    /// `index_files_inner` fails open — no panic, no propagated error — when
    /// the trusty-search daemon is unreachable.
    ///
    /// Why: this is the core "never block or fail a tool result on index
    /// error" contract the mid-task incremental re-index hook depends on. We
    /// force the daemon-down path the same way
    /// `ensure_project_indexed_returns_derived_id_when_daemon_down` does:
    /// point the data dir at an empty temp dir so `resolve_daemon_base_url`
    /// finds no address file, guaranteeing no HTTP call is attempted.
    /// What: seeds a git-rooted scratch project with one real file, calls
    /// `index_files_inner` with that file's path, and asserts it returns
    /// promptly without panicking.
    /// Test: this test.
    #[test]
    fn index_files_inner_skips_gracefully_when_daemon_down() {
        let _guard = crate::data_dir::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let data_dir = scratch_dir("data-incr");
        fs::create_dir_all(&data_dir).unwrap();
        // SAFETY: guarded by ENV_LOCK; removed below before returning.
        unsafe {
            std::env::set_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV, &data_dir);
        }

        let project = scratch_dir("git-incr");
        fs::create_dir_all(project.join(".git")).unwrap();
        fs::write(project.join("main.rs"), "fn main() {}\n").unwrap();

        index_files_inner(&project, &[PathBuf::from("main.rs")]);

        unsafe {
            std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
        }
        let _ = fs::remove_dir_all(&project);
        let _ = fs::remove_dir_all(&data_dir);
        // No assertion beyond "did not panic" — fail-open with no daemon
        // means there is nothing further to observe from this call.
    }

    /// `relative_index_path` strips the project root prefix so the posted
    /// path matches the corpus's existing `file` field convention.
    ///
    /// Why: the reindex walker stores chunk `file` fields relative to the
    /// index root; posting an absolute path for an incremental update would
    /// create a second, differently-keyed corpus entry for the same file
    /// instead of updating the walker's original one.
    /// What: builds `root/src/main.rs`, asserts `relative_index_path` returns
    /// `"src/main.rs"`.
    /// Test: this test.
    #[test]
    fn relative_index_path_strips_root_prefix() {
        let root = Path::new("/Users/dev/my-project");
        let abs = root.join("src/main.rs");
        assert_eq!(relative_index_path(root, &abs), "src/main.rs");
    }

    /// `relative_index_path` falls back to the absolute path (lossy) rather
    /// than panicking when the candidate does not live under `root`.
    ///
    /// Why: should not happen for a working-directory-scoped tool write, but
    /// the fallback must fail safe, not crash the caller's thread.
    /// What: passes a path with a different root; asserts the returned string
    /// equals the absolute path.
    /// Test: this test.
    #[test]
    fn relative_index_path_falls_back_for_paths_outside_root() {
        let root = Path::new("/Users/dev/my-project");
        let elsewhere = Path::new("/somewhere/else/file.py");
        assert_eq!(
            relative_index_path(root, elsewhere),
            "/somewhere/else/file.py"
        );
    }

    /// `index_file_request_body` targets exactly `{path, content}` with no
    /// extraneous fields — in particular, no `allow_sensitive_path` (the
    /// per-file endpoint does not consult the denylist at all; see
    /// `index_files_best_effort`'s doc comment).
    ///
    /// Why: pins the wire shape the daemon's `IndexFileRequest`
    /// (`crates/trusty-search/src/service/server/router.rs`) expects, and
    /// documents — via a negative assertion — the Step 0 finding that this
    /// endpoint needs no sensitive-path opt-in.
    /// What: builds the body for a relative path + content, asserts both
    /// fields round-trip and that no `allow_sensitive_path` key is present.
    /// Test: this test.
    #[test]
    fn index_file_request_body_targets_relative_path_and_content() {
        let body = index_file_request_body("src/main.rs", "fn main() {}\n");
        assert_eq!(
            body.get("path").and_then(serde_json::Value::as_str),
            Some("src/main.rs")
        );
        assert_eq!(
            body.get("content").and_then(serde_json::Value::as_str),
            Some("fn main() {}\n")
        );
        assert!(
            body.get("allow_sensitive_path").is_none(),
            "the per-file endpoint does not re-check the denylist, so no bypass \
             flag should be sent: {body:?}"
        );
    }

    /// `ensure_project_indexed`'s `POST /indexes` request body always sets
    /// `allow_sensitive_path: true` (owner directive: explicit index requests
    /// bypass the temp-dir denylist).
    ///
    /// Why: this is the trusty-common-side half of the bypass — without it,
    /// trusty-search would still hard-reject a tcode bake-off project living
    /// under an OS-temp prefix even after the daemon-side opt-in field exists.
    /// Exercising `create_index_request_body` directly (rather than spawning a
    /// thread and standing up a live daemon) keeps this test fast and offline.
    /// What: builds the request body for both a plain project root and a
    /// `/var/folders/…`-style scratch root, and asserts `allow_sensitive_path`
    /// is `true` in both cases (the flag is unconditional, not path-dependent —
    /// the daemon is the one that decides what it means).
    /// Test: this test.
    #[test]
    fn create_index_request_body_sets_allow_sensitive_path() {
        for root in [
            Path::new("/Users/dev/projects/my-repo"),
            Path::new("/private/var/folders/xx/scratch-project"),
        ] {
            let body = create_index_request_body("my-index", root);
            assert_eq!(
                body.get("allow_sensitive_path"),
                Some(&serde_json::Value::Bool(true)),
                "request body for root {root:?} must set allow_sensitive_path: true"
            );
            assert_eq!(
                body.get("id").and_then(serde_json::Value::as_str),
                Some("my-index")
            );
        }
    }

    #[test]
    fn index_is_fresh_true_when_recently_indexed_with_chunks() {
        // Why: the whole point of the optimisation is to skip a redundant reindex
        // when the index already has content and was built recently.
        let now = chrono::Utc::now();
        let status = serde_json::json!({
            "chunk_count": 42,
            "last_indexed": now.to_rfc3339(),
        });
        assert!(index_is_fresh(&status));
    }

    #[test]
    fn index_is_fresh_false_when_no_chunks() {
        // Why: a zero-chunk index is empty regardless of how recent `last_indexed`
        // claims to be — it must always be reindexed.
        let now = chrono::Utc::now();
        let status = serde_json::json!({
            "chunk_count": 0,
            "last_indexed": now.to_rfc3339(),
        });
        assert!(!index_is_fresh(&status));
    }

    #[test]
    fn index_is_fresh_false_when_stale() {
        // Why: an index last built more than an hour ago should be refreshed, even
        // though it has chunks.
        let stale = chrono::Utc::now() - chrono::Duration::hours(2);
        let status = serde_json::json!({
            "chunk_count": 10,
            "last_indexed": stale.to_rfc3339(),
        });
        assert!(!index_is_fresh(&status));
    }

    #[test]
    fn index_is_fresh_false_when_last_indexed_missing_or_malformed() {
        // Why: fail-open toward reindexing — a missing or unparsable timestamp
        // must never be treated as "fresh".
        assert!(!index_is_fresh(&serde_json::json!({ "chunk_count": 10 })));
        assert!(!index_is_fresh(&serde_json::json!({
            "chunk_count": 10,
            "last_indexed": "not-a-timestamp",
        })));
        assert!(!index_is_fresh(&serde_json::json!({})));
    }

    /// The per-file index retry backoff schedule is bounded, capped, and
    /// strictly increasing across the small attempt range we actually use.
    ///
    /// Why: issue #2785's retry loop must add only a small, predictable stall
    /// to a mid-task write (worst case ~200ms over 3 attempts) and never
    /// overflow for a large `attempt`. Pinning the schedule prevents a future
    /// edit from silently turning a best-effort retry into a multi-second stall.
    /// What: asserts the exact first three delays (50/150/450ms), that they
    /// increase, and that a very large attempt saturates to the 1s cap rather
    /// than panicking or overflowing.
    /// Test: this test.
    #[test]
    fn retry_backoff_is_bounded_and_increasing() {
        use std::time::Duration;
        assert_eq!(retry_backoff(1), Duration::from_millis(50));
        assert_eq!(retry_backoff(2), Duration::from_millis(150));
        assert_eq!(retry_backoff(3), Duration::from_millis(450));
        assert!(retry_backoff(2) > retry_backoff(1));
        assert!(retry_backoff(3) > retry_backoff(2));
        // Saturating + capped: no panic/overflow, never exceeds 1s.
        assert_eq!(retry_backoff(100), Duration::from_millis(1000));
    }

    /// A transient send failure on the per-file index POST is retried and the
    /// update ultimately succeeds (issue #2785 regression test).
    ///
    /// Why: this is the exact failure #2785 reports — under rapid repeated
    /// writes the per-file HTTP call intermittently fails at the transport
    /// layer. Before the fix a single such failure dropped the update; the fix
    /// retries transport errors with backoff. We reproduce a transport failure
    /// deterministically with a loopback server that drops the FIRST connection
    /// (no HTTP response → reqwest `send()` returns `Err`) then answers 200 on
    /// the SECOND, and assert the call retries and reports `Indexed`.
    /// What: binds an ephemeral 127.0.0.1 listener, serves the drop-then-200
    /// script, drives [`post_index_file_with_retries`] against it, and asserts
    /// the outcome is `Indexed` and that exactly two connections were made
    /// (one failed attempt + one successful retry). Uses its own private
    /// listener URL (never the global daemon-discovery path), so it cannot
    /// cross-talk with other tests.
    /// Test: this test.
    #[test]
    fn post_index_file_retries_transient_send_failure() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();

        let server = std::thread::spawn(move || {
            let mut accepted = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                accepted += 1;
                if accepted == 1 {
                    // Transient send failure: accept then close with no
                    // response, so the client's send() errors at the transport
                    // layer.
                    drop(stream);
                    continue;
                }
                // Successful retry: consume the request, answer 200.
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                );
                let _ = stream.flush();
                let _ = tx.send(accepted);
                break;
            }
        });

        let client = build_index_client().unwrap();
        let url = format!("http://{addr}/indexes/test-index/index-file");
        let body = index_file_request_body("src/main.rs", "fn main() {}\n");
        let outcome = post_index_file_with_retries(&client, &url, &body);

        let total_accepted = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("server should have answered the retry");
        let _ = server.join();

        assert_eq!(
            outcome,
            IndexOutcome::Indexed,
            "must recover via retry after a transient send failure"
        );
        assert_eq!(
            total_accepted, 2,
            "should retry exactly once (2 connections: 1 failed + 1 success)"
        );
    }
}
