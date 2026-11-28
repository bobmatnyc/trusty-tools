//! `dashboard_attach.save`: one upload body to `<dir>/<item id>/<name>`, never overwriting (#12029 P2h).
//!
//! Why: a stored attachment must be complete before any reader can see it,
//! must never replace another file, and must not stay behind half written.
//! What: [`save`]: the length checks, [`super::dirs::safe_dir`], a hidden
//! `.upload-*` temp file (`O_EXCL`, mode `0o600`) filled in 64 KiB reads,
//! hashed as they stream (memory stays one chunk), and fsynced, then `link(2)` to the name, or `<stem>-2<ext>` up to `-999` while
//! taken; the temp file is removed on every path.
//! Test: `save_links_without_overwriting_and_numbers_collisions`,
//! `an_oversize_body_is_refused_and_nothing_is_written`.

use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;

use super::AttachError;
use super::dirs::safe_dir;
use super::names::splitext;
use crate::dashboard::pyjsonl::join;
use crate::dashboard::store::py::PyStoreError;

/// `CHUNK`.
const CHUNK: usize = 1 << 16;

/// What [`save`] stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// The name it landed under.
    pub stored: String,
    /// Its full path.
    pub path: String,
    /// Bytes written.
    pub size: u64,
    /// The hex SHA-256 of the body.
    pub sha256: String,
}

fn store(msg: &str) -> AttachError {
    AttachError::Store(PyStoreError::Store(msg.into()))
}

fn os(path: &str) -> impl Fn(std::io::Error) -> AttachError + '_ {
    move |e| AttachError::Store(PyStoreError::os(path, &e))
}

/// A fresh hidden temp file in `dir`, as `tempfile.mkstemp(prefix=".upload-")`.
fn mkstemp(dir: &str) -> Result<(std::fs::File, String), AttachError> {
    use std::hash::{BuildHasher, Hasher};
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    for _ in 0..100 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(u64::from(std::process::id()));
        let mut n = h.finish();
        let tail: String = (0..8)
            .map(|_| {
                let c = LETTERS[(n % LETTERS.len() as u64) as usize];
                n /= LETTERS.len() as u64;
                char::from(c)
            })
            .collect();
        let path = join(dir, &format!(".upload-{tail}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(f) => return Ok((f, path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(os(&path)(e)),
        }
    }
    Err(store("no usable temporary name"))
}

/// Why: the one write path for an upload body.
/// What: `save(rfile, length, name, directory, item_id, limit)`; see the module doc.
/// Test: `save_links_without_overwriting_and_numbers_collisions`.
///
/// # Errors
/// `empty upload`, [`AttachError::TooLarge`], a short body, `too many files
/// with this name`, [`safe_dir`]'s refusals, an `OSError`.
pub fn save(
    body: &mut dyn Read,
    length: i64,
    name: &str,
    directory: &str,
    item_id: &str,
    limit: u64,
) -> Result<Saved, AttachError> {
    if length <= 0 {
        return Err(store("empty upload"));
    }
    let want = u64::try_from(length).unwrap_or(0);
    if want > limit {
        return Err(AttachError::TooLarge(format!(
            "file is {length} bytes; the limit is {limit}"
        )));
    }
    let target = safe_dir(directory, item_id).map_err(AttachError::Store)?;
    let (mut out, tmp) = mkstemp(&target)?;
    let done = fill_and_link(body, &mut out, want, &tmp, &target, name);
    drop(out);
    let _ = std::fs::remove_file(&tmp);
    done
}

fn fill_and_link(
    body: &mut dyn Read,
    out: &mut std::fs::File,
    want: u64,
    tmp: &str,
    target: &str,
    name: &str,
) -> Result<Saved, AttachError> {
    // #12029 P2h: hashed as it streams, so memory per upload is one chunk.
    let mut digest = crate::sha256::Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut got = 0u64;
    while got < want {
        let n = usize::try_from((want - got).min(CHUNK as u64)).unwrap_or(CHUNK);
        let read = body.read(&mut buf[..n]).map_err(os(tmp))?;
        if read == 0 {
            return Err(store("upload ended before Content-Length bytes"));
        }
        got += read as u64;
        digest.update(&buf[..read]);
        out.write_all(&buf[..read]).map_err(os(tmp))?;
    }
    out.flush().map_err(os(tmp))?;
    out.sync_all().map_err(os(tmp))?;
    let sha256 = digest.hex();
    let (stem, ext) = splitext(name);
    let (mut stored, mut n) = (name.to_owned(), 1);
    loop {
        let path = join(target, &stored);
        match std::fs::hard_link(tmp, &path) {
            Ok(()) => {
                return Ok(Saved {
                    stored,
                    path,
                    size: got,
                    sha256,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                n += 1;
                stored = format!("{stem}-{n}{ext}");
                if n > 999 {
                    return Err(store("too many files with this name"));
                }
            }
            Err(e) => return Err(os(&path)(e)),
        }
    }
}
