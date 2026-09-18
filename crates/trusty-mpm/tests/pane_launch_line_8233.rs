//! The managed launch must never hand a pane a line its tty can truncate
//! (#8233).
//!
//! Why an INTEGRATION test rather than a `claude_code_tests.rs` case: this is
//! the one assertion that has to be runnable against the code BEFORE the fix, to
//! prove it was red there. Everything it touches — `ClaudeCodeAdapter`,
//! `RuntimeAdapter::spawn`, `ManagedTmuxDriver` — is public library API that
//! predates the fix and survived it unchanged, so the same file compiles and
//! runs on either side. A unit test would have had to name `LaunchSpec` or
//! `MAX_PANE_COMMAND_BYTES`, neither of which exists pre-fix, and could then
//! only ever have been green.
//!
//! What: drives the REAL spawn path against a recording tmux driver and a
//! worst-case-but-ordinary launch (a deeply nested worktree cwd, a pinned `gh`
//! identity), then asserts on the bytes the driver was asked to type. The
//! numbers are spelled as literals for the same reason: a constant imported from
//! the crate would move with the fix.
//!
//! Test: this file IS the test.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use trusty_mpm::runtime::{ClaudeCodeAdapter, RuntimeAdapter};
use trusty_mpm::session_manager::{ManagedError, ManagedTmuxDriver};

/// macOS `MAX_CANON` — the canonical-mode line-discipline buffer a pane's tty
/// holds before the shell's line editor takes over (`sys/syslimits.h:89`;
/// `fpathconf(pty, _PC_MAX_CANON)` agrees). Bytes past it are dropped in the
/// KERNEL, where nothing downstream can see the loss.
const MAX_CANON: usize = 1024;

/// The ceiling this test enforces, with headroom under [`MAX_CANON`] for the
/// terminating newline and line-discipline overhead. Deliberately a literal: the
/// production constant did not exist before the fix.
const CEILING: usize = 960;

/// Serialises the `HOME`/`PATH` rewrites the two cases share.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Records every line the adapter asks tmux to type, session- or pane-scoped.
#[derive(Default)]
struct Recorder {
    typed: Mutex<Vec<String>>,
}

impl Recorder {
    fn typed(&self) -> Vec<String> {
        self.typed.lock().expect("recorder mutex").clone()
    }
}

impl ManagedTmuxDriver for Recorder {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, _name: &str, text: &str) -> Result<(), ManagedError> {
        self.typed
            .lock()
            .expect("recorder mutex")
            .push(text.to_owned());
        Ok(())
    }
    fn send_line_to_pane(&self, _name: &str, _pane: &str, text: &str) -> Result<(), ManagedError> {
        self.typed
            .lock()
            .expect("recorder mutex")
            .push(text.to_owned());
        Ok(())
    }
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }
}

/// Restores `HOME` and `PATH` when the case ends, panic or not.
struct EnvGuard {
    home: Option<String>,
    path: Option<std::ffi::OsString>,
}

impl EnvGuard {
    /// Point `HOME` at `home` and prepend `bin` to `PATH`.
    fn install(home: &Path, bin: &Path) -> Self {
        let prev_home = std::env::var("HOME").ok();
        let prev_path = std::env::var_os("PATH");
        let mut entries = vec![bin.to_path_buf()];
        if let Some(ref p) = prev_path {
            entries.extend(std::env::split_paths(p));
        }
        let joined = std::env::join_paths(entries).expect("join PATH");
        // SAFETY: every caller holds `env_lock`, and Drop restores both.
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("PATH", joined);
        }
        Self {
            home: prev_home,
            path: prev_path,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: as in `install`.
        unsafe {
            match self.home.take() {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
            match self.path.take() {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

/// An executable stub named `claude`, so the spawn path is entered on a machine
/// with no Claude Code install (a silently skipped test would be worse than none).
#[cfg(unix)]
fn plant_fake_claude(bin: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin).expect("mkdir bin");
    let exe = bin.join("claude");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").expect("write fake claude");
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A realistic worst case: the agent-worktree layout tm itself provisions, under
/// a long but perfectly ordinary project path.
fn deep_worktree(home: &Path) -> PathBuf {
    let cwd = home.join(
        "trusty-mpm-projects/bobmatnyc/trusty-tools/.claude/worktrees/\
         agent-a6a2f029ef1938cc6-engineer-session",
    );
    std::fs::create_dir_all(&cwd).expect("mkdir worktree");
    cwd
}

/// A pinned `gh` identity, as `resolve_gh_env` hands it to the adapter — an
/// ordinary GitHub token, not an inflated one.
fn gh_env() -> Vec<(String, String)> {
    vec![
        ("GH_TOKEN".to_owned(), format!("gho_{}", "A".repeat(36))),
        ("GH_USER".to_owned(), "bobmatnyc".to_owned()),
        (
            "GH_CONFIG_DIR".to_owned(),
            "/Users/masa/.config/gh-bobmatnyc".to_owned(),
        ),
    ]
}

/// #8233, the defect itself: the launch used to be TYPED into the pane as one
/// shell script, and its length grew with the cwd, `TMPDIR`, every env
/// assignment and every flag. At 1054 bytes it crossed `MAX_CANON`, the line
/// discipline dropped the tail, and session `dd0e2fb8-…` died with a command cut
/// off mid-path — invisibly, because the loss happens in the kernel.
///
/// This is the regression guard: whatever the launch carries, the bytes typed at
/// the pane's shell must fit the canonical buffer with room to spare.
#[test]
#[cfg(unix)]
fn a_worst_case_managed_spawn_never_types_a_line_the_tty_can_truncate() {
    let _lock = env_lock();
    let home = tempfile::tempdir().expect("tempdir");
    let bin = home.path().join("bin");
    plant_fake_claude(&bin);
    let _env = EnvGuard::install(home.path(), &bin);

    let cwd = deep_worktree(home.path());
    let tmux = Arc::new(Recorder::default());
    let adapter = ClaudeCodeAdapter::new(tmux.clone(), Some(true));

    adapter
        .spawn(
            "tm-trusty-tools-01",
            &cwd,
            "implement the parameterised pane launch",
            "c342df68-3e15-4b50-93fc-f24bcbae76c2",
            &gh_env(),
        )
        .expect("the spawn path must run with claude on PATH");

    let typed = tmux.typed();
    assert!(!typed.is_empty(), "the adapter must type something");
    for line in &typed {
        assert!(
            line.len() <= CEILING,
            "a {}-byte line is typed at the pane's shell; the tty's canonical buffer \
             holds at most MAX_CANON ({MAX_CANON}) bytes, so anything over {CEILING} is \
             silently truncated mid-command (#8233). Line: {line}",
            line.len()
        );
    }
}

/// The same invariant on the RESUME path, which #8233's own evidence shows is a
/// second caller of the defective builder (`resume relaunch did not take: no
/// runtime came up in pane …`, three occurrences on the issue). A fix applied to
/// the spawn builder alone would leave this red.
#[test]
#[cfg(unix)]
fn a_worst_case_managed_resume_never_types_a_line_the_tty_can_truncate() {
    let _lock = env_lock();
    let home = tempfile::tempdir().expect("tempdir");
    let bin = home.path().join("bin");
    plant_fake_claude(&bin);
    let _env = EnvGuard::install(home.path(), &bin);

    let cwd = deep_worktree(home.path());
    let tmux = Arc::new(Recorder::default());
    let adapter = ClaudeCodeAdapter::new(tmux.clone(), Some(true));

    adapter
        .spawn_resume(
            "tm-trusty-tools-01",
            Some("%7"),
            &cwd,
            "implement the parameterised pane launch",
            Some("7f3c1a90-0000-4000-8000-0123456789ab"),
            "c342df68-3e15-4b50-93fc-f24bcbae76c2",
            &gh_env(),
        )
        .expect("the resume path must run with claude on PATH");

    let typed = tmux.typed();
    assert!(!typed.is_empty(), "the adapter must type something");
    for line in &typed {
        assert!(
            line.len() <= CEILING,
            "a {}-byte resume line is typed at the pane's shell; over {CEILING} bytes the \
             tty's canonical buffer (MAX_CANON {MAX_CANON}) drops the tail (#8233). \
             Line: {line}",
            line.len()
        );
    }
}
