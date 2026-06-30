//! Scrollback + cwd snapshot capture before session stop (#1816).
//!
//! Why: the idle auto-stop and manual-stop paths both need to save the pane's
//! recent output and current working directory BEFORE killing tmux, so that
//! context is not lost and `resume()` can restore the operator's cwd. Keeping
//! this in a separate module lets the `manager` module stay under the 500-SLOC
//! cap and makes the capture logic unit-testable in isolation.
//! What: [`capture_into`] is the single entry point — it populates
//! `scrollback_path` and `last_cwd` on a [`SessionRecord`] in-place, then
//! returns. All I/O failures are non-fatal (logged as warnings) so callers can
//! proceed to the tmux kill unconditionally.
//! Test: `capture_into_no_workspace_skips_write`,
//! `capture_into_driver_failure_is_nonfatal`.

use std::path::Path;

use tracing::{debug, warn};

use super::manager::ManagedTmuxDriver;
use super::record::SessionRecord;

/// Number of scrollback lines to capture before stop.
///
/// Why: 5 000 lines gives enough history for a typical interactive session
/// without writing multi-megabyte snapshot files.
/// What: the `-S -N` argument to `tmux capture-pane`.
/// Test: implicitly via `capture_into` test stubs.
const SCROLLBACK_LINES: usize = 5_000;

/// Relative path inside the workspace where the scrollback file is written.
///
/// Why: a stable, well-known sub-path inside the session's workspace keeps
/// snapshots co-located with the work they describe without polluting the
/// repository root.
/// What: the directory is created on demand; the file is overwritten on every
/// stop so only the most recent snapshot is kept.
/// Test: `capture_into_writes_scrollback_file`.
const SCROLLBACK_SUBPATH: &str = ".trusty-mpm/scrollback.txt";

/// Capture scrollback and cwd into `record` before the session's tmux pane is killed.
///
/// Why: the stop path (both manual and idle-auto-stop) must capture context
/// before killing tmux; failures must be non-fatal so the kill always proceeds.
/// What: (1) tries to capture the pane's last [`SCROLLBACK_LINES`] lines and
/// write them to `<workspace_path>/.trusty-mpm/scrollback.txt`, setting
/// `record.scrollback_path` on success; (2) queries `driver.get_pane_cwd()` and
/// sets `record.last_cwd` on success. Skips step (1) if `workspace_path` is
/// absent (no place to write the file). All errors are logged and swallowed.
/// Test: `capture_into_no_workspace_skips_write`,
/// `capture_into_driver_failure_is_nonfatal`.
pub async fn capture_into(record: &mut SessionRecord, driver: &dyn ManagedTmuxDriver) {
    let name = &record.tmux_name;

    // Step 1: scrollback snapshot (only if we have a workspace dir to write into).
    if let Some(ws) = record.workspace_path.as_ref() {
        match driver.capture(name, SCROLLBACK_LINES) {
            Ok(text) => {
                let dest = ws.join(SCROLLBACK_SUBPATH);
                if let Err(e) = write_scrollback(&dest, &text).await {
                    warn!(name = %name, path = %dest.display(), "snapshot: scrollback write failed: {e}");
                } else {
                    debug!(name = %name, path = %dest.display(), "snapshot: scrollback written ({} bytes)", text.len());
                    record.scrollback_path = Some(dest);
                }
            }
            Err(e) => {
                warn!(name = %name, "snapshot: capture-pane failed (non-fatal): {e}");
            }
        }
    } else {
        debug!(name = %name, "snapshot: no workspace_path; skipping scrollback write");
    }

    // Step 2: pane cwd.
    if let Some(cwd) = driver.get_pane_cwd(name) {
        debug!(name = %name, cwd = %cwd.display(), "snapshot: last_cwd captured");
        record.last_cwd = Some(cwd);
    }
}

/// Write `content` to `dest`, creating parent directories as needed.
///
/// Why: the snapshot directory (`<workspace>/.trusty-mpm/`) may not exist yet
/// on the first stop of a session; creating it on demand avoids a separate
/// provisioning step.
/// What: runs `tokio::fs::create_dir_all` then `tokio::fs::write`.
/// Test: exercised by `capture_into_writes_scrollback_file` (uses a temp dir).
async fn write_scrollback(dest: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(dest, content).await
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::Utc;

    use crate::session_manager::manager::{ManagedError, ManagedTmuxDriver};
    use crate::session_manager::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

    use super::*;

    fn make_record(workspace: Option<PathBuf>) -> SessionRecord {
        SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-snap-test".into(),
            cwd: PathBuf::from("/tmp/cwd"),
            task: "test".into(),
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: workspace,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
        }
    }

    // A test driver that returns canned capture output and a fixed cwd.
    struct StubDriver {
        capture_result: Result<String, ManagedError>,
        cwd: Option<PathBuf>,
    }

    impl ManagedTmuxDriver for StubDriver {
        fn create_session(&self, _: &str, _: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn kill_session(&self, _: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn send_line(&self, _: &str, _: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn capture(&self, _: &str, _: usize) -> Result<String, ManagedError> {
            self.capture_result
                .as_ref()
                .map(|s| s.clone())
                .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
        }
        fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
            Ok(vec![])
        }
        fn get_pane_cwd(&self, _: &str) -> Option<PathBuf> {
            self.cwd.clone()
        }
    }

    /// Why: with no workspace_path the scrollback file cannot be written; the
    /// snapshot must skip the write without panicking and leave scrollback_path
    /// as None.
    /// What: calls capture_into with a driver that returns output and a record
    /// that has no workspace_path; asserts scrollback_path stays None.
    /// Test: this is the test.
    #[tokio::test]
    async fn capture_into_no_workspace_skips_write() {
        let mut record = make_record(None);
        let driver = StubDriver {
            capture_result: Ok("some output".into()),
            cwd: Some(PathBuf::from("/tmp/src")),
        };
        capture_into(&mut record, &driver).await;
        assert!(
            record.scrollback_path.is_none(),
            "no workspace → no scrollback file"
        );
        // last_cwd should still be set from get_pane_cwd.
        assert_eq!(record.last_cwd, Some(PathBuf::from("/tmp/src")));
    }

    /// Why: if the capture-pane driver call fails the stop must still proceed;
    /// scrollback_path must remain None and last_cwd may still be set.
    /// What: driver returns Err for capture; asserts scrollback_path is None.
    /// Test: this is the test.
    #[tokio::test]
    async fn capture_into_driver_failure_is_nonfatal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut record = make_record(Some(tmp.path().to_path_buf()));
        let driver = StubDriver {
            capture_result: Err(ManagedError::TmuxUnavailable("tmux gone".into())),
            cwd: None,
        };
        capture_into(&mut record, &driver).await;
        assert!(
            record.scrollback_path.is_none(),
            "capture failure → no file"
        );
        assert!(record.last_cwd.is_none(), "cwd None from driver stays None");
    }

    /// Why: the happy path must write the scrollback file and populate both fields.
    /// What: driver returns "hello output" and cwd; asserts both fields are set
    /// and the file on disk matches the output.
    /// Test: this is the test.
    #[tokio::test]
    async fn capture_into_writes_scrollback_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut record = make_record(Some(tmp.path().to_path_buf()));
        let driver = StubDriver {
            capture_result: Ok("hello output".into()),
            cwd: Some(PathBuf::from("/repo/src")),
        };
        capture_into(&mut record, &driver).await;
        let path = record
            .scrollback_path
            .expect("scrollback_path should be set");
        let on_disk = std::fs::read_to_string(&path).expect("file must exist");
        assert_eq!(on_disk, "hello output");
        assert_eq!(record.last_cwd, Some(PathBuf::from("/repo/src")));
    }
}
