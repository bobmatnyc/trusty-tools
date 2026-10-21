//! Path placement and resolution for the #8878 trust-anchor floor.
//!
//! Why: split out of `pm_guard_trust_anchor.rs` to keep it under the SLOC
//! cap. These helpers turn a word a command wrote into the path the kernel
//! would open, which is what the floor compares against the anchors.
//! What: [`place`] turns a word into an absolute path or [`Placed::Unknown`];
//! [`resolve`] follows it through the filesystem; [`starts_with_ci`] and
//! [`is_json`] compare the way the default case-insensitive APFS volume does.
//! Test: `pm_guard_trust_anchor_tests.rs` (`an_unresolvable_target_is_denied`,
//! `a_dangling_symlink_onto_the_anchor_is_denied`,
//! `anchor_names_match_without_case`).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Symlink hops followed through a dangling link before giving up.
const MAX_LINK_HOPS: usize = 40;

/// A file's `(device, inode)`, so a hard link to an anchor is recognised.
pub(crate) type FileId = (u64, u64);

#[cfg(unix)]
pub(crate) fn file_id(path: &Path) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
pub(crate) fn file_id(_path: &Path) -> Option<FileId> {
    None
}

/// A target's placement before the filesystem is consulted.
pub(crate) enum Placed {
    /// An absolute path.
    At(PathBuf),
    /// The path depends on an expansion, glob, `~user` or a `cd`.
    Unknown,
}

/// A placed path after symlinks are followed.
pub(crate) enum Resolved {
    /// The canonical path, or a missing leaf under a canonical parent.
    Path(PathBuf),
    /// A link loop, a permission error, or another error that is not
    /// "does not exist".
    Unresolvable,
}

/// `s` case-folded the way a case-insensitive APFS volume compares names.
///
/// Why (#8928 fix round): APFS folds more than ASCII. `ſ` (U+017F) opens the
/// same file as `s`, and `K` (U+212A) the same as `k`, so an ASCII-only
/// comparison let `.claude/ſettings.local.json` pass as an unrelated name.
/// What: full Unicode case folding for every character that folds into ASCII
/// or into a multi-character sequence — `ſ`, `ß`/`ẞ`, the Latin ligatures
/// U+FB00..U+FB06 and the final sigma — and Rust's lowercase mapping for the
/// rest (which covers `K` to `k` and `Å` to `å`). Folding more than APFS does
/// can only over-deny.
/// Test: `unicode_case_folds_cannot_dodge_the_rule`, `case_folding_matches_apfs`.
pub(crate) fn fold_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{17f}' => out.push('s'),
            '\u{df}' | '\u{1e9e}' => out.push_str("ss"),
            '\u{fb00}' => out.push_str("ff"),
            '\u{fb01}' => out.push_str("fi"),
            '\u{fb02}' => out.push_str("fl"),
            '\u{fb03}' => out.push_str("ffi"),
            '\u{fb04}' => out.push_str("ffl"),
            '\u{fb05}' | '\u{fb06}' => out.push_str("st"),
            '\u{3c2}' => out.push('\u{3c3}'),
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// Whether two names are the same file name on a case-insensitive volume.
pub(crate) fn eq_fold(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || fold_case(a) == fold_case(b)
}

/// [`eq_fold`] over raw path components.
fn os_eq_fold(a: &OsStr, b: &OsStr) -> bool {
    a.as_encoded_bytes()
        .eq_ignore_ascii_case(b.as_encoded_bytes())
        || eq_fold(&a.to_string_lossy(), &b.to_string_lossy())
}

/// Whether `path` starts with `prefix`, each component compared
/// case-insensitively ([`eq_fold`]; #8928 fix round: Unicode folds too).
///
/// Why (#8878 finding 3): macOS volumes are case-insensitive by default, so
/// `Config.TOML` opens `config.toml`. On a case-sensitive volume this can only
/// over-deny.
pub(crate) fn starts_with_ci(path: &Path, prefix: &Path) -> bool {
    let mut parts = path.components();
    prefix.components().all(|want| {
        parts
            .next()
            .is_some_and(|got| os_eq_fold(got.as_os_str(), want.as_os_str()))
    })
}

/// Whether two paths are the same, compared as [`starts_with_ci`] does.
pub(crate) fn same_ci(a: &Path, b: &Path) -> bool {
    starts_with_ci(a, b) && starts_with_ci(b, a)
}

/// Whether a file name ends in `.json`, in any case (#8878 finding 3).
pub(crate) fn is_json(name: &OsStr) -> bool {
    let name = name.as_encoded_bytes();
    name.len() >= 5 && name[name.len() - 5..].eq_ignore_ascii_case(b".json")
}

/// Whether a shell word carries an expansion or a pattern the guard does not
/// evaluate: `$`, a backtick, a glob (`*`, `?`, `[`) or a brace.
pub(crate) fn has_shell_pattern(word: &str) -> bool {
    word.contains(['$', '`', '*', '?', '[', '{'])
}

/// A raw redirect word with its quotes removed, when it lexes to one word.
/// The redirect scan hands back the word as written (`"$HOME/x"`).
pub(crate) fn dequote(word: &str) -> String {
    if !word.contains(['\'', '"', '\\']) {
        return word.to_string();
    }
    match shlex::split(word) {
        Some(mut words) if words.len() == 1 => words.remove(0),
        _ => word.to_string(),
    }
}

/// Place `word` as an absolute path, or [`Placed::Unknown`].
///
/// What: `~` and `~/…` join `home`; `~user` is unknown. With `shell`, a
/// first segment `$HOME`/`${HOME}` joins `home` and `$PWD`/`${PWD}` joins
/// `base`, and any other expansion or glob is unknown. A relative path joins
/// `base`, and is unknown without one.
pub(crate) fn place(word: &str, base: Option<&Path>, home: &Path, shell: bool) -> Placed {
    let (root, rest) = if let Some(rest) = word.strip_prefix('~') {
        if !(rest.is_empty() || rest.starts_with('/')) {
            return Placed::Unknown;
        }
        (Some(home), rest)
    } else if shell {
        match word.split_once('/').unwrap_or((word, "")) {
            ("$HOME" | "${HOME}", rest) => (Some(home), rest),
            ("$PWD" | "${PWD}", rest) => match base {
                Some(base) => (Some(base), rest),
                None => return Placed::Unknown,
            },
            _ => (None, word),
        }
    } else {
        (None, word)
    };
    if shell && has_shell_pattern(rest) {
        return Placed::Unknown;
    }
    let rest = rest.trim_start_matches('/');
    match root {
        Some(root) => Placed::At(root.join(rest)),
        None if Path::new(word).is_absolute() => Placed::At(PathBuf::from(word)),
        None => base.map_or(Placed::Unknown, |base| Placed::At(base.join(word))),
    }
}

/// Follow `path` through the filesystem.
///
/// What: the canonical path when it exists; for a dangling symlink, the link's
/// destination, resolved in turn (at most [`MAX_LINK_HOPS`]); for a missing
/// leaf, its resolved parent joined with the leaf name. An entry that exists
/// but will not canonicalize, and every error other than "not found", is
/// [`Resolved::Unresolvable`].
/// Test: `an_unresolvable_target_is_denied`,
/// `a_dangling_symlink_onto_the_anchor_is_denied`.
pub(crate) fn resolve(path: &Path) -> Resolved {
    resolve_hops(path, MAX_LINK_HOPS)
}

fn resolve_hops(path: &Path, hops: usize) -> Resolved {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Resolved::Path(canonical);
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() && hops > 0 => {
            let (Ok(dest), Some(parent)) = (std::fs::read_link(path), path.parent()) else {
                return Resolved::Unresolvable;
            };
            resolve_hops(&parent.join(dest), hops - 1)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
                return Resolved::Unresolvable;
            };
            match resolve_hops(parent, hops) {
                Resolved::Path(parent) => Resolved::Path(parent.join(name)),
                Resolved::Unresolvable => Resolved::Unresolvable,
            }
        }
        _ => Resolved::Unresolvable,
    }
}
