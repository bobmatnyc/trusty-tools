//! History-safe `GH_TOKEN`/`GH_USER` delivery for managed Claude Code spawns
//! (#3025 review follow-up).
//!
//! Why: the original implementation embedded `GH_TOKEN='<value>'` literally
//! in the `env …` command line sent to the tmux pane via `send-keys`. That
//! command line is TYPED into the pane's login shell, so the secret landed in
//! `~/.zsh_history`/`~/.bash_history` on disk, and was transiently visible in
//! `ps` output for the `env` process itself (its own assignment argv). A
//! managed session's `gh` identity must be deterministic without leaving the
//! token readable outside the process environment.
//! What: [`write_gh_env_file`] writes one `export NAME='value'` line per
//! resolved `(name, value)` pair to a fresh, mode-0600 (owner-only) temp
//! file — never world- or group-readable, even momentarily (mirrors the
//! established `secure_write` pattern already used by
//! `core::oauth_token`/`session_manager::snapshot` for the same
//! create-with-restrictive-mode-atomically requirement). [`gh_env_source_prefix`]
//! renders the shell fragment that `.` (source)s the file INTO THE CURRENT
//! SHELL — so the plain `env` invocation that follows inherits `GH_TOKEN`/
//! `GH_USER` automatically, with no literal value ever appearing in the typed
//! command or any process's argv — then immediately `rm -f`s it, so the
//! value touches disk only for the instant between write and source. An
//! empty `gh_env` slice writes nothing and yields an empty prefix, keeping an
//! unconfigured project's spawn command byte-identical to pre-#3025 behaviour.
//! Test: `write_gh_env_file_writes_export_lines_in_order`, `write_gh_env_file_empty_is_none`,
//! `write_gh_env_file_is_mode_0600` (unix only), `gh_env_source_prefix_sources_and_deletes`,
//! `gh_env_source_prefix_none_is_empty` (`claude_code_gh_env_tests.rs`).

use std::path::{Path, PathBuf};

use super::shell_single_quote;

/// Write `data` to `path` with Unix mode 0600, race-free (mirrors
/// `core::oauth_token::secure_write` / `session_manager::snapshot::secure_write`
/// — a third small private copy rather than a new cross-module export, per
/// this crate's existing precedent for this exact primitive).
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

#[cfg(not(unix))]
fn secure_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)
}

/// Write a resolved `gh_env` (`GH_TOKEN`/`GH_USER`) to a mode-0600 temp file
/// of `export NAME='value'` lines, or `None` when there is nothing to write.
///
/// Why: isolates the file-write step so `ClaudeCodeAdapter::spawn`/
/// `spawn_resume` stay a one-line call; testable without touching a real
/// tmux pane.
/// What: `gh_env.is_empty()` → `None` (no file, no regression for the common
/// unconfigured case). Otherwise writes one line per pair, single-quoting
/// each value via the shared [`shell_single_quote`], to
/// `<tmp>/trusty-mpm-gh-env-<uuid>.sh`; returns `None` (logged) on any write
/// failure so the spawn proceeds without `GH_TOKEN` rather than failing.
/// Test: `write_gh_env_file_writes_export_lines_in_order`, `write_gh_env_file_empty_is_none`,
/// `write_gh_env_file_is_mode_0600`.
pub(super) fn write_gh_env_file(gh_env: &[(String, String)]) -> Option<PathBuf> {
    if gh_env.is_empty() {
        return None;
    }
    let mut content = String::new();
    for (name, value) in gh_env {
        content.push_str(&format!("export {name}={}\n", shell_single_quote(value)));
    }
    let file = std::env::temp_dir().join(format!("trusty-mpm-gh-env-{}.sh", uuid::Uuid::new_v4()));
    match secure_write(&file, content.as_bytes()) {
        Ok(()) => Some(file),
        Err(err) => {
            tracing::warn!("failed to write gh-env temp file: {err}");
            None
        }
    }
}

/// Render the `. '<path>'; rm -f '<path>'; ` shell prefix that sources
/// `file` into the current shell and immediately deletes it, or an empty
/// string when `file` is `None`.
///
/// Why: `.` (POSIX `source`) runs the file's `export` statements in the
/// CURRENT shell process, so every command that follows on the same command
/// line — including the `env …` invocation `env_bin_prefix` builds — inherits
/// `GH_TOKEN`/`GH_USER` from the process environment with no literal value in
/// its own argv. Placed BEFORE `env_bin_prefix` in [`super::spawn_command`]/
/// [`super::resume_command`]'s body, so sourcing always happens before the
/// value is needed.
/// What: `None` → `""` (no regression for a project with no pinned
/// `gh_account`). `Some(path)` → the source-and-delete fragment,
/// single-quoted via [`shell_single_quote`].
/// Test: `gh_env_source_prefix_sources_and_deletes`, `gh_env_source_prefix_none_is_empty`.
pub(super) fn gh_env_source_prefix(file: Option<&Path>) -> String {
    match file {
        Some(p) => {
            let quoted = shell_single_quote(&p.display().to_string());
            format!(". {quoted}; rm -f {quoted}; ")
        }
        None => String::new(),
    }
}
