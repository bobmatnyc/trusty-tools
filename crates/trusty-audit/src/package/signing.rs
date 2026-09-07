//! Content signing for the return package, and the check that reads it back.
//!
//! Why (#5481): the return channel is manual (#5642) — the recipient emails a
//! zip. Nothing about a file that arrived that way says it is the file that was
//! written. This module makes an alteration between writing and receiving
//! detectable: every delivered member's SHA-256 goes into one small manifest,
//! and that manifest carries a detached ed25519 signature made with the
//! per-engagement private key. Signing one manifest rather than N files is the
//! whole reason the manifest exists.
//!
//! What: [`EngagementKey`] signs, [`RetainedKey`] verifies, and [`verify`]
//! reads a finished zip and reports one [`Verdict`]. The manifest and its
//! signature are two members of the same archive
//! ([`MANIFEST_ENTRY`], [`SIGNATURE_ENTRY`]), written last so they cover every
//! member that precedes them. Neither covers itself, which is why the manifest
//! names the signature's file rather than hashing it.
//!
//! ## The limit, stated rather than softened
//!
//! This is **tamper-evidence, not proof against the recipient**. The private
//! key travels to the recipient inside the inbound package, so the signature is
//! made on hardware they fully control and they can re-sign an altered bundle
//! with it. It defends against alteration in transit and by an unrelated third
//! party. The alternatives that would close the remaining gap — submission to
//! an endpoint the auditor controls, or the auditor re-running the audit — were
//! offered and declined (#5481). This is the accepted trust model, not an
//! interim one. Nothing in this module's output may be worded as if a valid
//! signature proved the recipient did not alter the content.
//!
//! ## No key configured is not a failure
//!
//! An engagement with no `[signing]` key still packages. The manifest is still
//! written — the hashes are worth having on their own — with no `[signature]`
//! table, and [`verify`] answers [`Verdict::Unsigned`]. That is its own outcome
//! rather than an error, and it is distinct from a signed package whose
//! signature member was removed, which is [`SigningError::SignatureMissing`].
//! The manifest's `[signature]` table is what separates the two.
//!
//! Test: `super::signing_tests`.

use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// The manifest listing every delivered member's SHA-256.
pub const MANIFEST_ENTRY: &str = "manifest.sha256.toml";

/// The detached ed25519 signature over [`MANIFEST_ENTRY`]'s exact bytes.
pub const SIGNATURE_ENTRY: &str = "manifest.sha256.sig";

/// Schema version of the manifest this module writes and reads.
const MANIFEST_VERSION: u32 = 1;

/// Bytes in an ed25519 private seed and in a compressed public key alike.
const KEY_BYTES: usize = 32;

/// What went wrong signing a package, or checking one.
///
/// Why: the three failures #5481 requires be told apart — a member altered
/// after signing, a signature removed, and a signature that does not belong to
/// the retained key — are three variants rather than three strings, so a caller
/// can act on them and a test can assert which one happened. Two more close the
/// same door from the other side: a manifest-listed member that is gone, and a
/// member the manifest never listed.
/// What: one enum for both directions. Key parsing is shared by them, so its
/// failure is shared too.
/// Test: `super::signing_tests`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SigningError {
    /// Key material is not `KEY_BYTES` bytes of lower-case hex.
    #[error("the {what} signing key is not {KEY_BYTES} bytes of hex: {reason}")]
    KeyMalformed {
        /// `engagement` for the private half, `retained` for the public one.
        what: &'static str,
        /// Why the material was rejected.
        reason: String,
    },

    /// The package carries no [`MANIFEST_ENTRY`], so there is nothing to check.
    #[error(
        "{path} carries no {MANIFEST_ENTRY} — it was not written by a version of \
         trusty-audit that hashes its members, or the manifest was removed"
    )]
    ManifestMissing {
        /// The package that was read.
        path: PathBuf,
    },

    /// [`MANIFEST_ENTRY`] is present but is not a manifest this version reads.
    #[error("{path}: {MANIFEST_ENTRY} is not a manifest this version reads: {reason}")]
    ManifestMalformed {
        /// The package that was read.
        path: PathBuf,
        /// Why the parse failed.
        reason: String,
    },

    /// The manifest declares a signature and the signature member is gone.
    #[error(
        "{path}: the manifest was signed by key {key_fingerprint} but {SIGNATURE_ENTRY} \
         is not in the package — the signature was removed after it was written"
    )]
    SignatureMissing {
        /// The package that was read.
        path: PathBuf,
        /// The key the manifest says signed it.
        key_fingerprint: String,
    },

    /// The signature member is present but is not 64 bytes of hex.
    #[error("{path}: {SIGNATURE_ENTRY} is not an ed25519 signature: {reason}")]
    SignatureMalformed {
        /// The package that was read.
        path: PathBuf,
        /// Why the signature could not be read.
        reason: String,
    },

    /// The signature does not verify against the key that was supplied.
    ///
    /// Both fingerprints are named because the two causes look identical from
    /// here: the package was signed by a different engagement's key, or the
    /// manifest was altered after signing. A fingerprint that differs from the
    /// supplied key's says which.
    #[error(
        "{path}: the manifest signature does not verify — the package was signed by key \
         {package_key} and the key supplied is {supplied_key}"
    )]
    SignatureInvalid {
        /// The package that was read.
        path: PathBuf,
        /// The key fingerprint the manifest claims.
        package_key: String,
        /// The fingerprint of the key the caller supplied.
        supplied_key: String,
    },

    /// A member's bytes no longer hash to what the signed manifest recorded.
    #[error(
        "{path}: {entry} was altered after the package was signed — the manifest records \
         SHA-256 {expected} and the member in the package hashes to {actual}"
    )]
    ContentMismatch {
        /// The package that was read.
        path: PathBuf,
        /// The member whose bytes changed.
        entry: String,
        /// What the signed manifest recorded.
        expected: String,
        /// What the member actually hashes to.
        actual: String,
    },

    /// The manifest lists a member the package does not contain.
    #[error("{path}: the signed manifest lists {entry}, which is not in the package")]
    MemberMissing {
        /// The package that was read.
        path: PathBuf,
        /// The member named by the manifest and absent from the archive.
        entry: String,
    },

    /// The package contains a member the signed manifest never listed.
    #[error("{path}: {entry} is in the package but not in the signed manifest — it was added")]
    MemberUnexpected {
        /// The package that was read.
        path: PathBuf,
        /// The member the manifest does not account for.
        entry: String,
    },

    /// The package could not be opened or read.
    #[error("cannot read the package {path}: {source}")]
    Archive {
        /// The package that was read.
        path: PathBuf,
        /// The underlying failure.
        source: std::io::Error,
    },
}

/// The per-engagement private key that signs one package's manifest.
///
/// Why: #5478's config generator mints one keypair per engagement and puts the
/// private half in the inbound package, so the recipient signs locally with no
/// call back to the auditor. This type is what that key becomes once read out
/// of [`crate::config::EngagementConfig`].
/// What: an ed25519 signing key, parsed from the hex the config carries. It
/// prints as its fingerprint, never as its bytes — the same posture
/// [`crate::config::SecretKey`] takes, so a key cannot reach a log through a
/// `Debug` on an enclosing struct.
/// Test: `super::signing_tests::a_signed_package_verifies_against_the_public_half`.
#[derive(Clone)]
pub struct EngagementKey(SigningKey);

impl std::fmt::Debug for EngagementKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EngagementKey({})", self.fingerprint())
    }
}

impl EngagementKey {
    /// Mint a fresh keypair from the operating system's randomness.
    ///
    /// #5478's generator is the production caller — one keypair per engagement,
    /// its public half retained by the auditor out of band. Until that lands
    /// this is reachable only from the library, which is why the crate README
    /// says an engagement is unsigned unless a key was written into its config
    /// by hand.
    #[must_use]
    pub fn generate() -> Self {
        Self(SigningKey::generate(&mut rand_core::OsRng))
    }

    /// Read a key from the `KEY_BYTES`-byte hex seed a config carries.
    ///
    /// # Errors
    ///
    /// [`SigningError::KeyMalformed`] when the text is not exactly
    /// `KEY_BYTES` bytes of hex.
    pub fn from_hex(text: &str) -> Result<Self, SigningError> {
        Ok(Self(SigningKey::from_bytes(&key_bytes(
            text,
            "engagement",
        )?)))
    }

    /// The private seed as hex — the form [`Self::from_hex`] reads back.
    ///
    /// This is the one function that turns a key into printable text, so
    /// `git grep private_hex` finds every site that could write one out.
    #[must_use]
    pub fn private_hex(&self) -> String {
        to_hex(&self.0.to_bytes())
    }

    /// The public half as hex — what the auditor retains out of band.
    #[must_use]
    pub fn public_hex(&self) -> String {
        to_hex(self.0.verifying_key().as_bytes())
    }

    /// The short fingerprint the manifest records for this key.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.0.verifying_key())
    }
}

/// The public half the auditor retained, which checks a received package.
///
/// Retained OUT OF BAND — it never travels in either package, because a key
/// that arrives with the thing it authenticates authenticates nothing.
#[derive(Debug, Clone)]
pub struct RetainedKey(VerifyingKey);

impl RetainedKey {
    /// Read the retained public key from its hex form.
    ///
    /// # Errors
    ///
    /// [`SigningError::KeyMalformed`] when the text is not `KEY_BYTES` bytes of
    /// hex, or is not a point on the curve.
    pub fn from_hex(text: &str) -> Result<Self, SigningError> {
        let bytes = key_bytes(text, "retained")?;
        VerifyingKey::from_bytes(&bytes)
            .map(Self)
            .map_err(|e| SigningError::KeyMalformed {
                what: "retained",
                reason: e.to_string(),
            })
    }

    /// The short fingerprint of this key, in the manifest's own form.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.0)
    }
}

/// What [`verify`] concluded about a package.
///
/// `Unsigned` is deliberately not an error: an engagement with no key
/// configured produces a package with no signature, and that is a supported
/// state (see the module docs). A caller deciding whether to trust the contents
/// must still treat it as "not authenticated" — it is a distinct outcome, not a
/// weaker pass.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// Every member hashes to what the signed manifest records, and the
    /// signature over that manifest verifies against the retained key.
    Signed {
        /// The key that signed it, as a short fingerprint.
        key_fingerprint: String,
        /// How many members the manifest covers.
        files: usize,
    },
    /// The package carries a manifest and no signature, because the engagement
    /// configured no signing key. Nothing here is authenticated.
    Unsigned {
        /// How many members the manifest covers.
        files: usize,
    },
}

/// One delivered member, as the manifest records it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestFile {
    /// Path inside the zip.
    pub entry: String,
    /// Lower-case hex SHA-256 of the member's uncompressed bytes.
    pub sha256: String,
    /// Uncompressed size.
    pub bytes: u64,
}

/// The `[signature]` table, present exactly when the manifest was signed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SignatureBlock {
    scheme: String,
    key_fingerprint: String,
    file: String,
}

/// The manifest document itself.
///
/// Field order is the emitted order: TOML puts every scalar before the first
/// table, so `version` and `algorithm` must precede `signature`, and the array
/// of file tables must come last.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestDoc {
    version: u32,
    algorithm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<SignatureBlock>,
    #[serde(default, rename = "file")]
    file: Vec<ManifestFile>,
}

/// The two members [`render`] produced, ready to write into the archive.
#[derive(Debug, Clone)]
pub(super) struct SignedManifest {
    /// [`MANIFEST_ENTRY`]'s exact bytes.
    pub(super) manifest: String,
    /// [`SIGNATURE_ENTRY`]'s bytes, or `None` when no key was configured.
    pub(super) signature: Option<String>,
}

/// Build the manifest over `files`, signing it when a key was configured.
///
/// Why: `key` is an `Option` because a missing key must not stop a package
/// being built — an engagement that never configured one still has a
/// deliverable to send, and refusing to write it would trade a real report for
/// a property that engagement never asked for.
/// What: renders the manifest, then signs its exact bytes. The signature covers
/// the rendered text rather than the structure, so verification re-reads the
/// stored bytes and never re-serializes.
/// Test: `super::signing_tests::a_signed_package_verifies_against_the_public_half`,
/// `super::signing_tests::a_package_with_no_key_configured_is_unsigned_not_refused`.
///
/// # Errors
///
/// [`SigningError::ManifestMalformed`] when the manifest cannot be serialized,
/// which needs a `ManifestFile` TOML rejects.
pub(super) fn render(
    files: Vec<ManifestFile>,
    key: Option<&EngagementKey>,
) -> Result<SignedManifest, SigningError> {
    let doc = ManifestDoc {
        version: MANIFEST_VERSION,
        algorithm: "sha256".to_owned(),
        signature: key.map(|k| SignatureBlock {
            scheme: "ed25519".to_owned(),
            key_fingerprint: k.fingerprint(),
            file: SIGNATURE_ENTRY.to_owned(),
        }),
        file: files,
    };
    let manifest = toml::to_string_pretty(&doc).map_err(|e| SigningError::ManifestMalformed {
        path: PathBuf::from(MANIFEST_ENTRY),
        reason: e.to_string(),
    })?;
    let signature = key.map(|k| format!("{}\n", to_hex(&k.0.sign(manifest.as_bytes()).to_bytes())));
    Ok(SignedManifest {
        manifest,
        signature,
    })
}

/// Check a received package against the retained public key.
///
/// Why (#5481 closure condition 2): a signature nobody checks is decoration.
/// This is the check, as a library call — #5563 owns the `trusty-audit verify`
/// CLI arm that drives it, so that issue adds a `session::Command` variant and
/// this function stays the single implementation behind both it and any front
/// end.
///
/// # Preconditions
/// `package` is a zip this crate assembled. Any other zip fails
/// [`SigningError::ManifestMissing`] rather than passing vacuously.
///
/// # Postconditions
/// [`Verdict::Signed`] is returned only when the signature verifies against
/// `retained` AND every manifest-listed member hashes to its recorded digest
/// AND the archive holds no member the manifest does not list. Every other
/// state is an `Err` or [`Verdict::Unsigned`]; there is no path on which a
/// failed check returns `Signed`.
///
/// What: opens the archive, reads the manifest's stored bytes, verifies the
/// detached signature over exactly those bytes, then re-hashes every member.
/// The signature is checked BEFORE the hashes, so a package whose manifest was
/// rewritten reports the signature failure rather than a list of content
/// mismatches derived from an unauthenticated manifest.
/// Test: `super::signing_tests::a_tampered_member_fails_verification`,
/// `super::signing_tests::a_removed_signature_fails_verification`,
/// `super::signing_tests::a_different_key_fails_verification`,
/// `super::signing_tests::an_added_member_fails_verification`.
///
/// # Errors
///
/// Every [`SigningError`] variant except [`SigningError::KeyMalformed`], which
/// is raised by [`RetainedKey::from_hex`] before this is reached.
pub fn verify(package: &Path, retained: &RetainedKey) -> Result<Verdict, SigningError> {
    let mut archive = open(package)?;
    let manifest = match read_member(&mut archive, package, MANIFEST_ENTRY)? {
        Some(bytes) => bytes,
        None => {
            return Err(SigningError::ManifestMissing {
                path: package.to_path_buf(),
            });
        }
    };
    let doc: ManifestDoc = toml::from_str(&String::from_utf8_lossy(&manifest)).map_err(|e| {
        SigningError::ManifestMalformed {
            path: package.to_path_buf(),
            reason: e.to_string(),
        }
    })?;

    let Some(block) = doc.signature.as_ref() else {
        // No `[signature]` table means the engagement configured no key. The
        // hashes are NOT checked here on purpose: an unsigned manifest is
        // rewritable by anyone holding the zip, so checking members against it
        // would report an integrity result that means nothing.
        return Ok(Verdict::Unsigned {
            files: doc.file.len(),
        });
    };
    verify_signature(&mut archive, package, &manifest, block, retained)?;
    check_members(&mut archive, package, &doc.file)?;
    Ok(Verdict::Signed {
        key_fingerprint: block.key_fingerprint.clone(),
        files: doc.file.len(),
    })
}

/// The detached-signature half of [`verify`].
fn verify_signature(
    archive: &mut Archive,
    package: &Path,
    manifest: &[u8],
    block: &SignatureBlock,
    retained: &RetainedKey,
) -> Result<(), SigningError> {
    let raw = read_member(archive, package, &block.file)?.ok_or_else(|| {
        SigningError::SignatureMissing {
            path: package.to_path_buf(),
            key_fingerprint: block.key_fingerprint.clone(),
        }
    })?;
    let text = String::from_utf8_lossy(&raw);
    let bytes: [u8; Signature::BYTE_SIZE] = from_hex(text.trim())
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| SigningError::SignatureMalformed {
        path: package.to_path_buf(),
        reason: format!(
            "expected {} bytes of hex, found {} characters",
            Signature::BYTE_SIZE,
            text.trim().len()
        ),
    })?;
    retained
        .0
        .verify(manifest, &Signature::from_bytes(&bytes))
        .map_err(|_| SigningError::SignatureInvalid {
            path: package.to_path_buf(),
            package_key: block.key_fingerprint.clone(),
            supplied_key: retained.fingerprint(),
        })
}

/// The content half of [`verify`]: every listed member, and nothing else.
fn check_members(
    archive: &mut Archive,
    package: &Path,
    listed: &[ManifestFile],
) -> Result<(), SigningError> {
    for file in listed {
        let bytes = read_member(archive, package, &file.entry)?.ok_or_else(|| {
            SigningError::MemberMissing {
                path: package.to_path_buf(),
                entry: file.entry.clone(),
            }
        })?;
        let actual = to_hex(&Sha256::digest(&bytes));
        if actual != file.sha256 {
            return Err(SigningError::ContentMismatch {
                path: package.to_path_buf(),
                entry: file.entry.clone(),
                expected: file.sha256.clone(),
                actual,
            });
        }
    }
    // #5481: an addition is as much an alteration as an edit, and hashing only
    // what the manifest lists would never see one. The two signing members
    // account for themselves — neither can be in a manifest it is part of.
    let names: Vec<String> = archive.file_names().map(str::to_owned).collect();
    for name in names {
        if name == MANIFEST_ENTRY || name == SIGNATURE_ENTRY || name.ends_with('/') {
            continue;
        }
        if !listed.iter().any(|f| f.entry == name) {
            return Err(SigningError::MemberUnexpected {
                path: package.to_path_buf(),
                entry: name,
            });
        }
    }
    Ok(())
}

type Archive = zip::ZipArchive<std::io::BufReader<std::fs::File>>;

/// Open the package, mapping every failure onto one path-naming variant.
fn open(package: &Path) -> Result<Archive, SigningError> {
    let file = std::fs::File::open(package).map_err(|source| SigningError::Archive {
        path: package.to_path_buf(),
        source,
    })?;
    zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| SigningError::Archive {
        path: package.to_path_buf(),
        source: std::io::Error::other(e),
    })
}

/// One member's uncompressed bytes, or `None` when the archive has no such
/// entry. An unreadable entry that IS present is an error, never a `None` —
/// silently treating a read failure as absence is how a tampered archive passes.
fn read_member(
    archive: &mut Archive,
    package: &Path,
    entry: &str,
) -> Result<Option<Vec<u8>>, SigningError> {
    use std::io::Read as _;
    let mut member = match archive.by_name(entry) {
        Ok(m) => m,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => {
            return Err(SigningError::Archive {
                path: package.to_path_buf(),
                source: std::io::Error::other(e),
            });
        }
    };
    let mut bytes = Vec::with_capacity(usize::try_from(member.size()).unwrap_or_default());
    member
        .read_to_end(&mut bytes)
        .map_err(|source| SigningError::Archive {
            path: package.to_path_buf(),
            source,
        })?;
    Ok(Some(bytes))
}

/// The short fingerprint form both halves of a keypair produce.
///
/// The `GithubAccess::fingerprint` shape (#5980): the leading 8 bytes of a
/// SHA-256, hex. Short enough to read out loud, long enough that two
/// engagements' keys will not collide.
fn fingerprint(key: &VerifyingKey) -> String {
    Sha256::digest(key.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `KEY_BYTES` bytes of hex, or the one error that says which key it was.
fn key_bytes(text: &str, what: &'static str) -> Result<[u8; KEY_BYTES], SigningError> {
    from_hex(text.trim())
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| SigningError::KeyMalformed {
            what,
            reason: format!(
                "expected {} hex characters, found {}",
                KEY_BYTES * 2,
                text.trim().len()
            ),
        })
}

/// Lower-case hex, the form every digest and key in this module is written in.
fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The inverse, returning `None` for anything that is not whole lower-case or
/// upper-case hex. Hand-rolled rather than a `hex` dependency, because
/// `to_hex`'s counterpart is four lines and the crate already hand-rolls the
/// forward direction for the credential fingerprint (#5980).
fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod signing_tests {
    use super::*;

    /// A package with the members `entries` names, signed when `key` is set.
    fn package(dir: &Path, entries: &[(&str, &str)], key: Option<&EngagementKey>) -> PathBuf {
        let path = dir.join("package.zip");
        let file = std::fs::File::create(&path).expect("create");
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        let mut files = Vec::new();
        for (entry, body) in entries {
            zip.start_file((*entry).to_owned(), options).expect("start");
            std::io::Write::write_all(&mut zip, body.as_bytes()).expect("write");
            files.push(ManifestFile {
                entry: (*entry).to_owned(),
                sha256: to_hex(&Sha256::digest(body.as_bytes())),
                bytes: body.len() as u64,
            });
        }
        let signed = render(files, key).expect("render");
        zip.start_file(MANIFEST_ENTRY.to_owned(), options)
            .expect("start");
        std::io::Write::write_all(&mut zip, signed.manifest.as_bytes()).expect("write");
        if let Some(sig) = signed.signature {
            zip.start_file(SIGNATURE_ENTRY.to_owned(), options)
                .expect("start");
            std::io::Write::write_all(&mut zip, sig.as_bytes()).expect("write");
        }
        zip.finish().expect("finish");
        path
    }

    /// Rebuild `source`'s archive with `edit` applied to its member list.
    fn rewrite(source: &Path, edit: impl Fn(&str, &[u8]) -> Option<Vec<u8>>) -> PathBuf {
        let destination = source.with_file_name("rewritten.zip");
        let mut archive = open(source).expect("open");
        let names: Vec<String> = archive.file_names().map(str::to_owned).collect();
        let file = std::fs::File::create(&destination).expect("create");
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        for name in names {
            let bytes = read_member(&mut archive, source, &name)
                .expect("read")
                .expect("present");
            if let Some(replacement) = edit(&name, &bytes) {
                zip.start_file(name, options).expect("start");
                std::io::Write::write_all(&mut zip, &replacement).expect("write");
            }
        }
        zip.finish().expect("finish");
        destination
    }

    fn retained(key: &EngagementKey) -> RetainedKey {
        RetainedKey::from_hex(&key.public_hex()).expect("public half parses")
    }

    #[test]
    fn a_signed_package_verifies_against_the_public_half() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = EngagementKey::generate();
        let path = package(
            dir.path(),
            &[
                ("README.md", "read me"),
                ("reports/a/report.md", "findings"),
            ],
            Some(&key),
        );
        assert_eq!(
            verify(&path, &retained(&key)).expect("verifies"),
            Verdict::Signed {
                key_fingerprint: key.fingerprint(),
                files: 2,
            }
        );
    }

    #[test]
    fn a_key_round_trips_through_its_hex_form() {
        let key = EngagementKey::generate();
        let read_back = EngagementKey::from_hex(&key.private_hex()).expect("parses");
        assert_eq!(read_back.public_hex(), key.public_hex());
        assert_eq!(read_back.fingerprint(), key.fingerprint());
    }

    #[test]
    fn a_key_that_is_not_thirty_two_bytes_of_hex_is_refused() {
        let short = EngagementKey::from_hex("abcd");
        assert!(matches!(short, Err(SigningError::KeyMalformed { .. })));
        let not_hex = RetainedKey::from_hex(&"zz".repeat(32));
        assert!(matches!(not_hex, Err(SigningError::KeyMalformed { .. })));
    }

    /// The first of #5481's three distinguishable failures.
    #[test]
    fn a_tampered_member_fails_verification() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = EngagementKey::generate();
        let path = package(dir.path(), &[("README.md", "read me")], Some(&key));
        let tampered = rewrite(&path, |name, bytes| {
            Some(if name == "README.md" {
                b"read mf".to_vec()
            } else {
                bytes.to_vec()
            })
        });
        match verify(&tampered, &retained(&key)) {
            Err(SigningError::ContentMismatch { entry, .. }) => assert_eq!(entry, "README.md"),
            other => panic!("expected a content mismatch, got {other:?}"),
        }
    }

    /// The second: the signature member removed from a package that had one.
    #[test]
    fn a_removed_signature_fails_verification() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = EngagementKey::generate();
        let path = package(dir.path(), &[("README.md", "read me")], Some(&key));
        let stripped = rewrite(&path, |name, bytes| {
            (name != SIGNATURE_ENTRY).then(|| bytes.to_vec())
        });
        match verify(&stripped, &retained(&key)) {
            Err(SigningError::SignatureMissing {
                key_fingerprint, ..
            }) => assert_eq!(key_fingerprint, key.fingerprint()),
            other => panic!("expected a missing signature, got {other:?}"),
        }
    }

    /// The third: a valid package checked against somebody else's key.
    #[test]
    fn a_different_key_fails_verification() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = EngagementKey::generate();
        let other = EngagementKey::generate();
        let path = package(dir.path(), &[("README.md", "read me")], Some(&key));
        match verify(&path, &retained(&other)) {
            Err(SigningError::SignatureInvalid {
                package_key,
                supplied_key,
                ..
            }) => {
                assert_eq!(package_key, key.fingerprint());
                assert_eq!(supplied_key, other.fingerprint());
                assert_ne!(package_key, supplied_key);
            }
            other => panic!("expected an invalid signature, got {other:?}"),
        }
    }

    #[test]
    fn a_package_with_no_key_configured_is_unsigned_not_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = package(dir.path(), &[("README.md", "read me")], None);
        assert_eq!(
            verify(&path, &retained(&EngagementKey::generate())).expect("reads"),
            Verdict::Unsigned { files: 1 }
        );
    }

    #[test]
    fn an_added_member_fails_verification() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = EngagementKey::generate();
        let path = package(dir.path(), &[("README.md", "read me")], Some(&key));
        let destination = dir.path().join("with-extra.zip");
        let mut archive = open(&path).expect("open");
        let names: Vec<String> = archive.file_names().map(str::to_owned).collect();
        let file = std::fs::File::create(&destination).expect("create");
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        for name in names {
            let bytes = read_member(&mut archive, &path, &name)
                .expect("read")
                .expect("present");
            zip.start_file(name, options).expect("start");
            std::io::Write::write_all(&mut zip, &bytes).expect("write");
        }
        zip.start_file("reports/planted.md".to_owned(), options)
            .expect("start");
        std::io::Write::write_all(&mut zip, b"planted").expect("write");
        zip.finish().expect("finish");
        match verify(&destination, &retained(&key)) {
            Err(SigningError::MemberUnexpected { entry, .. }) => {
                assert_eq!(entry, "reports/planted.md");
            }
            other => panic!("expected an unexpected member, got {other:?}"),
        }
    }

    #[test]
    fn a_removed_member_fails_verification() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = EngagementKey::generate();
        let path = package(
            dir.path(),
            &[
                ("README.md", "read me"),
                ("reports/a/report.md", "findings"),
            ],
            Some(&key),
        );
        let stripped = rewrite(&path, |name, bytes| {
            (name != "reports/a/report.md").then(|| bytes.to_vec())
        });
        match verify(&stripped, &retained(&key)) {
            Err(SigningError::MemberMissing { entry, .. }) => {
                assert_eq!(entry, "reports/a/report.md");
            }
            other => panic!("expected a missing member, got {other:?}"),
        }
    }

    #[test]
    fn a_rewritten_manifest_fails_the_signature_before_the_hashes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = EngagementKey::generate();
        let path = package(dir.path(), &[("README.md", "read me")], Some(&key));
        // Re-render the manifest over a digest the forger prefers, keeping the
        // original signature. The hashes would now agree with the altered file;
        // only the signature says the manifest is not the one that was signed.
        let forged = rewrite(&path, |name, bytes| {
            Some(if name == MANIFEST_ENTRY {
                let doc = ManifestDoc {
                    version: MANIFEST_VERSION,
                    algorithm: "sha256".to_owned(),
                    signature: Some(SignatureBlock {
                        scheme: "ed25519".to_owned(),
                        key_fingerprint: key.fingerprint(),
                        file: SIGNATURE_ENTRY.to_owned(),
                    }),
                    file: vec![ManifestFile {
                        entry: "README.md".to_owned(),
                        sha256: to_hex(&Sha256::digest(b"read me")),
                        bytes: 7,
                    }],
                };
                let mut text = toml::to_string_pretty(&doc).expect("render");
                text.push_str("\n# forged\n");
                text.into_bytes()
            } else {
                bytes.to_vec()
            })
        });
        assert!(matches!(
            verify(&forged, &retained(&key)),
            Err(SigningError::SignatureInvalid { .. })
        ));
    }

    #[test]
    fn a_zip_that_carries_no_manifest_is_refused_rather_than_passing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("plain.zip");
        let file = std::fs::File::create(&path).expect("create");
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zip.start_file("README.md".to_owned(), options)
            .expect("start");
        std::io::Write::write_all(&mut zip, b"read me").expect("write");
        zip.finish().expect("finish");
        assert!(matches!(
            verify(&path, &retained(&EngagementKey::generate())),
            Err(SigningError::ManifestMissing { .. })
        ));
    }

    #[test]
    fn a_key_never_prints_its_bytes_through_debug() {
        let key = EngagementKey::generate();
        let rendered = format!("{key:?}");
        assert!(!rendered.contains(&key.private_hex()), "{rendered}");
        assert!(rendered.contains(&key.fingerprint()), "{rendered}");
    }
}
