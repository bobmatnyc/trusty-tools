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
     "cyclomatic":31,"cognitive":40},
    {"id":"b","file":"CHECKOUT/src/pay.rs","start_line":20,"end_line":30,"content":"",
     "cyclomatic":18,"cognitive":22},
    {"id":"c","file":"CHECKOUT/src/auth.rs","start_line":1,"end_line":9,"content":"",
     "cyclomatic":12,"cognitive":15}
]}"#;

const EMPTY_HOTSPOTS: &str = r#"{"index_id":"acme-api","top_n":60,"hotspots":[]}"#;

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
    let out = ground(&t, Path::new("/"), "acme-api").await;
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
    let out = ground(&t, Path::new("/w/repos/acme-api"), "acme-api").await;
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
    let out = ground(&t, Path::new("/w/repos/acme-api"), "acme-api").await;
    assert!(out.priorities.is_empty());
    assert_eq!(out.index_id.as_deref(), Some("acme-api"));
    assert_eq!(out.gaps.len(), 1, "{:?}", out.gaps);
    assert!(out.gaps[0].contains("acme-api"), "{:?}", out.gaps);
    assert!(out.gaps[0].contains("not allowlisted"), "{:?}", out.gaps);
}

/// The second daemon. It is reached only once the index exists, and its loss is
/// stated as itself rather than as an empty hotspot list.
#[cfg(unix)]
#[tokio::test]
async fn an_analyze_daemon_that_will_not_start_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let t = tools(
        approving_search(tmp.path()),
        stub_daemon(None).await,
        PathBuf::from("/nonexistent/trusty-analyze"),
        dead_url().await,
    );
    let out = ground(&t, Path::new("/w/repos/acme-api"), "acme-api").await;
    assert!(out.priorities.is_empty());
    assert_eq!(out.gaps.len(), 1, "{:?}", out.gaps);
    assert!(out.gaps[0].contains("acme-api"), "{:?}", out.gaps);
    assert!(out.gaps[0].contains("trusty-analyze"), "{:?}", out.gaps);
}

/// Reachable but broken: the daemon answers `/health` and then 500s the query.
/// That is a different failure from an unreachable daemon and gets its own line.
#[cfg(unix)]
#[tokio::test]
async fn an_unreachable_hotspots_endpoint_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let t = tools(
        approving_search(tmp.path()),
        stub_daemon(None).await,
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_daemon(None).await,
    );
    let out = ground(&t, Path::new("/w/repos/acme-api"), "acme-api").await;
    assert!(out.priorities.is_empty());
    assert_eq!(out.gaps.len(), 1, "{:?}", out.gaps);
    assert!(out.gaps[0].contains("500"), "{:?}", out.gaps);
    assert!(out.gaps[0].contains("path name alone"), "{:?}", out.gaps);
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
        stub_daemon(None).await,
        PathBuf::from("/nonexistent/trusty-analyze"),
        stub_daemon(Some(EMPTY_HOTSPOTS)).await,
    );
    let out = ground(&t, Path::new("/w/repos/acme-api"), "acme-api").await;
    assert!(out.priorities.is_empty());
    assert_eq!(out.gaps.len(), 1, "{:?}", out.gaps);
    assert!(
        out.gaps[0].contains("no complexity hotspot"),
        "{:?}",
        out.gaps
    );
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
    let out = ground(&t, Path::new("/w/repos/acme-api"), "acme-api").await;
    assert!(out.priorities.is_empty());
    assert_eq!(out.gaps.len(), 1, "{:?}", out.gaps);
    assert!(
        out.gaps[0].contains("did not answer /health"),
        "{:?}",
        out.gaps
    );
}

// ─── The happy path, end to end ──────────────────────────────────────────────

/// Every leg succeeds: the ranking reaches the manifest, in complexity order,
/// with one entry per file, and no gap is stated.
#[cfg(unix)]
#[tokio::test]
async fn hotspots_become_ranked_inspect_priority_in_the_manifest() {
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
        stub_daemon(None).await,
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
        .iter()
        .map(|v| v.as_str().expect("string"))
        .collect::<Vec<_>>();
    assert_eq!(declared, vec!["src/pay.rs", "src/auth.rs"], "{written}");
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
        stub_daemon(None).await,
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
