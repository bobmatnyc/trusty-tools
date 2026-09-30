//! Host probe for the `tcp_listeners` doctor row (#8926): which processes hold
//! a TCP socket in LISTEN state.
//!
//! Why: the row must not shell out to `lsof`/`netstat`, whose output format and
//! presence vary by host. The kernel exposes the same facts directly.
//! What: macOS reads libproc (`proc_listallpids`, `proc_pidpath`,
//! `proc_pidinfo(PROC_PIDLISTFDS)`, `proc_pidfdinfo(PROC_PIDFDSOCKETINFO)`);
//! Linux reads `/proc/net/tcp{,6}` and maps each LISTEN inode to a pid through
//! `/proc/<pid>/fd`. Only processes whose executable basename passes `want`
//! have their descriptors read. Any other platform is an error, so the row
//! reports UNKNOWN rather than OK.
//! Test: `doctor_tcp_listeners_tests.rs` (`procfs_*`, `parse_proc_net_tcp_*`,
//! `the_host_probe_finds_a_listener_this_test_holds`).

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;

use super::{ListenSocket, ProbeReport, Uninspected};

/// List LISTEN sockets of the processes `want` accepts, on this host.
///
/// What: dispatches to the platform probe; `Err` when the probe could not run.
/// Test: `the_host_probe_finds_a_listener_this_test_holds`.
pub(crate) fn listening_sockets(want: fn(&str) -> bool) -> Result<ProbeReport, String> {
    #[cfg(target_os = "macos")]
    {
        macos::listening_sockets(want)
    }
    #[cfg(target_os = "linux")]
    {
        procfs_listening_sockets(Path::new("/proc"), want)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = want;
        Err(format!(
            "no TCP-listener probe for {}",
            std::env::consts::OS
        ))
    }
}

/// Parse `/proc/net/tcp` or `/proc/net/tcp6` into LISTEN inode -> (addr, port).
///
/// Why: the kernel prints each address as native-endian 32-bit words in hex, so
/// the decode is its own tested step.
/// What: skips the header; keeps rows whose state (`st`) is `0A` (LISTEN).
/// Any row it cannot parse is an error, so a format change fails closed.
/// Test: `parse_proc_net_tcp_decodes_ipv4_and_ipv6`,
/// `parse_proc_net_tcp_refuses_a_malformed_row`.
// Compiled on every host so the fixture tests run on macOS too; only Linux
// calls it outside tests.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_proc_net_tcp(text: &str) -> Result<HashMap<u64, (String, u16)>, String> {
    let mut out = HashMap::new();
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.is_empty() {
            continue;
        }
        let bad = || format!("unparseable /proc/net/tcp row: {line}");
        let (local, state, inode) = match fields[..] {
            [_, local, _, state, _, _, _, _, _, inode, ..] => (local, state, inode),
            _ => return Err(bad()),
        };
        if state != "0A" {
            continue;
        }
        let (addr_hex, port_hex) = local.split_once(':').ok_or_else(bad)?;
        let port = u16::from_str_radix(port_hex, 16).map_err(|_| bad())?;
        let inode: u64 = inode.parse().map_err(|_| bad())?;
        let mut bytes = Vec::with_capacity(16);
        for chunk in addr_hex.as_bytes().chunks(8) {
            let word = std::str::from_utf8(chunk).map_err(|_| bad())?;
            let word = u32::from_str_radix(word, 16).map_err(|_| bad())?;
            bytes.extend_from_slice(&word.to_ne_bytes());
        }
        let addr = match <[u8; 4]>::try_from(bytes.as_slice()) {
            Ok(v4) => Ipv4Addr::from(v4).to_string(),
            Err(_) => Ipv6Addr::from(<[u8; 16]>::try_from(bytes.as_slice()).map_err(|_| bad())?)
                .to_string(),
        };
        out.insert(inode, (addr, port));
    }
    Ok(out)
}

/// Probe a procfs tree rooted at `root` (`/proc` on Linux; a fixture in tests).
///
/// What: reads `net/tcp` (required) and `net/tcp6` (optional: absent when IPv6
/// is off), then every numeric `<pid>` dir: its name from the `exe` link
/// basename (falling back to `comm`), and, when `want` accepts it, its `fd`
/// links of the form `socket:[<inode>]`. An `fd` dir that exists but cannot be
/// read makes the process [`Uninspected`]; a pid that vanished is skipped.
/// Test: `procfs_probe_maps_listen_inodes_to_trusty_pids`,
/// `procfs_probe_without_a_tcp_table_is_an_error`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn procfs_listening_sockets(
    root: &Path,
    want: fn(&str) -> bool,
) -> Result<ProbeReport, String> {
    let mut by_inode = HashMap::new();
    for table in ["net/tcp", "net/tcp6"] {
        let path = root.join(table);
        match std::fs::read_to_string(&path) {
            Ok(text) => by_inode.extend(parse_proc_net_tcp(&text)?),
            Err(e) if table == "net/tcp6" && e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        }
    }
    let dirs = std::fs::read_dir(root).map_err(|e| format!("read {}: {e}", root.display()))?;
    let mut report = ProbeReport::default();
    for entry in dirs.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let dir = entry.path();
        let Some(process) = procfs_process_name(&dir) else {
            continue;
        };
        if !want(&process) {
            continue;
        }
        let fds = match std::fs::read_dir(dir.join("fd")) {
            Ok(fds) => fds,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                report.uninspected.push(Uninspected {
                    pid,
                    process,
                    error: e.to_string(),
                });
                continue;
            }
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let Some(inode) = target
                .to_str()
                .and_then(|t| t.strip_prefix("socket:["))
                .and_then(|t| t.strip_suffix(']'))
                .and_then(|t| t.parse::<u64>().ok())
            else {
                continue;
            };
            if let Some((addr, port)) = by_inode.get(&inode) {
                report.listeners.push(ListenSocket {
                    pid,
                    process: process.clone(),
                    addr: addr.clone(),
                    port: *port,
                });
            }
        }
    }
    Ok(report)
}

/// A procfs process's executable basename: the `exe` link, else `comm`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn procfs_process_name(dir: &Path) -> Option<String> {
    if let Ok(exe) = std::fs::read_link(dir.join("exe")) {
        let name = exe.file_name()?.to_string_lossy().into_owned();
        return Some(name.trim_end_matches(" (deleted)").to_string());
    }
    std::fs::read_to_string(dir.join("comm"))
        .ok()
        .map(|s| s.trim_end().to_string())
}

#[cfg(target_os = "macos")]
#[path = "doctor_tcp_listeners_libproc.rs"]
mod macos;
