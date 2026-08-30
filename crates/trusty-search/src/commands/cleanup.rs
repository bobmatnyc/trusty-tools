//! Handler for `trusty-search cleanup`.
//!
//! Why: over time, projects come and go and the daemon's `indexes.toml` accumulates
//! stale registrations for projects that were never successfully indexed (0 chunks).
//! These entries clutter `status` / `list` output and waste a tiny amount of memory
//! per index handle. A focused cleanup subcommand lets operators reclaim those slots
//! without resorting to manual `DELETE /indexes/:id` curl calls or hand-editing the
//! registry file.
//!
//! What: enumerates every registered index via `GET /indexes`, fetches each one's
//! `chunk_count` via `GET /indexes/:id/status`, collects the ids with zero chunks,
//! optionally prompts for confirmation, re-reads each id's registration
//! immediately before its own delete, and removes it via
//! `DELETE /indexes/:id?delete_data=true&expected_root_path=…`.
//! `--yes` skips the prompt; `--dry-run` short-circuits before any DELETE (and
//! overrides `--yes`).
//!
//! Why the re-read (#6410): the listing is a fact with an expiry and the confirm
//! step is human-paced. An index id is derived deterministically from its
//! `root_path`, so a root wiped and recreated between the listing and the
//! keypress names a live, freshly-reindexed index under the same id — and this
//! command deletes with `delete_data=true`. [`recheck_root`] closes the window
//! the operator sits in, and the `expected_root_path` it hands the daemon closes
//! the residual one, because only the daemon can compare under the teardown lock
//! (`service::server::delete_guard`). Same shape as the console's `OrphanGuard`
//! (#6380).
//!
//! Test: register an empty index (`POST /indexes` with no follow-up reindex), run
//! `trusty-search cleanup --yes`, then verify `GET /indexes` no longer lists it.
//! The refusal arms are covered by the `cleanup_*` tests at the foot of this
//! file.

use super::daemon_utils::daemon_base_url;
use anyhow::{bail, Result};
use colored::Colorize;
use std::io::{BufRead, Write};

/// Why: a small record per empty index keeps the table-printing step and the
/// DELETE loop independent of the JSON shape returned by the daemon.
/// What: holds the index id and the root path the listing showed for it. The
/// root is not display-only since #6410 — it is what [`recheck_root`] compares
/// the fresh reading against, so an index whose root the daemon omitted is never
/// a candidate and this field is never empty.
/// Test: covered transitively by `handle_cleanup`'s integration usage, and
/// directly by the `cleanup_*` tests at the foot of this file.
struct EmptyIndex {
    id: String,
    root_path: String,
}

/// Why: extracted so `main()` doesn't inline the multi-step cleanup pipeline.
/// What: lists indexes, filters to those with `chunk_count == 0`, prints a
/// table, prompts unless `yes`, then deletes them. Returns `Err` only on
/// unrecoverable daemon errors so `main()` can render the friendly red-✗ line.
/// Test: `cargo run -p trusty-search -- cleanup --dry-run` prints the table
/// and exits without deleting; `cleanup --yes` deletes without prompting.
pub async fn handle_cleanup(yes: bool, dry_run: bool) -> Result<()> {
    let base = daemon_base_url();
    let client = trusty_common::server::daemon_http_client()?;

    // 1) List registered index ids.
    let list_url = format!("{}/indexes", base);
    let list_body: serde_json::Value = match client.get(&list_url).send().await {
        Ok(resp) if resp.status().is_success() => resp
            .json()
            .await
            .unwrap_or_else(|_| serde_json::json!({"indexes": []})),
        Ok(resp) => bail!("daemon returned {} for {}", resp.status(), list_url),
        Err(e) => bail!("could not reach daemon at {}: {e}", base),
    };

    let empty_arr: Vec<serde_json::Value> = Vec::new();
    let ids: Vec<String> = list_body
        .get("indexes")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty_arr)
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    // 2) Fetch per-index status concurrently and collect the empty ones.
    let mut joinset = tokio::task::JoinSet::new();
    for id in &ids {
        let n = id.clone();
        let url = format!("{}/indexes/{}/status", base, n);
        let c = client.clone();
        joinset.spawn(async move {
            let body: serde_json::Value = match c.get(&url).send().await {
                Ok(r) if r.status().is_success() => {
                    r.json().await.unwrap_or_else(|_| serde_json::json!({}))
                }
                _ => serde_json::json!({}),
            };
            (n, body)
        });
    }

    let mut empties: Vec<EmptyIndex> = Vec::new();
    let mut unchecked = 0usize;
    while let Some(j) = joinset.join_next().await {
        let Ok((id, body)) = j else {
            unchecked += 1;
            continue;
        };
        let root = body.get("root_path").and_then(|v| v.as_str()).unwrap_or("");
        // #6410: an index whose status did not come back is not a known-empty
        // index. The former `unwrap_or(0)` read an unreachable daemon as a
        // zero-chunk candidate and offered it for deletion.
        match body.get("chunk_count").and_then(|v| v.as_u64()) {
            Some(0) if !root.is_empty() => empties.push(EmptyIndex {
                id,
                root_path: root.to_string(),
            }),
            // Zero chunks but no root to pin the delete to, or no readable
            // count at all: not a candidate, and say so rather than skip it
            // silently.
            Some(0) | None => unchecked += 1,
            Some(_) => {}
        }
    }
    empties.sort_by(|a, b| a.id.cmp(&b.id));

    if unchecked > 0 {
        println!(
            "{} {} indexes could not be checked and were left alone.",
            "!".yellow(),
            unchecked
        );
    }

    // 3) Nothing to do?
    if empties.is_empty() {
        println!("Nothing to clean up.");
        return Ok(());
    }

    // 4) Show what would be removed.
    let count = empties.len();
    println!(
        "{} {} empty indexes (0 chunks):",
        "Found".bold(),
        count.to_string().bold()
    );
    let name_width = empties.iter().map(|e| e.id.len()).max().unwrap_or(0).max(4);
    for e in &empties {
        println!(
            "  {:<width$}  {}",
            e.id.bold(),
            e.root_path.dimmed(),
            width = name_width
        );
    }

    // 5) Dry-run wins over --yes.
    if dry_run {
        println!("{} dry-run: no indexes were removed.", "ℹ".cyan());
        return Ok(());
    }

    // 6) Prompt unless --yes.
    if !yes && !confirm(&format!("Remove these {} indexes?", count))? {
        println!("Aborted.");
        return Ok(());
    }

    // 7) Re-check then DELETE each empty index, counting successes and refusals.
    let (removed, failed) = delete_empty_indexes(&client, &base, &empties).await;

    // 8) Summary.
    if failed.is_empty() {
        println!(
            "{} Removed {} empty indexes.",
            "✓".green(),
            removed.to_string().bold()
        );
    } else {
        println!(
            "{} Removed {} of {} empty indexes ({} not removed):",
            "!".yellow(),
            removed,
            count,
            failed.len()
        );
        for (id, err) in &failed {
            println!("  {} {} — {}", "✗".red(), id, err.dimmed());
        }
        bail!("{} index removals were refused or failed", failed.len());
    }

    Ok(())
}

/// Re-check and delete each confirmed index, returning `(removed, not removed)`.
///
/// Why: the confirm step is human-paced, so every id is re-read immediately
/// before its own delete rather than once for the batch — a single check at the
/// top would leave the last delete acting on a minutes-old fact, which is the
/// window this exists to close (#6410).
/// What: [`recheck_root`] per id, then
/// `DELETE /indexes/{id}?delete_data=true&expected_root_path=…` carrying the root
/// that re-read just reported. A refusal is recorded against that id and the
/// batch continues with the next one; nothing is deleted on a refusal.
/// Test: `cleanup_refuses_a_delete_whose_root_moved_after_the_listing`,
/// `cleanup_pins_the_delete_to_the_root_it_just_re_read`,
/// `cleanup_refuses_a_populated_index_the_listing_called_empty`.
async fn delete_empty_indexes(
    client: &reqwest::Client,
    base: &str,
    empties: &[EmptyIndex],
) -> (usize, Vec<(String, String)>) {
    let mut removed = 0usize;
    let mut failed: Vec<(String, String)> = Vec::new();
    for e in empties {
        let expected = match recheck_root(client, base, e).await {
            Ok(root) => root,
            Err(refusal) => {
                failed.push((e.id.clone(), refusal));
                continue;
            }
        };
        // Issue #4123: `DELETE` preserves on-disk data unless `delete_data=true`
        // is passed. Opt in so `cleanup` keeps fully reclaiming the stub.
        // #6410: `expected_root_path` makes the daemon repeat the comparison
        // under the teardown lock, so the gap between the re-read above and this
        // request cannot be exploited either.
        let url = format!("{}/indexes/{}", base, e.id);
        let request = client.delete(&url).query(&[
            ("delete_data", "true"),
            ("expected_root_path", expected.as_str()),
        ]);
        match request.send().await {
            Ok(resp) if resp.status().is_success() => removed += 1,
            Ok(resp) => failed.push((e.id.clone(), format!("HTTP {}", resp.status()))),
            Err(err) => failed.push((e.id.clone(), err.to_string())),
        }
    }
    (removed, failed)
}

/// Re-read `e`'s registration and hand back the root the delete must pin to.
///
/// Why: `cleanup` deletes with `delete_data=true`, so acting on a stale listing
/// destroys a live corpus. An id is derived from its `root_path`; a root wiped
/// and recreated between the listing and the operator's keypress carries the same
/// id, and by then it may hold chunks again (#6410).
/// What: one fresh `GET /indexes/{id}/status`. The delete proceeds only when that
/// call succeeded, the body parsed, `chunk_count` is still `0`, and `root_path`
/// is still exactly what the listing showed.
///
/// # Errors
///
/// A string naming what stopped the check, reported against that id. Every arm
/// that is not an exact match refuses: unreachable, non-2xx, unparseable, a count
/// or root the body omits, a populated index, and a moved root. "I could not
/// check" is never "it still matches".
///
/// Test: `cleanup_refuses_a_delete_whose_root_moved_after_the_listing`,
/// `cleanup_refuses_a_populated_index_the_listing_called_empty`,
/// `cleanup_refuses_every_id_once_the_daemon_stops_answering`,
/// `cleanup_refuses_a_status_body_it_cannot_read`.
async fn recheck_root(
    client: &reqwest::Client,
    base: &str,
    e: &EmptyIndex,
) -> Result<String, String> {
    let id = &e.id;
    let url = format!("{}/indexes/{}/status", base, id);
    let resp = client.get(&url).send().await.map_err(|err| {
        format!("not deleted: could not re-check '{id}' before deleting it ({err})")
    })?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!(
            "not deleted: re-checking '{id}' returned HTTP {status}, so it was never confirmed \
             still empty"
        ));
    }
    let body: serde_json::Value = resp.json().await.map_err(|err| {
        format!("not deleted: the re-check of '{id}' returned a body that did not parse ({err})")
    })?;
    let chunks = body
        .get("chunk_count")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| format!("not deleted: the re-check of '{id}' reported no chunk_count"))?;
    if chunks != 0 {
        return Err(format!(
            "not deleted: '{id}' now holds {chunks} chunks, so the listing that called it empty \
             is out of date"
        ));
    }
    let current = body
        .get("root_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!(
                "not deleted: the re-check of '{id}' reported no root_path to pin the delete to"
            )
        })?;
    if current != e.root_path {
        return Err(format!(
            "not deleted: '{id}' now points at {current}, not at the listed {}; the registration \
             changed after it was listed",
            e.root_path
        ));
    }
    Ok(current.to_string())
}

/// Why: keep the y/N prompt isolated so tests of `handle_cleanup` can stub
/// stdin in the future without touching the HTTP plumbing.
/// What: prints `<prompt> [y/N] ` to stdout, reads one line from stdin, returns
/// `true` when the trimmed reply starts with `y` or `Y`. Empty input → false.
/// Test: side-effect-only; exercised manually via `cargo run -- cleanup`.
fn confirm(prompt: &str) -> Result<bool> {
    print!("{} [y/N] ", prompt);
    std::io::stdout().flush().ok();
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let answer = line.trim();
    Ok(matches!(answer.chars().next(), Some('y') | Some('Y')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    /// The root the listing showed the operator, before anything moved.
    const LISTED_ROOT: &str = "/tmp/ts-6410-wiped";

    /// A stub daemon serving one canned status body and recording every DELETE.
    ///
    /// `deletes` holds the raw query string of each `DELETE /indexes/{id}` that
    /// arrived — empty means the delete never left the client, which is what a
    /// refusal must produce.
    struct StubDaemon {
        base: String,
        deletes: Arc<Mutex<Vec<String>>>,
    }

    impl StubDaemon {
        fn deletes(&self) -> Vec<String> {
            self.deletes.lock().expect("delete log").clone()
        }
    }

    /// Bind an ephemeral port serving `status_body` for every id's status.
    async fn stub_daemon(status_body: serde_json::Value) -> StubDaemon {
        let deletes: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = deletes.clone();
        let app = axum::Router::new()
            .route(
                "/indexes/{id}/status",
                axum::routing::get(move || {
                    let body = status_body.clone();
                    async move { axum::Json(body) }
                }),
            )
            .route(
                "/indexes/{id}",
                axum::routing::delete(
                    move |axum::extract::RawQuery(query): axum::extract::RawQuery| {
                        let recorded = recorded.clone();
                        async move {
                            recorded
                                .lock()
                                .expect("delete log")
                                .push(query.unwrap_or_default());
                            axum::Json(json!({ "ok": true, "removed": true }))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        StubDaemon {
            base: format!("http://{addr}"),
            deletes,
        }
    }

    fn listed() -> Vec<EmptyIndex> {
        vec![EmptyIndex {
            id: "wiped".to_string(),
            root_path: LISTED_ROOT.to_string(),
        }]
    }

    fn client() -> reqwest::Client {
        trusty_common::server::daemon_http_client().expect("daemon http client")
    }

    /// Why (#6410): the incident this fix exists for. The operator confirmed a
    /// 0-chunk listing; by the time the keypress landed the root had been wiped
    /// and recreated, so the same id named a different, live registration and
    /// `delete_data=true` destroyed it. The delete must not be sent at all.
    /// Test: this is the test — it fails against the pre-fix loop, which sent
    /// the DELETE straight from the listing.
    #[tokio::test(flavor = "multi_thread")]
    async fn cleanup_refuses_a_delete_whose_root_moved_after_the_listing() {
        let daemon = stub_daemon(json!({
            "index_id": "wiped",
            "root_path": "/tmp/ts-6410-recreated",
            "chunk_count": 0,
        }))
        .await;

        let (removed, failed) = delete_empty_indexes(&client(), &daemon.base, &listed()).await;

        assert_eq!(removed, 0, "a moved root must remove nothing");
        assert_eq!(
            daemon.deletes(),
            Vec::<String>::new(),
            "no DELETE may be sent"
        );
        let (id, why) = failed.first().expect("the refusal must be reported");
        assert_eq!(id, "wiped");
        assert!(
            why.contains("not deleted") && why.contains("/tmp/ts-6410-recreated"),
            "the row must say nothing was deleted and name the root it found: {why}"
        );
    }

    /// Why (#6410): a root recreated AND reindexed inside the confirm window
    /// reads as populated, which is the loudest possible signal that the listing
    /// expired. `delete_data=true` on it destroys a live corpus.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn cleanup_refuses_a_populated_index_the_listing_called_empty() {
        let daemon = stub_daemon(json!({
            "index_id": "wiped",
            "root_path": LISTED_ROOT,
            "chunk_count": 14823,
        }))
        .await;

        let (removed, failed) = delete_empty_indexes(&client(), &daemon.base, &listed()).await;

        assert_eq!(removed, 0);
        assert_eq!(daemon.deletes(), Vec::<String>::new());
        assert!(
            failed[0].1.contains("14823 chunks"),
            "the row must name what it found: {}",
            failed[0].1
        );
    }

    /// Why: a status body missing the two fields the decision rests on says
    /// nothing about whether the index is still the empty one that was listed,
    /// so it must not read as a pass.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn cleanup_refuses_a_status_body_it_cannot_read() {
        for body in [json!({}), json!({ "chunk_count": 0 })] {
            let daemon = stub_daemon(body.clone()).await;
            let (removed, failed) = delete_empty_indexes(&client(), &daemon.base, &listed()).await;
            assert_eq!(removed, 0, "{body}");
            assert_eq!(daemon.deletes(), Vec::<String>::new(), "{body}");
            assert!(failed[0].1.contains("not deleted"), "{body}");
        }
    }

    /// Why (#6410): a daemon that stops answering partway through a batch must
    /// fail the remaining ids rather than let them through unchecked.
    /// Test: this is the test — port 1 is reserved, so the connect is refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn cleanup_refuses_every_id_once_the_daemon_stops_answering() {
        let (removed, failed) =
            delete_empty_indexes(&client(), "http://127.0.0.1:1", &listed()).await;

        assert_eq!(removed, 0);
        assert!(
            failed[0].1.contains("not deleted") && failed[0].1.contains("re-check"),
            "the row must say the delete did not happen and why: {}",
            failed[0].1
        );
    }

    /// Why (#6410): the re-read narrows the window; only the daemon can close it,
    /// by re-comparing under the teardown lock. That is what
    /// `expected_root_path` buys, so the request has to actually carry it.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn cleanup_pins_the_delete_to_the_root_it_just_re_read() {
        let daemon = stub_daemon(json!({
            "index_id": "wiped",
            "root_path": LISTED_ROOT,
            "chunk_count": 0,
        }))
        .await;

        let (removed, failed) = delete_empty_indexes(&client(), &daemon.base, &listed()).await;

        assert_eq!(removed, 1);
        assert!(failed.is_empty(), "{failed:?}");
        let query = daemon.deletes().first().cloned().expect("one DELETE");
        assert!(query.contains("delete_data=true"), "{query}");
        assert!(
            query.contains("expected_root_path=%2Ftmp%2Fts-6410-wiped"),
            "the delete must pin itself to the re-read root: {query}"
        );
    }
}
