//! macOS libproc probe for the `tcp_listeners` doctor row (#8926).
//!
//! Why: `libc` binds the libproc calls but not `struct socket_fdinfo`, so the
//! prefix this probe reads is declared here from `<sys/proc_info.h>`, with the
//! field offsets pinned at compile time.
//! What: lists every pid, keeps those whose executable basename `want`
//! accepts, lists their socket descriptors, and reads each one's
//! `socket_fdinfo`; a TCP socket in `TSI_S_LISTEN` becomes a [`ListenSocket`].
//! Test: `the_host_probe_finds_a_listener_this_test_holds`.

use std::ffi::{c_int, c_void};
use std::mem::{offset_of, size_of};
use std::net::{Ipv4Addr, Ipv6Addr};

use super::{ListenSocket, ProbeReport, Uninspected};

/// `PROC_PIDFDSOCKETINFO` flavor of `proc_pidfdinfo` (`<sys/proc_info.h>`).
const PROC_PIDFDSOCKETINFO: c_int = 3;
/// `socket_info.soi_kind` for a TCP socket.
const SOCKINFO_TCP: c_int = 2;
/// `tcp_sockinfo.tcpsi_state` for LISTEN.
const TSI_S_LISTEN: c_int = 1;

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
    family: c_int,
    _shorts: [i16; 8], // options linger state qlen incqlen qlimit timeo error
    _oobmark: u32,
    _rcv: [u32; 6], // struct sockbuf_info
    _snd: [u32; 6],
    kind: c_int,
    _rfu: u32,
}

/// `struct in_sockinfo`.
#[repr(C)]
struct InSockInfo {
    _fport: c_int,
    lport: c_int,
    _gencnt: u64,
    _flags: u32,
    _flow: u32,
    _vflag: u8,
    _ip_ttl: u8,
    _rfu: u32,
    _faddr: [u32; 4],
    laddr: [u32; 4],
    _v4: u8,
    _v6: [i32; 3],
}

/// `struct socket_fdinfo` up to `soi_proto.pri_tcp.tcpsi_state`.
#[repr(C)]
struct SocketFdInfoHead {
    _pfi: ProcFileInfo,
    psi: SocketInfoHead,
    tcp_ini: InSockInfo,
    tcp_state: c_int,
}

// The offsets `<sys/proc_info.h>` gives on 64-bit Darwin.
const _: () = {
    assert!(size_of::<ProcFileInfo>() == 24);
    assert!(offset_of!(SocketFdInfoHead, psi) == 24);
    assert!(offset_of!(SocketFdInfoHead, tcp_ini) == 264);
    assert!(size_of::<InSockInfo>() == 80);
    assert!(offset_of!(SocketFdInfoHead, tcp_state) == 344);
};

/// A buffer at least as large as the kernel's whole `socket_fdinfo` (792
/// bytes today), aligned for [`SocketFdInfoHead`].
#[repr(C, align(8))]
struct FdInfoBuf([u8; 1024]);

/// List LISTEN sockets of the processes `want` accepts.
///
/// What: `Err` only when the pid list itself cannot be read. A process whose
/// path cannot be read (exited, or another user's) is skipped; one `want`
/// accepts whose descriptors cannot be read is [`Uninspected`].
pub(super) fn listening_sockets(want: fn(&str) -> bool) -> Result<ProbeReport, String> {
    let mut report = ProbeReport::default();
    for pid in all_pids()? {
        let Some(process) = pid_name(pid) else {
            continue;
        };
        if !want(&process) {
            continue;
        }
        let pid_u32 = u32::try_from(pid).unwrap_or_default();
        match pid_listen_sockets(pid) {
            Ok(socks) => report
                .listeners
                .extend(socks.into_iter().map(|(addr, port)| ListenSocket {
                    pid: pid_u32,
                    process: process.clone(),
                    addr,
                    port,
                })),
            Err(e) if e.raw_os_error() == Some(libc::ESRCH) => {}
            Err(e) => report.uninspected.push(Uninspected {
                pid: pid_u32,
                process,
                error: e.to_string(),
            }),
        }
    }
    Ok(report)
}

/// Every pid on the host.
fn all_pids() -> Result<Vec<c_int>, String> {
    // SAFETY: a null buffer of size 0 only asks for the pid count.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Err(format!(
            "proc_listallpids: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut pids: Vec<c_int> = vec![0; n as usize + 64];
    let bytes = c_int::try_from(pids.len() * size_of::<c_int>()).map_err(|e| e.to_string())?;
    // SAFETY: the buffer holds `bytes` bytes of c_int and outlives the call.
    let got = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast::<c_void>(), bytes) };
    if got <= 0 {
        return Err(format!(
            "proc_listallpids: {}",
            std::io::Error::last_os_error()
        ));
    }
    pids.truncate(got as usize);
    pids.retain(|&p| p > 0);
    Ok(pids)
}

/// A pid's executable basename, or `None` when its path cannot be read.
fn pid_name(pid: c_int) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is PROC_PIDPATHINFO_MAXSIZE bytes and outlives the call.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast::<c_void>(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    let path = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
    path.rsplit('/').next().map(str::to_string)
}

/// The (address, port) of each TCP LISTEN socket `pid` holds.
fn pid_listen_sockets(pid: c_int) -> std::io::Result<Vec<(String, u16)>> {
    let fd_size = size_of::<libc::proc_fdinfo>();
    // SAFETY: a null buffer of size 0 only asks for the byte count.
    let need =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if need <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut fds = vec![
        libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0,
        };
        need as usize / fd_size + 16
    ];
    let bytes = c_int::try_from(fds.len() * fd_size).map_err(std::io::Error::other)?;
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
    if got <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    fds.truncate(got as usize / fd_size);
    let mut out = Vec::new();
    for fd in fds
        .iter()
        .filter(|f| f.proc_fdtype == libc::PROX_FDTYPE_SOCKET as u32)
    {
        let mut buf = FdInfoBuf([0; 1024]);
        // SAFETY: the buffer is size_of::<FdInfoBuf>() bytes and outlives the call.
        let n = unsafe {
            libc::proc_pidfdinfo(
                pid,
                fd.proc_fd,
                PROC_PIDFDSOCKETINFO,
                (&raw mut buf).cast::<c_void>(),
                size_of::<FdInfoBuf>() as c_int,
            )
        };
        // A descriptor closed since the listing (EBADF) is simply gone.
        if n <= 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EBADF) {
                continue;
            }
            return Err(err);
        }
        if (n as usize) < size_of::<SocketFdInfoHead>() {
            return Err(std::io::Error::other(format!(
                "socket_fdinfo is {n} bytes, shorter than the {} this probe reads",
                size_of::<SocketFdInfoHead>()
            )));
        }
        // SAFETY: the kernel wrote at least size_of::<SocketFdInfoHead>() bytes
        // into an 8-aligned buffer, and the head is plain integers.
        let head: SocketFdInfoHead =
            unsafe { std::ptr::read_unaligned((&raw const buf).cast::<SocketFdInfoHead>()) };
        if head.psi.kind != SOCKINFO_TCP || head.tcp_state != TSI_S_LISTEN {
            continue;
        }
        // insi_lport holds the port in network byte order.
        let port = u16::from_be(head.tcp_ini.lport as u16);
        let addr = if head.psi.family == libc::AF_INET {
            Ipv4Addr::from(head.tcp_ini.laddr[3].to_ne_bytes()).to_string()
        } else {
            let mut v6 = [0u8; 16];
            for (i, word) in head.tcp_ini.laddr.iter().enumerate() {
                v6[i * 4..i * 4 + 4].copy_from_slice(&word.to_ne_bytes());
            }
            Ipv6Addr::from(v6).to_string()
        };
        out.push((addr, port));
    }
    Ok(out)
}
