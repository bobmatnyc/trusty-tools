//! Tests for working-directory index scoping (#5264).
//!
//! Why: this tier decides the index EVERY project-scoped MCP tool falls back to
//! — `search`, `grep`, `chat`, `index_status` and the rest all resolve through
//! the session pin — so a wrong pin is a wrong answer on every call in the
//! session, reported as success. The refusal branches carry most of the risk
//! and get most of the assertions here.
//!
//! Hermetic by construction: the two tests that speak HTTP bind an ephemeral
//! port and serve their own fixture, so nothing here can reach the machine's
//! real trusty-search daemon on 7878.
//!
//! Test: `cargo test -p trusty-search --bin trusty-search scope_tests`

use crate::commands::serve_scope::*;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

/// A unique empty scratch directory for one test.
fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let p = std::env::temp_dir().join(format!(
        "trusty-serve-scope-{tag}-{}-{nanos}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).expect("create scratch dir");
    p
}

/// Make `dir` look like a git repository root.
fn git_init(dir: &Path) {
    fs::create_dir_all(dir.join(".git")).expect("create .git");
}

fn entry(id: &str, root: &str) -> DaemonIndex {
    DaemonIndex {
        id: id.to_string(),
        root_path: Some(PathBuf::from(root)),
    }
}

// ---------------------------------------------------------------------------
// Candidate derivation
// ---------------------------------------------------------------------------

/// Why: the derived id must match what trusty-mpm's register-and-pin derives
/// for the same tree (#1373), which means resolving to the git root rather than
/// the directory the process happens to sit in.
#[test]
fn cwd_candidate_uses_git_root() {
    let root = scratch("gitroot");
    git_init(&root);
    let nested = root.join("crates").join("deep");
    fs::create_dir_all(&nested).expect("create nested dir");

    let c = derive_cwd_candidate(&nested).expect("a git root yields a candidate");
    assert_eq!(
        c.project_root, root,
        "derivation must walk up to the git root, not stop at the cwd"
    );
    assert_eq!(
        c.index_id,
        root.file_name().unwrap().to_string_lossy(),
        "the index id is the git root's basename"
    );
    let _ = fs::remove_dir_all(&root);
}

/// Why: a directory outside any repository is still indexable by its own
/// basename, and the CLI already treats it that way.
#[test]
fn cwd_candidate_falls_back_to_basename() {
    let dir = scratch("norepo");
    let c = derive_cwd_candidate(&dir).expect("a plain directory still yields a candidate");
    assert_eq!(c.project_root, dir);
    assert_eq!(c.index_id, dir.file_name().unwrap().to_string_lossy());
    let _ = fs::remove_dir_all(&dir);
}

/// Why: `derive_index_id` documents an empty string for a filesystem-root path.
/// An empty index id addresses nothing, so it must never reach the pin.
#[test]
fn cwd_candidate_none_for_root_path() {
    assert_eq!(
        derive_cwd_candidate(Path::new("/")),
        None,
        "the filesystem root derives an empty id and must yield no candidate"
    );
}

// ---------------------------------------------------------------------------
// Confirmation — the fail-open guard
// ---------------------------------------------------------------------------

/// Why: the ordinary success path. The daemon serves this id from exactly the
/// root the id was derived from, so the pin is safe.
#[test]
fn confirm_accepts_matching_root() {
    let c = CwdCandidate {
        index_id: "api".into(),
        project_root: PathBuf::from("/work/acme/api"),
    };
    assert_eq!(
        confirm_candidate(&c, &[entry("api", "/work/acme/api")]),
        Confirmation::Confirmed
    );
}

/// Why (#5264, the fail-open branch): `derive_index_id` is a bare path
/// basename, so two unrelated checkouts both named `api` derive the SAME index
/// id. Matching on the id alone would pin this session to the other project's
/// index and then answer every `search` from that project's code — a session
/// that looks healthy and is wrong on every call. Comparing the daemon's
/// `root_path` is what turns that collision into a refusal.
///
/// This test fails against an existence-only check, which is the exact
/// regression it exists to prevent.
#[test]
fn confirm_rejects_colliding_basename() {
    let c = CwdCandidate {
        index_id: "api".into(),
        project_root: PathBuf::from("/work/acme/api"),
    };
    // The daemon serves an index called `api`, but it is a DIFFERENT project.
    let verdict = confirm_candidate(&c, &[entry("api", "/work/globex/api")]);
    assert_eq!(
        verdict,
        Confirmation::RootMismatch {
            serving_root: PathBuf::from("/work/globex/api")
        },
        "an id that matches while the root does not must be refused, not pinned"
    );
    assert_ne!(
        verdict,
        Confirmation::Confirmed,
        "matching on the index id alone is the wrong-index defect this guards"
    );
}

/// Why: an unindexed working directory derives a plausible id that no index
/// backs. Pinning it would make every tool call fail against a phantom index.
#[test]
fn confirm_rejects_unknown_index() {
    let c = CwdCandidate {
        index_id: "never-indexed".into(),
        project_root: PathBuf::from("/work/never-indexed"),
    };
    assert_eq!(
        confirm_candidate(&c, &[entry("api", "/work/acme/api")]),
        Confirmation::NotServed
    );
}

/// Why: a null `root_path` leaves identity unconfirmable. Fail closed — an
/// unverifiable candidate is exactly what must not be presented as confirmed.
#[test]
fn confirm_rejects_null_root() {
    let c = CwdCandidate {
        index_id: "api".into(),
        project_root: PathBuf::from("/work/acme/api"),
    };
    let served = DaemonIndex {
        id: "api".into(),
        root_path: None,
    };
    assert_eq!(
        confirm_candidate(&c, &[served]),
        Confirmation::RootUnknown,
        "an index with no reported root cannot confirm identity"
    );
}

/// Why: a git worktree is routinely reached through a symlink, and on macOS the
/// same directory is reachable as both `/tmp/x` and `/private/tmp/x`. A byte
/// comparison would refuse a pin that is in fact correct, so the check
/// canonicalizes before deciding.
#[cfg(unix)]
#[test]
fn confirm_accepts_symlinked_root() {
    let base = scratch("symlink");
    let real = base.join("real");
    let proj = real.join("proj");
    fs::create_dir_all(&proj).expect("create real project");
    git_init(&proj);
    let link = base.join("link");
    std::os::unix::fs::symlink(&real, &link).expect("create symlink");

    let via_link = link.join("proj");
    let c = derive_cwd_candidate(&via_link).expect("candidate through the symlink");
    assert_eq!(c.index_id, "proj");

    // The daemon reports the REAL path; the session reached it through a link.
    let served = DaemonIndex {
        id: "proj".into(),
        root_path: Some(proj.clone()),
    };
    assert_eq!(
        confirm_candidate(&c, &[served]),
        Confirmation::Confirmed,
        "the same directory reached through a symlink must confirm"
    );
    let _ = fs::remove_dir_all(&base);
}

// ---------------------------------------------------------------------------
// Decision + reporting
// ---------------------------------------------------------------------------

fn candidate() -> CwdCandidate {
    CwdCandidate {
        index_id: "api".into(),
        project_root: PathBuf::from("/work/acme/api"),
    }
}

/// Why: a confirmed candidate pins, and the report must name the working
/// directory as the source so an operator can tell it from an explicit flag.
#[test]
fn decide_pins_on_confirmation() {
    let c = candidate();
    let pin = decide_auto_pin(&c, Confirmation::Confirmed);
    let choice = pin.choice().expect("a confirmed candidate pins");
    assert_eq!(choice.index_id, "api");
    assert_eq!(choice.source, PinSource::WorkingDir);
    let report = pin.report();
    assert!(
        report.contains("api") && report.contains("working directory"),
        "the report must name both the index and its source; got {report:?}"
    );
}

/// Why: the refusal must leave the session unpinned AND say enough for the
/// operator to act — which directory, which id, and that explicit ids still
/// work. A silent refusal is the same defect as a silent wrong pin.
#[test]
fn decide_refuses_on_root_mismatch() {
    let c = candidate();
    let pin = decide_auto_pin(
        &c,
        Confirmation::RootMismatch {
            serving_root: PathBuf::from("/work/globex/api"),
        },
    );
    assert!(pin.choice().is_none(), "a mismatched root must not pin");
    let r = pin.report();
    assert!(
        r.contains("UNPINNED"),
        "report must say unpinned; got {r:?}"
    );
    assert!(
        r.contains("/work/globex/api"),
        "report must name the conflicting root so the collision is diagnosable; got {r:?}"
    );
    assert!(
        r.contains("index_id"),
        "report must state the remedy; got {r:?}"
    );
}

/// Why: an unindexed directory must not hard-fail a session that can still
/// serve explicit `index_id` calls — staying unpinned is what a bare `serve`
/// did before this tier existed, so refusing costs nothing that worked before.
#[test]
fn decide_refuses_when_not_served() {
    let pin = decide_auto_pin(&candidate(), Confirmation::NotServed);
    assert!(pin.choice().is_none());
    let r = pin.report();
    assert!(r.contains("UNPINNED") && r.contains("api"), "got {r:?}");
}

// ---------------------------------------------------------------------------
// Explicit flags outrank the working directory
// ---------------------------------------------------------------------------

/// Why: an explicit `--index` is the operator's direct instruction. The working
/// directory is a guess, and a guess must never outrank an instruction.
#[test]
fn explicit_index_resolves_with_flag_source() {
    let c = resolve_pinned_index(Some("chosen".into()), Some("/work/acme/api".into()))
        .expect("an explicit index resolves");
    assert_eq!(c.index_id, "chosen");
    assert_eq!(
        c.source,
        PinSource::Flag,
        "--index must win over --project and be reported as the flag"
    );
}

/// Why: `--project` is also explicit, and must be distinguishable from the
/// working-directory tier in the report even though both derive from a path.
#[test]
fn explicit_project_resolves_with_project_source() {
    let root = scratch("projflag");
    git_init(&root);
    let c = resolve_pinned_index(None, Some(root.to_string_lossy().into_owned()))
        .expect("an explicit project resolves");
    assert_eq!(c.index_id, root.file_name().unwrap().to_string_lossy());
    assert_eq!(c.source, PinSource::Project);
    let _ = fs::remove_dir_all(&root);
}

/// Why: with neither flag the explicit tier must yield nothing, so `serve`
/// falls through to the working-directory tier. A non-`None` here would skip
/// that tier entirely.
#[test]
fn no_flags_leaves_the_explicit_tier_empty() {
    assert_eq!(resolve_pinned_index(None, None), None);
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

#[test]
fn parse_entries_reads_id_and_root() {
    let body = json!({"indexes": [
        {"id": "api", "root_path": "/work/acme/api", "size_bytes": 12},
        {"id": "web", "root_path": "/work/acme/web"},
    ]});
    assert_eq!(
        parse_index_entries(&body),
        vec![
            entry("api", "/work/acme/api"),
            entry("web", "/work/acme/web")
        ]
    );
}

/// Why: an entry whose root is absent or non-string must survive as an entry
/// with no root — dropping it would report "not served" for an index the daemon
/// plainly serves, sending the operator to fix the wrong thing.
#[test]
fn parse_entries_tolerates_null_root() {
    let body = json!({"indexes": [{"id": "api", "root_path": null}]});
    let parsed = parse_index_entries(&body);
    assert_eq!(parsed.len(), 1, "the entry must survive");
    assert_eq!(parsed[0].id, "api");
    assert_eq!(parsed[0].root_path, None);
}

#[test]
fn parse_entries_empty_when_shape_unexpected() {
    assert!(parse_index_entries(&json!({})).is_empty());
    assert!(parse_index_entries(&json!({"indexes": "nope"})).is_empty());
}

// ---------------------------------------------------------------------------
// Transport — hermetic, ephemeral port only
// ---------------------------------------------------------------------------

/// Spawn a fixture daemon on an ephemeral port returning `body` for
/// `GET /indexes`. Never binds 7878, so the machine's real daemon is untouched.
async fn fixture_daemon(body: serde_json::Value) -> String {
    let app = axum::Router::new().route(
        "/indexes",
        axum::routing::get(move || {
            let body = body.clone();
            async move { axum::Json(body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// Why: proves the `?details=true` round-trip actually yields the `root_path`
/// the confirmation depends on, rather than only the flat id list.
#[tokio::test(flavor = "multi_thread")]
async fn fetch_reads_entries_from_a_live_server() {
    let base = fixture_daemon(json!({"indexes": [
        {"id": "api", "root_path": "/work/acme/api", "size_bytes": 1}
    ]}))
    .await;

    let entries = fetch_index_entries(&base).await.expect("fetch succeeds");
    assert_eq!(entries, vec![entry("api", "/work/acme/api")]);
}

/// Why: the end-to-end shape every project-scoped tool inherits. A real
/// directory, a real HTTP round-trip, and a pin that names its source.
#[tokio::test(flavor = "multi_thread")]
async fn auto_pin_confirms_against_a_live_server() {
    let root = scratch("e2e-ok");
    git_init(&root);
    let id = root.file_name().unwrap().to_string_lossy().into_owned();
    let base = fixture_daemon(json!({"indexes": [
        {"id": id, "root_path": root.to_string_lossy()}
    ]}))
    .await;

    let pin = auto_pin_from_cwd(&base, &root)
        .await
        .expect("a real directory yields a candidate");
    let choice = pin.choice().expect("matching root confirms the pin");
    assert_eq!(choice.index_id, id);
    assert_eq!(choice.source, PinSource::WorkingDir);
    let _ = fs::remove_dir_all(&root);
}

/// Why (#5264, fail-open): the whole-path version of
/// `confirm_rejects_colliding_basename`. The daemon serves an index with the
/// derived id from a DIFFERENT root; the session must come back unpinned.
#[tokio::test(flavor = "multi_thread")]
async fn auto_pin_refuses_a_colliding_index_end_to_end() {
    let root = scratch("e2e-collide");
    git_init(&root);
    let id = root.file_name().unwrap().to_string_lossy().into_owned();
    // Same id, different project.
    let base = fixture_daemon(json!({"indexes": [
        {"id": id, "root_path": "/somewhere/else/entirely"}
    ]}))
    .await;

    let pin = auto_pin_from_cwd(&base, &root).await.expect("a candidate");
    assert!(
        pin.choice().is_none(),
        "a same-id different-root index must leave the session unpinned"
    );
    assert!(
        pin.report().contains("/somewhere/else/entirely"),
        "the refusal must name the conflicting root; got {:?}",
        pin.report()
    );
    let _ = fs::remove_dir_all(&root);
}

/// Why: a daemon that cannot be listed leaves the candidate unconfirmable. The
/// session must stay unpinned and say so — never pin an unverified guess, and
/// never abort a session that can still serve explicit ids.
#[tokio::test(flavor = "multi_thread")]
async fn auto_pin_refuses_when_the_daemon_cannot_be_listed() {
    let root = scratch("e2e-nodaemon");
    git_init(&root);
    // Port 1 on loopback refuses immediately; no fixture is served here.
    let pin = auto_pin_from_cwd("http://127.0.0.1:1", &root)
        .await
        .expect("a candidate is still derived");
    assert!(
        pin.choice().is_none(),
        "an unreachable daemon must not produce a pin"
    );
    assert!(pin.report().contains("UNPINNED"), "got {:?}", pin.report());
    let _ = fs::remove_dir_all(&root);
}
