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
use std::sync::OnceLock;

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

/// Compiled regex for secret redaction; initialized once on first call.
///
/// Why: compiling a regex is expensive — a `OnceLock` amortises the cost
/// across all snapshot writes (typically one per stopped session).
/// What: matches four pattern families — explicit API-key env vars, `sk-*`
/// tokens, Bearer tokens, and generic token/secret/password/api-key
/// assignments — and captures prefix vs. secret as separate groups so the
/// key name is preserved in the output.
/// Test: `redaction_scrubs_secrets_before_write`.
static REDACT_RE: OnceLock<regex::Regex> = OnceLock::new();

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

/// Write `content` to `dest` with secret redaction and restrictive permissions.
///
/// Why: pane scrollback is a classic secret-leak vector (API keys, Bearer
/// tokens, env echoes). Redacting before write and restricting the file to
/// 0600 / parent dir to 0700 provides defence-in-depth for other processes
/// running as the same uid.
/// What: creates parent dirs, sets the parent dir to 0700 (best-effort),
/// redacts the content via [`redact_secrets`], and writes the file with mode
/// 0600 on Unix (falling back to a plain async write on non-Unix).
/// Test: `scrollback_file_has_restrictive_permissions`,
/// `redaction_scrubs_secrets_before_write`.
async fn write_scrollback(dest: &Path, content: &str) -> std::io::Result<()> {
    let parent = dest.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "scrollback path has no parent directory",
        )
    })?;
    tokio::fs::create_dir_all(parent).await?;

    // Best-effort: restrict the .trusty-mpm/ snapshot dir to owner-only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).await;
    }

    let redacted = redact_secrets(content);

    // On Unix: offload the blocking OpenOptions write to a thread pool thread
    // so we do not stall the Tokio executor during the file creation.
    #[cfg(unix)]
    {
        let dest = dest.to_path_buf();
        let data = redacted.into_bytes();
        tokio::task::spawn_blocking(move || secure_write(&dest, &data))
            .await
            .map_err(std::io::Error::other)??;
    }
    // On non-Unix: plain async write (no mode bits available).
    #[cfg(not(unix))]
    tokio::fs::write(dest, redacted.as_bytes()).await?;

    Ok(())
}

/// Create (or truncate) `path` and write `data` with mode 0600 on Unix.
///
/// Why: `OpenOptions::mode()` is the only race-free way to create a file with
/// restrictive permissions; a write-then-`chmod` sequence has a TOCTOU window.
/// What: opens with `O_CREAT | O_TRUNC | O_WRONLY | mode=0o600` and writes
/// `data` in a single blocking call.
/// Test: `scrollback_file_has_restrictive_permissions`.
#[cfg(unix)]
fn secure_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)
}

/// Scrub obvious secrets from pane scrollback before persisting to disk.
///
/// Why: pane output frequently contains `export API_KEY=…`, echo of env vars,
/// or `curl -H "Bearer …"` commands. A conservative redaction pass reduces
/// secret exposure if the snapshot file is read by diagnostic tools or
/// inadvertently synced.
/// What: replaces the VALUE portion of four pattern families with
/// `***REDACTED***`, preserving the key name / prefix for debuggability.
/// Patterns (case-insensitive):
/// 1. `ANTHROPIC_API_KEY=<value>` / `OPENROUTER_API_KEY=<value>` (explicit vars)
/// 2. `sk-<alphanumeric>` (Anthropic / OpenAI-style tokens)
/// 3. `Bearer <token>`
/// 4. `(token|secret|password|api_key|api-key) [:=] <value>` (generic)
///
/// Test: `redaction_scrubs_secrets_before_write`.
fn redact_secrets(content: &str) -> String {
    let re = REDACT_RE.get_or_init(|| {
        // Each alternation captures (prefix, secret) as consecutive groups:
        //   alt 1: groups 1-2  — named API key env vars
        //   alt 2: groups 3-4  — sk- token prefix + body
        //   alt 3: groups 5-6  — Bearer + token
        //   alt 4: groups 7-8  — generic key[:=]value
        regex::Regex::new(concat!(
            r"(?i)",
            r"((?:anthropic|openrouter)_api_key\s*=\s*)(\S+)",
            r"|",
            r"(sk-)([A-Za-z0-9_-]+)",
            r"|",
            r"(bearer\s+)([A-Za-z0-9._-]+)",
            r"|",
            r"((?:token|secret|password|api[_-]?key)\s*[:=]\s*)(\S+)",
        ))
        .expect("REDACT_RE must compile — static pattern")
    });
    re.replace_all(content, |caps: &regex::Captures| {
        let prefix = if caps.get(1).is_some() {
            caps.get(1).map_or("", |m| m.as_str())
        } else if caps.get(3).is_some() {
            caps.get(3).map_or("", |m| m.as_str())
        } else if caps.get(5).is_some() {
            caps.get(5).map_or("", |m| m.as_str())
        } else {
            caps.get(7).map_or("", |m| m.as_str())
        };
        format!("{prefix}***REDACTED***")
    })
    .into_owned()
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
            deliverable_id: None,
            pane_id: None,
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

    /// Why: the scrollback file must be readable only by the owner to prevent
    /// other processes running as the same uid from reading captured secrets.
    /// What: writes a snapshot to a temp dir and asserts the resulting file
    /// has Unix permissions 0o600 (owner rw, group/other none).
    /// Test: this is the test. Unix-only; skipped on non-Unix targets.
    #[cfg(unix)]
    #[tokio::test]
    async fn scrollback_file_has_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut record = make_record(Some(tmp.path().to_path_buf()));
        let driver = StubDriver {
            capture_result: Ok("output".into()),
            cwd: None,
        };
        capture_into(&mut record, &driver).await;
        let path = record.scrollback_path.expect("scrollback_path must be set");
        let meta = std::fs::metadata(&path).expect("file must exist");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "scrollback file must be 0600, got {mode:o}");
    }

    /// Why: pane scrollback often contains API keys or Bearer tokens that must
    /// not be persisted in plaintext; the redaction pass is the primary guard.
    /// What: writes a snapshot containing `ANTHROPIC_API_KEY=sk-abc123` and
    /// `Bearer xyz`; asserts both values are replaced with `***REDACTED***`
    /// and the keys/prefixes remain visible.
    /// Test: this is the test.
    #[tokio::test]
    async fn redaction_scrubs_secrets_before_write() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut record = make_record(Some(tmp.path().to_path_buf()));
        let raw = "ANTHROPIC_API_KEY=sk-abc123\ncurl -H \"Bearer xyz\" https://api.example.com\n";
        let driver = StubDriver {
            capture_result: Ok(raw.into()),
            cwd: None,
        };
        capture_into(&mut record, &driver).await;
        let path = record.scrollback_path.expect("scrollback_path must be set");
        let on_disk = std::fs::read_to_string(&path).expect("file must exist");
        assert!(
            !on_disk.contains("sk-abc123"),
            "raw API key must not appear on disk: {on_disk:?}"
        );
        assert!(
            !on_disk.contains("Bearer xyz"),
            "raw Bearer token must not appear on disk: {on_disk:?}"
        );
        assert!(
            on_disk.contains("***REDACTED***"),
            "redaction marker must be present: {on_disk:?}"
        );
        assert!(
            on_disk.contains("ANTHROPIC_API_KEY=***REDACTED***"),
            "full key name must be preserved: {on_disk:?}"
        );
    }
}
