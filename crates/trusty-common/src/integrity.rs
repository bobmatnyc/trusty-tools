//! SHA-256 digest parsing, hashing and verification — the one integrity
//! implementation shared by every pinned download (ADR-0064 decision 5 (i)).
//!
//! Why: trusty-installer's pinned-tool install and the content resolver both
//! pin an artifact by its sha256. ADR-0064 rules that this is one
//! implementation, not two, so the check lives here and trusty-installer's
//! `download/fetch.rs` delegates to it.
//! What: [`Sha256Digest`](crate::integrity::Sha256Digest) is a validated, lowercase 64-hex digest. It parses a
//! bare digest or a `sha256sum` sidecar line, hashes bytes or a file, and
//! [`verify`](crate::integrity::Sha256Digest::verify) fails closed with
//! [`IntegrityError::Mismatch`](crate::integrity::IntegrityError::Mismatch).
//! Test: `integrity::tests` (this file).

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Failure to parse, compute or match a SHA-256 digest.
///
/// Why: a caller must tell "the bytes are wrong" apart from "the digest text
/// is malformed" and "the file could not be read"; all three stop the caller,
/// but they name different causes to the operator.
/// What: `#[non_exhaustive]` so a later variant is additive.
/// Test: `parse_hex_rejects_wrong_length`, `verify_reports_both_digests_on_mismatch`,
/// `of_file_reports_the_path_it_could_not_read`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IntegrityError {
    /// The text is not a 64-character hexadecimal SHA-256 digest.
    #[error("invalid SHA-256 digest (expected 64 hex characters, got {value:?})")]
    InvalidDigest {
        /// The rejected text.
        value: String,
    },
    /// A checksum sidecar held no digest at all.
    #[error("empty checksum file")]
    EmptySidecar,
    /// The file to hash could not be opened or read.
    #[error("could not read {} for hashing: {source}", path.display())]
    Io {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The bytes hash to a different digest than the one pinned.
    #[error("sha256 mismatch: expected {expected}, got {actual}")]
    Mismatch {
        /// The digest that was pinned.
        expected: Sha256Digest,
        /// The digest the bytes actually hash to.
        actual: Sha256Digest,
    },
}

/// A validated SHA-256 digest, stored as 64 lowercase hex characters.
///
/// Why: a digest compared as a raw `String` compares case-sensitively and
/// accepts truncated text; a newtype makes "validated and normalised" a
/// property of the type.
/// What: constructed only through [`Sha256Digest::parse_hex`],
/// [`Sha256Digest::from_sidecar`], [`Sha256Digest::of_bytes`] or
/// [`Sha256Digest::of_file`]; equality is exact on the normalised form.
/// Test: `parse_hex_normalises_uppercase`, `from_sidecar_accepts_sha256sum_line`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    /// Parses a bare 64-character hex digest, normalising it to lowercase.
    ///
    /// Test: `parse_hex_normalises_uppercase`, `parse_hex_rejects_wrong_length`,
    /// `parse_hex_rejects_non_hex`.
    pub fn parse_hex(value: &str) -> Result<Self, IntegrityError> {
        let hex = value.to_ascii_lowercase();
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(IntegrityError::InvalidDigest {
                value: value.to_owned(),
            });
        }
        Ok(Self(hex))
    }

    /// Parses the first field of a `sha256sum`-style sidecar
    /// (`<hex>  <filename>`, or a bare digest).
    ///
    /// Test: `from_sidecar_accepts_sha256sum_line`, `from_sidecar_rejects_empty`.
    pub fn from_sidecar(content: &str) -> Result<Self, IntegrityError> {
        let first = content
            .split_whitespace()
            .next()
            .ok_or(IntegrityError::EmptySidecar)?;
        Self::parse_hex(first)
    }

    /// Hashes an in-memory byte slice.
    ///
    /// Test: `of_bytes_matches_known_vector`.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }

    /// Hashes a file by streaming it in 64 KiB blocks.
    ///
    /// Test: `of_file_matches_of_bytes`, `of_file_reports_the_path_it_could_not_read`.
    pub fn of_file(path: &Path) -> Result<Self, IntegrityError> {
        let io_err = |source| IntegrityError::Io {
            path: path.to_path_buf(),
            source,
        };
        let mut file = std::fs::File::open(path).map_err(io_err)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 65536];
        loop {
            let n = file.read(&mut buf).map_err(io_err)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(Self(format!("{:x}", hasher.finalize())))
    }

    /// Returns `Ok(())` when `self` (the computed digest) equals `expected`,
    /// and [`IntegrityError::Mismatch`] naming both digests otherwise.
    ///
    /// Test: `verify_accepts_equal_digests`, `verify_reports_both_digests_on_mismatch`.
    pub fn verify(&self, expected: &Sha256Digest) -> Result<(), IntegrityError> {
        if self == expected {
            Ok(())
        } else {
            Err(IntegrityError::Mismatch {
                expected: expected.clone(),
                actual: self.clone(),
            })
        }
    }

    /// The digest as 64 lowercase hex characters.
    pub fn as_hex(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256Digest({})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `echo -n abc | sha256sum` — the FIPS 180-2 test vector.
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn of_bytes_matches_known_vector() {
        assert_eq!(Sha256Digest::of_bytes(b"abc").as_hex(), ABC);
    }

    #[test]
    fn of_file_matches_of_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"abc").expect("write");
        let digest = Sha256Digest::of_file(&path).expect("hash");
        assert_eq!(digest, Sha256Digest::of_bytes(b"abc"));
    }

    #[test]
    fn of_file_reports_the_path_it_could_not_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("absent.bin");
        match Sha256Digest::of_file(&missing) {
            Err(IntegrityError::Io { path, .. }) => assert_eq!(path, missing),
            other => panic!("expected Io, got {other:?}"),
        }
    }

    #[test]
    fn parse_hex_normalises_uppercase() {
        let upper = ABC.to_ascii_uppercase();
        assert_eq!(
            Sha256Digest::parse_hex(&upper).expect("parse").as_hex(),
            ABC
        );
    }

    #[test]
    fn parse_hex_rejects_wrong_length() {
        let short = &ABC[..63];
        assert!(matches!(
            Sha256Digest::parse_hex(short),
            Err(IntegrityError::InvalidDigest { .. })
        ));
    }

    #[test]
    fn parse_hex_rejects_non_hex() {
        let bad = format!("{}z", &ABC[..63]);
        assert!(matches!(
            Sha256Digest::parse_hex(&bad),
            Err(IntegrityError::InvalidDigest { .. })
        ));
    }

    #[test]
    fn from_sidecar_accepts_sha256sum_line() {
        let line = format!("{ABC}  content-v0.1.0.tar.gz\n");
        assert_eq!(
            Sha256Digest::from_sidecar(&line).expect("parse").as_hex(),
            ABC
        );
        assert_eq!(Sha256Digest::from_sidecar(ABC).expect("bare").as_hex(), ABC);
    }

    #[test]
    fn from_sidecar_rejects_empty() {
        assert!(matches!(
            Sha256Digest::from_sidecar("  \n"),
            Err(IntegrityError::EmptySidecar)
        ));
    }

    #[test]
    fn verify_accepts_equal_digests() {
        let d = Sha256Digest::of_bytes(b"abc");
        assert!(
            d.verify(&Sha256Digest::parse_hex(ABC).expect("parse"))
                .is_ok()
        );
    }

    #[test]
    fn verify_reports_both_digests_on_mismatch() {
        let actual = Sha256Digest::of_bytes(b"tampered");
        let expected = Sha256Digest::parse_hex(ABC).expect("parse");
        match actual.verify(&expected) {
            Err(IntegrityError::Mismatch {
                expected: e,
                actual: a,
            }) => {
                assert_eq!(e, expected);
                assert_eq!(a, actual);
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }
}
