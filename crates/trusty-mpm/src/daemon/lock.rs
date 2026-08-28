//! Daemon lock file: written on bind, removed on shutdown by its owner.
//!
//! Why: clients discover the daemon's actual bound address here, which differs
//! from the compiled-in default whenever port auto-selection kicked in.
//! What: a thin daemon-side facade over [`crate::core::daemon_identity`], which
//! owns the record format, the product-magic check, and the ownership rule for
//! removal. This module exists so the daemon's call sites read as
//! `lock::write_lock(&url)` rather than reaching across into `core`.
//! Test: the record's behaviour is covered hermetically in
//! `core::daemon_identity`. This facade carries no test of its own — the note
//! at the foot of this file says why (#1731).

use crate::core::daemon_identity;

/// Write the lock file with the actual bound address, the Unix socket, and this
/// process's PID.
///
/// Why: must be called after `TcpListener::local_addr()` is known. As of
/// ADR-0011's loopback-only doctrine (issue #3330) the daemon never binds a
/// second (Tailscale) listener, so the record carries no `tailscale_addr`.
/// `socket` is the path this daemon serves alongside `addr` (#6288) — recorded
/// so a reader can tell a socket-serving daemon from a pre-#6288 one.
/// What: delegates to [`daemon_identity::write_lock`]; best-effort.
/// Test: `read_lock_returns_written_record` in `core::daemon_identity`.
pub fn write_lock(addr: &str, socket: &str) {
    daemon_identity::write_lock(addr, socket);
}

/// Remove the lock file on clean shutdown — but only if it still names us.
///
/// Why (#1731): this used to delete whatever file sat at the path. During a
/// restart the incoming daemon binds and writes its own record while the
/// outgoing daemon is still running its shutdown handler, so an unconditional
/// delete left a live daemon with no lock file — the state the reopened issue
/// reports.
/// What: delegates to [`daemon_identity::remove_lock_owned_by`] naming this
/// process, so a successor's record survives.
/// Test: `remove_lock_owned_by_keeps_newer_daemons_record` in
/// `core::daemon_identity`.
pub fn remove_lock() {
    daemon_identity::remove_lock_owned_by(&[std::process::id()]);
}

// #1731: there is deliberately no test here. The test this file used to carry
// called `write_lock` / `remove_lock` against the REAL `~/.trusty-mpm/daemon.lock`
// to prove they "don't panic" — so every `cargo test -p trusty-mpm` run
// overwrote a live daemon's record with the test process's PID and then deleted
// it, leaving a running daemon with no lock file. That is the reported state.
// The behaviour these two functions delegate to is covered hermetically, against
// a tempdir path, in `core::daemon_identity`.
