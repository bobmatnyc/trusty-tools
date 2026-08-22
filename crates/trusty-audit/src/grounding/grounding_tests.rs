//! One test per way grounding can degrade, and one for when it does not.
//!
//! Why a whole file: #6081's contract is that no leg of the code-analysis
//! chain may fail silently, and "fail-open" is only a promise if every arm of it
//! is exercised. Each test below drives ONE leg to failure against a stub
//! daemon or a stub binary and asserts that the reason names the repository and
//! survives into the caller's gap list — the shape a bare `Ok(())` return would
//! hide.
//!
//! Nothing here reaches a real daemon or a real binary: every address is a
//! stub server bound to an ephemeral port or a port with nothing on it, and
//! every binary is a shell script. Nothing reads or writes the process
//! environment either, which is what lets these run in parallel with the rest
//! of the suite.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

/// A one-connection-at-a-time HTTP stub standing in for a trusty-* daemon.
///
/// Answers 200 to anything whose path contains `health`, and `hotspots` (when
/// given) to anything containing `complexity_hotspots`. `None` answers 500 —
/// the reachable-but-broken daemon, which is a different failure from an
/// unreachable one and gets its own test.
async fn stub_daemon(hotspots: Option<&'static str>) -> String {
    stub_daemon_with(hotspots, None).await
}

/// [`stub_daemon`], plus an answer for #6082's `/indexes/{id}/search` queries.
///
/// Why a second constructor rather than a parameter on the first: the discovery
/// leg asks a dozen-plus queries per repository, and most tests here drive a
/// DIFFERENT leg to failure — they want the search leg to answer quietly,
/// without restating that at every call site.
async fn stub_daemon_with(hotspots: Option<&'static str>, search: Option<&'static str>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = [0u8; 2048];
            let read = {
                use tokio::io::AsyncReadExt as _;
                socket.read(&mut buffer).await.unwrap_or(0)
            };
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let response = if request.contains("health") {
                body(200, "text/plain", "ok")
            } else if request.contains("complexity_hotspots") {
                match hotspots {
                    Some(json) => body(200, "application/json", json),
                    None => body(500, "text/plain", "index is not loaded"),
                }
            } else if request.contains("/search") {
                match search {
                    Some(json) => body(200, "application/json", json),
                    None => body(404, "text/plain", "no such index"),
                }
            } else {
                body(404, "text/plain", "no")
            };
            use tokio::io::AsyncWriteExt as _;
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    format!("http://{addr}")
}

fn body(status: u16, content_type: &str, payload: &str) -> String {
    format!(
        "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    )
}

/// A listener that accepts connections and refuses every request.
///
/// The middle case `daemons::wait_ready`'s two budgets exist to separate: the
/// port IS bound, so this is a daemon that is starting rather than absent.
async fn listening_but_unhealthy() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            use tokio::io::AsyncWriteExt as _;
            let _ = socket
                .write_all(body(503, "text/plain", "warming up").as_bytes())
                .await;
            let _ = socket.shutdown().await;
        }
    });
    format!("http://{addr}")
}

/// An address with nothing listening on it: bound, read back, then released.
async fn dead_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    format!("http://{addr}")
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
fn tools(search: PathBuf, search_url: String, analyze: PathBuf, analyze_url: String) -> Tools {
    Tools {
        search,
        search_url,
        analyze,
        analyze_url,
        bind_timeout: Duration::from_millis(60),
        startup_timeout: Duration::from_millis(150),
        poll_interval: Duration::from_millis(20),
    }
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

/// What #6082's `/indexes/{id}/search` answers with. The stub cannot vary its
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
    let dead = dead_url().await;
    let t = tools(
        PathBuf::from("/nonexistent/trusty-search"),
        dead.clone(),
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead,
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
    assert_eq!(out.gaps.len(), 1, "{:?}", out.gaps);
    assert!(out.gaps[0].contains("acme-api"), "{:?}", out.gaps);
    assert!(
        out.gaps[0].contains("no final path component"),
        "{:?}",
        out.gaps
    );
}

/// #6081's headline case: the search daemon is the first link, and losing it
/// must produce a line rather than an empty code-analysis section.
#[tokio::test]
async fn a_search_daemon_that_will_not_start_is_a_named_gap() {
    let dead = dead_url().await;
    let t = tools(
        PathBuf::from("/nonexistent/trusty-search"),
        dead.clone(),
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead,
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
    assert_eq!(out.gaps.len(), 1, "{:?}", out.gaps);
    assert!(out.gaps[0].contains("acme-api"), "{:?}", out.gaps);
    assert!(out.gaps[0].contains("trusty-search"), "{:?}", out.gaps);
    assert!(out.gaps[0].contains("not assessed"), "{:?}", out.gaps);
}

/// A daemon that is already answering is not restarted, so a resumed sweep and
/// a machine with a running instance both cost one probe.
#[cfg(unix)]
#[tokio::test]
async fn a_reachable_search_daemon_is_not_restarted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let search_url = stub_daemon(None).await;
    // The binary does not exist: reaching the spawn at all would fail the leg.
    let t = tools(
        PathBuf::from("/nonexistent/trusty-search"),
        search_url,
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead_url().await,
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
        stub_daemon(None).await,
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead_url().await,
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
    assert_eq!(out.gaps.len(), 1, "{:?}", out.gaps);
    assert!(out.gaps[0].contains("acme-api"), "{:?}", out.gaps);
    assert!(out.gaps[0].contains("not allowlisted"), "{:?}", out.gaps);
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
        stub_daemon_with(None, Some(SEARCH_HITS)).await,
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead_url().await,
    );
    let out = ground(
        &t,
        Path::new("/w/repos/acme-api"),
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;
    assert_eq!(out.gaps.len(), 1, "{:?}", out.gaps);
    assert!(out.gaps[0].contains("acme-api"), "{:?}", out.gaps);
    assert!(out.gaps[0].contains("trusty-analyze"), "{:?}", out.gaps);
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

/// Reachable but broken: the daemon answers `/health` and then 500s the query.
/// That is a different failure from an unreachable daemon and gets its own line.
#[cfg(unix)]
#[tokio::test]
async fn an_unreachable_hotspots_endpoint_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let t = tools(
        approving_search(tmp.path()),
        stub_daemon_with(None, Some(SEARCH_HITS)).await,
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_daemon(None).await,
    );
    let out = ground(
        &t,
        Path::new("/w/repos/acme-api"),
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;
    assert_eq!(out.gaps.len(), 1, "{:?}", out.gaps);
    assert!(out.gaps[0].contains("500"), "{:?}", out.gaps);
    assert!(
        out.gaps[0].contains("search-derived evidence only"),
        "{:?}",
        out.gaps
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
        stub_daemon_with(None, Some(SEARCH_HITS)).await,
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_daemon(Some(EMPTY_HOTSPOTS)).await,
    );
    let out = ground(
        &t,
        Path::new("/w/repos/acme-api"),
        "acme-api",
        None,
        priority::Budget::from_env(),
    )
    .await;
    assert_eq!(out.gaps.len(), 1, "{:?}", out.gaps);
    assert!(
        out.gaps[0].contains("no complexity hotspot"),
        "{:?}",
        out.gaps
    );
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
        stub_daemon_with(None, Some(r#"{"results":[]}"#)).await,
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_daemon(Some(EMPTY_HOTSPOTS)).await,
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
        stub_daemon(None).await, // answers /health, 404s every query
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_daemon(Some(EMPTY_HOTSPOTS)).await,
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
    assert!(failures[0].contains("404"), "{failures:?}");
}

/// A daemon that binds its port and never becomes healthy gets the full
/// readiness budget and then a line — the second of `wait_ready`'s two phases.
#[cfg(unix)]
#[tokio::test]
async fn a_daemon_that_binds_but_never_answers_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let t = tools(
        approving_search(tmp.path()),
        listening_but_unhealthy().await,
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead_url().await,
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
    assert_eq!(out.gaps.len(), 1, "{:?}", out.gaps);
    assert!(
        out.gaps[0].contains("did not answer /health"),
        "{:?}",
        out.gaps
    );
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
        stub_daemon_with(None, Some(SEARCH_HITS)).await,
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_daemon(Some(payload)).await,
    );
    let gaps = ground_manifest(&manifest, &t, &checkout, "acme-api").await;
    assert!(gaps.is_empty(), "{gaps:?}");

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
        stub_daemon_with(None, Some(r#"{"results":[]}"#)).await,
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_daemon(Some(EMPTY_HOTSPOTS)).await,
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
            stub_daemon_with(None, Some(SEARCH_HITS)).await,
            PathBuf::from("/nonexistent/trusty-analyze"),
            stub_daemon(Some(payload)).await,
        )
    };

    // No snapshot beside the manifest: nothing has rendered it yet, and the
    // ordinary path says nothing.
    let quiet = ground_manifest(&manifest, &stubs().await, &checkout, "acme-api").await;
    assert!(
        !quiet.iter().any(|g| g.contains("already been rendered")),
        "a manifest nobody has rendered is not a degradation: {quiet:?}"
    );

    // The snapshot trusty-review writes when it investigates a manifest.
    std::fs::write(tmp.path().join("investigation.json"), "{}").expect("snapshot");
    let late = ground_manifest(&manifest, &stubs().await, &checkout, "acme-api").await;
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

/// A gap produced by an earlier leg reaches the manifest too, so the RENDERED
/// report states it — not only the console output of the run that produced it.
#[cfg(unix)]
#[tokio::test]
async fn a_gap_is_recorded_in_the_manifest_the_renderer_reads() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("repos").join("acme-api");
    std::fs::create_dir_all(&checkout).expect("mkdir checkout");
    let manifest = manifest_naming(tmp.path(), &checkout);
    let dead = dead_url().await;
    let t = tools(
        PathBuf::from("/nonexistent/trusty-search"),
        dead.clone(),
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead,
    );
    let gaps = ground_manifest(&manifest, &t, &checkout, "acme-api").await;
    assert_eq!(gaps.len(), 1, "{gaps:?}");

    let written = std::fs::read_to_string(&manifest).expect("read back");
    let parsed: toml::Value = toml::from_str(&written).expect("still valid TOML");
    let stated = parsed["report"]["gaps"].as_array().expect("gaps");
    assert_eq!(stated.len(), 1, "{written}");
    assert!(
        stated[0]
            .as_str()
            .expect("string")
            .contains("trusty-search"),
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
        stub_daemon_with(None, Some(SEARCH_HITS)).await,
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_daemon(Some(payload)).await,
    );
    let gaps = ground_manifest(&manifest, &t, &checkout, "acme-api").await;
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].contains("acme-api"), "{gaps:?}");
    assert!(gaps[0].contains("does not state"), "{gaps:?}");
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
