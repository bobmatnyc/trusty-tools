//! Low-level detection helpers shared by all service connectors.
//!
//! Why: Centralising the binary probes avoids duplicating them across every
//! connector and makes them independently testable.
//! What: `binary_on_path` and `binary_version`. #11632: the `http_addr`,
//! TCP-probe and HTTP-health helpers and the `detect_service` orchestrator
//! left with their one user, the trusty-agents connector.
//! Test: Unit tests at the bottom of this file. Run with
//! `cargo test -p trusty-console`.

use std::time::Duration;

// ─── primitive helpers ────────────────────────────────────────────────────────

/// Return true if `binary` is found on PATH.
///
/// Why: The binary presence check is the outermost gate — if the binary isn't
/// installed, no daemon can ever be running.
/// What: Delegates to the `which` crate.
/// Test: Covered indirectly by every connector test; mocked via PATH override.
pub(super) fn binary_on_path(binary: &str) -> bool {
    which::which(binary).is_ok()
}

// ─── version probe (on-demand members) ────────────────────────────────────────

/// How long `<binary> --version` may take before the probe gives up.
///
/// A cold `trusty-review` start was measured at 191 ms (#5028) and `--version`
/// does strictly less than that — clap prints and exits before any config,
/// credential or network work. Two seconds is far past any honest answer and
/// still short enough that one wedged binary cannot stall the console's whole
/// detection pass.
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// What running `<binary> --version` established.
///
/// Why: "no version string" and "the binary cannot execute" are opposite facts
/// that a bare `Option<String>` collapses into the same `None` (#6290). The
/// second is the exact state the spawn exists to catch, and reporting it as
/// `Available` also puts the console at odds with `tctl`, whose
/// `probe_presence` calls it `ProbeFailed`.
pub(super) enum VersionProbe {
    /// `--version` ran to completion; the payload is the parsed version, which
    /// is `None` only when the output did not have clap's `<name> <version>`
    /// shape. The binary works either way.
    Ran(Option<String>),
    /// On `PATH` but not runnable — a broken signature, a truncated download, a
    /// hang. The payload is the operator-facing reason.
    CannotExecute(String),
}

/// Run `<binary> --version` and report what happened.
///
/// Why: for an on-demand member the binary IS the service, so presence plus a
/// clean run is the whole health question — and the run yields the version the
/// card renders. Presence alone would report a binary that cannot execute (a
/// broken signature, a truncated download) as usable.
/// What: spawns `<binary> --version`, waits up to [`VERSION_TIMEOUT`], and takes
/// the second whitespace-separated token of the first line (clap's
/// `<name> <version>` shape). A non-zero exit, a spawn failure, or a hang is
/// [`VersionProbe::CannotExecute`], never a silent `None`.
/// Test: `a_binary_that_cannot_execute_is_not_available`,
/// `binary_version_reads_the_version_off_a_real_binary`.
pub(super) fn binary_version(binary: &str) -> VersionProbe {
    let mut child = match std::process::Command::new(binary)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return VersionProbe::CannotExecute(format!("spawn `{binary} --version`: {e}")),
    };

    // `wait_timeout` is not in std; poll the child instead of blocking forever
    // on a binary that hangs before printing.
    let deadline = std::time::Instant::now() + VERSION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => {
                let _ = child.wait();
                return VersionProbe::CannotExecute(format!(
                    "`{binary} --version` exited {status}"
                ));
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return VersionProbe::CannotExecute(format!(
                    "`{binary} --version` did not exit within {VERSION_TIMEOUT:?}"
                ));
            }
            Err(e) => {
                return VersionProbe::CannotExecute(format!(
                    "waiting on `{binary} --version`: {e}"
                ));
            }
        }
    }

    match child.wait_with_output() {
        Ok(output) => VersionProbe::Ran(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .map(str::to_owned),
        ),
        Err(e) => VersionProbe::CannotExecute(format!("reading `{binary} --version`: {e}")),
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── binary_version ──────────────────────────────────────────────────────

    /// REGRESSION (#6290): a binary that cannot execute is not `Available`.
    ///
    /// Why: `binary_version` returned `None` for BOTH "no version in the output"
    /// and "the binary exited non-zero", and `detect` reported `Available`
    /// regardless — the exact "cannot execute" state the spawn exists to catch,
    /// reported as usable. It also disagreed with `tctl`, whose `probe_presence`
    /// calls the same host `ProbeFailed`.
    /// What: probes a binary that exists and always exits 1 (`/usr/bin/false`),
    /// and asserts the outcome carries the reason.
    #[cfg(unix)]
    #[test]
    fn a_binary_that_cannot_execute_is_not_available() {
        match binary_version("/usr/bin/false") {
            VersionProbe::CannotExecute(why) => {
                assert!(
                    why.contains("exited"),
                    "the operator needs the reason: {why}"
                );
            }
            VersionProbe::Ran(v) => {
                panic!("a binary exiting 1 must not read as a clean run, got {v:?}")
            }
        }
    }

    /// Why (#6416): both on-demand connectors render the card's version off this
    /// one probe now, so the clap `<name> <version>` parse has to keep working.
    /// What: probes `cargo`, which every machine running this test has.
    /// Test: this test itself.
    #[test]
    fn binary_version_reads_the_version_off_a_real_binary() {
        if which::which("cargo").is_err() {
            eprintln!("skip: no cargo on PATH to probe as a stand-in binary");
            return;
        }
        match binary_version("cargo") {
            VersionProbe::Ran(version) => {
                assert!(version.is_some(), "`cargo --version` prints a version");
            }
            VersionProbe::CannotExecute(why) => panic!("cargo must run: {why}"),
        }
    }
}
