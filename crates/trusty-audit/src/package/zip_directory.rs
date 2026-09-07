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
//! Names come back as RAW BYTES, undecoded. `zip` picks UTF-8 or CP437 by
//! general-purpose flag bit 11 (`read.rs:1313-1316`), so decoding here with one
//! fixed rule would make a legitimate CP437-flagged name disagree with the
//! parser's and refuse an ordinary archive. The caller compares bytes and
//! counts, which needs no encoding rule at all and catches a collision between
//! two records that decode alike as readily as one between identical bytes.
//!
//! Scope: names and record framing. Nothing here reads a local header, a
//! compressed byte, or a declared size — a size in this directory is
//! attacker-controlled and is never used to size an allocation.
//!
//! Test: `super::signing::signing_tests::a_duplicate_central_directory_record_is_refused`,
//! `super::signing::signing_tests::a_cp437_flagged_name_is_not_refused`.

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

/// How many self-framing EOCD candidates are tried before the archive is
/// refused. A real archive offers one; a comment can be stuffed with decoys.
const MAX_EOCD_CANDIDATES: usize = 64;

/// Every member name in the raw central directory, as bytes, repeats included.
///
/// Why: see the module docs — a repeated name is invisible through `ZipArchive`,
/// and it is exactly the shape that makes one archive verify here and extract
/// differently elsewhere.
/// What: locates the EOCD (scanning back over any archive comment), resolves the
/// zip64 record when the classic count or offset is saturated, then walks the
/// declared number of file headers, checking each signature and framing. The
/// names come back undecoded, in directory order.
///
/// # Preconditions
/// `package` is an archive this crate assembled: the zip begins at byte 0 with
/// no prepended data. The EOCD's central-directory offset is therefore used as
/// an absolute file offset, where `zip` corrects for a prefix with its own
/// `archive_offset` (`read.rs:531`). An archive with prepended bytes — a
/// self-extracting stub, a concatenated file — fails
/// [`SigningError::DirectoryMalformed`] here rather than being misread, so the
/// narrower assumption fails closed; it is stated because the failure would
/// otherwise look like corruption.
///
/// # Postconditions
/// The returned vector has exactly the length the directory declares. A record
/// whose signature or framing is wrong is [`SigningError::DirectoryMalformed`],
/// never a short read that would under-report the member set.
/// Test: `super::signing::signing_tests::a_duplicate_central_directory_record_is_refused`,
/// `super::signing::signing_tests::the_raw_directory_matches_what_the_zip_parser_exposes`,
/// `super::signing::signing_tests::a_comment_carrying_a_fake_eocd_signature_is_refused_not_believed`,
/// `super::signing::signing_tests::a_directory_truncated_mid_record_is_refused`,
/// `super::signing::signing_tests::a_saturated_eocd_with_no_zip64_locator_is_refused`,
/// `super::signing::signing_tests::a_forged_entry_count_is_refused_without_a_panic`.
///
/// # Errors
///
/// [`SigningError::DirectoryMalformed`] when no EOCD is found, a zip64 record
/// is required and absent, the directory exceeds [`MAX_DIRECTORY_BYTES`], or a
/// record's signature or lengths do not frame the directory; and
/// [`SigningError::Archive`] for any read failure.
pub(super) fn member_names(package: &Path) -> Result<Vec<Vec<u8>>, SigningError> {
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
    let tail_at = length - tail_len;
    let tail = read_at(&mut file, package, tail_at, tail_len)?;

    // A comment can carry a whole well-framed EOCD of its own — an empty one
    // sitting at the end of the file satisfies every check a single candidate
    // could make — so candidates are tried in turn and the first that describes
    // a directory it can actually walk wins. An attacker can only satisfy that
    // by supplying a real directory, which the caller's duplicate and count
    // checks then judge.
    let mut refusal = None;
    for eocd in eocd_candidates(&tail, tail_at, length) {
        match resolve(&mut file, package, &tail, eocd, tail_at, length) {
            Ok(names) => return Ok(names),
            Err(e) => refusal = Some(e),
        }
    }
    Err(refusal.unwrap_or_else(|| malformed(package, "no end-of-central-directory record")))
}

/// One candidate EOCD, resolved through to a walked directory.
fn resolve(
    file: &mut std::fs::File,
    package: &Path,
    tail: &[u8],
    eocd: usize,
    tail_at: u64,
    length: u64,
) -> Result<Vec<Vec<u8>>, SigningError> {
    let (entries, size, offset, directory_end) = locate(file, package, tail, eocd)?;
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
    // The directory ends where the record that describes it begins. This is
    // what tells a real EOCD from a well-framed decoy in a comment, and it is
    // also the check that turns the no-prepended-bytes precondition from an
    // assumption into a refusal: a prefixed archive's stored offsets are short
    // by the prefix, so they do not meet their own record.
    let directory_end = directory_end.unwrap_or(tail_at + eocd as u64);
    if offset.saturating_add(size) != directory_end {
        return Err(malformed(
            package,
            "the central directory does not end where the record describing it begins",
        ));
    }
    let directory = read_at(file, package, offset, size)?;
    walk(package, &directory, entries)
}

/// The EOCD's entry count, directory size and directory offset, widened through
/// the zip64 record whenever a classic field is saturated.
///
/// A package member can exceed the 4 GiB classic ceiling (`super::start` writes
/// a zip64 local header for one), so zip64 is handled rather than refused.
///
/// The fourth element is where the directory must end when zip64 answered —
/// the zip64 record's own offset, since that record and its locator sit between
/// the directory and the classic EOCD. `None` means the classic EOCD is the end.
fn locate(
    file: &mut std::fs::File,
    package: &Path,
    tail: &[u8],
    eocd: usize,
) -> Result<(u64, u64, u64, Option<u64>), SigningError> {
    let entries = u64::from(u16(tail, eocd + 10));
    let size = u64::from(u32(tail, eocd + 12));
    let offset = u64::from(u32(tail, eocd + 16));
    // The parser's own rule, matched exactly: `spec.rs:369`'s `may_be_zip64`
    // tests the entry count and the directory offset, and NOT the directory
    // size. A directory that happens to be 0xFFFFFFFF bytes long is a classic
    // archive to `zip`, and reading it as zip64 here would disagree with the
    // parser on an archive neither of us should refuse.
    let saturated = entries == u64::from(u16::MAX) || offset == u64::from(u32::MAX);
    if !saturated {
        return Ok((entries, size, offset, None));
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
    Ok((
        u64(&record, 32),
        u64(&record, 40),
        u64(&record, 48),
        Some(record_at),
    ))
}

/// One pass over the directory, returning a raw name per declared record.
fn walk(package: &Path, directory: &[u8], entries: u64) -> Result<Vec<Vec<u8>>, SigningError> {
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
        // Undecoded: `zip` chooses UTF-8 or CP437 by flag bit 11, so any single
        // rule applied here would disagree with it on a legitimate archive.
        names.push(directory[name_at..name_at + name_len].to_vec());
        at = end;
    }
    Ok(names)
}

/// Candidate EOCD positions, latest first.
///
/// The archive comment is arbitrary bytes and may carry the EOCD signature, so
/// "the last signature in the file" is a rule an attacker writes the answer to.
/// A candidate must at least frame itself — its declared comment length has to
/// end exactly at the end of the file, since the comment is the archive's last
/// bytes by definition. That still admits a decoy, which is why the caller
/// judges each one by whether its directory walks.
///
/// Capped, so a comment stuffed with signatures costs a bounded number of
/// seeks rather than one per byte.
fn eocd_candidates(tail: &[u8], tail_at: u64, length: u64) -> Vec<usize> {
    let Some(last) = tail.len().checked_sub(EOCD_LEN) else {
        return Vec::new();
    };
    (0..=last)
        .rev()
        .filter(|at| {
            u32(tail, *at) == EOCD_SIGNATURE
                && tail_at + *at as u64 + EOCD_LEN as u64 + u64::from(u16(tail, at + 20)) == length
        })
        .take(MAX_EOCD_CANDIDATES)
        .collect()
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
