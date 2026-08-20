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
//! trusty-review derives the id it looks up from the checkout path's basename
//! (`trusty_review::report::analyze_adapter::derive_index_id`), and
//! `tga::audit::repo_index::index_id_for` applies that same rule from the other
//! side. [`index_id_for`] is the third copy of one rule, and it is a copy
//! deliberately: this crate has a Cargo edge to neither, and an id derived any
//! other way would index every repository under a name nobody ever queries —
//! leaving the reports exactly as hollow as before while looking fixed.
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
/// Why/What/Test: see the module docs — the final path component, exactly as
/// `derive_index_id` returns it. `None` for a path with no basename, e.g. `/`.
/// Test: `index_tests::the_index_id_is_the_checkout_basename`.
#[must_use]
pub fn index_id_for(path: &Path) -> Option<String> {
    path.file_name().map(|n| n.to_string_lossy().into_owned())
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

    /// The rule three crates apply independently. A drift here indexes every
    /// repository under a name nobody queries.
    #[test]
    fn the_index_id_is_the_checkout_basename() {
        assert_eq!(
            index_id_for(Path::new("/w/repos/acme-api")).as_deref(),
            Some("acme-api")
        );
        assert_eq!(index_id_for(Path::new("/")), None);
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
}
