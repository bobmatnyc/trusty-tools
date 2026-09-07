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
//! ## The member set is established before anything is read
//!
//! `zip` keys its entry table by member name, so an archive whose central
//! directory repeats a name presents one member here and a different one to
//! `zipfile` or Info-ZIP. [`verify`] therefore walks the raw directory itself
//! ([`super::zip_directory`]) before reading the manifest, refuses a repeat as
//! [`SigningError::DuplicateMember`], and refuses any other disagreement with
//! the parser as [`SigningError::DirectoryMismatch`]. Nothing after that point
//! sizes an allocation from the archive's own declared sizes either — members
//! are streamed through a fixed buffer.
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

    /// The raw central directory holds two records under one member name.
    ///
    /// Why (security review of #5481): `zip` 2.4.2 keys its entry table by name
    /// and collapses the second record before any caller sees it, so `verify`
    /// would check whichever copy that parser picked while `zipfile`, Info-ZIP
    /// or Finder extracted the other. A repeated name is refused outright — it
    /// has no legitimate use in a package this crate wrote.
    #[error(
        "{path}: the central directory holds more than one record named {entry} — the archive \
         extracts differently depending on which tool opens it, so nothing about it can be verified"
    )]
    DuplicateMember {
        /// The package that was read.
        path: PathBuf,
        /// The name carried by more than one record.
        entry: String,
    },

    /// The raw directory and the zip parser disagree about the member set.
    ///
    /// The duplicate check above catches a repeated name; this catches every
    /// other way the two views could diverge, so a shape neither this crate nor
    /// its review anticipated fails closed rather than passing on the parser's
    /// word.
    #[error(
        "{path}: the raw central directory and the zip parser disagree about the members: {reason}"
    )]
    DirectoryMismatch {
        /// The package that was read.
        path: PathBuf,
        /// How the two views differ.
        reason: String,
    },

    /// The central directory could not be framed.
    #[error("{path}: the central directory cannot be read: {reason}")]
    DirectoryMalformed {
        /// The package that was read.
        path: PathBuf,
        /// What stopped the walk.
        reason: String,
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
    // Security review of #5481: FIRST, before a single member is read. The zip
    // parser's view of the archive is deduped by name, so an archive whose raw
    // directory repeats a name has no single answer to "what is in it" — and
    // reading the manifest through the parser would already have picked one
    // copy. See `super::zip_directory`.
    let members = agreed_members(&mut archive, package)?;
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
    check_members(&mut archive, package, &doc.file, &members)?;
    Ok(Verdict::Signed {
        key_fingerprint: block.key_fingerprint.clone(),
        files: doc.file.len(),
    })
}

/// The member set, once the raw directory and the zip parser agree on it.
///
/// Why: `zip` 2.4.2 keys its entry table by name, so a second central-directory
/// record under an existing name never reaches a caller — see
/// [`super::zip_directory`]'s module docs for what that lets an archive do.
/// What: walks the raw directory, refuses two records carrying the same name
/// BYTES, then refuses any count that differs from what the parser exposes.
///
/// Comparing bytes and counts rather than decoded names is deliberate. `zip`
/// picks UTF-8 or CP437 by general-purpose flag bit 11, so a decoded comparison
/// would refuse a legitimate CP437-flagged member — and it would still have to
/// answer the harder case, two records whose different bytes decode alike,
/// which the parser also collapses. The count catches that one without this
/// module owning an encoding rule at all: a collapse of any kind leaves the
/// parser exposing fewer members than the directory holds records.
///
/// # Postconditions
/// On `Ok`, the returned names are the parser's, and the raw directory holds
/// exactly as many records with no two carrying the same bytes.
/// Test: `super::signing_tests::a_duplicate_central_directory_record_is_refused`,
/// `super::signing_tests::the_raw_directory_matches_what_the_zip_parser_exposes`,
/// `super::signing_tests::a_cp437_flagged_name_is_not_refused`.
fn agreed_members(archive: &mut Archive, package: &Path) -> Result<Vec<String>, SigningError> {
    let raw = super::zip_directory::member_names(package)?;
    let mut seen: std::collections::BTreeSet<&[u8]> = std::collections::BTreeSet::new();
    for name in &raw {
        if !seen.insert(name.as_slice()) {
            return Err(SigningError::DuplicateMember {
                path: package.to_path_buf(),
                // Lossy for the MESSAGE only. Nothing decides anything on it.
                entry: String::from_utf8_lossy(name).into_owned(),
            });
        }
    }
    let exposed: Vec<String> = archive.file_names().map(str::to_owned).collect();
    if exposed.len() != raw.len() {
        return Err(SigningError::DirectoryMismatch {
            path: package.to_path_buf(),
            reason: format!(
                "the directory holds {} record(s) and the parser exposes {} member(s), so at \
                 least one record was collapsed",
                raw.len(),
                exposed.len()
            ),
        });
    }
    Ok(exposed)
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
    members: &[String],
) -> Result<(), SigningError> {
    for file in listed {
        // Streamed, never materialized: a member can be a multi-gigabyte
        // extract database, and its declared size is attacker-controlled.
        let actual = digest_member(archive, package, &file.entry)?.ok_or_else(|| {
            SigningError::MemberMissing {
                path: package.to_path_buf(),
                entry: file.entry.clone(),
            }
        })?;
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
    // `members` is the raw central directory's own list, already proven to
    // agree with the parser's by `agreed_members` — so a member the parser
    // hides is not a member this loop can miss.
    for name in members {
        if name == MANIFEST_ENTRY || name == SIGNATURE_ENTRY || name.ends_with('/') {
            continue;
        }
        if !listed.iter().any(|f| &f.entry == name) {
            return Err(SigningError::MemberUnexpected {
                path: package.to_path_buf(),
                entry: name.clone(),
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

/// Bytes read per turn of either member loop.
///
/// Fixed, because the alternative is sizing an allocation from the archive's own
/// declared size — which is attacker-controlled metadata, not a measurement
/// (security review of #5481). `credential_scan::copy_member` streams the same
/// way on the write side.
const CHUNK_BYTES: usize = 64 * 1024;

/// A member this module reads WHOLE — the manifest and its signature — is
/// refused past this size rather than buffered.
///
/// Both are small by construction: the manifest is one row per delivered file
/// and the signature is 129 bytes. A package claiming otherwise is not one this
/// crate wrote, and reading it would mean trusting an attacker about how much
/// memory to spend. The ceiling is generous enough for a package with hundreds
/// of thousands of members.
const MAX_INLINE_MEMBER_BYTES: u64 = 32 * 1024 * 1024;

/// One member's uncompressed bytes, or `None` when the archive has no such
/// entry. An unreadable entry that IS present is an error, never a `None` —
/// silently treating a read failure as absence is how a tampered archive passes.
///
/// Only for the two small members this module reads whole; everything else goes
/// through [`digest_member`], which never materializes one. The read streams
/// through a fixed buffer and stops at [`MAX_INLINE_MEMBER_BYTES`], so no
/// allocation here is sized from the archive's own claim about the member.
/// Test: `super::signing_tests::a_forged_member_size_does_not_size_an_allocation`.
fn read_member(
    archive: &mut Archive,
    package: &Path,
    entry: &str,
) -> Result<Option<Vec<u8>>, SigningError> {
    let Some(mut member) = member(archive, package, entry)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    let mut buffer = vec![0_u8; CHUNK_BYTES];
    loop {
        let read = read_chunk(&mut member, package, &mut buffer)?;
        if read == 0 {
            return Ok(Some(bytes));
        }
        if bytes.len() as u64 + read as u64 > MAX_INLINE_MEMBER_BYTES {
            return Err(SigningError::DirectoryMalformed {
                path: package.to_path_buf(),
                reason: format!(
                    "{entry} is larger than the {MAX_INLINE_MEMBER_BYTES} bytes this reader will \
                     hold for it"
                ),
            });
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
}

/// One member's SHA-256, computed without holding the member in memory.
///
/// Why: an extract database can exceed 4 GiB, and its declared size in the
/// archive is a number an attacker chose. Streaming answers both — the hash is
/// over the bytes that actually decompress, and the only allocation is one
/// fixed buffer (security review of #5481).
/// Test: `super::signing_tests::a_tampered_member_fails_verification`,
/// `super::signing_tests::a_forged_member_size_does_not_size_an_allocation`.
fn digest_member(
    archive: &mut Archive,
    package: &Path,
    entry: &str,
) -> Result<Option<String>, SigningError> {
    let Some(mut member) = member(archive, package, entry)? else {
        return Ok(None);
    };
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; CHUNK_BYTES];
    loop {
        let read = read_chunk(&mut member, package, &mut buffer)?;
        if read == 0 {
            return Ok(Some(to_hex(&digest.finalize())));
        }
        digest.update(&buffer[..read]);
    }
}

/// The named member, or `None` when the archive does not hold one.
fn member<'a>(
    archive: &'a mut Archive,
    package: &Path,
    entry: &str,
) -> Result<Option<zip::read::ZipFile<'a>>, SigningError> {
    match archive.by_name(entry) {
        Ok(m) => Ok(Some(m)),
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(SigningError::Archive {
            path: package.to_path_buf(),
            source: std::io::Error::other(e),
        }),
    }
}

fn read_chunk(
    member: &mut zip::read::ZipFile<'_>,
    package: &Path,
    buffer: &mut [u8],
) -> Result<usize, SigningError> {
    use std::io::Read as _;
    member.read(buffer).map_err(|source| SigningError::Archive {
        path: package.to_path_buf(),
        source,
    })
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

    /// CRC-32 (IEEE), bitwise. Ten lines of test code rather than a dependency
    /// pulled in so one hand-built local header can be genuinely extractable.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFF_u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    fn le16(v: u16) -> [u8; 2] {
        v.to_le_bytes()
    }

    fn le32(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    fn read_u32(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
    }

    /// The offset of the archive's end-of-central-directory record.
    fn eocd_at(raw: &[u8]) -> usize {
        (0..=raw.len() - 22)
            .rev()
            .find(|at| read_u32(raw, *at) == 0x0605_4b50)
            .expect("an EOCD")
    }

    /// A STORED local file header plus its data, for a hand-built second copy.
    fn local_header(name: &str, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&le32(0x0403_4b50));
        out.extend_from_slice(&le16(20)); // version needed
        out.extend_from_slice(&le16(0)); // flags
        out.extend_from_slice(&le16(0)); // method: stored
        out.extend_from_slice(&le16(0)); // time
        out.extend_from_slice(&le16(0)); // date
        out.extend_from_slice(&le32(crc32(body)));
        out.extend_from_slice(&le32(body.len() as u32));
        out.extend_from_slice(&le32(body.len() as u32));
        out.extend_from_slice(&le16(name.len() as u16));
        out.extend_from_slice(&le16(0)); // extra
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(body);
        out
    }

    /// A central-directory file header naming `name` at `header_start`.
    fn central_header(name: &str, body: &[u8], header_start: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&le32(0x0201_4b50));
        out.extend_from_slice(&le16(20)); // version made by
        out.extend_from_slice(&le16(20)); // version needed
        out.extend_from_slice(&le16(0)); // flags
        out.extend_from_slice(&le16(0)); // method: stored
        out.extend_from_slice(&le16(0)); // time
        out.extend_from_slice(&le16(0)); // date
        out.extend_from_slice(&le32(crc32(body)));
        out.extend_from_slice(&le32(body.len() as u32));
        out.extend_from_slice(&le32(body.len() as u32));
        out.extend_from_slice(&le16(name.len() as u16));
        out.extend_from_slice(&le16(0)); // extra
        out.extend_from_slice(&le16(0)); // comment
        out.extend_from_slice(&le16(0)); // disk
        out.extend_from_slice(&le16(0)); // internal attrs
        out.extend_from_slice(&le32(0)); // external attrs
        out.extend_from_slice(&le32(header_start));
        out.extend_from_slice(name.as_bytes());
        out
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

    /// 🔴 Security review of #5481: `zip` 2.4.2 keys its entry table by member
    /// name, so a second central-directory record under an already-signed name
    /// collapses before any caller sees it — `file_names()` yields the name
    /// once and `by_name` resolves to one record. `verify` would then report
    /// `Signed` over whichever copy this parser picked while Python's
    /// `zipfile`, Info-ZIP or Finder extracted the other.
    ///
    /// The archive here is hand-built from a valid signed package's bytes: a
    /// second STORED local header carrying altered data is appended, then a
    /// duplicate central-directory record naming the same member and pointing
    /// at it, with the EOCD's entry count, directory size and directory offset
    /// patched to frame the enlarged directory. Both copies are genuinely
    /// extractable — the planted one carries a correct CRC.
    #[test]
    fn a_duplicate_central_directory_record_is_refused() {
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
        let raw = std::fs::read(&path).expect("read the package");

        let eocd = eocd_at(&raw);
        let entries = u16::from_le_bytes(raw[eocd + 10..eocd + 12].try_into().expect("two bytes"));
        let directory_size = read_u32(&raw, eocd + 12) as usize;
        let directory_at = read_u32(&raw, eocd + 16) as usize;

        let planted = b"read MF";
        let mut forged = raw[..directory_at].to_vec();
        let planted_at = forged.len() as u32;
        forged.extend_from_slice(&local_header("README.md", planted));
        let directory_moved_to = forged.len() as u32;
        forged.extend_from_slice(&raw[directory_at..directory_at + directory_size]);
        let duplicate = central_header("README.md", planted, planted_at);
        let duplicate_len = duplicate.len();
        forged.extend_from_slice(&duplicate);
        // The EOCD, reframed: one more entry, a longer directory, a new offset.
        let mut eocd_record = raw[eocd..].to_vec();
        eocd_record[8..10].copy_from_slice(&le16(entries + 1));
        eocd_record[10..12].copy_from_slice(&le16(entries + 1));
        eocd_record[12..16].copy_from_slice(&le32((directory_size + duplicate_len) as u32));
        eocd_record[16..20].copy_from_slice(&le32(directory_moved_to));
        forged.extend_from_slice(&eocd_record);

        let forged_path = dir.path().join("forged.zip");
        std::fs::write(&forged_path, &forged).expect("write the forged package");

        // The exploit is real: the parser hides the second record entirely, so
        // nothing downstream of it could have caught this.
        let hidden = open(&forged_path).expect("the forged archive still opens");
        assert_eq!(
            hidden.file_names().filter(|n| *n == "README.md").count(),
            1,
            "the parser is expected to collapse the duplicate — that is the whole finding"
        );
        assert_eq!(
            super::super::zip_directory::member_names(&forged_path)
                .expect("the raw directory walks")
                .iter()
                .filter(|n| n.as_slice() == b"README.md")
                .count(),
            2,
            "the raw directory must show what the parser hides"
        );

        match verify(&forged_path, &retained(&key)) {
            Err(SigningError::DuplicateMember { entry, .. }) => assert_eq!(entry, "README.md"),
            other => panic!("expected a duplicate member, got {other:?}"),
        }
    }

    /// The raw walk and the parser agree on an archive nobody tampered with, so
    /// the check above refuses forgeries rather than ordinary packages.
    #[test]
    fn the_raw_directory_matches_what_the_zip_parser_exposes() {
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
        let archive = open(&path).expect("open");
        let exposed: std::collections::BTreeSet<String> =
            archive.file_names().map(str::to_owned).collect();
        let raw: std::collections::BTreeSet<String> =
            super::super::zip_directory::member_names(&path)
                .expect("walks")
                .iter()
                .map(|n| String::from_utf8_lossy(n).into_owned())
                .collect();
        assert_eq!(raw, exposed);
        assert_eq!(raw.len(), 4, "two members plus the manifest and signature");
    }

    /// 🔴 Security review of #5481: a member's declared uncompressed size is
    /// metadata an attacker writes, so it must never size an allocation. Here
    /// the central-directory record claims ~2 GiB for a seven-byte member; the
    /// read streams through a fixed buffer, so the call returns a verdict
    /// instead of trying to reserve what the archive asked for.
    #[test]
    fn a_forged_member_size_does_not_size_an_allocation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = EngagementKey::generate();
        let path = package(dir.path(), &[("README.md", "read me")], Some(&key));
        let mut raw = std::fs::read(&path).expect("read the package");

        let eocd = eocd_at(&raw);
        let directory_at = read_u32(&raw, eocd + 16) as usize;
        // Walk to the record for README.md and overwrite its uncompressed size.
        let mut at = directory_at;
        let patched = loop {
            assert_eq!(
                read_u32(&raw, at),
                0x0201_4b50,
                "a central-directory record"
            );
            let name_len =
                u16::from_le_bytes(raw[at + 28..at + 30].try_into().expect("two bytes")) as usize;
            let extra_len =
                u16::from_le_bytes(raw[at + 30..at + 32].try_into().expect("two bytes")) as usize;
            let comment_len =
                u16::from_le_bytes(raw[at + 32..at + 34].try_into().expect("two bytes")) as usize;
            let name = String::from_utf8_lossy(&raw[at + 46..at + 46 + name_len]).into_owned();
            if name == "README.md" {
                raw[at + 24..at + 28].copy_from_slice(&le32(0x7FFF_FFF0));
                break true;
            }
            at += 46 + name_len + extra_len + comment_len;
        };
        assert!(patched);

        let forged_path = dir.path().join("forged-size.zip");
        std::fs::write(&forged_path, &raw).expect("write");

        // The point is that this RETURNS. A verdict either way is a pass; an
        // abort or an out-of-memory kill is the failure this guards against.
        let outcome = verify(&forged_path, &retained(&key));
        assert!(
            matches!(
                outcome,
                Ok(Verdict::Signed { .. }) | Err(SigningError::Archive { .. })
            ),
            "expected a verdict rather than an abort, got {outcome:?}"
        );
    }

    /// A minimal one-member archive, built byte by byte, with `flags` on both
    /// headers and `comment` after the EOCD. The escape hatch the framing tests
    /// need: a real `ZipWriter` cannot produce any of these shapes.
    fn hand_built(name: &[u8], body: &[u8], flags: u16, comment: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&le32(0x0403_4b50));
        out.extend_from_slice(&le16(20));
        out.extend_from_slice(&le16(flags));
        out.extend_from_slice(&le16(0));
        out.extend_from_slice(&le16(0));
        out.extend_from_slice(&le16(0));
        out.extend_from_slice(&le32(crc32(body)));
        out.extend_from_slice(&le32(body.len() as u32));
        out.extend_from_slice(&le32(body.len() as u32));
        out.extend_from_slice(&le16(name.len() as u16));
        out.extend_from_slice(&le16(0));
        out.extend_from_slice(name);
        out.extend_from_slice(body);

        let directory_at = out.len() as u32;
        let mut record = Vec::new();
        record.extend_from_slice(&le32(0x0201_4b50));
        record.extend_from_slice(&le16(20));
        record.extend_from_slice(&le16(20));
        record.extend_from_slice(&le16(flags));
        record.extend_from_slice(&le16(0));
        record.extend_from_slice(&le16(0));
        record.extend_from_slice(&le16(0));
        record.extend_from_slice(&le32(crc32(body)));
        record.extend_from_slice(&le32(body.len() as u32));
        record.extend_from_slice(&le32(body.len() as u32));
        record.extend_from_slice(&le16(name.len() as u16));
        record.extend_from_slice(&le16(0));
        record.extend_from_slice(&le16(0));
        record.extend_from_slice(&le16(0));
        record.extend_from_slice(&le16(0));
        record.extend_from_slice(&le32(0));
        record.extend_from_slice(&le32(0));
        record.extend_from_slice(name);
        let directory_size = record.len() as u32;
        out.extend_from_slice(&record);

        out.extend_from_slice(&le32(0x0605_4b50));
        out.extend_from_slice(&le16(0));
        out.extend_from_slice(&le16(0));
        out.extend_from_slice(&le16(1));
        out.extend_from_slice(&le16(1));
        out.extend_from_slice(&le32(directory_size));
        out.extend_from_slice(&le32(directory_at));
        out.extend_from_slice(&le16(comment.len() as u16));
        out.extend_from_slice(comment);
        out
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write");
        path
    }

    /// The EOCD sits behind the archive comment, so the scan has to reach past
    /// one. An ordinary comment must leave the verdict alone.
    #[test]
    fn an_ordinary_archive_comment_does_not_change_the_verdict() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = EngagementKey::generate();
        let commented = write(
            dir.path(),
            "commented.zip",
            &with_comment(
                &std::fs::read(package(dir.path(), &[("README.md", "read me")], Some(&key)))
                    .expect("read"),
                b"delivered 2026-09-07",
            ),
        );

        assert_eq!(
            super::super::zip_directory::member_names(&commented)
                .expect("the EOCD is found behind the comment")
                .len(),
            3,
            "one member plus the manifest and its signature"
        );
        let outcome = verify(&commented, &retained(&key));
        assert!(
            matches!(outcome, Ok(Verdict::Signed { .. })),
            "a comment must not change the verdict: {outcome:?}"
        );
    }

    /// 🔴 The archive comment is arbitrary bytes whoever sent the package
    /// wrote, so "the last EOCD signature in the file" is a rule they can
    /// answer: a planted `PK\x05\x06` sits at a HIGHER offset than the real
    /// record. The walk is not fooled — a candidate must frame itself, its
    /// declared comment length reaching exactly the end of the file, and its
    /// directory must end where the record describing it begins.
    ///
    /// `zip` 2.4.2's own scan IS fooled by this archive and reports it empty,
    /// which is the case this whole check exists for: the two views disagree,
    /// so verification refuses rather than reporting Signed over a member set
    /// that is not what the file would extract.
    #[test]
    fn a_comment_carrying_a_fake_eocd_signature_is_refused_not_believed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = EngagementKey::generate();
        let path = package(dir.path(), &[("README.md", "read me")], Some(&key));
        // A whole fake EOCD inside the comment, with bytes after it.
        let mut decoy = Vec::new();
        decoy.extend_from_slice(&le32(0x0605_4b50));
        decoy.extend_from_slice(&[0_u8; 18]);
        decoy.extend_from_slice(b"trailing");
        let doctored = write(
            dir.path(),
            "decoy.zip",
            &with_comment(&std::fs::read(&path).expect("read"), &decoy),
        );

        assert_eq!(
            super::super::zip_directory::member_names(&doctored)
                .expect("the real EOCD is still found")
                .len(),
            3,
            "the walk must not take the decoy"
        );
        let parser = open(&doctored).expect("opens");
        assert_eq!(
            parser.file_names().count(),
            0,
            "zip 2.4.2 is expected to take the decoy — that is why the two views are compared"
        );
        match verify(&doctored, &retained(&key)) {
            Err(SigningError::DirectoryMismatch { reason, .. }) => {
                assert!(reason.contains("collapsed"), "{reason}");
            }
            other => panic!("expected a refusal rather than a verdict, got {other:?}"),
        }
    }

    /// `raw` with `comment` appended and the EOCD's comment length updated.
    fn with_comment(raw: &[u8], comment: &[u8]) -> Vec<u8> {
        let eocd = eocd_at(raw);
        let mut out = raw[..eocd].to_vec();
        let mut record = raw[eocd..].to_vec();
        record[20..22].copy_from_slice(&le16(comment.len() as u16));
        out.extend_from_slice(&record);
        out.extend_from_slice(comment);
        out
    }

    /// A directory that stops in the middle of a record is refused rather than
    /// walked off the end of the buffer.
    #[test]
    fn a_directory_truncated_mid_record_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut raw = hand_built(b"README.md", b"read me", 0, b"");
        let eocd = eocd_at(&raw);
        // Claim the whole directory but leave twenty bytes of it: the record's
        // fixed part no longer fits, let alone its name.
        let size = read_u32(&raw, eocd + 12);
        raw[eocd + 12..eocd + 16].copy_from_slice(&le32(size));
        let directory_at = read_u32(&raw, eocd + 16) as usize;
        let mut truncated = raw[..directory_at + 20].to_vec();
        truncated.extend_from_slice(&raw[eocd..]);
        let eocd = eocd_at(&truncated);
        truncated[eocd + 16..eocd + 20].copy_from_slice(&le32(directory_at as u32));
        let path = write(dir.path(), "truncated.zip", &truncated);

        let outcome = super::super::zip_directory::member_names(&path);
        assert!(
            matches!(outcome, Err(SigningError::DirectoryMalformed { .. })),
            "{outcome:?}"
        );
    }

    /// A saturated entry count says zip64, and a zip64 archive with no locator
    /// is refused rather than read as if the classic fields meant anything.
    #[test]
    fn a_saturated_eocd_with_no_zip64_locator_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut raw = hand_built(b"README.md", b"read me", 0, b"");
        let eocd = eocd_at(&raw);
        raw[eocd + 10..eocd + 12].copy_from_slice(&le16(u16::MAX));
        let path = write(dir.path(), "saturated.zip", &raw);

        let outcome = super::super::zip_directory::member_names(&path);
        assert!(
            matches!(outcome, Err(SigningError::DirectoryMalformed { .. })),
            "{outcome:?}"
        );
    }

    /// An entry count far past what the directory can hold is refused on the
    /// record that does not frame, without indexing past the buffer.
    #[test]
    fn a_forged_entry_count_is_refused_without_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut raw = hand_built(b"README.md", b"read me", 0, b"");
        let eocd = eocd_at(&raw);
        // 0xFFFE rather than 0xFFFF: a huge count that is NOT the zip64
        // sentinel, so the walk runs rather than diverting to the locator.
        raw[eocd + 8..eocd + 10].copy_from_slice(&le16(0xFFFE));
        raw[eocd + 10..eocd + 12].copy_from_slice(&le16(0xFFFE));
        let path = write(dir.path(), "forged-count.zip", &raw);

        let outcome = super::super::zip_directory::member_names(&path);
        assert!(
            matches!(outcome, Err(SigningError::DirectoryMalformed { .. })),
            "{outcome:?}"
        );
    }

    /// 🔴 `zip` decodes a member name as UTF-8 or CP437 by general-purpose flag
    /// bit 11 (`read.rs:1313-1316`). A walk that decoded every name one fixed
    /// way would disagree with the parser about a legitimate CP437-flagged
    /// name — 0x81 is `ü` there and not valid UTF-8 on its own — and refuse an
    /// ordinary archive. The comparison is bytes and counts, so it does not.
    #[test]
    fn a_cp437_flagged_name_is_not_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Flags 0: bit 11 clear, so the name is CP437 to every reader.
        let raw = hand_built(b"gr\x81ss.md", b"hello", 0, b"");
        let path = write(dir.path(), "cp437.zip", &raw);

        let walked = super::super::zip_directory::member_names(&path).expect("walks");
        assert_eq!(walked, vec![b"gr\x81ss.md".to_vec()], "raw, undecoded");

        let archive = open(&path).expect("open");
        let exposed: Vec<String> = archive.file_names().map(str::to_owned).collect();
        assert_eq!(
            exposed,
            vec!["grüss.md".to_owned()],
            "the parser decodes CP437"
        );
        assert_ne!(
            String::from_utf8_lossy(&walked[0]),
            exposed[0],
            "a decoded comparison would have disagreed here — that is the finding"
        );

        // The archive carries no manifest, so this stops at the manifest rather
        // than at the member set: the encoding did not refuse it.
        let outcome = verify(&path, &retained(&EngagementKey::generate()));
        assert!(
            matches!(outcome, Err(SigningError::ManifestMissing { .. })),
            "a CP437 name must not be refused as a directory disagreement: {outcome:?}"
        );
    }
}
