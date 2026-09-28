//! A private default tmux server for each trusty-mpm test process (#6542).
//!
//! Why: tmux is machine-global, and every trusty-mpm test binary inherited the
//! operator's `$TMUX` and default socket. Any test, or any code under test,
//! that ran `tmux` without `-L`/`-S` therefore read and wrote the operator's
//! live server. A logged `cargo test -p trusty-mpm -- --include-ignored` run on
//! 2026-09-27 found such calls in the lib, both bin targets and four
//! integration targets — `new-session`, `kill-session` by generated name,
//! `set-option -g` — and two of them leaked `tm_proj_0` and `tm-r99488-01`
//! onto the operator's server when a run was killed. A per-test
//! `PrivateTmuxServer` (#8752) covers a test that spawns a session on purpose;
//! it cannot cover a call the test does not know the code under test makes.
//!
//! What: [`isolate_for_this_process`] points `TMUX_TMPDIR` at a fresh
//! `/tmp/tm-tmux-u<uid>/<pid>` directory and removes `TMUX` and `TMUX_PANE`.
//! tmux then puts this process's default server, and every `-L` server it
//! names, in that directory, and so do the children it spawns. The first
//! in-process tmux resolution starts that default server without the
//! operator's `~/.tmux.conf` ([`ensure_default_server`]); a process that never
//! runs tmux starts no server. [`teardown_for_this_process`] kills each server
//! there and removes the directory when the process exits. A process killed
//! before exit leaves its directory behind; the next isolation call on this
//! host reaps every such directory whose owning pid is dead.
//!
//! The reaper runs `kill-server` against paths it finds on disk, and the
//! operator's real server is one `kill-server` away. It therefore acts only on
//! entries it has checked without following a symlink — real directories and
//! Unix sockets owned by this uid, inside directories no other uid can write —
//! and re-checks each one's identity immediately before acting. A directory's
//! pid basename only nominates it; ownership decides.
//!
//! These are `pub` only because integration targets link the non-test lib.
//! Their only callers are the pre-`main` constructors and exit destructors in
//! `test_support` (lib), `src/bin/tm/test_support.rs` and `tests/common`, plus
//! [`crate::core::tmux::resolve_tmux_binary`].
//! Test: `tests::a_relocated_directory_names_its_owner`,
//! `tests::reaping_a_directory_kills_its_servers_and_removes_it`,
//! `tests::reaping_never_follows_a_planted_symlink_or_touches_a_non_socket`,
//! `tests::only_an_unchanged_owned_socket_or_private_dir_is_reapable`,
//! `tests::this_test_binary_runs_on_a_relocated_tmux_server`,
//! `tests::the_first_resolution_starts_a_config_free_default_server`.

use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};

/// Basename prefix of the per-uid parent under [`RELOCATION_ROOT`]. The `u`
/// keeps `tm-tmux-u<uid>` from parsing as the `tm-tmux-<pid>` directory an
/// earlier revision of this module reaped.
const DIR_PREFIX: &str = "tm-tmux-u";

/// Where the per-uid parent lives. Short on purpose: tmux refuses a socket
/// path over the platform's `sun_path` limit (104 bytes on macOS), and every
/// `-L` socket name a fixture mints is appended under a directory in here.
const RELOCATION_ROOT: &str = "/tmp";

/// The directory [`isolate_for_this_process`] relocated this process into.
static RELOCATED: OnceLock<PathBuf> = OnceLock::new();

/// The real tmux binary, resolved once — in the constructor, before any test
/// can point `PATH` at a fake tmux.
static TMUX_BIN: OnceLock<Option<String>> = OnceLock::new();

/// Guards the one config-free start of the relocated default server.
static DEFAULT_SERVER: Once = Once::new();

/// Relocate this process's tmux servers into a private directory, once.
///
/// Why: see the module docs.
/// What: creates or verifies the per-uid parent `/tmp/tm-tmux-u<uid>` (mode
/// 0700, owned by this uid, not a symlink), reaps dead-owner directories in
/// it, creates `<parent>/<pid>`, resolves the tmux binary, then sets
/// `TMUX_TMPDIR` and removes `TMUX` and `TMUX_PANE`. It starts no tmux server.
/// A second call returns the first call's directory and writes nothing.
///
/// The caller must run before any other thread exists, because this writes
/// the process environment. Every caller is a `#[ctor::ctor]`.
/// Errors when the parent is not a private directory of this uid — another
/// local user may have planted it — or a directory cannot be created; the
/// environment is then left untouched, and the caller is expected to abort
/// rather than let tests reach the operator's server.
/// Test: `tests::this_test_binary_runs_on_a_relocated_tmux_server`.
#[doc(hidden)]
pub fn isolate_for_this_process() -> std::io::Result<&'static Path> {
    if let Some(dir) = RELOCATED.get() {
        return Ok(dir);
    }
    let uid = effective_uid();
    let parent = private_parent(uid)?;
    // #6542: resolve before any test can mutate `PATH`; reaping reuses it.
    tmux_bin();
    reap_dead_owner_dirs(&parent);
    let dir = parent.join(std::process::id().to_string());
    // A directory already named for this pid belongs to a dead process that
    // held the pid before this one.
    reap_dir(&dir);
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    // SAFETY: the only callers are pre-`main` constructors, so no other thread
    // exists to read the environment concurrently.
    unsafe {
        std::env::set_var("TMUX_TMPDIR", &dir);
        std::env::remove_var("TMUX");
        std::env::remove_var("TMUX_PANE");
    }
    Ok(RELOCATED.get_or_init(|| dir))
}

/// Start the relocated default server with no config file, once, on demand.
///
/// Why: whichever tmux call first reaches the relocated default server starts
/// it, and a server started by a bare `tmux` reads the operator's
/// `~/.tmux.conf`. With tmux-continuum's `@continuum-restore` on, that
/// recreated every saved session inside each test server. Starting it here,
/// on the first in-process resolution rather than in every constructor,
/// spares the many test processes that never run tmux a server start and a
/// teardown `kill-server`.
/// What: a no-op outside a relocated process. Otherwise runs `-f /dev/null
/// start-server ; set-option -g exit-empty off` through the constructor's
/// binary, so the server outlives having no sessions until
/// [`teardown_for_this_process`] kills it. Concurrent callers wait for the
/// first. Best-effort: with no tmux on the host there is no server to start.
/// Test: `tests::the_first_resolution_starts_a_config_free_default_server`.
#[doc(hidden)]
pub fn ensure_default_server() {
    if RELOCATED.get().is_none() {
        return;
    }
    DEFAULT_SERVER.call_once(|| {
        let args = [
            "-f",
            "/dev/null",
            "start-server",
            ";",
            "set-option",
            "-g",
            "exit-empty",
            "off",
        ]
        .map(String::from);
        if let Some(bin) = tmux_bin() {
            let _ = crate::core::spawn_disclaim::disclaimed_output(bin, &args);
        }
    });
}

/// The directory this process was relocated into, if it was.
#[doc(hidden)]
pub fn relocated_dir() -> Option<&'static Path> {
    RELOCATED.get().map(PathBuf::as_path)
}

/// Kill every tmux server this process relocated, then remove its directory.
///
/// A no-op when [`isolate_for_this_process`] never ran. Best-effort: it runs
/// from an exit destructor, where a panic would abort the process.
/// Test: `tests::reaping_a_directory_kills_its_servers_and_removes_it`.
#[doc(hidden)]
pub fn teardown_for_this_process() {
    if let Some(dir) = RELOCATED.get() {
        reap_dir(dir);
    }
}

/// The tmux binary the constructor resolved; `None` when the host has none.
///
/// #6542: `bin_resolve` directly, not `resolve_tmux_binary`, which calls
/// [`ensure_default_server`] and would re-enter it.
fn tmux_bin() -> Option<&'static str> {
    TMUX_BIN
        .get_or_init(|| {
            trusty_common::bin_resolve::resolve_binary("tmux")
                .and_then(|p| p.to_str().map(str::to_string))
        })
        .as_deref()
}

/// This process's effective uid: the owner of everything it creates.
fn effective_uid() -> u32 {
    // SAFETY: `geteuid` takes no arguments, reads a process property and
    // cannot fail.
    unsafe { libc::geteuid() }
}

/// Create `/tmp/tm-tmux-u<uid>` with mode 0700, or accept it only when it is
/// already a private directory of `uid`.
fn private_parent(uid: u32) -> std::io::Result<PathBuf> {
    let parent = Path::new(RELOCATION_ROOT).join(format!("{DIR_PREFIX}{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&parent) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    match checked(&parent, Kind::PrivateDir) {
        Some(_) => Ok(parent),
        None => Err(std::io::Error::other(format!(
            "#6542: {} is not a directory owned by uid {uid} and closed to other \
             users; remove it and re-run",
            parent.display()
        ))),
    }
}

/// The pid a relocated directory's basename names as its owner.
///
/// Test: `tests::a_relocated_directory_names_its_owner`.
fn owner_pid(basename: &str) -> Option<u32> {
    if basename.is_empty() || !basename.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    basename.parse().ok()
}

/// Reap every relocated directory under `parent` whose owner pid is dead.
///
/// The pid only nominates a directory; [`reap_dir`] still refuses anything
/// not owned by this uid.
fn reap_dead_owner_dirs(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let owner = entry.file_name().to_str().and_then(owner_pid);
        if owner.is_some_and(|pid| {
            pid != std::process::id() && !crate::core::daemon_identity::pid_alive(pid)
        }) {
            reap_dir(&entry.path());
        }
    }
}

/// What an entry must be for the reaper to act on it.
#[derive(Clone, Copy)]
enum Kind {
    /// A real directory no other uid can add entries to.
    PrivateDir,
    /// A Unix socket.
    Socket,
}

/// An inode, as the reaper saw it when it decided to act.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Identity {
    dev: u64,
    ino: u64,
}

/// Whether `meta`, read WITHOUT following a symlink, is a `kind` entry owned
/// by `uid`.
///
/// Test: `tests::only_an_unchanged_owned_socket_or_private_dir_is_reapable`.
fn reapable(meta: &std::fs::Metadata, uid: u32, kind: Kind) -> bool {
    let file_type = meta.file_type();
    if file_type.is_symlink() || meta.uid() != uid {
        return false;
    }
    match kind {
        Kind::PrivateDir => file_type.is_dir() && meta.mode() & 0o022 == 0,
        Kind::Socket => file_type.is_socket(),
    }
}

/// `path`'s identity when it is a reapable `kind` entry of this uid, checked
/// with `symlink_metadata` so a planted link is never followed.
fn checked(path: &Path, kind: Kind) -> Option<Identity> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    reapable(&meta, effective_uid(), kind).then(|| Identity {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

/// Kill the server behind every socket under `dir`, then remove what it
/// killed and every directory left empty.
///
/// What: tmux keeps its sockets in `<TMUX_TMPDIR>/tmux-<uid>/`, so this walks
/// one level of subdirectories and sends `kill-server` through each socket
/// with `-S`. Killing a server also kills every pane process it holds, which
/// removing the directory alone would leave running. `dir` and each
/// subdirectory must be private directories of this uid and each socket a
/// socket of this uid, all checked without following a symlink; immediately
/// before each `kill-server` and each removal, the whole chain is re-checked
/// against the inodes first seen. Anything else — a symlink, a regular file,
/// another uid's entry — is left in place, so its directory stays too.
/// Test: `tests::reaping_a_directory_kills_its_servers_and_removes_it`,
/// `tests::reaping_never_follows_a_planted_symlink_or_touches_a_non_socket`.
fn reap_dir(dir: &Path) {
    let Some(dir_id) = checked(dir, Kind::PrivateDir) else {
        return;
    };
    let Ok(subdirs) = std::fs::read_dir(dir) else {
        return;
    };
    for sub in subdirs.flatten().map(|entry| entry.path()) {
        let Some(sub_id) = checked(&sub, Kind::PrivateDir) else {
            continue;
        };
        let Ok(sockets) = std::fs::read_dir(&sub) else {
            continue;
        };
        for socket in sockets.flatten().map(|entry| entry.path()) {
            let Some(socket_id) = checked(&socket, Kind::Socket) else {
                continue;
            };
            let unchanged = || {
                checked(dir, Kind::PrivateDir) == Some(dir_id)
                    && checked(&sub, Kind::PrivateDir) == Some(sub_id)
                    && checked(&socket, Kind::Socket) == Some(socket_id)
            };
            if !unchanged() {
                continue;
            }
            if let Some(bin) = tmux_bin() {
                let args = [
                    "-S".to_string(),
                    socket.to_string_lossy().into_owned(),
                    "kill-server".to_string(),
                ];
                // #7060: the disclaiming primitive every production tmux spawn uses.
                let _ = crate::core::spawn_disclaim::disclaimed_output(bin, &args);
            }
            if unchanged() {
                let _ = std::fs::remove_file(&socket);
            }
        }
        // `remove_dir` never follows a link and fails, leaving the directory,
        // when an entry above was left in place.
        if checked(&sub, Kind::PrivateDir) == Some(sub_id) {
            let _ = std::fs::remove_dir(&sub);
        }
    }
    if checked(dir, Kind::PrivateDir) == Some(dir_id) {
        let _ = std::fs::remove_dir(dir);
    }
}

#[cfg(test)]
#[path = "tmux_test_isolation_tests.rs"]
mod tests;
