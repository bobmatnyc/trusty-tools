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

/// The whole tmux argv for `command` on `server` (#10101).
///
/// Why: a call that builds its own argv can forget the `-L` prefix and reach
/// the default server, where the Architect's pane lives. One builder for every
/// `TmuxDriver` call and every core spawn helper leaves no such path.
/// What: [`server_args`] then `command` when `server` is `Some`; `command`
/// unchanged when `None`, so a default-server call is byte-identical to the
/// argv it had before this function existed.
/// Test: `server_argv_prefixes_only_a_named_server`, and the per-method
/// `*_reaches_the_named_server` tests in `daemon/tmux_tests.rs`.
pub fn server_argv(server: Option<&str>, command: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut argv: Vec<String> = server.into_iter().flat_map(server_args).collect();
    argv.extend(command);
    argv
}

/// How long one tmux server probe (a socket connect, or one listing) may take
/// before the server reads as unknown (#10101).
pub const PROBE_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// The tmux servers with a probe in flight, by socket path (#10101).
static PROBES_IN_FLIGHT: std::sync::Mutex<std::collections::BTreeSet<PathBuf>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Holds `key` in [`PROBES_IN_FLIGHT`]; dropping it, on return or unwind,
/// frees the server for its next probe.
struct InFlight(PathBuf);

impl InFlight {
    /// Claim `key`; `None` when a probe of that server is already in flight.
    fn claim(key: &Path) -> Option<Self> {
        let mut held = PROBES_IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.insert(key.to_path_buf())
            .then(|| Self(key.to_path_buf()))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        PROBES_IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// Whether a probe of the server at socket `key` is in flight (#10101).
/// Test: `a_server_with_a_probe_in_flight_is_not_probed_again`.
pub fn probe_in_flight(key: &Path) -> bool {
    PROBES_IN_FLIGHT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(key)
}

/// Run `probe` of the tmux server at socket `key` within `budget`, at most
/// one at a time per server process-wide (#10101).
///
/// Why: a probe that outlives its budget is abandoned with its thread and its
/// tmux client still blocked. Against a hung server, one new probe per reap
/// tick, dedup check or by-name call would pile up threads and clients
/// without bound.
/// What: `None` at once, with nothing spawned, while an earlier probe of
/// `key` is still running; otherwise `probe` runs on its own thread and this
/// waits at most `budget`, `None` on expiry or a thread that cannot start.
/// The server is freed when `probe` returns, before its answer is sent.
/// Test: `a_server_with_a_probe_in_flight_is_not_probed_again`,
/// `a_hung_workspace_server_holds_one_probe_and_refuses_the_call`.
pub fn bounded_probe<T: Send + 'static>(
    key: &Path,
    budget: std::time::Duration,
    probe: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let held = InFlight::claim(key)?;
    bounded_call(budget, move || {
        let _held = held;
        probe()
    })
}

/// `call` on its own thread, waited on for at most `budget`; `None` on expiry
/// or a thread that cannot start. Not limited per server: a caller that names
/// a server uses [`bounded_probe`].
/// Test: `a_server_with_a_probe_in_flight_is_not_probed_again`.
pub fn bounded_call<T: Send + 'static>(
    budget: std::time::Duration,
    call: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("tmux-probe".to_owned())
        .spawn(move || {
            let _ = tx.send(call());
        })
        .ok()?;
    rx.recv_timeout(budget).ok()
}

/// Whether the Unix socket at `path` accepts a connection within `budget`:
/// `Some(true)` it did, `Some(false)` no server listens there,
/// `None` it did not answer in time or the connect failed some other way
/// (#10101).
///
/// Why: a stuck server can hold a blocking `connect` without bound, and every
/// caller of this probe runs on a path that must not hang on tmux.
/// What: the connect runs through [`bounded_probe`] keyed on `path`, so a
/// server with a probe in flight reads `None` without another connect.
/// Test: `a_socket_probe_answers_and_refuses`.
pub fn socket_answers(path: &Path, budget: std::time::Duration) -> Option<bool> {
    let target = path.to_path_buf();
    bounded_probe(path, budget, move || {
        match std::os::unix::net::UnixStream::connect(&target) {
            Ok(_) => Some(true),
            // No server: refused, gone, or a socket of another type (EPROTOTYPE).
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) || e.raw_os_error() == Some(nix::libc::EPROTOTYPE) =>
            {
                Some(false)
            }
            Err(_) => None,
        }
    })
    .flatten()
}

/// When this host last booted, in Unix seconds; `None` when it cannot be read
/// (#10101).
///
/// Why: a reboot that wipes `/tmp` takes every workspace server's socket with
/// it, which reads like a deliberate kill; a session started before the boot
/// was lost to the reboot instead.
/// What: read once per process (the boot time does not change while it
/// runs): `btime` in `/proc/stat` on Linux, `sysctl -n kern.boottime` on
/// macOS.
/// Test: `boot_time_is_parsed_from_proc_stat_and_sysctl`.
pub fn last_host_boot() -> Option<u64> {
    static BOOT: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *BOOT.get_or_init(|| {
        if cfg!(target_os = "linux") {
            parse_proc_stat_btime(&std::fs::read_to_string("/proc/stat").ok()?)
        } else {
            let out = std::process::Command::new("sysctl")
                .args(["-n", "kern.boottime"])
                .output()
                .ok()
                .filter(|o| o.status.success())?;
            parse_kern_boottime(&String::from_utf8_lossy(&out.stdout))
        }
    })
}

/// The `btime <secs>` line of `/proc/stat`.
fn parse_proc_stat_btime(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix("btime "))
        .and_then(|v| v.trim().parse().ok())
}

/// The `sec = N` field of `{ sec = N, usec = M } <date>`.
fn parse_kern_boottime(text: &str) -> Option<u64> {
    let digits: String = text
        .split_once("sec = ")?
        .1
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The workspace servers in `socket_dir` that accept a connection, by name.
///
/// What: entries named [`WORKSPACE_SERVER_PREFIX`]`*` that are sockets and
/// answer a connect, or do not answer within [`PROBE_BUDGET`], sorted. A missing directory has none; a socket that
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
        // #10101: a socket that does not answer in time is a live, stuck
        // server — listed, so its sessions read as unknown, never gone.
        if is_socket && socket_answers(&entry.path(), PROBE_BUDGET) != Some(false) {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
#[path = "tmux_server_tests.rs"]
mod tests;
