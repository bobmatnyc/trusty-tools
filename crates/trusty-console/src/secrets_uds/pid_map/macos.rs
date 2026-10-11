//! macOS: client socket -> owning pid through libproc (#9066).
//!
//! Why: macOS has no socket table in a file system; libproc reports each
//! descriptor's socket, and `libc` binds the calls but not
//! `struct socket_fdinfo`, so the prefix read here is declared from
//! `<sys/proc_info.h>` with offsets pinned at compile time (the same layout
//! trusty-mpm's `doctor_tcp_listeners_libproc.rs` reads).
//! What: [`pid_of`] lists the pids of this effective uid
//! (`proc_listpids(PROC_UID_ONLY)`; another uid's descriptors are unreadable
//! anyway), newest first, then each pid's descriptors
//! (`PROC_PIDLISTFDS`), and reads each socket's `socket_fdinfo`
//! (`PROC_PIDFDSOCKETINFO`). The first TCP socket whose local end is the
//! client and whose foreign end is the console names the pid.
//! Test: `a_spawned_client_resolves_to_its_pid`, `a_closed_peer_resolves_to_none`.

use std::ffi::{c_int, c_void};
use std::mem::{offset_of, size_of};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use super::same_endpoint;

/// `proc_listpids` type: pids whose effective uid is `typeinfo` (`<libproc.h>`).
const PROC_UID_ONLY: u32 = 4;
/// `PROC_PIDFDSOCKETINFO` flavor of `proc_pidfdinfo`.
const PROC_PIDFDSOCKETINFO: c_int = 3;
/// `socket_info.soi_kind` for a TCP socket.
const SOCKINFO_TCP: c_int = 2;
/// `in_sockinfo.insi_vflag` bit for an IPv4 address.
const INI_IPV4: u8 = 0x1;

/// `struct proc_fileinfo`.
#[repr(C)]
struct ProcFileInfo {
    _openflags: u32,
    _status: u32,
    _offset: i64,
    _type: i32,
    _guardflags: u32,
}

/// `struct socket_info` up to (not including) the `soi_proto` union.
#[repr(C)]
struct SocketInfoHead {
    _stat: [u64; 17], // struct vinfo_stat, 136 bytes
    _so: u64,
    _pcb: u64,
    _type: c_int,
    _protocol: c_int,
    _family: c_int,
    _shorts: [i16; 8],
    _oobmark: u32,
    _rcv: [u32; 6],
    _snd: [u32; 6],
    kind: c_int,
    _rfu: u32,
}

/// `struct in_sockinfo`.
#[repr(C)]
struct InSockInfo {
    fport: c_int,
    lport: c_int,
    _gencnt: u64,
    _flags: u32,
    _flow: u32,
    vflag: u8,
    _ip_ttl: u8,
    _rfu: u32,
    faddr: [u32; 4],
    laddr: [u32; 4],
    _v4: u8,
    _v6: [i32; 3],
}

/// `struct socket_fdinfo` up to the end of `soi_proto.pri_tcp.tcpsi_ini`.
#[repr(C)]
struct SocketFdInfoHead {
    _pfi: ProcFileInfo,
    psi: SocketInfoHead,
    ini: InSockInfo,
}

// The offsets `<sys/proc_info.h>` gives on 64-bit Darwin.
const _: () = {
    assert!(size_of::<ProcFileInfo>() == 24);
    assert!(offset_of!(SocketFdInfoHead, psi) == 24);
    assert!(offset_of!(SocketFdInfoHead, ini) == 264);
    assert!(size_of::<InSockInfo>() == 80);
};

/// Larger than the kernel's whole `socket_fdinfo` (792 bytes), 8-aligned.
#[repr(C, align(8))]
struct FdInfoBuf([u8; 1024]);

/// The pid holding the socket `peer` -> `local`.
pub(super) fn pid_of(local: SocketAddr, peer: SocketAddr) -> Option<u32> {
    user_pids()?
        .into_iter()
        .find(|&pid| holds(pid, local, peer))
        .and_then(|pid| u32::try_from(pid).ok())
}

/// Every pid of this effective uid, newest first; `None` if unlistable.
fn user_pids() -> Option<Vec<c_int>> {
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    // SAFETY: a null buffer of size 0 only asks for the byte count.
    let need = unsafe { libc::proc_listpids(PROC_UID_ONLY, uid, std::ptr::null_mut(), 0) };
    let need = usize::try_from(need).ok().filter(|n| *n > 0)?;
    let mut pids: Vec<c_int> = vec![0; need / size_of::<c_int>() + 64];
    let bytes = c_int::try_from(pids.len() * size_of::<c_int>()).ok()?;
    // SAFETY: the buffer holds `bytes` bytes of c_int and outlives the call.
    let got = unsafe {
        libc::proc_listpids(
            PROC_UID_ONLY,
            uid,
            pids.as_mut_ptr().cast::<c_void>(),
            bytes,
        )
    };
    let got = usize::try_from(got).ok().filter(|n| *n > 0)?;
    pids.truncate(got / size_of::<c_int>());
    pids.retain(|&p| p > 0);
    pids.sort_unstable_by(|a, b| b.cmp(a));
    Some(pids)
}

/// Whether `pid` holds a TCP socket whose local end is `peer` and whose
/// foreign end is `local`. Any read failure answers `false`.
fn holds(pid: c_int, local: SocketAddr, peer: SocketAddr) -> bool {
    let fd_size = size_of::<libc::proc_fdinfo>();
    // SAFETY: a null buffer of size 0 only asks for the byte count.
    let need =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    let Some(need) = usize::try_from(need).ok().filter(|n| *n > 0) else {
        return false;
    };
    let empty = libc::proc_fdinfo {
        proc_fd: 0,
        proc_fdtype: 0,
    };
    let mut fds = vec![empty; need / fd_size + 16];
    let Ok(bytes) = c_int::try_from(fds.len() * fd_size) else {
        return false;
    };
    // SAFETY: the buffer holds `bytes` bytes of proc_fdinfo and outlives the call.
    let got = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast::<c_void>(),
            bytes,
        )
    };
    let Some(got) = usize::try_from(got).ok().filter(|n| *n > 0) else {
        return false;
    };
    fds.truncate(got / fd_size);
    fds.iter()
        .filter(|fd| fd.proc_fdtype == libc::PROX_FDTYPE_SOCKET as u32)
        .filter_map(|fd| socket_ends(pid, fd.proc_fd))
        .any(|(here, there)| same_endpoint(here, peer) && same_endpoint(there, local))
}

/// The (local, foreign) ends of descriptor `fd` of `pid`, if it is TCP.
fn socket_ends(pid: c_int, fd: i32) -> Option<(SocketAddr, SocketAddr)> {
    let mut buf = FdInfoBuf([0; 1024]);
    // SAFETY: the buffer is size_of::<FdInfoBuf>() bytes and outlives the call.
    let n = unsafe {
        libc::proc_pidfdinfo(
            pid,
            fd,
            PROC_PIDFDSOCKETINFO,
            (&raw mut buf).cast::<c_void>(),
            size_of::<FdInfoBuf>() as c_int,
        )
    };
    if usize::try_from(n).ok()? < size_of::<SocketFdInfoHead>() {
        return None;
    }
    // SAFETY: the kernel wrote at least size_of::<SocketFdInfoHead>() bytes
    // into an 8-aligned buffer, and the head is plain integers.
    let head: SocketFdInfoHead =
        unsafe { std::ptr::read_unaligned((&raw const buf).cast::<SocketFdInfoHead>()) };
    if head.psi.kind != SOCKINFO_TCP {
        return None;
    }
    let ini = head.ini;
    let v4 = ini.vflag & INI_IPV4 != 0;
    // insi_lport / insi_fport hold the port in network byte order.
    let here = SocketAddr::new(ip(ini.laddr, v4), u16::from_be(ini.lport as u16));
    let there = SocketAddr::new(ip(ini.faddr, v4), u16::from_be(ini.fport as u16));
    Some((here, there))
}

/// An `in4in6_addr` / `in6_addr` union as an address.
fn ip(words: [u32; 4], v4: bool) -> IpAddr {
    if v4 {
        return IpAddr::V4(Ipv4Addr::from(words[3].to_ne_bytes()));
    }
    let mut v6 = [0u8; 16];
    for (i, word) in words.iter().enumerate() {
        v6[i * 4..i * 4 + 4].copy_from_slice(&word.to_ne_bytes());
    }
    IpAddr::V6(Ipv6Addr::from(v6))
}
