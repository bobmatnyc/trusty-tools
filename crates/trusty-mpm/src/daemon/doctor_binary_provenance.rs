//! Doctor probe: install provenance of the running binary (issue #4033).
//!
//! Why: doctor observed liveness and file state but never binary versioning or
//! install source, so a stale path-installed binary passed every check while
//! serving code the operator believed had been replaced. See
//! [`crate::core::binary_provenance`] for the measured incident.
//!
//! What: [`check_binary_provenance`] resolves the running executable and cargo's
//! `$CARGO_HOME/.crates2.json` ledger, then hands both to
//! [`check_binary_provenance_with`], which is pure over its inputs. This file is
//! only I/O: exe resolution, `CARGO_HOME` resolution, one file read, and a real
//! filesystem probe for the path-install source. READ-ONLY — it never installs,
//! moves, or deletes anything.
//!
//! Test: `provenance_is_unknown_without_a_ledger`,
//! `provenance_is_unknown_without_an_exe`,
//! `provenance_reports_the_ledger_verdict`; the full verdict table lives in
//! `core::binary_provenance`'s own tests.

use std::path::{Path, PathBuf};

use crate::core::binary_provenance::{parse_cargo_installs, provenance_report};
use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Name of this check as it appears in `tm doctor` output.
const CHECK_NAME: &str = "binary_provenance";

/// Probe where the running binary came from and whether that source survives.
///
/// Why: see the module doc — "is what's running what you think it is?" is the
/// observation doctor's health model lacked (#4033).
/// What: resolves `std::env::current_exe()` and the cargo install-ledger path,
/// reads the ledger, and delegates to [`check_binary_provenance_with`]. The
/// running version is this binary's compiled-in `CARGO_PKG_VERSION`.
/// Test: the pure core is covered below; this wrapper is exe + file resolution.
pub(super) fn check_binary_provenance() -> DoctorCheck {
    let exe = std::env::current_exe().ok();
    let ledger_raw = cargo_ledger_path().and_then(|p| std::fs::read_to_string(p).ok());
    check_binary_provenance_with(
        exe.as_deref(),
        ledger_raw.as_deref(),
        env!("CARGO_PKG_VERSION"),
    )
}

/// Pure core of [`check_binary_provenance`].
///
/// Why: taking the executable path and the raw ledger text as parameters makes
/// every branch testable without mutating `CARGO_HOME` — an env mutation that
/// would race the rest of the suite under `cargo test`'s thread pool.
/// What: `Unknown` when the executable cannot be resolved (nothing to attribute)
/// or when `ledger_raw` is `None`/unparseable (nothing to attribute it to);
/// otherwise the verdict from [`provenance_report`], with a real
/// `Path::exists` probe for a path install's source directory.
/// Test: `provenance_is_unknown_without_a_ledger`,
/// `provenance_is_unknown_without_an_exe`,
/// `provenance_reports_the_ledger_verdict`.
fn check_binary_provenance_with(
    exe: Option<&Path>,
    ledger_raw: Option<&str>,
    running_version: &str,
) -> DoctorCheck {
    let Some(bin_name) = exe
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(str::to_owned)
    else {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "the running executable's path could not be resolved — install provenance \
             cannot be verified (issue #4033)",
        );
    };

    let ledger = ledger_raw.and_then(parse_cargo_installs);
    let (status, message) =
        provenance_report(&bin_name, running_version, exe, ledger.as_deref(), &|dir| {
            dir.exists()
        });
    DoctorCheck::new(CHECK_NAME, status, message)
}

/// Resolve the path to cargo's install ledger.
///
/// Why: `CARGO_HOME` is routinely relocated (rustup layouts, CI images, the
/// prebuilt installer), and reading only `~/.cargo` would report `Unknown` on
/// every such host — an honest answer, but a needlessly uninformative one when
/// the real ledger is one env var away.
/// What: `$CARGO_HOME/.crates2.json` when `CARGO_HOME` is set and non-empty,
/// else `~/.cargo/.crates2.json`, else `None` when no home resolves.
/// Test: exercised live by `run_doctor`; the branch it feeds is covered by
/// `provenance_is_unknown_without_a_ledger`.
fn cargo_ledger_path() -> Option<PathBuf> {
    let base = match std::env::var("CARGO_HOME") {
        Ok(h) if !h.is_empty() => PathBuf::from(h),
        _ => dirs::home_dir()?.join(".cargo"),
    };
    Some(base.join(".crates2.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host with no cargo ledger must report UNKNOWN, never `Ok`.
    ///
    /// Why: the #4033 rule in its smallest form — a probe that could not
    /// determine provenance has not confirmed it.
    /// Test: this test.
    #[test]
    fn provenance_is_unknown_without_a_ledger() {
        let check =
            check_binary_provenance_with(Some(Path::new("/usr/local/bin/tm")), None, "1.3.1");
        assert_eq!(
            check.status,
            CheckStatus::Unknown,
            "message: {}",
            check.message
        );
        assert_eq!(check.name, CHECK_NAME);
    }

    /// An unresolvable executable is also UNKNOWN, not a pass.
    #[test]
    fn provenance_is_unknown_without_an_exe() {
        let check = check_binary_provenance_with(None, Some("{}"), "1.3.1");
        assert_eq!(check.status, CheckStatus::Unknown);
    }

    /// The LIVE check, run against this machine's real cargo ledger, must
    /// report UNKNOWN for the test binary.
    ///
    /// Why: this is the #4033 rule exercised end-to-end on real state rather
    /// than fixtures. A `cargo test` binary lives in `target/debug/deps/` and is
    /// not something cargo installed, so its provenance genuinely cannot be
    /// verified — and the check must say so instead of reporting `Ok`. A
    /// regression that made an unrecorded binary read as healthy would flip this
    /// to `Ok` and fail here.
    /// Test: this test (prints the live message under `--nocapture`).
    #[test]
    fn live_check_reports_unknown_for_an_uninstalled_binary() {
        let check = check_binary_provenance();
        println!(
            "LIVE binary_provenance -> {:?}: {}",
            check.status, check.message
        );
        assert_eq!(
            check.status,
            CheckStatus::Unknown,
            "a binary cargo never installed must be UNKNOWN, not Ok: {}",
            check.message
        );
    }

    /// The wrapper must pass the ledger verdict through unchanged.
    ///
    /// Why: proves the plumbing (bin-name derivation from the exe path, ledger
    /// parse, delegation) actually reaches the decision table rather than
    /// short-circuiting on one of the `Unknown` guards.
    /// Test: this test.
    #[test]
    fn provenance_reports_the_ledger_verdict() {
        let ledger = r#"{"installs":{
            "trusty-mpm 1.3.1 (registry+https://github.com/rust-lang/crates.io-index)":
            {"bins":["tm"]}}}"#;
        let check = check_binary_provenance_with(
            Some(Path::new("/Users/x/.cargo/bin/tm")),
            Some(ledger),
            "1.3.1",
        );
        assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
        assert!(check.message.contains("crates.io"), "{}", check.message);
    }
}
