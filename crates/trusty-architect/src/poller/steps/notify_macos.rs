//! `fleet-poll.py`'s `notify_macos` and its one call site (Architect
//! 0d1ef3b; the function unchanged since 7892d0f).
//!
//! Why: a dead, frozen or silent Architect raises an `architect_*` alert,
//! and Bob sees it as a macOS notification even when no session reads the
//! inbox (#9801 slice 1, watchdog f4).
//! What: [`applescript`] builds `display notification "<message>" with title
//! "<title>"`, escaping `\` and `"` as the prototype does. [`Osascript`] runs
//! `osascript -e <script>` through the crate's bounded runner (10 s, killed
//! and reaped after), behind the [`Notifier`] seam so tests never notify.
//! [`notify_watchdog_alerts`] notifies each new `architect_*` alert under
//! [`WATCHDOG_TITLE`] and returns one `notify ERROR` line per failure.
//!
//! Fail-Open Check (#9801): a notification changes no poller state, so a
//! failed one cannot be mistaken for a sent one; it is logged and not
//! retried (the alert itself is already in `alerts.jsonl`). The timeout
//! bounds a hung `osascript`. The prototype logs a spawn failure or timeout
//! and ignores a non-zero exit; the port logs all three.
//! Test: `notify_argv_matches_python_goldens` (tests/parity_usage.rs),
//! `failed_notifications_are_logged_and_the_rest_still_sent`,
//! `osascript_that_hangs_is_killed_at_the_timeout`,
//! `osascript_failures_are_errors` (tests/steps_fail_open.rs).

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use crate::classify::Alert;
pub use crate::error::NotifyError;
use crate::process::{RunFailure, run_bounded};

/// The prototype's `subprocess.run(..., timeout=10)`.
pub const NOTIFY_TIMEOUT: Duration = Duration::from_secs(10);
/// The title every watchdog notification carries.
pub const WATCHDOG_TITLE: &str = "Architect watchdog";

/// The AppleScript line `notify_macos` passes to `osascript -e`.
pub fn applescript(title: &str, message: &str) -> String {
    let esc = |t: &str| t.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "display notification \"{}\" with title \"{}\"",
        esc(message),
        esc(title)
    )
}

/// Shows one notification.
pub trait Notifier {
    /// Shows `message` under `title`.
    ///
    /// # Errors
    /// The notification could not be shown.
    fn notify(&self, title: &str, message: &str) -> Result<(), NotifyError>;
}

/// The real notifier: `osascript -e <script>`, bounded by `timeout`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Osascript {
    /// The program run; `osascript` on `PATH` by default.
    pub program: PathBuf,
    /// How long it may run before it is killed.
    pub timeout: Duration,
}

impl Default for Osascript {
    fn default() -> Self {
        Self {
            program: PathBuf::from("osascript"),
            timeout: NOTIFY_TIMEOUT,
        }
    }
}

impl Notifier for Osascript {
    fn notify(&self, title: &str, message: &str) -> Result<(), NotifyError> {
        let mut cmd = Command::new(&self.program);
        cmd.arg("-e").arg(applescript(title, message));
        match run_bounded(cmd, self.timeout) {
            Ok(done) if done.status.success() => Ok(()),
            Ok(done) => Err(NotifyError::Failed {
                code: done.status.code(),
                stderr: String::from_utf8_lossy(&done.stderr).trim().to_owned(),
            }),
            Err(RunFailure::Spawn(e)) => Err(NotifyError::Spawn(e)),
            Err(RunFailure::Timeout) => Err(NotifyError::Timeout(self.timeout)),
            Err(RunFailure::Wait(e)) => Err(NotifyError::Wait(e)),
        }
    }
}

/// A notifier for tests: records each `(title, message)`;
/// [`RecordingNotifier::failing`] also fails each one.
#[derive(Debug, Default)]
pub struct RecordingNotifier {
    sent: Mutex<Vec<(String, String)>>,
    fail: bool,
}

impl RecordingNotifier {
    /// A notifier whose every notification fails (after being recorded).
    pub fn failing() -> Self {
        Self {
            fail: true,
            ..Self::default()
        }
    }

    /// Every `(title, message)` asked for, oldest first.
    pub fn sent(&self) -> Vec<(String, String)> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Notifier for RecordingNotifier {
    fn notify(&self, title: &str, message: &str) -> Result<(), NotifyError> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((title.to_owned(), message.to_owned()));
        if self.fail {
            return Err(NotifyError::Failed {
                code: Some(1),
                stderr: "recording notifier refuses".to_owned(),
            });
        }
        Ok(())
    }
}

/// True for an alert the cycle notifies: an `architect_*` kind.
pub fn notifies(a: &Alert) -> bool {
    a.kind.starts_with("architect_")
}

/// Notifies each `architect_*` alert in `new`; returns a `notify ERROR`
/// line for each one that failed. Every alert is tried.
pub fn notify_watchdog_alerts(notifier: &dyn Notifier, new: &[Alert]) -> Vec<String> {
    new.iter()
        .filter(|a| notifies(a))
        .filter_map(|a| notifier.notify(WATCHDOG_TITLE, &a.display).err())
        .map(|e| format!("notify ERROR {e}"))
        .collect()
}
