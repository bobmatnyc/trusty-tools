//! `dashboard_attach.py`'s name rules: `clean_name`, `content_type`, `max_bytes`, `ITEM_ID` (#12029 P2h).
//!
//! Why: an upload name comes from the client; it must never carry a path
//! and must reduce to the same stored name as the frozen script gives, so
//! both writers agree on what lands on disk.
//! What: [`clean_name`] (NFC, the traversal and control-character
//! refusals, the drive-letter refusal, odd characters to `_`, the `. ` trim,
//! the 100-character cut keeping up to 16 extension characters),
//! [`content_type`] by extension, [`max_bytes`] from `DASH_ATTACH_MAX_MB`
//! (25 MiB when unset, not a number or not positive), [`item_id_ok`], and
//! `os.path.splitext`.
//! Test: `clean_name_follows_python`, `max_bytes_falls_back_to_25_mib`.

use super::nfc::nfc;
use crate::dashboard::store::py::PyStoreError;
use crate::pystr::strip;

/// `MAX_NAME`.
pub const MAX_NAME: usize = 100;

/// `os.path.splitext(p)` (posix): `(root, ext)`; leading dots are not an extension.
pub(crate) fn splitext(p: &str) -> (&str, &str) {
    let sep = p.rfind('/').map_or(0, |i| i + 1);
    let Some(dot) = p.rfind('.') else {
        return (p, "");
    };
    if dot < sep || p[sep..dot].bytes().all(|b| b == b'.') {
        return (p, "");
    }
    (&p[..dot], &p[dot..])
}

/// The inline image types (`INLINE_TYPES`).
pub(crate) fn inline_type(ext: &str) -> Option<&'static str> {
    match ext {
        ".png" => Some("image/png"),
        ".jpg" | ".jpeg" => Some("image/jpeg"),
        ".gif" => Some("image/gif"),
        ".webp" => Some("image/webp"),
        _ => None,
    }
}

/// `content_type(name)`: by extension only; unknown is `application/octet-stream`.
pub fn content_type(name: &str) -> &'static str {
    let ext = splitext(name).1.to_lowercase();
    inline_type(&ext).unwrap_or(match ext.as_str() {
        ".txt" | ".log" => "text/plain",
        ".md" => "text/markdown",
        ".json" => "application/json",
        ".pdf" => "application/pdf",
        ".csv" => "text/csv",
        _ => "application/octet-stream",
    })
}

/// Why: the upload cap is read per call, so a changed environment applies at once.
/// What: `max_bytes()`: `int(float($DASH_ATTACH_MAX_MB or "25") * 1 MiB)`
/// when positive, else 25 MiB; a value `float()` refuses is 25 MiB too.
/// A cap past `u64` saturates (the script's `int` is unbounded; an infinite
/// value raises `OverflowError` there).
/// Test: `max_bytes_falls_back_to_25_mib`.
pub fn max_bytes(env: &dyn Fn(&str) -> Option<String>) -> u64 {
    const DEFAULT: u64 = 25 << 20;
    let raw = env("DASH_ATTACH_MAX_MB").unwrap_or_else(|| "25".into());
    match crate::poller::pyfmt::py_float(&raw) {
        Some(mb) if mb > 0.0 => (mb * 1024.0 * 1024.0) as u64,
        _ => DEFAULT,
    }
}

/// `ITEM_ID.match(s)`: `^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$`, `$` also before a final `\n`.
pub fn item_id_ok(s: &str) -> bool {
    let s = s.strip_suffix('\n').unwrap_or(s);
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_alphanumeric())
        && s.chars().count() <= 32
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_' || x == '-')
}

fn refuse<T>(msg: &str) -> Result<T, PyStoreError> {
    Err(PyStoreError::Store(msg.into()))
}

/// A character `_BAD` keeps: `[A-Za-z0-9._ ()+,=@-]`.
fn kept(c: char) -> bool {
    c.is_ascii_alphanumeric() || "._ ()+,=@-".contains(c)
}

/// Why: the stored name must be one safe path component.
/// What: `clean_name(raw)`; see the module doc. A non-string raw value is
/// `None` (the script's `isinstance` check).
/// Test: `clean_name_follows_python`.
///
/// # Errors
/// [`PyStoreError::Store`] with the script's message for each refusal.
pub fn clean_name(raw: Option<&str>) -> Result<String, PyStoreError> {
    let Some(raw) = raw.filter(|r| !strip(r).is_empty()) else {
        return refuse("file name required");
    };
    let name = nfc(raw);
    let name = strip(&name);
    let bad = |c: char| (c as u32) < 32 || c as u32 == 127;
    if name.contains(['/', '\\', '\0']) || name.contains("..") || name.chars().any(bad) {
        return refuse("file name must not contain a path, '..' or control characters");
    }
    let b = name.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return refuse("file name must not be an absolute path");
    }
    let swapped: String = name
        .chars()
        .map(|c| if kept(c) { c } else { '_' })
        .collect();
    let mut name = swapped
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' '])
        .to_owned();
    if name.is_empty() {
        return refuse("file name has no usable characters");
    }
    // Every character is ASCII now, so byte and code-point lengths agree.
    if name.len() > MAX_NAME {
        let (stem, ext) = splitext(&name);
        let ext = &ext[..ext.len().min(16)];
        let keep = MAX_NAME - ext.len();
        name = format!("{}{ext}", &stem[..stem.len().min(keep)]);
    }
    Ok(name)
}
