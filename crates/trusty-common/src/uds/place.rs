//! Bind under a temporary name, then publish the socket at its final path
//! (#9714).
//!
//! Why: `UnixListener::bind` creates the socket file at the umask mode, and a
//! client can dial it at once. Before #9714 [`super::bind_hardened`] bound the
//! final path and chmodded it afterwards, so a same-uid client that dialled in
//! the gap saw `0755`, and [`super::verify_socket_for_connect`] refused it as
//! untrusted.
//!
//! What: [`bind_temp_beside`] binds a fresh name in the final path's own
//! directory. The caller narrows it, then [`publish`] hard-links it to the
//! final path and unlinks the temp name. `link(2)` is atomic and, unlike
//! `rename(2)`, refuses an occupied destination with `EEXIST`, so a live
//! socket already at the final path is never replaced — the same refusal a
//! bare `bind` gave. The final path therefore appears at `0600` or not at all.
//!
//! Test: `bind_hardened_never_exposes_the_final_path_below_0600`,
//! `bind_hardened_leaves_no_temp_socket_when_the_final_path_is_taken`,
//! `bind_hardened_never_replaces_a_file_that_appears_during_the_bind`,
//! `temp_socket_name_fits_the_final_name_budget`.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::net::UnixListener;

use super::UdsSecurityError;

/// Fresh temp names tried before a bind gives up. A name is skipped only when
/// something already sits on it, which no name this process made can do.
const TEMP_BIND_ATTEMPTS: usize = 8;

/// Characters a temp name is spelled from. None is `.` or `/`.
const NAME_ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Longest token a temp name carries; a `u64` holds about 12 base-36 digits.
const MAX_TOKEN_LEN: usize = 12;

/// A temp file name for a socket whose final file name is `final_name`.
///
/// What: `.` plus a base-36 token derived from `pid` and `seq`, cut so the
/// whole name is no longer than `final_name`. A one-byte final name gets a
/// one-character token and no dot. The result is never `.` or `..`. It can
/// equal `final_name`; the caller skips that candidate.
/// Test: `temp_socket_name_fits_the_final_name_budget`.
pub(crate) fn temp_socket_name(final_name: &OsStr, pid: u32, seq: u64) -> OsString {
    let budget = final_name.as_encoded_bytes().len();
    let mut x = splitmix64((u64::from(pid) << 32) ^ seq);
    let mut token = String::with_capacity(MAX_TOKEN_LEN);
    for _ in 0..MAX_TOKEN_LEN {
        token.push(char::from(NAME_ALPHABET[(x % 36) as usize]));
        x /= 36;
    }
    let name = if budget >= 2 {
        format!(".{}", &token[..(budget - 1).min(MAX_TOKEN_LEN)])
    } else {
        token[..1].to_string()
    };
    OsString::from(name)
}

/// One round of SplitMix64, so neighbouring `seq` values give unrelated names.
fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Bind a listener on a fresh temp name in `final_path`'s directory.
///
/// What: tries up to [`TEMP_BIND_ATTEMPTS`] names from [`temp_socket_name`].
/// A name something already occupies is left untouched and skipped. Same
/// directory as the final path, so [`publish`]'s link never crosses a
/// filesystem; no longer than the final name, so a path inside the `sun_path`
/// budget stays inside it.
///
/// # Errors
///
/// [`UdsSecurityError::Bind`] naming the temp path for any bind failure, or
/// the last occupied candidate when every attempt was taken.
pub(crate) fn bind_temp_beside(
    final_path: &Path,
    parent: &Path,
) -> Result<(PathBuf, UnixListener), UdsSecurityError> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let final_name = final_path
        .file_name()
        .ok_or_else(|| UdsSecurityError::Bind {
            path: final_path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "socket path has no file name"),
        })?;

    let mut attempt = 0;
    loop {
        attempt += 1;
        let name = temp_socket_name(
            final_name,
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed),
        );
        if name == final_name {
            continue;
        }
        let temp = parent.join(name);
        match UnixListener::bind(&temp) {
            Ok(listener) => return Ok((temp, listener)),
            Err(e) if e.kind() == io::ErrorKind::AddrInUse && attempt < TEMP_BIND_ATTEMPTS => {}
            Err(source) => return Err(UdsSecurityError::Bind { path: temp, source }),
        }
    }
}

/// Hard-link the narrowed temp socket to `final_path`, then drop the temp name.
///
/// What: `link(temp, final_path)`. An occupied final path answers `EEXIST`,
/// reported as `EADDRINUSE` — the errno a bare `bind` gave for it. On any
/// failure the temp name is removed. A temp name that cannot be removed after
/// the link rolls the final path back too, so success leaves exactly the final
/// path and failure leaves neither.
///
/// # Errors
///
/// [`UdsSecurityError::Bind`] naming `final_path` when the link fails, or
/// naming the temp path when it cannot be unlinked.
pub(crate) fn publish(temp: &Path, final_path: &Path) -> Result<(), UdsSecurityError> {
    if let Err(e) = std::fs::hard_link(temp, final_path) {
        let _ = std::fs::remove_file(temp);
        let source = if e.kind() == io::ErrorKind::AlreadyExists {
            io::Error::from_raw_os_error(libc::EADDRINUSE)
        } else {
            e
        };
        return Err(UdsSecurityError::Bind {
            path: final_path.to_path_buf(),
            source,
        });
    }
    std::fs::remove_file(temp).map_err(|source| {
        let _ = std::fs::remove_file(final_path);
        UdsSecurityError::Bind {
            path: temp.to_path_buf(),
            source,
        }
    })
}
