//! Path confinement for every bundle write (SPEC-OKF-01 §2.11).
//!
//! Why: every trusty-okf write must stay under the bundle root: concept
//! files, `index.md`, `log.md`, `_sources/` files and temporary files. A
//! lexical `..` check alone is not enough, because an in-root symbolic link
//! can redirect a write, and a case- or normalisation-insensitive file
//! system lets two IDs address one file.
//! What: [`confine`] (moved from trusty-kb `roots.rs`, #10746) joins
//! caller segments, refusing any non-normal component; [`canonical_root`]
//! requires the root to exist (file system rule 1); [`confined_file`]
//! refuses a path whose existing components include a symbolic link, even
//! one that resolves inside the root (rule 2); [`atomic_write`] writes
//! through a `create_new` temporary file in the target's directory and a
//! rename (rule 3), every file 0600 on Unix (#10746); [`fold_key`]
//! and `fold_conflict` apply the fold rule.
//! Residual risk (rule 4): a directory swapped for a link between the check
//! and the rename is not caught; `openat` with `O_NOFOLLOW` would close it.
//! Test: `confine_rejects_traversal`, `fold_key_folds_case_and_normalization`,
//! `c53_put_across_an_in_root_symlink_is_path_escape`,
//! `c54_open_on_a_missing_root_is_an_error`,
//! `a3_put_writes_the_concept_0600_whatever_its_prior_mode`,
//! `a3_every_bundle_write_path_leaves_files_0600`, `c140_fold_collision_on_put_and_move`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use unicode_normalization::UnicodeNormalization;

use crate::OkfError;

pub(crate) fn io_err(path: &Path, source: std::io::Error) -> OkfError {
    OkfError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Join caller-supplied relative segments under `root`, refusing traversal.
///
/// Why: a segment is caller input; it must never hold `..`, a root or
/// prefix component, or an absolute path. Moved from trusty-kb
/// `roots::confine` (SPEC-OKF-01 §2.7.2); the agent crate's root mapping
/// wraps it.
/// What: every component of every segment must be `Normal`; an empty
/// segment or any other component is [`OkfError::PathEscape`] naming the
/// segment. Lexical only: [`confined_file`] adds the link check.
/// Test: `confine_rejects_traversal`.
pub fn confine(root: &Path, segments: &[&str]) -> Result<PathBuf, OkfError> {
    let mut out = root.to_path_buf();
    for seg in segments {
        if seg.is_empty() {
            return Err(OkfError::PathEscape(PathBuf::new()));
        }
        for comp in Path::new(seg).components() {
            match comp {
                Component::Normal(part) => out.push(part),
                _ => return Err(OkfError::PathEscape(PathBuf::from(seg))),
            }
        }
    }
    Ok(out)
}

/// Canonicalise the bundle root once; it must exist and be a directory
/// (§2.11 file system rule 1). Test: `c54_open_on_a_missing_root_is_an_error`.
pub fn canonical_root(root: &Path) -> Result<PathBuf, OkfError> {
    let canon = root.canonicalize().map_err(|e| io_err(root, e))?;
    if !canon.is_dir() {
        return Err(OkfError::PathEscape(canon));
    }
    Ok(canon)
}

/// The absolute path of bundle-relative `rel` (`/`-separated) under the
/// canonical `root`, refusing any existing component that is a symbolic link.
///
/// What: `rel` goes through [`confine`]; then each existing component from
/// the root down is checked with `symlink_metadata` (§2.11 rule 2). The
/// first missing component ends the check: nothing below it exists.
/// Test: `c53_put_across_an_in_root_symlink_is_path_escape`.
pub fn confined_file(root: &Path, rel: &str) -> Result<PathBuf, OkfError> {
    let root = canonical_root(root)?;
    let target = confine(&root, &[rel])?;
    let mut at = root.clone();
    for comp in Path::new(rel).components() {
        at.push(comp);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(OkfError::PathEscape(at)),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(io_err(&at, e)),
        }
    }
    Ok(target)
}

/// #10746: the mode of every regular file trusty-okf creates or replaces in
/// a bundle. A bundle can hold secrets (SPEC-OKF-01 §3k, §2.15), so no file
/// is group- or world-readable, whatever the umask or a prior file's mode.
#[cfg(unix)]
pub(crate) const BUNDLE_FILE_MODE: u32 = 0o600;

/// `opts` set to create a file with [`BUNDLE_FILE_MODE`] on Unix, so the new
/// file is never wider than 0600 from the moment it exists.
fn bundle_open_options() -> OpenOptions {
    #[allow(unused_mut)]
    let mut opts = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // #10746: 0600 at create time; the umask can only narrow it.
        opts.mode(BUNDLE_FILE_MODE);
    }
    opts
}

/// Set an open bundle file to exactly [`BUNDLE_FILE_MODE`] on Unix.
///
/// What: undoes a narrowing umask on a new file and a wider mode on an
/// existing one, before any byte is written. A no-op elsewhere.
fn restrict_file(file: &File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(BUNDLE_FILE_MODE))?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

/// Write `bytes` to `target` through a `create_new` temporary file in the
/// same directory and a rename (§2.11 rule 3, §4.4).
///
/// Why: a rename replaces a link instead of following it, and a reader never
/// sees a half-written file. #10746: a bundle can hold secrets, so the
/// result is 0600 on Unix.
/// What: creates the parent directories; refuses a `target` that is a link.
/// On Unix the temporary file is created with mode 0600 and set to exactly
/// 0600 before any byte is written, whatever the umask or the replaced
/// file's mode. Elsewhere a replaced file's permissions (its read-only flag)
/// are applied after the write. The temporary file is synced, renamed over
/// the target, and the directory is synced (best effort). On failure only a
/// temporary file this call created is removed.
/// Test: `a3_put_writes_the_concept_0600_whatever_its_prior_mode`,
/// `a3_every_bundle_write_path_leaves_files_0600`.
pub fn atomic_write(target: &Path, bytes: &[u8]) -> Result<(), OkfError> {
    let dir = target
        .parent()
        .ok_or_else(|| OkfError::PathEscape(target.to_path_buf()))?;
    std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    let perms = match std::fs::symlink_metadata(target) {
        Ok(m) if m.file_type().is_symlink() => {
            return Err(OkfError::PathEscape(target.to_path_buf()));
        }
        Ok(m) => Some(m.permissions()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_err(target, e)),
    };
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Process ID plus a counter: two writes in one process never share a name.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.okf-tmp-{}-{n}", std::process::id()));
    let mut opts = bundle_open_options();
    opts.write(true).create_new(true);
    // #10746: a failed `create_new` created nothing, so nothing is removed.
    let mut file = opts.open(&tmp).map_err(|e| io_err(&tmp, e))?;
    let result = fill(&mut file, perms.as_ref(), bytes).and_then(|()| {
        drop(file);
        std::fs::rename(&tmp, target)
    });
    if let Err(e) = result {
        // Removes only the temporary file this call created; a concept file
        // is never deleted (criterion 10).
        let _ = std::fs::remove_file(&tmp); // okf: temp-cleanup
        return Err(io_err(target, e));
    }
    // Best effort: a directory sync makes the rename durable on most file
    // systems; a failure here leaves the new bytes in place.
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Set the mode, write and sync. On Unix the file is 0600 before the bytes
/// land and `perms` is unused; elsewhere a read-only flag would block the
/// write, so the replaced file's `perms` are applied last.
fn fill(
    file: &mut File,
    perms: Option<&std::fs::Permissions>,
    bytes: &[u8],
) -> std::io::Result<()> {
    restrict_file(file)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    #[cfg(not(unix))]
    if let Some(p) = perms {
        file.set_permissions(p.clone())?;
    }
    #[cfg(unix)]
    let _ = perms;
    Ok(())
}

/// Append one line to a bundle-side log file, creating it when absent.
///
/// What: confined like any write; a file that does not end in a newline
/// (a torn last line) gets one first, so the new line stands alone. On Unix
/// the file is created 0600 and set to exactly 0600 before the append
/// (#10746). Used for the append-only verification record.
/// Test: `a3_every_bundle_write_path_leaves_files_0600`,
/// `append_refuses_a_leaf_swapped_for_a_link`.
pub(crate) fn append_line(root: &Path, rel: &str, line: &str) -> Result<(), OkfError> {
    let path = confined_file(root, rel)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    }
    append_confined(&path, line)
}

/// `opts` set so that opening a symbolic link fails instead of following it
/// (§2.11 rule 3). On Unix the open carries `O_NOFOLLOW`; elsewhere the
/// caller's `confined_file` check is the only guard.
fn no_follow(opts: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    opts
}

/// Map an open error: a leaf that is a link (`ELOOP` under `O_NOFOLLOW`) is
/// [`OkfError::PathEscape`], anything else an I/O error.
fn open_err(path: &Path, e: std::io::Error) -> OkfError {
    #[cfg(unix)]
    if e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) {
        return OkfError::PathEscape(path.to_path_buf());
    }
    io_err(path, e)
}

/// Append `line` to the already confined `path`, never through a link.
///
/// #10746 (§2.11 rule 3): a leaf swapped for a link after `confined_file`
/// is refused with `PathEscape`; the link's target gets no byte and no
/// `chmod`, because both opens carry `O_NOFOLLOW` and the mode is set on
/// the opened descriptor.
/// Test: `append_refuses_a_leaf_swapped_for_a_link`.
fn append_confined(path: &Path, line: &str) -> Result<(), OkfError> {
    // #10746 S5: read the last byte only, so an append costs O(1) in the
    // file's size; the revision and operation journals grow with every write.
    let probe = no_follow(OpenOptions::new().read(true)).open(path);
    let needs_newline = match probe {
        Ok(mut f) => {
            matches!(last_byte(&mut f).map_err(|e| io_err(path, e))?, Some(b) if b != b'\n')
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(open_err(path, e)),
    };
    // #10746: `O_NOFOLLOW` on the append open too, so a swap between the
    // probe and this open is refused as well.
    let mut f = no_follow(bundle_open_options().create(true).append(true))
        .open(path)
        .map_err(|e| open_err(path, e))?;
    // #10746: an existing record keeps no wider mode than 0600.
    restrict_file(&f).map_err(|e| io_err(path, e))?;
    let mut out = String::new();
    if needs_newline {
        out.push('\n');
    }
    out.push_str(line);
    out.push('\n');
    f.write_all(out.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| io_err(path, e))
}

/// The last byte of `f`, or `None` for an empty file.
fn last_byte(f: &mut File) -> std::io::Result<Option<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    if f.metadata()?.len() == 0 {
        return Ok(None);
    }
    f.seek(SeekFrom::End(-1))?;
    let mut b = [0u8; 1];
    f.read_exact(&mut b)?;
    Ok(Some(b[0]))
}

/// The bytes at bundle-relative `rel`, or `None` when absent. Confined.
pub(crate) fn read_rel(root: &Path, rel: &str) -> Result<Option<Vec<u8>>, OkfError> {
    let path = confined_file(root, rel)?;
    match std::fs::read(&path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(&path, e)),
    }
}

/// The §2.11 fold key: Unicode case folding, then NFC.
///
/// What: decomposes (NFD), maps each char through upper case then lower
/// case, and recomposes (NFC). The upper-then-lower round trip covers the
/// full-folding cases that plain lower-casing misses (`ß`/`ẞ` to `ss`, long
/// `ſ` to `s`, final `ς` to `σ`, the `ﬀ` ligatures). It folds a little more
/// than CaseFolding.txt (dotless `ı` meets `i`), which only refuses more
/// writes, never fewer.
/// Test: `fold_key_folds_case_and_normalization`.
pub fn fold_key(s: &str) -> String {
    let folded: String = s
        .nfd()
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .collect();
    folded.nfc().collect()
}

/// The existing file whose bundle-relative path folds to the same key as
/// `rel`, when it is a different path (§2.11 fold rule).
///
/// What: walks the directory listings from the root along `rel`, keeping
/// every entry whose name folds equal to the next segment. A match on the
/// last segment that is a file and whose listed path differs from `rel`
/// byte for byte is returned. Lexical over listings, so the answer is the
/// same on case-sensitive and case-insensitive file systems. Links are not
/// followed.
/// Test: `c140_fold_collision_on_put_and_move`.
pub(crate) fn fold_conflict(root: &Path, rel: &str) -> Result<Option<String>, OkfError> {
    let segments: Vec<&str> = rel.split('/').collect();
    let mut frontier: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    for (i, seg) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        let key = fold_key(seg);
        let mut next = Vec::new();
        for (dir, prefix) in &frontier {
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io_err(dir, e)),
            };
            for entry in entries {
                let entry = entry.map_err(|e| io_err(dir, e))?;
                let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if fold_key(&name) != key {
                    continue;
                }
                let ft = entry.file_type().map_err(|e| io_err(&entry.path(), e))?;
                let path = format!("{prefix}{name}");
                if last && ft.is_file() && path != rel {
                    return Ok(Some(path));
                }
                if !last && ft.is_dir() {
                    next.push((entry.path(), format!("{path}/")));
                }
            }
        }
        frontier = next;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: traversal via a caller-supplied segment is the primary attack.
    /// What: `..`, absolute and empty segments are refused; normal ones join.
    /// Test: self-contained.
    #[test]
    fn confine_rejects_traversal() {
        let root = Path::new("/kb/root");
        assert_eq!(
            confine(root, &["people", "ada.md"]).unwrap(),
            PathBuf::from("/kb/root/people/ada.md")
        );
        for bad in [&["..", "x"][..], &["a/../../etc"], &["/etc/passwd"], &[""]] {
            assert!(matches!(confine(root, bad), Err(OkfError::PathEscape(_))));
        }
    }

    /// Why: the fold rule must catch case and Unicode-normalisation twins.
    /// What: case, NFC/NFD, `ß` and final sigma fold together; distinct
    /// names do not.
    /// Test: self-contained.
    #[test]
    fn fold_key_folds_case_and_normalization() {
        assert_eq!(fold_key("Notes/A"), fold_key("notes/a"));
        assert_eq!(fold_key("caf\u{e9}"), fold_key("cafe\u{301}"));
        assert_eq!(fold_key("Stra\u{df}e"), fold_key("STRASSE"));
        assert_eq!(fold_key("\u{3c3}"), fold_key("\u{3c2}"));
        assert_ne!(fold_key("notes/a"), fold_key("notes/b"));
    }

    /// Why: #10746 §2.11 rule 3 — a record leaf swapped for a link after
    /// `confined_file` must not redirect an append or a `chmod`.
    /// What: the leaf is a link to a 0644 file outside the bundle, as after
    /// the swap; the append is `PathEscape`, and the target keeps its bytes
    /// and its mode. Then the error arm's sibling: a missing leaf is created
    /// and appended, so the guard refuses only a link.
    /// Test: self-contained.
    #[cfg(unix)]
    #[test]
    fn append_refuses_a_leaf_swapped_for_a_link() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside.txt");
        std::fs::write(&outside, "secret\n").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o644)).unwrap();
        let root = tmp.path().join("bundle");
        std::fs::create_dir(&root).unwrap();
        let leaf = root.join("record.jsonl");
        std::os::unix::fs::symlink(&outside, &leaf).unwrap();
        let got = append_confined(&leaf, "{\"forged\":true}");
        assert!(matches!(got, Err(OkfError::PathEscape(_))), "{got:?}");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret\n");
        let mode = std::fs::metadata(&outside).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "the link target was not chmodded");
        let fresh = root.join("fresh.jsonl");
        append_confined(&fresh, "{}").unwrap();
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "{}\n");
    }
}
