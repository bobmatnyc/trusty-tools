//! The private tmux server of a workspace (#8928 step 4).
//!
//! Why: under the Claude Code sandbox a PM reaches tmux only through a socket
//! its settings allow by resolved path, and that grant covers every verb on
//! the server, `send-keys` included. On the default server it would open the
//! Architect's pane, so each workspace gets a server of its own (#8928's owner
//! ruling of 2026-09-30: one tmux server per workspace). Every tmux call goes
//! to the default server today.
//! What: [`workspace_server_name`] names a workspace's server
//! (`tm-ws-<slug>-<hash>`, stable for the canonical path);
//! [`socket_dir`] / [`ambient_socket_dir`] resolve tmux's socket directory the
//! way tmux does (`$TMUX_TMPDIR` or `/tmp`, then `tmux-<uid>`), symlinks
//! followed, so the sandbox entry is `/private/tmp/tmux-<uid>/<name>`;
//! [`server_args`] is the `-L` prefix a call to that server carries; and
//! [`live_workspace_servers`] lists the workspace servers that answer, which
//! is how the daemon snapshot and the fleet poller find them. Launching a PM
//! on its server is step 6 (every launch path).
//! Test: `tmux_server_tests.rs`.

use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The name prefix every workspace server carries.
pub const WORKSPACE_SERVER_PREFIX: &str = "tm-ws-";

/// Longest workspace slug kept in a server name.
const SLUG_MAX: usize = 24;

/// The server name for `workspace`: `tm-ws-<slug>-<hash>`.
///
/// What: the canonical path when it resolves (else the path as given) is
/// hashed with SHA-256 and the first 12 hex digits kept; the slug is the
/// basename lowercased, every other character than `[a-z0-9]` turned to `-`,
/// cut to [`SLUG_MAX`]. Two spellings of one directory name one server; two
/// directories with one basename do not collide.
/// Test: `a_workspace_name_is_stable_and_distinct`,
/// `a_workspace_name_is_a_safe_socket_name`.
pub fn workspace_server_name(workspace: &Path) -> String {
    let canonical = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let hash: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    let base = canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let slug: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(SLUG_MAX)
        .collect();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        format!("{WORKSPACE_SERVER_PREFIX}{hash}")
    } else {
        format!("{WORKSPACE_SERVER_PREFIX}{slug}-{hash}")
    }
}

/// tmux's socket directory for `uid`: `<tmux_tmpdir or /tmp>/tmux-<uid>`,
/// with the base's symlinks followed (`/tmp` is `/private/tmp` on macOS).
/// Test: `the_socket_dir_follows_the_tmp_symlink`.
pub fn socket_dir(tmux_tmpdir: Option<&Path>, uid: u32) -> PathBuf {
    let base = tmux_tmpdir
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("/tmp"));
    let base = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    base.join(format!("tmux-{uid}"))
}

/// [`socket_dir`] for this process: `$TMUX_TMPDIR` and the real uid.
pub fn ambient_socket_dir() -> PathBuf {
    let tmpdir = std::env::var_os(crate::core::tmux_access::TMUX_TMPDIR_ENV).map(PathBuf::from);
    socket_dir(tmpdir.as_deref(), nix::unistd::getuid().as_raw())
}

/// The socket a named server listens on: `<socket_dir>/<name>`.
/// Test: `the_socket_dir_follows_the_tmp_symlink`.
pub fn server_socket_path(socket_dir: &Path, name: &str) -> PathBuf {
    socket_dir.join(name)
}

/// The `-L <name>` words that select a named server, ahead of the command.
/// Test: `server_args_select_the_named_server`.
pub fn server_args(name: &str) -> [String; 2] {
    ["-L".to_owned(), name.to_owned()]
}

/// The workspace servers in `socket_dir` that accept a connection, by name.
///
/// What: entries named [`WORKSPACE_SERVER_PREFIX`]`*` that are sockets and
/// answer a connect, sorted. A missing directory has none; a socket that
/// refuses (a server that exited) is skipped.
///
/// # Errors
///
/// The directory exists and cannot be read.
///
/// Test: `live_servers_are_listed_and_dead_ones_skipped`.
pub fn live_workspace_servers(socket_dir: &Path) -> std::io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(socket_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(WORKSPACE_SERVER_PREFIX) {
            continue;
        }
        let is_socket = entry.file_type().is_ok_and(|t| t.is_socket());
        if is_socket && std::os::unix::net::UnixStream::connect(entry.path()).is_ok() {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
#[path = "tmux_server_tests.rs"]
mod tests;
