//! Getting one checkout into trusty-search, under the id the renderer looks up.
//!
//! Why: trusty-analyze answers from a trusty-search index, so a repository
//! nobody indexed has no complexity to measure and no findings to report. The
//! renderer's own membership check misses, fails open, and produces a report
//! whose code-analysis sections are empty over a clean exit — which reads as a
//! clean bill of health.
//!
//! ## The index id is a cross-process contract
//!
//! Three crates derive the id independently — this one when it indexes,
//! `trusty_review::report::analyze_adapter::derive_index_id` when the renderer
//! looks it up, `tga::audit::repo_index::index_id_for` from the sweep's other
//! side — and an id derived any other way indexes a repository under a name
//! nobody ever queries. Until #6149 the rule was the checkout BASENAME, copied
//! into all three; two checkouts of one repository therefore collided on one id
//! and the daemon served whichever tree registered first. All three now call
//! [`trusty_common::derive_checkout_index_id`], which hashes the canonical path
//! into the id, so the agreement is a call rather than a copy and two checkouts
//! can no longer collide.
//!
//! ## Two verbs, not one
//!
//! trusty-search is default-deny: `index add <path>` writes the allowlist row,
//! and only then does `index <path>` proceed. The approval is
//! [`crate::run::approve::approve_for_indexing`] rather than a second spawn here
//! — one implementation, one set of refusal rules, whichever leg reaches it
//! first.
//!
//! Test: `index_tests`, and `super::grounding_tests` for the live arms.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Output};

use crate::run::approve::approve_for_indexing;

/// What became of one repository's index.
///
/// Why: "already served" and "indexed by this run" are both silence to the
/// operator, but they are different facts to anyone reading a log — a sweep
/// that indexes nothing on its second pass is resuming, not skipping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IndexStatus {
    /// trusty-search already served this index; nothing was spawned.
    AlreadyServed,
    /// This call indexed the repository.
    Indexed,
}

/// The trusty-search index id for a checkout path.
///
/// Why/What/Test: see the module docs. A thin forward to
/// [`trusty_common::derive_checkout_index_id`], kept as a named function here
/// because the module's callers and tests read better against it and because it
/// is the seam a future scheme change moves. `None` for a path with no final
/// component, e.g. `/`.
/// Test: `index_tests::the_index_id_distinguishes_two_checkouts_of_one_repo`.
#[must_use]
pub fn index_id_for(path: &Path) -> Option<String> {
    trusty_common::derive_checkout_index_id(path)
}

/// Wall-clock budget for the index-root probe.
const ROOT_PROBE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// Refuse an index that is rooted at a DIFFERENT checkout than the audited one.
///
/// Why: this is the BACKSTOP, not the primary defence. The primary defence is
/// the id itself: since #6149 it hashes the canonical checkout path, so two
/// checkouts of one repository can no longer collide by construction. What is
/// left for this guard is everything a derivation cannot see — an id registered
/// under the old basename scheme, a directory that moved after it was indexed,
/// and a daemon whose registry disagrees with this machine's filesystem. The
/// graded self-audit of 2026-08-21 is what both defences answer: the audited
/// tree was the engagement's own clone, the daemon was already serving an index
/// of the same name rooted at `~/Projects/trusty-tools` at a different branch
/// and commit, and every complexity hotspot the report stamped "measured" was
/// measured against code that was never audited.
///
/// What: reads `root_path` off `GET /indexes/{id}/status` and compares it to
/// `checkout`, canonicalising both so a symlinked or `..`-laden path still
/// matches. A daemon that will not answer, or a response with no `root_path`,
/// is `Ok` — this guard exists to catch a WRONG root, and every other failure
/// already has a gap of its own; failing closed here would turn one unreadable
/// field into a repository with no code analysis at all.
///
/// # Errors
///
/// One line naming both roots when the served index is rooted elsewhere. The
/// caller turns it into a gap and degrades the legs that read the index.
///
/// # Postconditions
/// Never panics.
///
/// Test: `index_tests::{same_tree_resolves_before_it_compares,
/// an_unreachable_daemon_is_not_a_refusal}`.
pub async fn root_matches(base_url: &str, index_id: &str, checkout: &Path) -> Result<(), String> {
    let url = format!(
        "{}/indexes/{index_id}/status",
        base_url.trim_end_matches('/')
    );
    let Ok(client) = trusty_common::http_client::loopback_client_builder()
        .timeout(ROOT_PROBE_BUDGET)
        .build()
    else {
        return Ok(());
    };
    let Ok(response) = client.get(&url).send().await else {
        return Ok(());
    };
    if !response.status().is_success() {
        return Ok(());
    }
    let Ok(body) = response.json::<serde_json::Value>().await else {
        return Ok(());
    };
    let Some(served) = body.get("root_path").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    if same_tree(Path::new(served), checkout) {
        return Ok(());
    }
    Err(format!(
        "the trusty-search index '{index_id}' is rooted at {served}, not at {} — another checkout \
         is registered under this id, so its content would be reported as this one's",
        checkout.display()
    ))
}

/// True when two paths name the same tree, symlinks and `..` resolved.
///
/// Falls back to a separator-normalised string compare when either path cannot
/// be canonicalised, which is the case in tests and for a root the daemon holds
/// but this machine cannot stat.
fn same_tree(served: &Path, checkout: &Path) -> bool {
    match (served.canonicalize(), checkout.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => {
            let norm = |p: &Path| {
                p.to_string_lossy()
                    .replace('\\', "/")
                    .trim_end_matches('/')
                    .to_owned()
            };
            norm(served) == norm(checkout)
        }
    }
}

/// The membership probe's argument vector.
///
/// `index-status <id>` asks the daemon for `/indexes/<id>/status` and exits
/// non-zero on its 404 — the same question the renderer's own membership check
/// asks, so a repository that would satisfy the renderer is never re-indexed.
fn probe_args(index_id: &str) -> Vec<OsString> {
    vec!["index-status".into(), index_id.into()]
}

/// The indexing invocation's argument vector.
///
/// `--name` is what binds the index to the id the renderer will look up; without
/// it trusty-search names the index after whatever directory this process
/// happens to be running from.
fn index_args(path: &Path, index_id: &str) -> Vec<OsString> {
    vec![
        "index".into(),
        path.as_os_str().to_owned(),
        "--name".into(),
        index_id.into(),
    ]
}

/// Approve and index `checkout`, unless trusty-search already serves it.
///
/// # Errors
///
/// One line, safe to show the recipient, naming the checkout and quoting what
/// trusty-search said. The caller turns it into a gap; nothing here fails a run.
///
/// This is synchronous inside an async caller for the same reason
/// [`approve_for_indexing`] is: `trusty-search index` returns once the index
/// stops making progress rather than blocking forever, which bounds an
/// unattended run at the cost of the strongest claim — a detached index answers
/// the renderer's membership check while still filling, so its sections render
/// from whatever had been embedded by then.
///
/// Test: `index_tests::{a_served_index_is_not_rebuilt, an_index_failure_is_a_reason}`.
pub fn ensure_indexed(
    search: &Path,
    checkout: &Path,
    index_id: &str,
) -> Result<IndexStatus, String> {
    approve_for_indexing(search, checkout)?;
    if run(search, &probe_args(index_id))?.status.success() {
        return Ok(IndexStatus::AlreadyServed);
    }
    let output = run(search, &index_args(checkout, index_id))?;
    match refusal(checkout, index_id, &output) {
        Some(reason) => Err(reason),
        None => Ok(IndexStatus::Indexed),
    }
}

/// Run the binary, turning an unspawnable one into a reason rather than a panic.
fn run(search: &Path, args: &[OsString]) -> Result<Output, String> {
    Command::new(search).args(args).output().map_err(|e| {
        format!(
            "could not run `{} {}`: {e}",
            search.display(),
            args.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        )
    })
}

/// A non-zero index exit rendered as one line naming what the report loses.
fn refusal(checkout: &Path, index_id: &str, output: &Output) -> Option<String> {
    if output.status.success() {
        return None;
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let detail = said
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no reason given");
    Some(format!(
        "`trusty-search index {} --name {index_id}` failed ({}): {detail}",
        checkout.display(),
        output.status,
    ))
}

#[cfg(test)]
mod index_tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;

    fn output(code: i32, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// #6149: the rule three crates apply, now via one shared implementation.
    /// A drift here indexes every repository under a name nobody queries.
    #[test]
    fn the_index_id_distinguishes_two_checkouts_of_one_repo() {
        let engagement = Path::new("/w/dogfood-audit-final/repos/local/trusty-tools");
        let working = Path::new("/Users/masa/Projects/trusty-tools");

        let a = index_id_for(engagement).expect("id");
        let b = index_id_for(working).expect("id");
        assert_ne!(a, b, "{a} vs {b}");
        assert!(a.starts_with("trusty-tools-"), "{a}");
        assert_eq!(index_id_for(Path::new("/")), None);
    }

    /// The three sites are one function now, so the agreement is checkable
    /// rather than asserted: this crate's id IS trusty-common's.
    #[test]
    fn the_index_id_is_the_shared_derivation() {
        for path in [
            "/w/repos/acme-api",
            "/Users/masa/Projects/trusty-tools",
            "/",
        ] {
            let path = Path::new(path);
            assert_eq!(
                index_id_for(path),
                trusty_common::derive_checkout_index_id(path),
                "{}",
                path.display()
            );
        }
    }

    #[test]
    fn the_index_invocation_names_the_path_and_the_id() {
        assert_eq!(
            index_args(Path::new("/w/repos/xsv"), "xsv"),
            vec![
                OsString::from("index"),
                OsString::from("/w/repos/xsv"),
                OsString::from("--name"),
                OsString::from("xsv"),
            ]
        );
        assert_eq!(
            probe_args("xsv"),
            vec![OsString::from("index-status"), OsString::from("xsv")]
        );
    }

    #[test]
    fn a_zero_index_exit_is_not_a_refusal() {
        assert_eq!(
            refusal(Path::new("/w/repos/xsv"), "xsv", &output(0, "")),
            None
        );
    }

    #[test]
    fn an_index_failure_names_the_checkout_and_quotes_what_search_said() {
        let reason = refusal(
            Path::new("/w/repos/xsv"),
            "xsv",
            &output(1, "\n  indexing refused: root is not allowlisted\n"),
        )
        .expect("a non-zero exit is a refusal");
        assert!(reason.contains("/w/repos/xsv"), "{reason}");
        assert!(reason.contains("root is not allowlisted"), "{reason}");
        assert_eq!(reason.lines().count(), 1, "must stay one line: {reason}");
    }

    #[test]
    fn a_silent_index_failure_still_produces_a_reason() {
        let reason = refusal(Path::new("/w/repos/xsv"), "xsv", &output(2, ""))
            .expect("a non-zero exit is a refusal");
        assert!(reason.contains("no reason given"), "{reason}");
    }

    /// A binary that cannot be run is this repository's gap, never a panic.
    #[test]
    fn a_search_binary_that_cannot_be_run_is_a_reason_not_a_panic() {
        let err = ensure_indexed(
            Path::new("/nonexistent/trusty-search"),
            Path::new("/w/repos/xsv"),
            "xsv",
        )
        .expect_err("an unspawnable binary must degrade");
        assert!(err.contains("could not run"), "{err}");
    }

    /// The whole spawn path against a stub: an index trusty-search already
    /// serves is probed and left alone, so a resumed sweep costs nothing.
    #[cfg(unix)]
    #[test]
    fn a_served_index_is_not_rebuilt() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let calls = tmp.path().join("calls");
        let stub = tmp.path().join("trusty-search");
        std::fs::write(
            &stub,
            format!("#!/bin/sh\necho \"$1\" >> {}\nexit 0\n", calls.display()),
        )
        .expect("write stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let status = ensure_indexed(&stub, Path::new("/w/repos/xsv"), "xsv").expect("served");
        assert_eq!(status, IndexStatus::AlreadyServed);
        let seen = std::fs::read_to_string(&calls).expect("stub ran");
        assert_eq!(
            seen.lines().collect::<Vec<_>>(),
            vec!["index", "index-status"]
        );
    }

    /// The other direction: a repository trusty-search does not serve is
    /// indexed, and the id it is indexed under is the one that was asked for.
    #[cfg(unix)]
    #[test]
    fn an_unserved_index_is_built_under_the_requested_id() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let calls = tmp.path().join("calls");
        let stub = tmp.path().join("trusty-search");
        // `index add` and `index … --name` succeed; `index-status` reports the
        // 404 the real daemon reports for an index it does not serve.
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\necho \"$*\" >> {}\n[ \"$1\" = index-status ] && exit 1\nexit 0\n",
                calls.display()
            ),
        )
        .expect("write stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let status = ensure_indexed(&stub, Path::new("/w/repos/xsv"), "xsv").expect("indexed");
        assert_eq!(status, IndexStatus::Indexed);
        let seen = std::fs::read_to_string(&calls).expect("stub ran");
        assert!(seen.contains("index /w/repos/xsv --name xsv"), "{seen}");
    }

    /// The backstop still has to be precise: the comparison resolves symlinks
    /// and `..`, or a legitimate root reads as a mismatch and the repository
    /// loses its code analysis for nothing.
    #[test]
    fn same_tree_resolves_before_it_compares() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("repos/acme-api");
        std::fs::create_dir_all(&real).expect("tree");

        assert!(same_tree(&real, &real));
        assert!(
            same_tree(&tmp.path().join("repos/../repos/acme-api"), &real),
            "a `..`-laden path names the same tree"
        );
        assert!(
            same_tree(
                Path::new("/w/repos/acme-api/"),
                Path::new("/w/repos/acme-api")
            ),
            "an unstattable pair falls back to a normalised string compare"
        );
        assert!(
            !same_tree(
                Path::new("/Users/masa/Projects/trusty-tools"),
                Path::new("/w/repos/trusty-tools")
            ),
            "the collision the self-audit hit: same basename, different tree"
        );
    }

    /// A daemon that will not answer is not a refusal — every other failure has
    /// a gap of its own, and failing closed here would cost a repository its
    /// whole code-analysis leg over one unreadable field.
    #[tokio::test]
    async fn an_unreachable_daemon_is_not_a_refusal() {
        // Port 1 is never a trusty-search daemon.
        let out = root_matches("http://127.0.0.1:1", "acme-api", Path::new("/w/a")).await;
        assert!(out.is_ok(), "{out:?}");
    }
}
