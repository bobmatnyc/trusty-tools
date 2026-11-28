//! One writer class per collection (#12029 AC2).
//!
//! Why: the prototype store (`dashboard_store.py`, ported byte for byte as
//! [`super::py`]) and the #10063 [`super::Store`] take the same `flock` on
//! `<root>/.lock` but write different bytes and keep different change logs.
//! Two classes writing one collection in a parallel window corrupt it, so the
//! first class to write a collection owns it and the other is refused, in
//! code.
//! What: [`WriterClass`] names the two classes. `admit` runs under the
//! exclusive store lock: it refuses a collection name outside
//! `[A-Za-z0-9_.-]`, refuses the batch with [`DashError::WriterClass`] when
//! any touched collection is claimed by the other class (names compared
//! without letter case, as APFS does), and appends one
//! `writer <class> <collection>` line per new claim to the lock file, synced;
//! a failed append is truncated away. The lock file holds the claims because
//! it is the one file every writer already opens, it is never replaced (a
//! rename would break the `flock` other waiters hold), and the prototype
//! opens it in append mode and never reads it. A complete line that does not
//! parse refuses every batch: a claim is never guessed. A last line with no
//! `\n` is a torn append, never a claim; it is ignored and dropped by the next
//! append. The Rust `Store` claims only when built with
//! `Store::with_writer_claim`, a test-only option until the P2g cutover.
//! Test: `a_second_writer_class_on_one_collection_is_refused`,
//! `an_unreadable_claim_refuses_the_batch`, `a_failed_claim_append_leaves_the_lock_file_as_it_was`.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use super::super::error::DashError;
use super::lock::LOCK_FILE;

/// The two implementations that write a dashboard store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriterClass {
    /// `dashboard_store.py` and its byte-for-byte port, [`super::py`].
    Python,
    /// The #10063 [`super::Store`].
    Rust,
}

impl WriterClass {
    /// The name a claim line carries.
    pub fn name(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Rust => "rust",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "python" => Some(Self::Python),
            "rust" => Some(Self::Rust),
            _ => None,
        }
    }
}

/// #12029: the bytes a claimable collection name may hold; a `\n` would
/// break the claim line, a space its fields.
fn claimable(coll: &str) -> bool {
    !coll.is_empty()
        && coll
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// The claims key: APFS folds letter case, so `Items` is `items`.
fn key(coll: &str) -> String {
    coll.to_ascii_lowercase()
}

/// The claims in `<root>/.lock`, keyed by [`key`], and the length of its
/// complete lines (a torn last line excluded).
fn read_claims(root: &Path) -> Result<(BTreeMap<String, WriterClass>, u64), DashError> {
    let path = root.join(LOCK_FILE);
    let blob = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(DashError::io(&path)(e)),
    };
    let bad = |n: usize| {
        DashError::WriterClass(format!(
            "{}: line {n} is not a writer claim; repair it before any write",
            path.display()
        ))
    };
    // #12029: a last line with no `\n` is a torn append, not a claim.
    let whole = blob.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let text = std::str::from_utf8(&blob[..whole]).map_err(|_| bad(1))?;
    let mut out = BTreeMap::new();
    for (n, line) in text.split_terminator('\n').enumerate() {
        let claim = line
            .strip_prefix("writer ")
            .and_then(|l| l.split_once(' '))
            .and_then(|(class, coll)| Some((WriterClass::parse(class)?, coll)))
            .filter(|(_, coll)| claimable(coll));
        let Some((class, coll)) = claim else {
            return Err(bad(n + 1));
        };
        // A second line for a collection never changes its owner.
        out.entry(key(coll)).or_insert(class);
    }
    Ok((out, whole as u64))
}

/// The claims recorded in `<root>/.lock`: lower-cased collection to class.
///
/// # Errors
/// [`DashError::WriterClass`] for a complete line that is not a claim;
/// [`DashError::Io`] when the file cannot be read.
#[cfg(test)]
pub(crate) fn claims(root: &Path) -> Result<BTreeMap<String, WriterClass>, DashError> {
    read_claims(root).map(|(c, _)| c)
}

/// The append step of [`admit`]; a seam so a test can fail it.
type Append<'w> = dyn FnMut(&mut File, &[u8]) -> std::io::Result<()> + 'w;

/// Why: AC2 says a collection has exactly one writer class; the guard must
/// hold in code, not by convention.
/// What: refuses a collection name outside `[A-Za-z0-9_.-]`, and the batch
/// when any of `colls` is claimed (letter case ignored) by a class other than
/// `class`; otherwise claims the unclaimed ones for `class`. The caller holds
/// the exclusive store lock, so two writers cannot both claim one collection.
/// Test: `a_second_writer_class_on_one_collection_is_refused`,
/// `a_case_variant_of_a_claimed_collection_is_refused_across_classes`,
/// `a_collection_name_with_a_newline_is_refused_before_its_claim`.
///
/// # Errors
/// [`DashError::WriterClass`] naming the collection and its owner, an
/// unclaimable name or an unreadable claim; [`DashError::Io`] when the claim
/// cannot be appended (the lock file is cut back to its complete lines).
pub(crate) fn admit<'a>(
    root: &Path,
    colls: impl IntoIterator<Item = &'a str>,
    class: WriterClass,
) -> Result<(), DashError> {
    admit_with(root, colls, class, &mut |f, b| f.write_all(b))
}

/// [`admit`] with the append step given.
fn admit_with<'a>(
    root: &Path,
    colls: impl IntoIterator<Item = &'a str>,
    class: WriterClass,
    append: &mut Append<'_>,
) -> Result<(), DashError> {
    let (held, whole) = read_claims(root)?;
    let mut new = BTreeMap::new();
    for coll in colls {
        // #12029: Python's `$` lets `items\n` through `_check_id`; its claim
        // line would be unreadable, so the name is refused before any write.
        if !claimable(coll) {
            return Err(DashError::WriterClass(format!(
                "collection {coll:?} cannot be claimed: use letters, digits, '_', '-', '.'"
            )));
        }
        match held.get(&key(coll)) {
            Some(owner) if *owner != class => {
                return Err(DashError::WriterClass(format!(
                    "{coll} is written by the {} writer class; the {} writer is refused \
                     (one writer class per collection)",
                    owner.name(),
                    class.name()
                )));
            }
            Some(_) => {}
            None => {
                new.entry(key(coll)).or_insert(coll);
            }
        }
    }
    if new.is_empty() {
        return Ok(());
    }
    let text: String = new
        .values()
        .map(|c| format!("writer {} {c}\n", class.name()))
        .collect();
    let path = root.join(LOCK_FILE);
    let mut f = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .map_err(DashError::io(&path))?;
    // #12029: drop a torn last line so the new claim starts a line.
    f.set_len(whole).map_err(DashError::io(&path))?;
    if let Err(e) = append(&mut f, text.as_bytes()).and_then(|()| f.sync_data()) {
        // #12029: a failed append (ENOSPC) leaves no part of its line behind.
        if let Err(cut) = f.set_len(whole).and_then(|()| f.sync_data()) {
            tracing::error!(error = %cut, path = %path.display(), "dashboard store: torn claim not cut back");
        }
        return Err(DashError::io(&path)(e));
    }
    Ok(())
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;
