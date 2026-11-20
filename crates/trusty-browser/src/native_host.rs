//! `trusty-browser native-host`: Chrome native messaging to a broker socket (`native_host.py`).
//!
//! Why: Chrome talks to native code only through a stdio child it launches;
//! the Rust port has no Python interpreter to run `native_host.py`, so the
//! same binary carries the relay as a subcommand.
//! What: refuses any origin but ours, a state directory or socket that is not
//! private to this user, and on `enrollment.sock` a broker that is not this
//! executable running `--state <dir> serve` ([`crate::peer_darwin::verify_enrollment_service`]).
//! Then two threads copy length-prefixed frames (1..=1 MiB, native-endian
//! header) each way; the first direction to end or fail ends the relay.
//! Test: `relay_copies_frames_both_ways`, `relay_refuses_bad_origin_and_shared_socket`
//! in `tests/native_host.rs`.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use crate::frame::{HEADER, MAX_FRAME};
use crate::peer_darwin::ORIGIN;
use crate::private;

fn denied(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, msg.to_owned())
}

fn private_directory(dir: &Path) -> io::Result<()> {
    let m = fs::symlink_metadata(dir)?;
    if !m.file_type().is_dir() || m.uid() != private::uid() || m.mode() & 0o077 != 0 {
        return Err(denied(
            "the service directory must belong to this user and have mode 0700",
        ));
    }
    Ok(())
}

/// Check the origin and the socket, connect, and (for enrollment) verify the broker.
pub fn connect(path: &Path, origin: &str) -> io::Result<UnixStream> {
    if origin != ORIGIN {
        return Err(denied("unexpected Chrome extension origin"));
    }
    private_directory(
        path.parent()
            .ok_or_else(|| denied("socket has no directory"))?,
    )?;
    let m = fs::symlink_metadata(path)?;
    if !m.file_type().is_socket() || m.uid() != private::uid() || m.mode() & 0o077 != 0 {
        return Err(denied("expected a private user-owned service socket"));
    }
    let service = UnixStream::connect(path)?;
    if path.file_name().is_some_and(|n| n == "enrollment.sock") {
        crate::peer_darwin::verify_enrollment_service(&service, path)?;
    }
    Ok(service)
}

/// Read exactly `buf.len()` bytes; `Ok(false)` on a clean EOF before the first byte.
fn read_exact_or_eof(src: &mut impl Read, buf: &mut [u8], allow_eof: bool) -> io::Result<bool> {
    let mut got = 0;
    while got < buf.len() {
        let n = src.read(&mut buf[got..])?;
        if n == 0 {
            if got == 0 && allow_eof {
                return Ok(false);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated native message",
            ));
        }
        got += n;
    }
    Ok(true)
}

/// Copy whole frames from `src` to `dst` until a clean EOF.
pub fn transfer(mut src: impl Read, mut dst: impl Write) -> io::Result<()> {
    let mut header = [0u8; HEADER];
    while read_exact_or_eof(&mut src, &mut header, true)? {
        let length = u32::from_ne_bytes(header) as usize;
        if length == 0 || length > MAX_FRAME {
            return Err(io::Error::other("invalid native message length"));
        }
        let mut message = vec![0u8; HEADER + length];
        message[..HEADER].copy_from_slice(&header);
        read_exact_or_eof(&mut src, &mut message[HEADER..], false)?;
        dst.write_all(&message)?;
        dst.flush()?;
    }
    Ok(())
}

/// Relay `input`/`output` (Chrome's stdio) to `service` until either side ends.
pub fn relay(
    service: UnixStream,
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
) -> io::Result<()> {
    let (done, wait) = mpsc::channel();
    let to_service = service.try_clone()?;
    let up = done.clone();
    // Each direction must notice EOF even if the opposite sender stalled mid-frame.
    thread::spawn(move || {
        let _ = up.send(transfer(input, to_service));
    });
    thread::spawn(move || {
        let _ = done.send(transfer(service, output));
    });
    wait.recv()
        .map_err(|_| io::Error::other("relay thread vanished"))?
}

/// The subcommand: `native-host --socket <path> <origin>`; exit status 1 on any refusal.
pub fn main(socket: &Path, origin: &str) -> i32 {
    let run = || -> io::Result<()> {
        let service = connect(socket, origin)?;
        relay(service, io::stdin(), io::stdout())
    };
    match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Trusty Browser host: {e}");
            1
        }
    }
}
