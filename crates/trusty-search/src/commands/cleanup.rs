//! Handler for `trusty-search cleanup`.
//!
//! Why: over time, projects come and go and the daemon's `indexes.toml` accumulates
//! stale registrations for projects that were never successfully indexed (0 chunks).
//! These entries clutter `status` / `list` output and waste a tiny amount of memory
//! per index handle. A focused cleanup subcommand lets operators reclaim those slots
//! without resorting to manual `DELETE /indexes/:id` curl calls or hand-editing the
//! registry file.
//!
//! What: over the daemon socket (#9214), enumerates every registered index via
//! `search.indexes.list`, fetches each one's `chunk_count` via
//! `search.index.status`, collects the ids with zero chunks, optionally prompts
//! for confirmation, re-reads each id's registration immediately before its own
//! delete, and removes it via `search.index.delete` with `delete_data: true` and
//! `expected_root_path`. `--yes` skips the prompt; `--dry-run` short-circuits
//! before any delete (and overrides `--yes`). A list or status read that fails
//! stops the run before any delete.
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
//! Test: `cleanup_lists_checks_and_deletes_over_the_socket`; the refusal arms
//! are covered by the other `cleanup_*` tests at the foot of this file.

use super::daemon_rpc::index_status;
use super::list::fetch_index_list;
use anyhow::{bail, Result};
use colored::Colorize;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use trusty_search::service::daemon_client::DaemonClient;
use trusty_search::service::rpc::writes::METHOD_INDEX_DELETE;

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

/// Flags for `trusty-search cleanup`.
///
/// Why they live here, not in `main.rs`'s `Commands` enum (#6822): `main.rs`
/// sits on a frozen line-cap budget, and a subcommand's flags belong beside the
/// handler that reads them regardless. The CLI surface is unchanged — a
/// `clap::Args` struct in a tuple variant derives the same flags the inline
/// fields did.
/// Test: `handle_cleanup`'s existing coverage in this module.
#[derive(clap::Args, Debug, Clone)]
pub struct CleanupArgs {
    /// Skip the confirmation prompt and remove empty indexes immediately
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Show what would be removed without deleting anything (overrides --yes)
    #[arg(long)]
    pub dry_run: bool,
}

/// Why: extracted so `main()` doesn't inline the multi-step cleanup pipeline.
/// What: lists indexes, filters to those with `chunk_count == 0`, prints a
/// table, prompts unless `yes`, then deletes them. Returns `Err` on any daemon
/// error so `main()` can render the friendly red-✗ line. Does not start the
/// daemon: a stopped daemon has nothing to clean, and the error names the
/// socket (#9214).
/// Test: `cleanup_lists_checks_and_deletes_over_the_socket`,
/// `cleanup_fails_closed_when_the_socket_is_absent`.
pub async fn handle_cleanup(yes: bool, dry_run: bool) -> Result<()> {
    // #9214: the socket, never the retiring HTTP listener.
    cleanup_with(&DaemonClient::resolve()?, yes, dry_run).await
}

/// [`handle_cleanup`] against an explicit daemon client.
async fn cleanup_with(client: &DaemonClient, yes: bool, dry_run: bool) -> Result<()> {
    // 1) List registered index ids.
    let ids = listed_ids(&fetch_index_list(client).await?)?;

    // 2) Fetch per-index status concurrently and collect the empty ones.
    let (mut empties, unchecked) = scan_for_empty(client, &ids).await?;
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
    let (removed, failed) = delete_empty_indexes(client, &empties).await;

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

/// The resident index ids a `search.indexes.list` body names.
///
/// # Errors
///
/// When the body carries no `indexes` array, or an entry that is not a string
/// id. A list that cannot be read is not an empty registry.
fn listed_ids(body: &Value) -> Result<Vec<String>> {
    // #9214: the HTTP port read an unparseable list as `{"indexes": []}`.
    let Some(rows) = body.get("indexes").and_then(Value::as_array) else {
        bail!("the daemon's index list carries no `indexes` array; nothing was deleted");
    };
    rows.iter()
        .map(|v| match v.as_str() {
            Some(id) => Ok(id.to_string()),
            None => bail!("the daemon's index list names a non-string id {v}; nothing was deleted"),
        })
        .collect()
}

/// Read every id's status concurrently and return `(empty, unchecked)`.
///
/// What: one `search.index.status` per id. A zero-chunk index with a root is
/// a candidate; a body without a readable count or root is counted unchecked
/// and left alone (#6410).
///
/// # Errors
///
/// When any status read fails or is refused: the run then deletes nothing,
/// because an index the scan could not read is not known to be non-empty and
/// the operator would confirm a partial table.
async fn scan_for_empty(client: &DaemonClient, ids: &[String]) -> Result<(Vec<EmptyIndex>, usize)> {
    let mut joinset = tokio::task::JoinSet::new();
    for id in ids {
        let n = id.clone();
        let c = client.clone();
        joinset.spawn(async move {
            let status = index_status(&c, &n).await;
            (n, status)
        });
    }

    let mut empties: Vec<EmptyIndex> = Vec::new();
    let mut unchecked = 0usize;
    let mut unread: Vec<String> = Vec::new();
    while let Some(j) = joinset.join_next().await {
        // #9214: a failed or refused status read used to become `{}` and the
        // run went on to delete the ids that did answer.
        let (id, body) = match j {
            Ok((id, Ok(body))) => (id, body),
            Ok((id, Err(e))) => {
                unread.push(format!("{id}: {e}"));
                continue;
            }
            Err(e) => {
                unread.push(format!("a status read did not finish: {e}"));
                continue;
            }
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
    if !unread.is_empty() {
        unread.sort();
        bail!(
            "could not read the status of {} indexes, so nothing was deleted: {}",
            unread.len(),
            unread.join("; ")
        );
    }
    Ok((empties, unchecked))
}

/// Re-check and delete each confirmed index, returning `(removed, not removed)`.
///
/// Why: the confirm step is human-paced, so every id is re-read immediately
/// before its own delete rather than once for the batch — a single check at the
/// top would leave the last delete acting on a minutes-old fact, which is the
/// window this exists to close (#6410).
/// What: [`recheck_root`] per id, then `search.index.delete` with
/// `delete_data: true` and `expected_root_path` carrying the root that re-read
/// just reported. A refusal is recorded against that id and the
/// batch continues with the next one; nothing is deleted on a refusal.
/// Test: `cleanup_refuses_a_delete_whose_root_moved_after_the_listing`,
/// `cleanup_pins_the_delete_to_the_root_it_just_re_read`,
/// `cleanup_refuses_a_populated_index_the_listing_called_empty`.
async fn delete_empty_indexes(
    client: &DaemonClient,
    empties: &[EmptyIndex],
) -> (usize, Vec<(String, String)>) {
    let mut removed = 0usize;
    let mut failed: Vec<(String, String)> = Vec::new();
    for e in empties {
        let expected = match recheck_root(client, e).await {
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
        let params = json!({
            "index_id": e.id,
            "delete_data": true,
            "expected_root_path": expected,
        });
        match client.call(METHOD_INDEX_DELETE, params).await {
            Ok(_) => removed += 1,
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
/// What: one fresh `search.index.status`. The delete proceeds only when that
/// call succeeded, `chunk_count` is still `0`, and `root_path` is still exactly
/// what the listing showed.
///
/// # Errors
///
/// A string naming what stopped the check, reported against that id. Every arm
/// that is not an exact match refuses: unreachable, refused, a count or root the
/// body omits, a populated index, and a moved root. "I could not check" is never
/// "it still matches".
///
/// Test: `cleanup_refuses_a_delete_whose_root_moved_after_the_listing`,
/// `cleanup_refuses_a_populated_index_the_listing_called_empty`,
/// `cleanup_refuses_every_id_once_the_daemon_stops_answering`,
/// `cleanup_refuses_a_status_body_it_cannot_read`.
async fn recheck_root(client: &DaemonClient, e: &EmptyIndex) -> Result<String, String> {
    let id = &e.id;
    let body = index_status(client, id).await.map_err(|err| {
        if err.code().is_some() {
            format!(
                "not deleted: re-checking '{id}' was refused ({err}), so it was never confirmed \
                 still empty"
            )
        } else {
            format!("not deleted: could not re-check '{id}' before deleting it ({err})")
        }
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
/// stdin in the future without touching the socket plumbing.
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
    use crate::commands::mock_socket::{mock_daemon, MockDaemon};
    use std::sync::{Arc, Mutex};
    use trusty_common::uds::server::RpcError;
    use trusty_search::service::rpc::error::CODE_UNAVAILABLE;
    use trusty_search::service::rpc::reads::{METHOD_INDEXES_LIST, METHOD_INDEX_STATUS};

    /// The root the listing showed the operator, before anything moved.
    const LISTED_ROOT: &str = "/tmp/ts-6410-wiped";

    /// Every `(method, params)` a stub daemon received, in arrival order.
    type CallLog = Arc<Mutex<Vec<(String, Value)>>>;

    /// A stub daemon on a scratch socket and the calls it received.
    struct StubDaemon {
        daemon: MockDaemon,
        calls: CallLog,
    }

    impl StubDaemon {
        fn client(&self) -> &DaemonClient {
            &self.daemon.client
        }

        /// The params of every `search.index.delete` that arrived — empty means
        /// no delete left the client, which is what a refusal must produce.
        fn deletes(&self) -> Vec<Value> {
            self.calls
                .lock()
                .expect("call log")
                .iter()
                .filter(|(m, _)| m == METHOD_INDEX_DELETE)
                .map(|(_, p)| p.clone())
                .collect()
        }

        fn methods(&self) -> Vec<String> {
            let calls = self.calls.lock().expect("call log");
            calls.iter().map(|(m, _)| m.clone()).collect()
        }
    }

    /// Serve `list` for the index list, `status(id)` for each status read, and
    /// accept every delete.
    async fn stub_daemon(
        list: Value,
        status: impl Fn(&str) -> Result<Value, RpcError> + Send + Sync + 'static,
    ) -> StubDaemon {
        let calls: CallLog = Arc::default();
        let log = Arc::clone(&calls);
        let daemon = mock_daemon(move |method, params| {
            log.lock()
                .expect("call log")
                .push((method.to_string(), params.clone()));
            match method {
                METHOD_INDEXES_LIST => Ok(list.clone()),
                METHOD_INDEX_STATUS => status(params["index_id"].as_str().unwrap_or("")),
                METHOD_INDEX_DELETE => Ok(json!({ "ok": true, "removed": true })),
                other => Err(RpcError::method_not_found(other, &[])),
            }
        })
        .await;
        StubDaemon { daemon, calls }
    }

    /// A stub whose every status read answers `body`.
    async fn status_stub(body: Value) -> StubDaemon {
        stub_daemon(json!({ "indexes": [] }), move |_| Ok(body.clone())).await
    }

    fn listed() -> Vec<EmptyIndex> {
        vec![EmptyIndex {
            id: "wiped".to_string(),
            root_path: LISTED_ROOT.to_string(),
        }]
    }

    fn empty_at(root: &str) -> Value {
        json!({ "index_id": "wiped", "root_path": root, "chunk_count": 0 })
    }

    /// Why (#6410): the incident this fix exists for. The operator confirmed a
    /// 0-chunk listing; by the time the keypress landed the root had been wiped
    /// and recreated, so the same id named a different, live registration and
    /// `delete_data=true` destroyed it. The delete must not be sent at all.
    /// Test: this is the test — it fails against the pre-fix loop, which sent
    /// the delete straight from the listing.
    #[tokio::test(flavor = "multi_thread")]
    async fn cleanup_refuses_a_delete_whose_root_moved_after_the_listing() {
        let daemon = status_stub(empty_at("/tmp/ts-6410-recreated")).await;

        let (removed, failed) = delete_empty_indexes(daemon.client(), &listed()).await;

        assert_eq!(removed, 0, "a moved root must remove nothing");
        assert_eq!(
            daemon.deletes(),
            Vec::<Value>::new(),
            "no delete may be sent"
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
        let daemon = status_stub(json!({
            "index_id": "wiped",
            "root_path": LISTED_ROOT,
            "chunk_count": 14823,
        }))
        .await;

        let (removed, failed) = delete_empty_indexes(daemon.client(), &listed()).await;

        assert_eq!(removed, 0);
        assert_eq!(daemon.deletes(), Vec::<Value>::new());
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
            let daemon = status_stub(body.clone()).await;
            let (removed, failed) = delete_empty_indexes(daemon.client(), &listed()).await;
            assert_eq!(removed, 0, "{body}");
            assert_eq!(daemon.deletes(), Vec::<Value>::new(), "{body}");
            assert!(failed[0].1.contains("not deleted"), "{body}");
        }
    }

    /// Why (#6410): a daemon that stops answering partway through a batch must
    /// fail the remaining ids rather than let them through unchecked.
    /// Test: this is the test — nothing serves the scratch socket.
    #[tokio::test(flavor = "multi_thread")]
    async fn cleanup_refuses_every_id_once_the_daemon_stops_answering() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let gone = DaemonClient::at(dir.path().join("gone.sock"));
        let (removed, failed) = delete_empty_indexes(&gone, &listed()).await;

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
        let daemon = status_stub(empty_at(LISTED_ROOT)).await;

        let (removed, failed) = delete_empty_indexes(daemon.client(), &listed()).await;

        assert_eq!(removed, 1);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(
            daemon.deletes(),
            vec![json!({
                "index_id": "wiped",
                "delete_data": true,
                "expected_root_path": LISTED_ROOT,
            })],
            "the delete must pin itself to the re-read root"
        );
    }

    /// #9214: the whole run over the socket — list, one status per id, then a
    /// re-check and one pinned delete for the empty index only.
    #[tokio::test(flavor = "multi_thread")]
    async fn cleanup_lists_checks_and_deletes_over_the_socket() {
        let daemon = stub_daemon(json!({ "indexes": ["wiped", "full"] }), |id| {
            Ok(match id {
                "wiped" => empty_at(LISTED_ROOT),
                _ => json!({ "index_id": id, "root_path": "/tmp/full", "chunk_count": 9 }),
            })
        })
        .await;

        cleanup_with(daemon.client(), true, false)
            .await
            .expect("cleaned");

        let calls = daemon.calls.lock().expect("call log").clone();
        assert_eq!(calls[0], (METHOD_INDEXES_LIST.to_string(), json!({})));
        let mut statuses: Vec<Value> = calls
            .iter()
            .filter(|(m, _)| m == METHOD_INDEX_STATUS)
            .map(|(_, p)| p.clone())
            .collect();
        statuses.sort_by_key(|p| p["index_id"].to_string());
        assert_eq!(
            statuses,
            vec![
                json!({ "index_id": "full" }),
                json!({ "index_id": "wiped" }),
                json!({ "index_id": "wiped" }),
            ],
            "one scan read per id, plus the re-check of the candidate"
        );
        assert_eq!(
            daemon.deletes(),
            vec![json!({
                "index_id": "wiped",
                "delete_data": true,
                "expected_root_path": LISTED_ROOT,
            })]
        );
    }

    /// #9214 (fix-bar): a list body that does not name its indexes is not an
    /// empty registry. The HTTP port read an unparseable list as `[]`.
    #[tokio::test(flavor = "multi_thread")]
    async fn cleanup_deletes_nothing_when_the_list_is_unreadable() {
        let daemon =
            stub_daemon(json!({ "unexpected": true }), |_| Ok(empty_at(LISTED_ROOT))).await;

        let err = cleanup_with(daemon.client(), true, false)
            .await
            .expect_err("an unreadable list must fail the run");

        assert!(err.to_string().contains("indexes"), "{err}");
        assert_eq!(daemon.methods(), vec![METHOD_INDEXES_LIST.to_string()]);
    }

    /// #9214 (fix-bar): a refused status read stops the run before any delete,
    /// even for the ids whose status did come back empty.
    #[tokio::test(flavor = "multi_thread")]
    async fn cleanup_deletes_nothing_when_a_status_is_refused() {
        let daemon = stub_daemon(json!({ "indexes": ["wiped", "busy"] }), |id| match id {
            "wiped" => Ok(empty_at(LISTED_ROOT)),
            _ => Err(RpcError::new(CODE_UNAVAILABLE, "index_loading")),
        })
        .await;

        let err = cleanup_with(daemon.client(), true, false)
            .await
            .expect_err("a refused status must fail the run");

        let text = err.to_string();
        assert!(
            text.contains("busy") && text.contains("index_loading"),
            "{text}"
        );
        assert_eq!(
            daemon.deletes(),
            Vec::<Value>::new(),
            "no delete may be sent"
        );
    }

    /// #9214: an absent socket fails closed, naming it, with no URL.
    #[tokio::test]
    async fn cleanup_fails_closed_when_the_socket_is_absent() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let socket = dir.path().join("absent.sock");
        let err = cleanup_with(&DaemonClient::at(&socket), true, false)
            .await
            .expect_err("nothing serves the socket");
        let text = err.to_string();
        assert!(text.starts_with("could not reach daemon"), "{text}");
        assert!(text.contains(&socket.display().to_string()), "{text}");
        assert!(!text.contains("http://"), "{text}");
    }
}
