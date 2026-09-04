//! One test per way grounding can degrade, and one for when it does not.
//!
//! Why a whole file: #6081's contract is that no leg of the code-analysis
//! chain may fail silently, and "fail-open" is only a promise if every arm of it
//! is exercised. Each test below drives ONE leg to failure against a stub
//! daemon or a stub binary and asserts that the reason names the repository and
//! survives into the caller's gap list — the shape a bare `Ok(())` return would
//! hide.
//!
//! Nothing here reaches a real daemon or a real binary: every daemon is a stub
//! socket under a temp directory, or a path nothing has ever bound, and every
//! binary is a shell script. Nothing reads or writes the process environment
//! either, which is what lets these run in parallel with the rest of the suite.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

/// A stand-in `trusty-search` socket (#6285).
///
/// Why a socket and not an HTTP listener: the search leg dials
/// `trusty_common::daemon_socket_path("trusty-search")` now, exactly as the
/// analyze leg has since #6287, so one stub shape serves both daemons.
///
/// What: binds a hardened socket under `dir` and answers two methods.
/// `search.health` always reports `ok` — the daemon's own `GET /health`
/// answered 200 unconditionally, and `search_is_healthy` keeps that. Queries
/// answer `hits` as their result, or the refusal a daemon spells for an index
/// it does not hold when `hits` is `None`, which is what the pre-#6285 stub's
/// HTTP 404 meant.
fn stub_search_socket(dir: &Path, hits: Option<&'static str>) -> PathBuf {
    answering_socket(dir, "trusty-search", move |request| {
        if request.contains("search.health") {
            r#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok","version":"0.0.0","indexes":1}}"#
                .to_owned()
        } else if request.contains("search.query") {
            match hits {
                // Compacted, not interpolated raw: the fixtures are
                // pretty-printed and a frame is newline-terminated, so
                // embedding one verbatim would end the frame at its first
                // line break.
                Some(json) => {
                    let value: serde_json::Value =
                        serde_json::from_str(json).expect("a valid search fixture");
                    format!(r#"{{"jsonrpc":"2.0","id":1,"result":{value}}}"#)
                }
                None => {
                    r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32004,"message":"no such index"}}"#
                        .to_owned()
                }
            }
        } else {
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no such method"}}"#
                .to_owned()
        }
    })
}

/// A socket that accepts connections and refuses every health call.
///
/// The middle case `daemons::wait_socket_ready`'s two budgets exist to
/// separate: the socket IS bound, so this is a daemon that is starting rather
/// than absent.
fn unhealthy_search_socket(dir: &Path) -> PathBuf {
    answering_socket(dir, "warming-search", |_| {
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"warming up"}}"#.to_owned()
    })
}

/// A socket path nothing has ever bound — the search equivalent of
/// [`dead_socket`].
fn dead_search_socket(dir: &Path) -> PathBuf {
    dir.join("absent-search.sock")
}

/// Bind one hardened socket under `dir` and answer every frame with `reply`.
///
/// A distinct filename per stub: one test builds its `Tools` twice from the
/// same temp dir, and a socket path can only be bound once.
fn answering_socket<F>(dir: &Path, name: &str, reply: F) -> PathBuf
where
    F: Fn(&str) -> String + Send + Sync + 'static,
{
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let socket = dir.join("sockets").join(format!("{name}-{n}.sock"));
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind the stub socket");
    let reply = std::sync::Arc::new(reply);
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            let reply = std::sync::Arc::clone(&reply);
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                let mut sink = Vec::new();
                let _ = conn.read_to_end(&mut sink).await;
                let request = String::from_utf8_lossy(&sink).into_owned();
                let _ = conn.write_all(reply(&request).as_bytes()).await;
                let _ = conn.write_all(b"\n").await;
                let _ = conn.flush().await;
            });
        }
    });
    socket
}

/// A `trusty-search` that approves and indexes whatever it is asked to.
#[cfg(unix)]
fn approving_search(at: &Path) -> PathBuf {
    stub_binary(at, "trusty-search", "#!/bin/sh\nexit 0\n")
}

#[cfg(unix)]
fn stub_binary(at: &Path, name: &str, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = at.join(name);
    let mut file = std::fs::File::create(&path).expect("create stub");
    file.write_all(script.as_bytes()).expect("write stub");
    drop(file);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// Tools whose budgets are short enough that no failing test waits on one.
fn tools(
    search: PathBuf,
    search_socket: PathBuf,
    analyze: PathBuf,
    analyze_socket: PathBuf,
) -> Tools {
    Tools {
        search,
        search_socket,
        analyze,
        analyze_socket,
        bind_timeout: Duration::from_millis(60),
        startup_timeout: Duration::from_millis(150),
        poll_interval: Duration::from_millis(20),
    }
}

/// A stand-in `trusty-analyze` socket (#6287).
///
/// What: binds a hardened socket under `dir` and answers two methods.
/// `analyze.health` always reports `ok`; `analyze.complexity_hotspots` returns
/// `hotspots` as its result, or a JSON-RPC error when `hotspots` is `None` —
/// which is what the pre-#6287 stub's HTTP 500 meant, and what the fetch leg
/// must still turn into a named gap rather than an empty ranking.
fn stub_analyze_socket(dir: &Path, hotspots: Option<&'static str>) -> PathBuf {
    // A distinct filename per stub: one test builds its `Tools` twice from the
    // same temp dir, and a socket path can only be bound once.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let socket = dir.join("sockets").join(format!("trusty-analyze-{n}.sock"));
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind the stub socket");
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                let mut sink = Vec::new();
                let _ = conn.read_to_end(&mut sink).await;
                let request = String::from_utf8_lossy(&sink).into_owned();
                let reply = if request.contains("analyze.health") {
                    r#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok","version":"0.0.0","search_reachable":true}}"#.to_owned()
                } else if request.contains("analyze.complexity_hotspots") {
                    match hotspots {
                        // Compacted, not interpolated raw: the fixtures are
                        // pretty-printed and a frame is newline-terminated, so
                        // embedding one verbatim would end the frame at its
                        // first line break.
                        Some(json) => {
                            let value: serde_json::Value =
                                serde_json::from_str(json).expect("a valid hotspot fixture");
                            format!(r#"{{"jsonrpc":"2.0","id":1,"result":{value}}}"#)
                        }
                        None => r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"index is not loaded"}}"#.to_owned(),
                    }
                } else {
                    r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no such method"}}"#
                        .to_owned()
                };
                let _ = conn.write_all(reply.as_bytes()).await;
                let _ = conn.write_all(b"\n").await;
                let _ = conn.flush().await;
            });
        }
    });
    socket
}

/// A socket path nothing has ever bound — the analyze equivalent of
/// [`dead_url`].
fn dead_socket(dir: &Path) -> PathBuf {
    dir.join("absent-analyze.sock")
}

const HOTSPOTS: &str = r#"{"index_id":"acme-api","top_n":60,"hotspots":[
    {"id":"a","file":"CHECKOUT/src/pay.rs","start_line":1,"end_line":9,"content":"",
     "function_name":"settle_invoice","cyclomatic":31,"cognitive":40},
    {"id":"b","file":"CHECKOUT/src/pay.rs","start_line":20,"end_line":30,"content":"",
     "function_name":"refund","cyclomatic":18,"cognitive":22},
    {"id":"c","file":"CHECKOUT/src/auth.rs","start_line":1,"end_line":9,"content":"",
     "function_name":"verify","cyclomatic":12,"cognitive":15}
]}"#;

const EMPTY_HOTSPOTS: &str = r#"{"index_id":"acme-api","top_n":60,"hotspots":[]}"#;

/// What #6082's `search.query` answers with. The stub cannot vary its
/// answer per query, so every dimension finds the same two files — enough to
/// assert attribution and that the leg survives a dead trusty-analyze.
const SEARCH_HITS: &str = r#"{"results":[
    {"id":"a","file":"CHECKOUT/src/login.rs","path":"src/login.rs","start_line":18,
     "end_line":40,"content":"","score":0.88,"match_reason":"hybrid"},
    {"id":"b","file":"CHECKOUT/src/session.rs","path":"src/session.rs","start_line":4,
     "end_line":20,"content":"","score":0.51,"match_reason":"bm25"}
],"intent":"Semantic"}"#;

/// A manifest shaped like the one `tga audit` leaves in `out/<stem>/`.
fn manifest_naming(dir: &Path, checkout: &Path) -> PathBuf {
    let path = dir.join("manifest.toml");
    std::fs::write(
        &path,
        format!(
            "[report]\ntitle = \"Acme\"\n\n[[repositories]]\nname = \"01-acme-api\"\npath = \"{}\"\n",
            checkout.display()
        ),
    )
    .expect("write manifest");
    path
}

// ─── The legs, one failure each ──────────────────────────────────────────────

/// A path with no basename yields no index id, and no daemon is contacted for
/// it — the first leg refuses before anything is spawned or probed.
#[tokio::test]
async fn a_checkout_with_no_basename_never_reaches_a_daemon() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let t = tools(
        PathBuf::from("/nonexistent/trusty-search"),
        dead_search_socket(tmp.path()),
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead_socket(tmp.path()),
    );
    let out = ground(
        &t,
        Path::new("/"),
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;
    assert!(out.index_id.is_none());
    assert!(out.priorities.is_empty());
    let gaps = without_churn_leg(out.gaps);
    assert_eq!(gaps.len(), 1, "{:?}", gaps);
    assert!(gaps[0].contains("acme-api"), "{:?}", gaps);
    assert!(gaps[0].contains("no final path component"), "{:?}", gaps);
}

/// #6081's headline case: the search daemon is the first link, and losing it
/// must produce a line rather than an empty code-analysis section.
#[tokio::test]
async fn a_search_daemon_that_will_not_start_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let t = tools(
        PathBuf::from("/nonexistent/trusty-search"),
        dead_search_socket(tmp.path()),
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead_socket(tmp.path()),
    );
    let out = ground(
        &t,
        Path::new("/w/repos/acme-api"),
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;
    assert!(out.priorities.is_empty());
    let gaps = without_churn_leg(out.gaps);
    assert_eq!(gaps.len(), 1, "{:?}", gaps);
    assert!(gaps[0].contains("acme-api"), "{:?}", gaps);
    assert!(gaps[0].contains("trusty-search"), "{:?}", gaps);
    assert!(gaps[0].contains("not assessed"), "{:?}", gaps);
}

/// A daemon that is already answering is not restarted, so a resumed sweep and
/// a machine with a running instance both cost one probe.
#[cfg(unix)]
#[tokio::test]
async fn a_reachable_search_daemon_is_not_restarted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let search_socket = stub_search_socket(tmp.path(), None);
    // The binary does not exist: reaching the spawn at all would fail the leg.
    let t = tools(
        PathBuf::from("/nonexistent/trusty-search"),
        search_socket,
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead_socket(tmp.path()),
    );
    let _ = tmp;
    daemons::ensure_search(&t)
        .await
        .expect("a live daemon needs no binary");
}

/// trusty-search answers but refuses the checkout. The refusal is quoted, so a
/// recipient learns which remedy applies instead of seeing an empty section.
#[cfg(unix)]
#[tokio::test]
async fn an_unindexable_checkout_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let search = stub_binary(
        tmp.path(),
        "trusty-search",
        "#!/bin/sh\necho 'indexing refused: root is not allowlisted' >&2\nexit 1\n",
    );
    let t = tools(
        search,
        stub_search_socket(tmp.path(), None),
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead_socket(tmp.path()),
    );
    let out = ground(
        &t,
        Path::new("/w/repos/acme-api"),
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;
    assert!(out.priorities.is_empty());
    assert_eq!(
        out.index_id,
        index::index_id_for(Path::new("/w/repos/acme-api"))
    );
    let gaps = without_churn_leg(out.gaps);
    assert_eq!(gaps.len(), 1, "{:?}", gaps);
    assert!(gaps[0].contains("acme-api"), "{:?}", gaps);
    assert!(gaps[0].contains("not allowlisted"), "{:?}", gaps);
}

/// The second daemon. It is reached only once the index exists, and its loss is
/// stated as itself rather than as an empty hotspot list.
///
/// #6082 also makes this the proof that the two ranking legs are independent:
/// trusty-analyze is unreachable, and the search-derived evidence still ranks.
#[cfg(unix)]
#[tokio::test]
async fn an_analyze_daemon_that_will_not_start_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let t = tools(
        approving_search(tmp.path()),
        stub_search_socket(tmp.path(), Some(SEARCH_HITS)),
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead_socket(tmp.path()),
    );
    let out = ground(
        &t,
        Path::new("/w/repos/acme-api"),
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;
    let gaps = without_churn_leg(out.gaps);
    assert_eq!(gaps.len(), 1, "{:?}", gaps);
    assert!(gaps[0].contains("acme-api"), "{:?}", gaps);
    assert!(gaps[0].contains("trusty-analyze"), "{:?}", gaps);
    assert!(
        !out.priorities.is_empty(),
        "search-derived evidence must survive a dead trusty-analyze"
    );
    assert!(
        out.priorities
            .iter()
            .all(|p| p.dimension.is_some() && p.reason.is_some()),
        "every ranked path names its dimension and its query: {:?}",
        out.priorities
    );
}

/// Reachable but broken: the daemon reports healthy and then refuses the query.
/// That is a different failure from an unreachable daemon and gets its own line.
#[cfg(unix)]
#[tokio::test]
async fn an_unreachable_hotspots_endpoint_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let t = tools(
        approving_search(tmp.path()),
        stub_search_socket(tmp.path(), Some(SEARCH_HITS)),
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_analyze_socket(tmp.path(), None),
    );
    let out = ground(
        &t,
        Path::new("/w/repos/acme-api"),
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;
    let gaps = without_churn_leg(out.gaps);
    assert_eq!(gaps.len(), 1, "{:?}", gaps);
    // #6287: the reachable-but-broken daemon answers a JSON-RPC error frame
    // where it used to answer HTTP 500. The gap must carry the daemon's own
    // reason either way — that is what separates it from an unreachable one.
    assert!(gaps[0].contains("index is not loaded"), "{:?}", gaps);
    assert!(
        gaps[0].contains("search-derived evidence only"),
        "{:?}",
        gaps
    );
}

/// Measured, and nothing was complex. Distinct from "could not measure", and it
/// still owes a line — otherwise a report with no ranking reads as one whose
/// ranking simply found nothing worth naming.
#[cfg(unix)]
#[tokio::test]
async fn an_empty_hotspot_list_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let t = tools(
        approving_search(tmp.path()),
        stub_search_socket(tmp.path(), Some(SEARCH_HITS)),
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_analyze_socket(tmp.path(), Some(EMPTY_HOTSPOTS)),
    );
    let out = ground(
        &t,
        Path::new("/w/repos/acme-api"),
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;
    let gaps = without_churn_leg(out.gaps);
    assert_eq!(gaps.len(), 1, "{:?}", gaps);
    assert!(gaps[0].contains("no complexity hotspot"), "{:?}", gaps);
}

/// #6082: the index answers, and matches nothing. That is not the same as a
/// dead daemon and it is not a clean bill of health — with no complexity
/// measurement either, the run says so twice and ranks nothing.
#[cfg(unix)]
#[tokio::test]
async fn a_search_that_answers_nothing_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let t = tools(
        approving_search(tmp.path()),
        stub_search_socket(tmp.path(), Some(r#"{"results":[]}"#)),
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_analyze_socket(tmp.path(), Some(EMPTY_HOTSPOTS)),
    );
    let out = ground(
        &t,
        Path::new("/w/repos/acme-api"),
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;
    assert!(out.priorities.is_empty());
    assert!(
        out.gaps
            .iter()
            .any(|g| g.contains("matched no evidence for any due-diligence dimension")),
        "{:?}",
        out.gaps
    );
    assert!(
        out.gaps.iter().any(|g| g.contains("path name alone")),
        "the degradation to the pre-#6082 behaviour is named: {:?}",
        out.gaps
    );
}

/// A search leg that errors on every query names the failure once, with a
/// count — not once per query.
#[cfg(unix)]
#[tokio::test]
async fn failing_evidence_queries_are_named_once_with_their_count() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let t = tools(
        approving_search(tmp.path()),
        stub_search_socket(tmp.path(), None), // healthy; refuses every query
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_analyze_socket(tmp.path(), Some(EMPTY_HOTSPOTS)),
    );
    let out = ground(
        &t,
        Path::new("/w/repos/acme-api"),
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;
    let failures: Vec<&String> = out
        .gaps
        .iter()
        .filter(|g| g.contains("evidence queries failed"))
        .collect();
    assert_eq!(failures.len(), 1, "{:?}", out.gaps);
    // #6285: the refusal is a JSON-RPC error frame now, and the gap must still
    // carry the daemon's own words rather than a generic failure.
    assert!(failures[0].contains("no such index"), "{failures:?}");
}

/// A daemon that binds its socket and never becomes healthy gets the full
/// readiness budget and then a line — the second of `wait_socket_ready`'s two
/// phases.
#[cfg(unix)]
#[tokio::test]
async fn a_daemon_that_binds_but_never_answers_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let t = tools(
        approving_search(tmp.path()),
        unhealthy_search_socket(tmp.path()),
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead_socket(tmp.path()),
    );
    let out = ground(
        &t,
        Path::new("/w/repos/acme-api"),
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;
    assert!(out.priorities.is_empty());
    let gaps = without_churn_leg(out.gaps);
    assert_eq!(gaps.len(), 1, "{:?}", gaps);
    assert!(gaps[0].contains("did not report healthy"), "{:?}", gaps);
}

// ─── The happy path, end to end ──────────────────────────────────────────────

/// Every leg succeeds: the ranking reaches the manifest, complexity first, with
/// the search-derived evidence interleaved and each entry naming why it is
/// there. No gap is stated.
#[cfg(unix)]
#[tokio::test]
async fn hotspots_and_search_hits_become_ranked_inspect_priority_in_the_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("repos").join("acme-api");
    std::fs::create_dir_all(&checkout).expect("mkdir checkout");
    let manifest = manifest_naming(tmp.path(), &checkout);
    let payload: &'static str = Box::leak(
        HOTSPOTS
            .replace("CHECKOUT", &checkout.display().to_string())
            .into_boxed_str(),
    );

    let t = tools(
        approving_search(tmp.path()),
        stub_search_socket(tmp.path(), Some(SEARCH_HITS)),
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_analyze_socket(tmp.path(), Some(payload)),
    );
    let gaps = ground_manifest(
        &manifest,
        &t,
        &checkout,
        "acme-api",
        priority::Budget::from_env(),
    )
    .await;
    assert!(without_churn_leg(without_secrets_leg(gaps)).is_empty());

    let written = std::fs::read_to_string(&manifest).expect("read back");
    let parsed: toml::Value = toml::from_str(&written).expect("still valid TOML");
    let declared = parsed["repositories"].as_array().expect("array")[0]["inspect_priority"]
        .as_array()
        .expect("a ranking was declared")
        .clone();

    let paths: Vec<&str> = declared
        .iter()
        .map(|v| match v {
            toml::Value::String(path) => path.as_str(),
            table => table["path"].as_str().expect("a path"),
        })
        .collect();
    assert_eq!(paths[0], "src/pay.rs", "complexity still leads: {written}");
    assert!(
        paths.contains(&"src/login.rs"),
        "the search-derived evidence is ranked too: {written}"
    );

    let attributed = declared
        .iter()
        .find(|v| v.get("dimension").is_some())
        .expect("at least one entry names its dimension");
    assert_eq!(
        attributed["dimension"].as_str(),
        Some("authentication & secrets"),
        "{written}"
    );
    assert!(
        attributed["reason"]
            .as_str()
            .expect("a reason")
            .contains("trusty-search hit for"),
        "{written}"
    );
    // #6145: the winning chunk's own name, range and cyclomatic count survive
    // the collapse to files and reach the manifest trusty-review reads. The
    // losing pay.rs chunk (`refund`, rank 2) must not be the one recorded.
    let hotspot = &declared[0]["hotspot"];
    assert_eq!(
        hotspot["function"].as_str(),
        Some("settle_invoice"),
        "{written}"
    );
    assert_eq!(hotspot["start_line"].as_integer(), Some(1), "{written}");
    assert_eq!(hotspot["end_line"].as_integer(), Some(9), "{written}");
    assert_eq!(hotspot["cyclomatic"].as_integer(), Some(31), "{written}");
    assert!(
        declared
            .iter()
            .filter(|entry| entry.get("path").and_then(toml::Value::as_str) == Some("src/login.rs"))
            .all(|entry| entry.get("hotspot").is_none()),
        "a search-only entry carries no measurement: {written}"
    );

    // #6082: the audit's investigation budget rides on the same interface.
    assert_eq!(
        parsed["report"]["investigate_max_files"].as_integer(),
        Some(priority::DEFAULT_MAX_FILES as i64),
        "{written}"
    );
}

/// #6082: a search leg that answered nothing still says so when the tree has a
/// build manifest to backfill the dependencies dimension with.
///
/// Why this is the regression: `quality::lead_with_manifests` ran BEFORE
/// `discovery_gaps` read the dimension list, and it finds `Cargo.toml` in every
/// Rust repository — so an empty search produced a non-empty `dimensions`, the
/// "matched no evidence" line never fired, and `attributed` was computed from
/// the backfilled list and came out TRUE. Against that code both assertions
/// below fail, and the manifest declares `attributed_only = true` over a sample
/// that has no search evidence in it at all. That combination shipped on
/// 2026-08-22: a report whose every examined file said "path-name heuristic",
/// with nothing in Gaps & Caveats saying why.
#[cfg(unix)]
#[tokio::test]
async fn a_search_that_found_nothing_is_a_gap_even_when_a_manifest_backfills_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("repos").join("acme-api");
    std::fs::create_dir_all(&checkout).expect("mkdir checkout");
    std::fs::write(checkout.join("Cargo.toml"), "[package]\nname = \"acme\"\n").expect("manifest");

    let t = tools(
        approving_search(tmp.path()),
        stub_search_socket(tmp.path(), Some(r#"{"results":[]}"#)),
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_analyze_socket(tmp.path(), Some(EMPTY_HOTSPOTS)),
    );
    let out = ground(
        &t,
        &checkout,
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;

    assert!(
        !out.priorities.is_empty(),
        "the enumerated build manifest is still ranked"
    );
    assert!(
        out.gaps
            .iter()
            .any(|g| g.contains("matched no evidence for any due-diligence dimension")),
        "the backfill must not swallow the search leg's silence: {:?}",
        out.gaps
    );
    assert!(
        !out.attributed,
        "a dimension the enumeration created is not search-derived evidence, so the ranking may \
         not decline trusty-review's heuristic top-up"
    );
}

/// #6082: a ranking written into a manifest that has already been rendered from
/// says so, and names the recovery.
///
/// Why this is the regression: `tga audit` writes the manifest and runs
/// `trusty-review report` against it inside one process, and `crate::run`
/// grounds that manifest only after the child exits. Against the pre-fix code
/// this degradation had no line anywhere — console, manifest or report — so the
/// 2026-08-22 run shipped 37 ranked paths beside an investigation that read none
/// of them and declared no gap.
#[cfg(unix)]
#[tokio::test]
async fn a_ranking_that_lands_after_the_render_says_so() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("repos").join("acme-api");
    std::fs::create_dir_all(&checkout).expect("mkdir checkout");
    let manifest = manifest_naming(tmp.path(), &checkout);
    let payload: &'static str = Box::leak(
        HOTSPOTS
            .replace("CHECKOUT", &checkout.display().to_string())
            .into_boxed_str(),
    );
    let stubs = || async {
        tools(
            approving_search(tmp.path()),
            stub_search_socket(tmp.path(), Some(SEARCH_HITS)),
            PathBuf::from("/nonexistent/trusty-analyze"),
            stub_analyze_socket(tmp.path(), Some(payload)),
        )
    };

    // No snapshot beside the manifest: nothing has rendered it yet, and the
    // ordinary path says nothing.
    let quiet = ground_manifest(
        &manifest,
        &stubs().await,
        &checkout,
        "acme-api",
        priority::Budget::from_env(),
    )
    .await;
    assert!(
        !quiet.iter().any(|g| g.contains("already been rendered")),
        "a manifest nobody has rendered is not a degradation: {quiet:?}"
    );

    // The snapshot trusty-review writes when it investigates a manifest.
    std::fs::write(tmp.path().join("investigation.json"), "{}").expect("snapshot");
    let late = ground_manifest(
        &manifest,
        &stubs().await,
        &checkout,
        "acme-api",
        priority::Budget::from_env(),
    )
    .await;
    let named = late
        .iter()
        .find(|g| g.contains("already been rendered"))
        .unwrap_or_else(|| panic!("the degradation must be named: {late:?}"));
    assert!(
        named.contains("taudit render"),
        "and must name the recovery: {named}"
    );

    // The line is for the operator only — a re-render DOES read the ranking the
    // manifest now carries, so stating it there would be false.
    let written = std::fs::read_to_string(&manifest).expect("read back");
    let parsed: toml::Value = toml::from_str(&written).expect("still valid TOML");
    let declared: Vec<&str> = parsed["report"]
        .get("gaps")
        .and_then(toml::Value::as_array)
        .map(|a| a.iter().filter_map(toml::Value::as_str).collect())
        .unwrap_or_default();
    assert!(
        !declared.iter().any(|g| g.contains("already been rendered")),
        "{written}"
    );
}

/// #6082: the brief the manifest declares becomes queries of its own — and a
/// manifest with no brief is the ordinary case, not a failure.
#[test]
fn the_brief_a_manifest_declares_is_read() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("repos").join("acme-api");
    let manifest = manifest_naming(tmp.path(), &checkout);
    assert!(instructions_from(&manifest).is_none(), "no brief declared");

    std::fs::write(tmp.path().join("brief.md"), "- Payment reconciliation\n").expect("brief");
    let text = std::fs::read_to_string(&manifest).expect("read");
    std::fs::write(
        &manifest,
        text.replace(
            "title = \"Acme\"",
            "title = \"Acme\"\ninstructions = \"brief.md\"",
        ),
    )
    .expect("declare the brief");

    let brief = instructions_from(&manifest).expect("the brief is read");
    assert!(brief.contains("Payment reconciliation"), "{brief}");
    assert_eq!(
        evidence::instruction_queries(Some(&brief)),
        vec!["Payment reconciliation".to_string()]
    );
}

/// The gaps left once the secrets leg's own line is removed, having checked it
/// spoke exactly once.
///
/// // #6077: unlike the CVE and license legs, the secrets scan reads no
/// dependency manifest, so it applies to EVERY checkout these fixtures build and
/// contributes one gap to each — "`gitleaks` is not installed" on a machine
/// without the binary, its clean-scan scope statement on one with it. WHICH of
/// the two depends on the machine; that there is exactly one does not, so these
/// tests assert the count and leave the wording to `secrets_tests`.
/// The gaps left once the churn leg's own line is removed, having checked it
/// spoke exactly once.
///
/// // #6079: the churn leg has no not-applicable arm — `local_repo` gives every
/// real checkout its history by `git clone`, so a fixture directory holding no
/// repository is an anomaly it names rather than a state it passes over in
/// silence. It therefore contributes exactly one line to every fixture here:
/// "holds no git repository" for a bare directory, an empty-window or
/// quiet-repository line for a real one. WHICH line depends on the fixture;
/// that there is exactly one does not, so these tests assert the count and
/// leave the wording to `churn::churn_tests`.
fn without_churn_leg(gaps: Vec<String>) -> Vec<String> {
    let marker = format!("{}:", churn::COLLECTOR);
    let (spoken, rest): (Vec<String>, Vec<String>) =
        gaps.into_iter().partition(|gap| gap.contains(&marker));
    assert_eq!(
        spoken.len(),
        1,
        "the churn leg speaks exactly once per repository: {spoken:?}"
    );
    rest
}

fn without_secrets_leg(gaps: Vec<String>) -> Vec<String> {
    let (spoken, rest): (Vec<String>, Vec<String>) = gaps
        .into_iter()
        .partition(|gap| gap.contains("secrets-scan:"));
    assert_eq!(
        spoken.len(),
        1,
        "the secrets leg speaks exactly once per repository: {spoken:?}"
    );
    rest
}

/// A gap produced by an earlier leg reaches the manifest too, so the RENDERED
/// report states it — not only the console output of the run that produced it.
#[cfg(unix)]
#[tokio::test]
async fn a_gap_is_recorded_in_the_manifest_the_renderer_reads() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("repos").join("acme-api");
    std::fs::create_dir_all(&checkout).expect("mkdir checkout");
    let manifest = manifest_naming(tmp.path(), &checkout);
    let tmp = tempfile::tempdir().expect("tempdir");
    let t = tools(
        PathBuf::from("/nonexistent/trusty-search"),
        dead_search_socket(tmp.path()),
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead_socket(tmp.path()),
    );
    let gaps = ground_manifest(
        &manifest,
        &t,
        &checkout,
        "acme-api",
        priority::Budget::from_env(),
    )
    .await;
    let gaps = without_churn_leg(without_secrets_leg(gaps));
    assert_eq!(gaps.len(), 1, "{gaps:?}");

    let written = std::fs::read_to_string(&manifest).expect("read back");
    let parsed: toml::Value = toml::from_str(&written).expect("still valid TOML");
    let stated: Vec<&str> = parsed["report"]["gaps"]
        .as_array()
        .expect("gaps")
        .iter()
        .map(|gap| gap.as_str().expect("string"))
        .collect();
    // The secrets leg's line is written through the SECOND `priority::write_into`
    // call, so both legs' gaps reach the key the renderer reads (#6077), and
    // #6079's churn line rides the same key.
    assert_eq!(stated.len(), 3, "{written}");
    assert!(
        stated.iter().any(|gap| gap.contains("trusty-search")),
        "{written}"
    );
    assert!(
        stated.iter().any(|gap| gap.contains("secrets-scan:")),
        "{written}"
    );
    // #6079: a checkout holding no repository reaches the RENDERER as a gap.
    // Stated anywhere short of this key, the report's Change Hotspots section is
    // empty with nothing saying why.
    assert!(
        stated
            .iter()
            .any(|gap| gap.contains(churn::COLLECTOR) && gap.contains("holds no")),
        "{written}"
    );
}

/// The manifest is the interface, so failing to write it is itself a gap — the
/// run must not report a ranking that reached no file.
#[cfg(unix)]
#[tokio::test]
async fn a_manifest_that_cannot_be_written_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("repos").join("acme-api");
    std::fs::create_dir_all(&checkout).expect("mkdir checkout");
    // A directory where the manifest should be: every read of it fails.
    let manifest = tmp.path().join("manifest.toml");
    std::fs::create_dir(&manifest).expect("mkdir in place of the manifest");
    let payload: &'static str = Box::leak(
        HOTSPOTS
            .replace("CHECKOUT", &checkout.display().to_string())
            .into_boxed_str(),
    );

    let t = tools(
        approving_search(tmp.path()),
        stub_search_socket(tmp.path(), Some(SEARCH_HITS)),
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_analyze_socket(tmp.path(), Some(payload)),
    );
    let gaps = ground_manifest(
        &manifest,
        &t,
        &checkout,
        "acme-api",
        priority::Budget::from_env(),
    )
    .await;
    let gaps = without_churn_leg(without_secrets_leg(gaps));
    // Two writes are attempted — the ranking, then the manifest legs' gaps — and
    // a directory in place of the manifest fails both (#6077).
    assert_eq!(gaps.len(), 2, "{gaps:?}");
    assert!(gaps.iter().all(|gap| gap.contains("acme-api")), "{gaps:?}");
    assert!(
        gaps.iter().all(|gap| gap.contains("does not state")),
        "{gaps:?}"
    );
    assert!(
        gaps[1].contains("secrets scan, and change hotspots are missing"),
        "the second names every leg whose reason went unwritten: {gaps:?}"
    );
}

// ─── Resolution ──────────────────────────────────────────────────────────────

#[test]
fn pinned_tools_keep_the_paths_they_were_given() {
    let t = Tools::pinned(PathBuf::from("/w/tools/s"), PathBuf::from("/w/tools/a"));
    assert_eq!(t.search, PathBuf::from("/w/tools/s"));
    assert_eq!(t.analyze, PathBuf::from("/w/tools/a"));
    assert_eq!(t.startup_timeout, daemons::STARTUP_TIMEOUT);
}

/// A recipient who ran the engagement re-renders with the binaries that
/// produced the reports, without having to say so.
#[test]
fn the_installed_copies_are_preferred_over_the_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let work = WorkDir::new(tmp.path().join("work"));
    work.create().expect("the working directory is created");
    let installed = RequiredTool::TrustySearch.path_in(&work);
    std::fs::write(&installed, "").expect("place a copy under tools/");

    let t = Tools::resolved(&work);
    assert_eq!(t.search, installed);
    // Nothing is installed for analyze, so it falls through to the override or
    // the bare name. Which of those applies depends on the ambient environment,
    // so what is asserted is only that it did NOT resolve to the tools/ copy.
    assert_ne!(t.analyze, RequiredTool::TrustyAnalyze.path_in(&work));
}

// ─── #6783: a 409 from the create route is resolved, never skipped ───────────

/// A socket that records what it was asked and answers the conflict-resolution
/// pair (#6783).
///
/// `registry` is the `search.indexes.list` result; `search.index.delete` always
/// succeeds and `search.index.status` reports `root`, so the root backstop
/// passes once the collision has been cleared.
fn recording_search_socket(
    dir: &Path,
    registry: serde_json::Value,
    root: &Path,
) -> (PathBuf, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&seen);
    let root = root.display().to_string();
    let socket = answering_socket(dir, "recording-search", move |request| {
        sink.lock()
            .expect("the stub log is not poisoned")
            .push(request.to_owned());
        if request.contains("search.health") {
            r#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok","version":"0.0.0","indexes":1}}"#
                .to_owned()
        } else if request.contains("search.indexes.list") {
            format!(r#"{{"jsonrpc":"2.0","id":1,"result":{registry}}}"#)
        } else if request.contains("search.index.delete") {
            r#"{"jsonrpc":"2.0","id":1,"result":{"deleted":true}}"#.to_owned()
        } else if request.contains("search.index.status") {
            format!(r#"{{"jsonrpc":"2.0","id":1,"result":{{"root_path":"{root}"}}}}"#)
        } else {
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32004,"message":"no such index"}}"#
                .to_owned()
        }
    });
    (socket, seen)
}

/// A `trusty-search` whose `index` verb 409s once and then succeeds.
///
/// `index add` always approves and `index-status` always reports the index as
/// unserved, so every call reaches the create. The marker file is what makes the
/// second create succeed — the shape of a stale registration having been cleared
/// between the two attempts.
#[cfg(unix)]
fn search_that_conflicts_once(at: &Path, marker: &Path) -> PathBuf {
    stub_binary(
        at,
        "trusty-search",
        &format!(
            "#!/bin/sh\n\
             if [ \"$1\" = 'index-status' ]; then exit 1; fi\n\
             if [ \"$2\" = 'add' ]; then exit 0; fi\n\
             if [ -f '{marker}' ]; then exit 0; fi\n\
             : > '{marker}'\n\
             echo 'daemon returned 409 Conflict for POST /indexes' >&2\n\
             exit 1\n",
            marker = marker.display()
        ),
    )
}

/// A `trusty-search` whose `index` verb always fails, with `reason` on stderr.
#[cfg(unix)]
fn search_that_always_fails(at: &Path, reason: &str) -> PathBuf {
    stub_binary(
        at,
        "trusty-search",
        &format!(
            "#!/bin/sh\n\
             if [ \"$1\" = 'index-status' ]; then exit 1; fi\n\
             if [ \"$2\" = 'add' ]; then exit 0; fi\n\
             echo '{reason}' >&2\n\
             exit 1\n"
        ),
    )
}

/// Tools pointing at one stub binary and one recording socket.
#[cfg(unix)]
fn conflict_tools(search: PathBuf, socket: PathBuf, tmp: &Path) -> Tools {
    tools(
        search,
        socket,
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead_socket(tmp),
    )
}

/// #6783's headline case, and the one a client run hit 59 times: #6149 changed
/// the id derivation, so an earlier run's row owns this tree under its old
/// basename id. The row is dropped and the create retried — the tier survives.
#[cfg(unix)]
#[tokio::test]
async fn a_409_registration_conflict_clears_the_stale_row_and_retries() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("repos").join("acme-api");
    std::fs::create_dir_all(&checkout).expect("create the checkout");
    let search = search_that_conflicts_once(tmp.path(), &tmp.path().join("conflicted"));
    // The stale row: the SAME tree, registered under the pre-#6149 basename id.
    let registry = serde_json::json!({
        "indexes": [{ "id": "acme-api", "root_path": checkout.display().to_string() }]
    });
    let (socket, seen) = recording_search_socket(tmp.path(), registry, &checkout);
    let t = conflict_tools(search, socket, tmp.path());

    let out = ground(
        &t,
        &checkout,
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;

    assert!(
        !search_tier_degraded(&out.gaps),
        "the collision was resolvable, so nothing may report the tier as lost: {:?}",
        out.gaps
    );
    let asked = seen.lock().expect("the stub log is not poisoned").clone();
    assert!(
        asked.iter().any(|r| r.contains("search.indexes.list")),
        "the registry must be read before anything is deleted: {asked:?}"
    );
    let deleted: Vec<&String> = asked
        .iter()
        .filter(|r| r.contains("search.index.delete"))
        .collect();
    assert_eq!(deleted.len(), 1, "exactly one stale row goes: {asked:?}");
    assert!(deleted[0].contains("\"acme-api\""), "{:?}", deleted[0]);
    assert!(
        deleted[0].contains("expected_root_path"),
        "the delete is guarded by the root it was decided on: {:?}",
        deleted[0]
    );
    assert!(
        !deleted[0].contains("delete_data"),
        "deregistration never destroys the corpus: {:?}",
        deleted[0]
    );
}

/// The other half of the fail-open contract: when the registry does not explain
/// the refusal there is nothing to clear, and the run says so in the headline
/// rather than shipping a report whose evidence tier is quietly empty.
#[cfg(unix)]
#[tokio::test]
async fn an_unresolvable_409_degrades_the_evidence_tier_out_loud() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("repos").join("acme-api");
    std::fs::create_dir_all(&checkout).expect("create the checkout");
    let search =
        search_that_always_fails(tmp.path(), "daemon returned 409 Conflict for POST /indexes");
    let (socket, seen) =
        recording_search_socket(tmp.path(), serde_json::json!({ "indexes": [] }), &checkout);
    let t = conflict_tools(search, socket, tmp.path());

    let out = ground(
        &t,
        &checkout,
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;

    let gaps = without_churn_leg(out.gaps);
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains(SEARCH_TIER_HEADLINE), "{:?}", gaps[0]);
    assert!(gaps[0].contains("acme-api"), "{:?}", gaps[0]);
    assert!(gaps[0].contains("409"), "{:?}", gaps[0]);
    assert!(
        gaps[0].contains("no trusty-analyze pass ran"),
        "{:?}",
        gaps[0]
    );
    assert_eq!(
        gaps[0].lines().count(),
        1,
        "must stay one line: {:?}",
        gaps[0]
    );
    let asked = seen.lock().expect("the stub log is not poisoned").clone();
    assert!(
        asked.iter().any(|r| r.contains("search.indexes.list")),
        "the resolution must have been attempted: {asked:?}"
    );
    assert!(
        !asked.iter().any(|r| r.contains("search.index.delete")),
        "a registry that explains nothing licenses no delete: {asked:?}"
    );
}

/// A refusal that is not a collision — an unapproved root, a dead embedder — is
/// not a stale row, so nothing is read and nothing is deleted on its account.
#[cfg(unix)]
#[tokio::test]
async fn a_non_conflict_index_failure_is_not_retried() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("repos").join("acme-api");
    std::fs::create_dir_all(&checkout).expect("create the checkout");
    let search = search_that_always_fails(tmp.path(), "indexing refused: root is not allowlisted");
    let registry = serde_json::json!({
        "indexes": [{ "id": "acme-api", "root_path": checkout.display().to_string() }]
    });
    let (socket, seen) = recording_search_socket(tmp.path(), registry, &checkout);
    let t = conflict_tools(search, socket, tmp.path());

    let out = ground(
        &t,
        &checkout,
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;

    let gaps = without_churn_leg(out.gaps);
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains("root is not allowlisted"), "{:?}", gaps[0]);
    let asked = seen.lock().expect("the stub log is not poisoned").clone();
    assert!(
        !asked.iter().any(|r| r.contains("search.indexes.list")),
        "only a collision is worth a registry read: {asked:?}"
    );
}

/// Every arm that loses the index leads with one phrase, because
/// `crate::index_report` counts on it and the report's gap section leads with
/// it. Four sentences that merely meant the same thing is what #6783 replaced.
#[test]
fn every_search_tier_gap_leads_with_the_headline() {
    let line = search_tier_gap("acme-api", "daemon returned 409 Conflict for POST /indexes");
    assert!(line.starts_with("acme-api: "), "{line}");
    assert!(line.contains(SEARCH_TIER_HEADLINE), "{line}");
    assert_eq!(line.lines().count(), 1, "must stay one line: {line}");
    assert!(search_tier_degraded(&[line]));
    assert!(!search_tier_degraded(&[
        "acme-api: trusty-analyze is unreachable".to_owned()
    ]));
    assert!(!search_tier_degraded(&[]));
}
