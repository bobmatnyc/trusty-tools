//! Linux: client socket -> inode -> owning pid (#9066).
//!
//! Why: Linux names a TCP socket's owner only through the socket inode, which
//! `/proc/net/tcp{,6}` lists and every holder's `/proc/<pid>/fd` links to.
//! What: [`pid_of`] finds the row whose local end is the client and whose
//! remote end is the console, then scans the processes of the row's uid for
//! a descriptor linking to `socket:[<inode>]`. An orphaned socket (inode 0)
//! has no owner. Compiled into tests on every OS, so the row parser is
//! tested off Linux too.
//! Test: `proc_net_rows_parse_both_families`, `a_spawned_client_resolves_to_its_pid`.

// #9066: off Linux only the parser is exercised, by tests.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::fs::MetadataExt as _;

use super::same_endpoint;

/// One `/proc/net/tcp{,6}` row: the two ends, the owner uid and the inode.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Row {
    pub(super) local: SocketAddr,
    pub(super) remote: SocketAddr,
    pub(super) uid: u32,
    pub(super) inode: u64,
}

/// The pid holding the socket `peer` -> `local`.
pub(super) fn pid_of(local: SocketAddr, peer: SocketAddr) -> Option<u32> {
    let row = ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .find_map(|path| {
            let text = std::fs::read_to_string(path).ok()?;
            text.lines().skip(1).filter_map(parse_row).find(|row| {
                row.inode != 0 && same_endpoint(row.local, peer) && same_endpoint(row.remote, local)
            })
        })?;
    owner(row.inode, row.uid)
}

/// The pid of a process of `uid` with a descriptor on socket `inode`.
///
/// What: newest pids first; another uid's process is skipped unread, since
/// its descriptors are unreadable without privilege.
fn owner(inode: u64, uid: u32) -> Option<u32> {
    let target = format!("socket:[{inode}]");
    let mut pids: Vec<u32> = std::fs::read_dir("/proc")
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .collect();
    pids.sort_unstable_by(|a, b| b.cmp(a));
    pids.into_iter().find(|pid| {
        let dir = format!("/proc/{pid}");
        if std::fs::metadata(&dir).map(|m| m.uid()).ok() != Some(uid) {
            return false;
        }
        let Ok(fds) = std::fs::read_dir(format!("{dir}/fd")) else {
            return false;
        };
        fds.filter_map(Result::ok).any(|fd| {
            std::fs::read_link(fd.path()).is_ok_and(|link| link.as_os_str() == target.as_str())
        })
    })
}

/// Parse one data row; `None` for the header or a malformed row.
///
/// What: fields 1 and 2 are `ADDR:PORT` in hex, the address as 32-bit words
/// in host byte order; field 7 is the uid, field 9 the inode.
pub(super) fn parse_row(line: &str) -> Option<Row> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    Some(Row {
        local: parse_endpoint(fields.get(1)?)?,
        remote: parse_endpoint(fields.get(2)?)?,
        uid: fields.get(7)?.parse().ok()?,
        inode: fields.get(9)?.parse().ok()?,
    })
}

fn parse_endpoint(text: &str) -> Option<SocketAddr> {
    let (addr, port) = text.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let mut bytes = Vec::with_capacity(16);
    for word in addr.as_bytes().chunks(8) {
        let word = std::str::from_utf8(word).ok()?;
        bytes.extend_from_slice(&u32::from_str_radix(word, 16).ok()?.to_ne_bytes());
    }
    let ip = match bytes.len() {
        4 => IpAddr::V4(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3])),
        16 => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(bytes).ok()?)),
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}
