//! Reading a zip's central directory as it is on disk, not as a parser dedupes it.
//!
//! Why (#5481 security review): `zip` 2.4.2 holds its entry table as an
//! `IndexMap<Box<str>, ZipFileData>` keyed by member name (`read.rs:50`), so a
//! central directory carrying two records under one name collapses to one
//! before any caller sees it. `file_names()` yields the name once, `len()`
//! returns the deduped count, and `by_name` resolves to whichever record won.
//! Python's `zipfile`, Info-ZIP and Finder each pick a record by their own rule,
//! so a package could verify here and extract different bytes there. Every
//! public accessor on `ZipArchive` reads that same map — verified against the
//! vendored 2.4.2 source — so there is no accessor to reach for and the
//! directory has to be walked by hand.
//!
//! What: [`member_names`] finds the end-of-central-directory record, follows the
//! zip64 locator when the classic fields are saturated, and walks each
//! central-directory file header in order, returning every name INCLUDING
//! repeats. Deciding what a repeat means is [`super::signing::verify`]'s job;
//! this module only refuses to hide one.
//!
//! Scope: names and record framing. Nothing here reads a local header, a
//! compressed byte, or a declared size — a size in this directory is
//! attacker-controlled and is never used to size an allocation.
//!
//! Test: `super::signing::signing_tests::a_duplicate_central_directory_record_is_refused`.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

use super::signing::SigningError;

/// End of central directory record.
const EOCD_SIGNATURE: u32 = 0x0605_4b50;
/// Zip64 end of central directory locator, 20 bytes ahead of the classic EOCD.
const EOCD64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
/// Zip64 end of central directory record.
const EOCD64_SIGNATURE: u32 = 0x0606_4b50;
/// Central directory file header.
const CENTRAL_HEADER_SIGNATURE: u32 = 0x0201_4b50;

/// Bytes in an EOCD record with no archive comment.
const EOCD_LEN: usize = 22;
/// Bytes in the fixed part of a central-directory file header.
const CENTRAL_HEADER_LEN: usize = 46;
/// Bytes in the zip64 locator.
const EOCD64_LOCATOR_LEN: usize = 20;

/// A central directory larger than this is refused rather than buffered.
///
/// The directory holds one ~100-byte record per member, so a package with tens
/// of thousands of files stays far inside this. It exists so a forged EOCD
/// cannot ask for an arbitrary allocation — the same rule the member reads
/// follow.
const MAX_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;

/// Every member name in the raw central directory, repeats included.
///
/// Why: see the module docs — a repeated name is invisible through `ZipArchive`,
/// and it is exactly the shape that makes one archive verify here and extract
/// differently elsewhere.
/// What: locates the EOCD (scanning back over any archive comment), resolves the
/// zip64 record when the classic count, size or offset is saturated, then walks
/// the declared number of file headers, checking each signature and framing.
/// The names come back in directory order.
///
/// # Postconditions
/// The returned vector has exactly the length the directory declares. A record
/// whose signature or framing is wrong is [`SigningError::DirectoryMalformed`],
/// never a short read that would under-report the member set.
/// Test: `super::signing::signing_tests::a_duplicate_central_directory_record_is_refused`,
/// `super::signing::signing_tests::the_raw_directory_matches_what_the_zip_parser_exposes`.
///
/// # Errors
///
/// [`SigningError::DirectoryMalformed`] when no EOCD is found, a zip64 record
/// is required and absent, the directory exceeds [`MAX_DIRECTORY_BYTES`], or a
/// record's signature or lengths do not frame the directory; and
/// [`SigningError::Archive`] for any read failure.
pub(super) fn member_names(package: &Path) -> Result<Vec<String>, SigningError> {
    let mut file = std::fs::File::open(package).map_err(|source| SigningError::Archive {
        path: package.to_path_buf(),
        source,
    })?;
    let length = file
        .metadata()
        .map_err(|source| SigningError::Archive {
            path: package.to_path_buf(),
            source,
        })?
        .len();

    let tail_len = length.min((EOCD_LEN + usize::from(u16::MAX)) as u64);
    let tail = read_at(&mut file, package, length - tail_len, tail_len)?;
    let eocd =
        find_eocd(&tail).ok_or_else(|| malformed(package, "no end-of-central-directory record"))?;

    let (entries, size, offset) = locate(&mut file, package, &tail, eocd)?;
    if size > MAX_DIRECTORY_BYTES {
        return Err(malformed(
            package,
            &format!("the central directory declares {size} bytes, past this reader's ceiling"),
        ));
    }
    if offset.saturating_add(size) > length {
        return Err(malformed(
            package,
            "the central directory runs past the end of the file",
        ));
    }
    let directory = read_at(&mut file, package, offset, size)?;
    walk(package, &directory, entries)
}

/// The EOCD's entry count, directory size and directory offset, widened through
/// the zip64 record whenever a classic field is saturated.
///
/// A package member can exceed the 4 GiB classic ceiling (`super::start` writes
/// a zip64 local header for one), so zip64 is handled rather than refused.
fn locate(
    file: &mut std::fs::File,
    package: &Path,
    tail: &[u8],
    eocd: usize,
) -> Result<(u64, u64, u64), SigningError> {
    let entries = u64::from(u16(tail, eocd + 10));
    let size = u64::from(u32(tail, eocd + 12));
    let offset = u64::from(u32(tail, eocd + 16));
    let saturated = entries == u64::from(u16::MAX)
        || size == u64::from(u32::MAX)
        || offset == u64::from(u32::MAX);
    if !saturated {
        return Ok((entries, size, offset));
    }

    let locator = eocd
        .checked_sub(EOCD64_LOCATOR_LEN)
        .filter(|at| u32(tail, *at) == EOCD64_LOCATOR_SIGNATURE)
        .ok_or_else(|| malformed(package, "a zip64 archive with no zip64 locator"))?;
    let record_at = u64(tail, locator + 8);
    let record = read_at(file, package, record_at, 56)?;
    if u32(&record, 0) != EOCD64_SIGNATURE {
        return Err(malformed(
            package,
            "the zip64 locator does not point at a zip64 record",
        ));
    }
    Ok((u64(&record, 32), u64(&record, 40), u64(&record, 48)))
}

/// One pass over the directory, returning a name per declared record.
fn walk(package: &Path, directory: &[u8], entries: u64) -> Result<Vec<String>, SigningError> {
    let entries = usize::try_from(entries).map_err(|_| {
        malformed(
            package,
            "the central directory declares more entries than fit in memory",
        )
    })?;
    let mut names = Vec::with_capacity(entries.min(4096));
    let mut at = 0_usize;
    for index in 0..entries {
        if at + CENTRAL_HEADER_LEN > directory.len()
            || u32(directory, at) != CENTRAL_HEADER_SIGNATURE
        {
            return Err(malformed(
                package,
                &format!("central-directory record {index} is not a file header"),
            ));
        }
        let name_len = usize::from(u16(directory, at + 28));
        let extra_len = usize::from(u16(directory, at + 30));
        let comment_len = usize::from(u16(directory, at + 32));
        let name_at = at + CENTRAL_HEADER_LEN;
        let end = name_at + name_len + extra_len + comment_len;
        if end > directory.len() {
            return Err(malformed(
                package,
                &format!("central-directory record {index} runs past the directory"),
            ));
        }
        // Lossy on purpose: a name this reader cannot decode still has to be
        // COUNTED, because refusing to name it is how a duplicate hides.
        names.push(String::from_utf8_lossy(&directory[name_at..name_at + name_len]).into_owned());
        at = end;
    }
    Ok(names)
}

/// The last EOCD signature in `tail` that leaves a whole record behind it.
///
/// Scanned backwards because the archive comment is arbitrary bytes and may
/// itself contain the signature; the real record is the last viable one.
fn find_eocd(tail: &[u8]) -> Option<usize> {
    (0..=tail.len().checked_sub(EOCD_LEN)?)
        .rev()
        .find(|at| u32(tail, *at) == EOCD_SIGNATURE)
}

fn read_at(
    file: &mut std::fs::File,
    package: &Path,
    offset: u64,
    length: u64,
) -> Result<Vec<u8>, SigningError> {
    let length = usize::try_from(length).map_err(|_| {
        malformed(
            package,
            "a zip structure larger than this machine's address space",
        )
    })?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| SigningError::Archive {
            path: package.to_path_buf(),
            source,
        })?;
    let mut bytes = vec![0_u8; length];
    file.read_exact(&mut bytes)
        .map_err(|source| SigningError::Archive {
            path: package.to_path_buf(),
            source,
        })?;
    Ok(bytes)
}

fn malformed(package: &Path, reason: &str) -> SigningError {
    SigningError::DirectoryMalformed {
        path: package.to_path_buf(),
        reason: reason.to_owned(),
    }
}

/// Little-endian reads that answer 0 past the end, so a truncated buffer fails
/// the signature check above rather than panicking on a slice.
fn u16(bytes: &[u8], at: usize) -> u16 {
    bytes
        .get(at..at + 2)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u16::from_le_bytes)
}

fn u32(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

fn u64(bytes: &[u8], at: usize) -> u64 {
    bytes
        .get(at..at + 8)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u64::from_le_bytes)
}
