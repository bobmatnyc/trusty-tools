//! The pid behind the console's `http_addr` (#11085, AC10).
//!
//! Why: a console whose UDS bind failed keeps serving HTTP (#10902), so the
//! module's socket path cannot find it. The `http_addr` file names its TCP
//! listener; this file turns that into a pid. The pid is a hint only: the
//! caller hands it to `stop_pid`/`restart_pid`, whose identity check decides
//! whether it is this console before any signal.
//! What: [`lookup`] reads the file, finds the LISTEN socket bound to exactly
//! that address in `/proc/net/tcp{,6}`, requires it to belong to this uid,
//! and finds the one process holding its inode. Every undecidable case is an
//! error, never a guess: an unreadable or unparseable file, two listeners, a
//! listener of another uid, no holder, or two holders.
//! Test: `proc_net_tcp_lines_parse_to_listeners`,
//! `addr_lookup_finds_this_process_and_nothing_on_a_dead_port`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;

use anyhow::{Context as _, Result, bail};

/// What the `http_addr` file leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddrLookup {
    /// The file does not exist.
    NoAddrFile,
    /// The file names an address nothing listens on.
    NoListener(SocketAddr),
    /// One process of this uid holds the listener on the address.
    Pid(u32, SocketAddr),
}

/// One LISTEN row of `/proc/net/tcp{,6}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Listener {
    pub(crate) addr: SocketAddr,
    pub(crate) uid: u32,
    pub(crate) inode: u64,
}

/// TCP state code for LISTEN in `/proc/net/tcp`.
const TCP_LISTEN: &str = "0A";

/// Resolve `addr_file` to the pid listening on the address it names.
///
/// Test: `addr_lookup_finds_this_process_and_nothing_on_a_dead_port`,
/// `addr_lookup_refuses_an_unparseable_file`.
pub(crate) fn lookup(addr_file: &Path) -> Result<AddrLookup> {
    let raw = match std::fs::read_to_string(addr_file) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(AddrLookup::NoAddrFile),
        Err(e) => {
            return Err(e).with_context(|| format!("read {}", addr_file.display()));
        }
    };
    let addr: SocketAddr = raw.trim().parse().with_context(|| {
        format!(
            "{} holds {:?}, not a socket address; cannot find the console by it",
            addr_file.display(),
            raw.trim()
        )
    })?;
    let table = if addr.is_ipv4() {
        "/proc/net/tcp"
    } else {
        "/proc/net/tcp6"
    };
    let text = std::fs::read_to_string(table).with_context(|| format!("read {table}"))?;
    let rows: Vec<Listener> = text
        .lines()
        .skip(1)
        .filter_map(parse_listener)
        .filter(|l| l.addr == addr)
        .collect();
    let listener = match rows.as_slice() {
        [] => return Ok(AddrLookup::NoListener(addr)),
        [one] => *one,
        _ => bail!(
            "{} sockets listen on {addr}; cannot tell which is the console",
            rows.len()
        ),
    };
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if listener.uid != euid {
        bail!(
            "{addr} is held by uid {}, not this user (uid {euid}); not the console to manage",
            listener.uid
        );
    }
    match holders(listener.inode)?.as_slice() {
        [] => bail!("a socket of this user listens on {addr}, but no readable process holds it"),
        [pid] => Ok(AddrLookup::Pid(*pid, addr)),
        many => bail!("pids {many:?} all hold the listener on {addr}; cannot tell which to manage"),
    }
}

/// Pids whose fd table holds `socket:[inode]`. Processes whose `fd` dir
/// cannot be read (another user's, or one that exited) are skipped: the pid
/// found still has to pass the module's identity check before any signal.
fn holders(inode: u64) -> Result<Vec<u32>> {
    let wanted = format!("socket:[{inode}]");
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").context("read /proc")? {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        let holds = fds.flatten().any(|fd| {
            std::fs::read_link(fd.path()).is_ok_and(|t| t.as_os_str() == wanted.as_str())
        });
        if holds {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    Ok(pids)
}

/// A LISTEN row of `/proc/net/tcp{,6}`; `None` for any other row.
///
/// Test: `proc_net_tcp_lines_parse_to_listeners`.
pub(crate) fn parse_listener(line: &str) -> Option<Listener> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 10 || fields[3] != TCP_LISTEN {
        return None;
    }
    let (ip_hex, port_hex) = fields[1].split_once(':')?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    Some(Listener {
        addr: SocketAddr::new(parse_ip(ip_hex)?, port),
        uid: fields[7].parse().ok()?,
        inode: fields[9].parse().ok()?,
    })
}

/// The kernel prints each 32-bit word of the address in host byte order.
fn parse_ip(hex: &str) -> Option<IpAddr> {
    let word = |i: usize| -> Option<[u8; 4]> {
        let chunk = hex.get(i * 8..i * 8 + 8)?;
        Some(u32::from_str_radix(chunk, 16).ok()?.to_ne_bytes())
    };
    match hex.len() {
        8 => Some(IpAddr::V4(Ipv4Addr::from(word(0)?))),
        32 => {
            let mut bytes = [0u8; 16];
            for i in 0..4 {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&word(i)?);
            }
            Some(IpAddr::V6(Ipv6Addr::from(bytes)))
        }
        _ => None,
    }
}
