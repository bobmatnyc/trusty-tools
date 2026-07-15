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
//! Test: `ensure_project_indexed_returns_derived_id_when_daemon_down`,
//! `ensure_project_indexed_none_for_root`, and the `index_is_fresh_*` predicate
//! tests in the `tests` module below.

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
}
