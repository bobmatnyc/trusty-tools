//! Daemon lock file: written on bind, deleted on shutdown.
//!
//! Why: Clients use the lock file to discover the daemon's actual address
//! (which may differ from the configured port if auto-selection kicked in).
//! The file lives at `~/.trusty-mpm/daemon.lock` — the same framework root
//! every other artifact uses — so the daemon and its clients always agree on
//! its location.
//! What: `write_lock` creates the TOML file; `remove_lock` deletes it.
//! Both are best-effort — errors are logged but not fatal.
//! Test: `write_lock` / `remove_lock` round-trip covered below.

use crate::core::lock_file_path;

/// Write the daemon lock file with the actual bound address and current PID.
///
/// Why: Must be called after `TcpListener::local_addr()` is known.
/// What: Creates parent dirs if needed, writes TOML, logs on error.
pub fn write_lock(addr: &str, tailscale_addr: Option<&str>) {
    let path = lock_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let pid = std::process::id();
    let ts_line = tailscale_addr
        .map(|a| format!("\ntailscale_addr = \"{a}\""))
        .unwrap_or_default();
    let started_at = chrono::Utc::now().to_rfc3339();
    let content =
        format!("pid = {pid}\naddr = \"{addr}\"{ts_line}\nstarted_at = \"{started_at}\"\n");
    if let Err(e) = std::fs::write(&path, content) {
        tracing::warn!("failed to write daemon lock file {}: {e}", path.display());
    } else {
        tracing::debug!("lock file written: {}", path.display());
    }
}

/// Remove the daemon lock file on clean shutdown.
///
/// Why: A deleted lock file signals that no daemon is running, avoiding stale
/// reads by clients that check the file before falling back to the default URL.
/// What: Best-effort `fs::remove_file`; silently ignores "not found".
pub fn remove_lock() {
    let path = lock_file_path();
    match std::fs::remove_file(&path) {
        Ok(()) => tracing::debug!("lock file removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("failed to remove lock file: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_remove_round_trip() {
        // Just verify the functions don't panic on a writable path.
        write_lock("http://127.0.0.1:7880", None);
        remove_lock();
    }
}
