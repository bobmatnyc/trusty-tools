//! Approving a clone for indexing, so the code-analysis leg has something to read.
//!
//! Why: #5915. `trusty-search` is default-deny — a path nobody approved is not
//! indexed, and `tga audit` runs `trusty-search index <checkout> --name <id>`,
//! whose handler refuses an unapproved root outright
//! (`trusty-search/src/commands/index.rs`, the `AllowlistVerdict::NotAllowlisted`
//! arm). Nothing in the audit chain ever approved anything, so that refusal fired
//! on every repository of every run. It cost the whole code-analysis leg, and it
//! cost it SILENTLY: `tga audit` exits 0 when the sweep completed, so the run
//! reported success and the report simply had nothing to say about the code.
//!
//! The approval is a separate verb from the index: `trusty-search index add
//! <path>` writes a row to `trusty-search`'s allowlist, and only then does
//! `trusty-search index <path>` proceed.
//!
//! What: [`approve_for_indexing`], plus the two pure halves it is made of so
//! they can be asserted without a live daemon — [`index_add_args`] and
//! [`refusal`].
//!
//! `--allow-sensitive-path` is deliberately NOT passed. It is not reachable from
//! tga's path anyway — `handle_index` calls the strict `is_denied`, so approving
//! a denied root would leave the run refused at the next step regardless — and it
//! does not relax `SENSITIVE_HOME_TOP_DIRS`, which is the rule a recipient
//! unzipping into `~/Downloads` actually hits. Placement is what fixes those;
//! see `crate::workdir::WorkDir::resolve`.
//!
//! Test: `super::run_tests::an_unapprovable_checkout_fails_the_repository_by_name`,
//! `super::run_tests::an_approved_checkout_proceeds_to_the_child`, and this
//! module's own `approve_tests`.

use std::ffi::OsString;
use std::path::Path;
use std::process::Output;

/// Approve `checkout` with the pinned `trusty-search`, or say why it refused.
///
/// Why: see the module docs — without this the sweep's code-analysis leg reads
/// nothing, and reports nothing, and exits 0.
/// What: runs `<search> index add <checkout>` and turns a non-zero exit into a
/// reason string. `index add` upserts, so re-approving a checkout a previous run
/// already approved succeeds rather than erroring — which is what makes this
/// safe to call on every repository of a resumed run.
///
/// # Postconditions
/// On `Ok`, `trusty-search` holds an allowlist row for `checkout` — written
/// OUTSIDE the work root, which is why `crate::workdir`'s module docs no longer
/// promise `rm -rf <root>` is a complete uninstall.
/// On `Err`, the string is one line, safe to show the recipient, and names the
/// checkout.
///
/// This is synchronous inside an async caller on purpose: `index add`
/// canonicalises a path and rewrites one small TOML file, so it returns in
/// well under the time the `tga` child it precedes takes to start.
pub(crate) fn approve_for_indexing(search: &Path, checkout: &Path) -> Result<(), String> {
    let output = std::process::Command::new(search)
        .args(index_add_args(checkout))
        .output()
        .map_err(|e| {
            format!(
                "could not run `{} index add {}`: {e} — the code analysis would have had nothing to read",
                search.display(),
                checkout.display()
            )
        })?;
    match refusal(checkout, &output) {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

/// The argv `trusty-search` is given, with no opt-in flag.
///
/// Kept separate so the absence of `--allow-sensitive-path` is asserted rather
/// than assumed — the flag is the fix this deliberately did not take (see the
/// module docs), and a later edit adding it would otherwise pass unnoticed.
fn index_add_args(checkout: &Path) -> Vec<OsString> {
    vec![
        OsString::from("index"),
        OsString::from("add"),
        checkout.as_os_str().to_owned(),
    ]
}

/// A non-zero exit rendered as one line the recipient can act on.
///
/// Why the child's own stderr is quoted rather than replaced: the refusals differ
/// in what the recipient must do about them — a denylisted path needs a different
/// working directory, an unreadable allowlist needs a permissions fix — and only
/// `trusty-search` knows which one fired.
fn refusal(checkout: &Path, output: &Output) -> Option<String> {
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
        "`trusty-search index add {}` refused ({}): {detail} — the code analysis \
         would have read nothing for this repository",
        checkout.display(),
        output.status,
    ))
}

#[cfg(test)]
mod approve_tests {
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

    /// The flag the owner ruled out. `--allow-sensitive-path` cannot help here:
    /// tga's own index call uses the strict check, so a root approved through the
    /// bypass is refused at the next step anyway.
    #[test]
    fn the_approval_never_asks_for_the_sensitive_path_bypass() {
        let args = index_add_args(Path::new(
            "/Users/r/.trusty-tools/trusty-audit/work/repos/xsv",
        ));
        assert_eq!(
            args,
            vec![
                OsString::from("index"),
                OsString::from("add"),
                OsString::from("/Users/r/.trusty-tools/trusty-audit/work/repos/xsv"),
            ]
        );
    }

    #[test]
    fn a_zero_exit_is_an_approval() {
        assert_eq!(refusal(Path::new("/w/repos/xsv"), &output(0, "")), None);
    }

    /// #5915 is entirely about this line existing. The refusal used to reach
    /// nobody: tga exited 0, so the run reported success with an empty leg.
    #[test]
    fn a_refusal_names_the_checkout_and_quotes_what_search_said() {
        let reason = refusal(
            Path::new("/var/folders/9x/T/work/repos/xsv"),
            &output(
                1,
                "\n  indexing refused: path is under a temporary directory\n",
            ),
        )
        .expect("a non-zero exit is a refusal");

        assert!(
            reason.contains("/var/folders/9x/T/work/repos/xsv"),
            "{reason}"
        );
        assert!(
            reason.contains("indexing refused: path is under a temporary directory"),
            "{reason}"
        );
        assert_eq!(reason.lines().count(), 1, "must stay one line: {reason}");
    }

    #[test]
    fn a_silent_refusal_still_produces_a_reason() {
        let reason = refusal(Path::new("/w/repos/xsv"), &output(2, ""))
            .expect("a non-zero exit is a refusal");
        assert!(reason.contains("no reason given"), "{reason}");
    }

    /// The spawn itself, not just the two pure halves: a stub standing in for
    /// `trusty-search` proves the argv reaches a child and its exit is read.
    #[cfg(unix)]
    #[test]
    fn a_stub_search_that_refuses_is_reported_rather_than_ignored() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let stub = tmp.path().join("trusty-search");
        std::fs::write(
            &stub,
            "#!/bin/sh\necho \"indexing refused: $3 is not approved\" >&2\nexit 1\n",
        )
        .expect("write stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let err = approve_for_indexing(&stub, Path::new("/w/repos/xsv"))
            .expect_err("a refusing search must fail the repository");
        assert!(err.contains("/w/repos/xsv"), "{err}");
        assert!(err.contains("is not approved"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_stub_search_that_approves_lets_the_repository_proceed() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let stub = tmp.path().join("trusty-search");
        std::fs::write(&stub, "#!/bin/sh\nexit 0\n").expect("write stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        approve_for_indexing(&stub, Path::new("/w/repos/xsv")).expect("a zero exit is an approval");
    }

    /// A missing binary is a failure of this repository, never a panic and never
    /// a silent skip — the pinned-tool preflight should have caught it, and if it
    /// did not, the recipient still gets a named reason.
    #[test]
    fn a_search_binary_that_cannot_be_run_is_a_reason_not_a_panic() {
        let err = approve_for_indexing(Path::new("/nonexistent/trusty-search"), Path::new("/w/x"))
            .expect_err("an unspawnable binary must fail the repository");
        assert!(err.contains("could not run"), "{err}");
    }
}
