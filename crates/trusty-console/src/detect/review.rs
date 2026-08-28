//! `ServiceConnector` implementation for `trusty-review`.
//!
//! Why (#6290): trusty-review has no daemon. #6277 moved it from a TCP port to
//! a Unix socket and this connector followed; ADR-0032's review lane retired
//! the listener outright, so there is nothing left to dial. A connector that
//! kept dialling would spend its 3-second budget on a socket nobody binds,
//! once per detection pass, and report `Available` at the end of it — the same
//! answer it reaches immediately by asking whether the binary is installed.
//!
//! What: `detect()` resolves `trusty-review` on PATH and reads the version off
//! `trusty-review --version`. `Running` is unreachable for this member and that
//! is correct, not a gap: a per-invocation tool is installed or it is not.
//!
//! The webhook path is unaffected and is NOT what this connector reports on.
//! Console still spawns `trusty-review webhook-listen` on demand for a relayed
//! GitHub delivery and SIGTERMs it (ADR-0034 §1); that process's health is
//! metered by `webhook::health` off the inbox backlog, not by a service card.
//!
//! Test: `review_connector_reports_available_when_the_binary_is_present`,
//! `review_connector_reports_absent_when_the_binary_is_missing`,
//! `review_connector_never_reaches_running`.

use std::time::Duration;

use crate::connector::{ServiceConnector, ServiceInfo, ServiceStatus};

use super::helpers::binary_on_path;

/// How long `trusty-review --version` may take before the probe gives up.
///
/// A cold `trusty-review` start was measured at 191 ms (#5028) and `--version`
/// does strictly less than that — clap prints and exits before any config,
/// credential or network work. Two seconds is far past any honest answer and
/// still short enough that one wedged binary cannot stall the console's whole
/// detection pass.
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// ServiceConnector for `trusty-review`.
///
/// Why: the console dashboard's Review tab still needs to say whether review is
/// usable on this machine. Since #6290 that question is "is the binary there
/// and does it run", not "is a daemon up".
/// What: implements `detect()` — binary on PATH, then one `--version` spawn.
/// Test: see the module docs.
pub struct ReviewConnector {
    /// Override for the binary name (used in tests).
    ///
    /// Why a name and not a path: `detect()` resolves through `PATH` exactly as
    /// production does, so a test that points this at a name nothing provides
    /// exercises the real resolution rather than a stubbed one. Before #6290
    /// this field was a socket path; the socket is gone with the daemon.
    binary: Option<String>,
}

impl ReviewConnector {
    /// Create a new `ReviewConnector`.
    pub fn new() -> Self {
        Self { binary: None }
    }

    /// Create a connector that probes `binary` instead of `trusty-review`.
    ///
    /// Why: the absent-binary verdict is otherwise unreachable on a developer
    /// machine, which has trusty-review installed. Overriding the NAME keeps
    /// the test free of environment variables — five sibling connectors run in
    /// the same pass and share this process's `PATH`.
    /// What: stores `binary` for use by `detect()`.
    /// Test: `review_connector_reports_absent_when_the_binary_is_missing`.
    pub fn with_binary(binary: impl Into<String>) -> Self {
        Self {
            binary: Some(binary.into()),
        }
    }

    /// The binary this connector looks for.
    fn binary(&self) -> &str {
        self.binary.as_deref().unwrap_or("trusty-review")
    }
}

impl Default for ReviewConnector {
    fn default() -> Self {
        Self::new()
    }
}

/// Read the version off `<binary> --version`, or `None`.
///
/// Why: presence on `PATH` alone would report a binary that cannot execute — a
/// broken signature, a truncated download — as usable. Running it is the
/// cheapest check that tells the two apart, and it yields the version the card
/// renders anyway.
/// What: spawns `<binary> --version`, waits up to [`VERSION_TIMEOUT`], and
/// takes the second whitespace-separated token of the first line (clap's
/// `<name> <version>` shape). Any failure is `None`; the caller still reports
/// `Available`, because the binary being there is the fact that was observed.
/// Test: `review_connector_reports_available_when_the_binary_is_present`.
fn binary_version(binary: &str) -> Option<String> {
    let mut child = std::process::Command::new(binary)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    // `wait_timeout` is not in std; poll the child instead of blocking forever
    // on a binary that hangs before printing.
    let deadline = std::time::Instant::now() + VERSION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => {
                let _ = child.wait();
                return None;
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    }

    let output = child.wait_with_output().ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)
        .map(str::to_owned)
}

impl ServiceConnector for ReviewConnector {
    fn id(&self) -> &'static str {
        "trusty-review"
    }

    fn display_name(&self) -> &'static str {
        "Trusty Review"
    }

    /// Detect trusty-review status.
    ///
    /// Why: `tctl` asks the same question for a different reason, and the two
    /// must agree — since #6290 both ask it by presence
    /// (`trusty_installer::commands::probe_http::probe_presence`), not by
    /// dialling.
    /// What: binary on PATH → `Absent` if not, otherwise `Available` carrying
    /// whatever `--version` printed. `url` is `None`: there is no address, and
    /// ADR-0032 makes trusty-console the only HTTP surface in the workspace, so
    /// a synthesised one would be a link that cannot work.
    /// Test: see the module docs.
    fn detect(&self) -> ServiceInfo {
        let binary = self.binary();
        if !binary_on_path(binary) {
            return ServiceInfo {
                id: self.id().to_string(),
                display_name: self.display_name().to_string(),
                status: ServiceStatus::Absent,
                version: None,
                url: None,
                hint: None,
            };
        }

        ServiceInfo {
            id: self.id().to_string(),
            display_name: self.display_name().to_string(),
            status: ServiceStatus::Available,
            version: binary_version(binary),
            url: None,
            hint: None,
        }
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// REGRESSION (#6290): the console must answer for trusty-review with NO
    /// trusty-review process anywhere, and must not hang doing it.
    ///
    /// Why: nothing binds the review socket any more. A connector that still
    /// dialled would burn its full 3-second budget on every detection pass and
    /// arrive at exactly the verdict presence gives immediately. This test runs
    /// in a process where no review daemon exists — which is every process, now
    /// — and asserts a real answer comes back.
    /// What: detects against this workspace's own binary (present on any machine
    /// running this test), asserts `Available` with a version, and bounds the
    /// call so a reintroduced dial would fail rather than merely be slow.
    /// Test: this is the test.
    #[test]
    fn review_connector_reports_available_when_the_binary_is_present() {
        if which::which("cargo").is_err() {
            eprintln!("skip: no cargo on PATH to probe as a stand-in binary");
            return;
        }
        let started = std::time::Instant::now();
        let info = ReviewConnector::with_binary("cargo").detect();
        let elapsed = started.elapsed();

        assert_eq!(
            info.status,
            ServiceStatus::Available,
            "a present per-invocation binary is Available"
        );
        assert!(
            info.version.is_some(),
            "the card renders the version read off `--version`"
        );
        assert_eq!(info.id, "trusty-review");
        assert_eq!(info.display_name, "Trusty Review");
        assert!(info.url.is_none(), "a per-invocation tool has no URL");
        assert!(
            elapsed < Duration::from_secs(3),
            "the detect must not dial anything — a socket dial's own budget is \
             3 s, so this bound is what a reintroduced dial would trip: {elapsed:?}"
        );
    }

    /// Why: `Absent` is the one verdict that must stay reachable — an operator
    /// whose install failed needs the card to say so rather than to say
    /// `Available` with no version.
    /// What: probes a name no binary can have.
    /// Test: this is the test.
    #[test]
    fn review_connector_reports_absent_when_the_binary_is_missing() {
        let info = ReviewConnector::with_binary("trusty-review-does-not-exist-9f3a").detect();
        assert_eq!(info.status, ServiceStatus::Absent);
        assert!(info.version.is_none());
        assert!(info.hint.is_none());
    }

    /// Why: `Running` means "a daemon answered a health check", and trusty-review
    /// has no daemon. Reporting it would tell an operator a process exists that
    /// they could stop, restart or find in `ps` — none of which is true. This is
    /// what keeps a future edit from reaching for the more reassuring word.
    /// What: neither the present nor the absent path may produce `Running` or
    /// `Degraded`.
    /// Test: this is the test.
    #[test]
    fn review_connector_never_reaches_running() {
        for connector in [
            ReviewConnector::new(),
            ReviewConnector::with_binary("trusty-review-does-not-exist-9f3a"),
        ] {
            let status = connector.detect().status;
            assert!(
                matches!(status, ServiceStatus::Available | ServiceStatus::Absent),
                "a per-invocation member has only two honest verdicts, got {status:?}"
            );
        }
    }
}
