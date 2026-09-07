//! Checking a received handoff package against the retained public key (#5563).
//!
//! Why: #5481 signed the return package and left the check a library call, so
//! the only way to run it was to write a Rust program. The operator who
//! receives a zip needs one command, and a status their shell can read — DOC-68
//! §14 Q5 settles that as a CLI subcommand rather than a hand-run script.
//!
//! What: [`VerifyReport`] and [`verify`]. The check itself is
//! [`crate::package::signing::verify`] and stays the single implementation
//! behind every front end; this module reads the retained key off disk, hands
//! it over, and adds no second opinion about what verifies.
//!
//! A separate module rather than another method on `Session`, for the reason
//! [`super::init`] gives: `session.rs` sits at the 500-SLOC production cap.
//!
//! ## The key is named by the operator, never taken from the package
//!
//! There is no `[signing]` public half to fall back on. The engagement config
//! carries the PRIVATE key, it travels inside the inbound package, and a key
//! that arrives with the thing it authenticates authenticates nothing — so
//! deriving the public half from a config the recipient held would verify a
//! package against its own sender. `--public-key` names a file the auditor
//! retained out of band, which is the only key that answers the question.
//!
//! A file rather than a value on argv: argv lands in shell history and in
//! `ps`, and a retained key is already a file.
//!
//! ## Unsigned is not a pass
//!
//! An engagement that configured no key produces a package with no
//! `[signature]` table, and so does anyone who rewrites the manifest of a
//! signed one. [`crate::package::signing::verify`] reports both as
//! [`Verdict::Unsigned`], which is why this capability gives it a non-zero exit
//! of its own ([`super::EXIT_UNSIGNED`]) rather than folding it into success.
//! It is distinct from a signed package whose signature member was removed —
//! that is `SignatureMissing`, an error.
//!
//! Test: `verify_tests`.

use std::path::{Path, PathBuf};

use crate::error::AuditError;
use crate::package::signing::{self, RetainedKey, Verdict};

/// What [`verify`] concluded, and about which file.
///
/// Why: the verdict alone does not name the package, and the CLI line has to —
/// an operator checking a directory of returned zips reads the filename before
/// the fingerprint.
/// What: the package that was read, and the library's verdict over it,
/// unmodified.
/// Test: `verify_tests::a_signed_package_verifies_through_the_session_seam`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct VerifyReport {
    /// The package that was checked.
    pub package: PathBuf,
    /// What [`crate::package::signing::verify`] answered.
    pub verdict: Verdict,
}

impl VerifyReport {
    /// Whether the package carries no signature to check.
    ///
    /// Read by [`super::Outcome::exit_code`], so the "nothing here is
    /// authenticated" state reaches `$?` rather than only the printed text.
    /// Test: `verify_tests::an_unsigned_package_has_its_own_exit_code`.
    pub fn is_unsigned(&self) -> bool {
        matches!(self.verdict, Verdict::Unsigned { .. })
    }
}

/// Read the retained public key, then check `package` against it.
///
/// # Preconditions
/// `public_key` is a file holding the retained ed25519 public half as 64
/// lower-case hex characters, with surrounding whitespace ignored.
///
/// # Postconditions
/// Neither `package` nor anything beside it is written, read-only being the
/// whole point of a check. On `Ok`, the report's verdict is exactly what
/// [`crate::package::signing::verify`] returned.
///
/// What: reads the key, parses it, and calls the library. The two failures it
/// owns are its own — a key file that cannot be read, and one whose contents
/// are not a key. Everything after that is the library's verdict, carried up
/// unflattened by [`AuditError::Signing`]'s transparent wrapper so each of the
/// distinguishable failures still reads as itself.
/// Test: `verify_tests::a_signed_package_verifies_through_the_session_seam`,
/// `verify_tests::a_missing_key_file_is_refused_before_the_package_is_read`,
/// `verify_tests::a_key_file_that_is_not_a_key_is_refused`.
///
/// # Errors
///
/// [`AuditError::Read`] when `public_key` cannot be read, and
/// [`AuditError::Signing`] for a key that does not parse or for any verdict
/// [`crate::package::signing::verify`] refuses.
pub fn verify(package: &Path, public_key: &Path) -> Result<VerifyReport, AuditError> {
    let hex = std::fs::read_to_string(public_key).map_err(|source| AuditError::Read {
        path: public_key.to_path_buf(),
        source,
    })?;
    let retained = RetainedKey::from_hex(hex.trim())?;
    Ok(VerifyReport {
        package: package.to_path_buf(),
        verdict: signing::verify(package, &retained)?,
    })
}

#[cfg(test)]
mod verify_tests {
    use super::*;
    use crate::package::signing::fixture;
    use crate::package::signing::{EngagementKey, SigningError};
    use crate::session::{Command, EXIT_UNSIGNED, Outcome, Session};
    use crate::workdir::WorkDir;

    /// Where a package and its key live, with nothing else in the directory —
    /// so "did the check write anything" is answerable by listing it.
    struct Received {
        dir: tempfile::TempDir,
        package: PathBuf,
        key: PathBuf,
    }

    /// A package carrying two members, signed unless `signed` is false, beside
    /// a key file holding the public half of the key that signed it.
    fn received(signed: bool) -> Received {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = EngagementKey::generate();
        let package = fixture::package(
            dir.path(),
            &[
                ("README.md", "read me"),
                ("reports/a/report.md", "findings"),
            ],
            signed.then_some(&key),
        );
        let key_path = dir.path().join("retained.pub");
        std::fs::write(&key_path, format!("{}\n", key.public_hex())).expect("write the key");
        Received {
            dir,
            package,
            key: key_path,
        }
    }

    /// Drive one check through the door every front end uses.
    async fn execute(package: &Path, key: &Path) -> Result<Outcome, AuditError> {
        let root = package
            .parent()
            .expect("the package has a parent")
            .join("work");
        Session::new(WorkDir::new(root))
            .execute(Command::Verify {
                package: package.to_path_buf(),
                public_key: key.to_path_buf(),
            })
            .await
    }

    /// Every path under `dir`, relative and sorted — the "wrote nothing" probe.
    fn tree(dir: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path() != dir)
            .map(|e| {
                e.path()
                    .strip_prefix(dir)
                    .expect("under the root")
                    .to_path_buf()
            })
            .collect();
        found.sort();
        found
    }

    fn signing_error(outcome: Result<Outcome, AuditError>) -> SigningError {
        match outcome {
            Err(AuditError::Signing { source }) => source,
            other => panic!("expected a signing failure, got {other:?}"),
        }
    }

    /// #5563 closure condition 1: a valid, untampered package verifies, exits
    /// zero, and reports the key that signed it and how much it covers.
    #[tokio::test]
    async fn a_signed_package_verifies_through_the_session_seam() {
        let received = received(true);
        let outcome = execute(&received.package, &received.key)
            .await
            .expect("the package verifies");
        assert_eq!(outcome.exit_code(), 0);
        let Outcome::Verified(report) = &outcome else {
            panic!("expected a verification outcome, got {outcome:?}");
        };
        // Two: the manifest covers the delivered members, and neither it nor
        // the signature can appear in a manifest it is part of.
        match &report.verdict {
            Verdict::Signed {
                key_fingerprint,
                files,
            } => {
                assert_eq!(*files, 2);
                assert!(
                    crate::cli::render(&outcome).contains(key_fingerprint),
                    "the rendered line must name the key: {}",
                    crate::cli::render(&outcome)
                );
            }
            other => panic!("expected a signed verdict, got {other:?}"),
        }
        assert!(crate::cli::render(&outcome).contains("2 members"));
    }

    /// #5563 closure condition 4, and the reason this capability is safe to run
    /// on a file that arrived by email: it reads and writes nothing at all.
    #[tokio::test]
    async fn checking_a_package_writes_nothing_beside_it() {
        let received = received(true);
        let before = tree(received.dir.path());
        let bytes = std::fs::read(&received.package).expect("read the package");
        let modified = std::fs::metadata(&received.package)
            .and_then(|m| m.modified())
            .expect("the package has an mtime");

        execute(&received.package, &received.key)
            .await
            .expect("the package verifies");

        assert_eq!(tree(received.dir.path()), before, "no file may appear");
        assert_eq!(
            std::fs::read(&received.package).expect("read the package"),
            bytes,
            "the package's bytes may not change"
        );
        assert_eq!(
            std::fs::metadata(&received.package)
                .and_then(|m| m.modified())
                .expect("the package has an mtime"),
            modified,
            "the package may not even be touched"
        );
    }

    /// #5563 closure condition 3: an engagement that configured no key produces
    /// a package nothing authenticates, and that is neither success nor the
    /// removed-signature failure.
    #[tokio::test]
    async fn an_unsigned_package_has_its_own_exit_code() {
        let received = received(false);
        let outcome = execute(&received.package, &received.key)
            .await
            .expect("an unsigned package is a verdict, not an error");
        assert_eq!(outcome.exit_code(), EXIT_UNSIGNED);
        assert_ne!(EXIT_UNSIGNED, 0);
        let rendered = crate::cli::render(&outcome);
        assert!(rendered.contains("UNSIGNED"), "{rendered}");
        assert!(
            !rendered.contains("removed"),
            "an unsigned package must not read as a stripped signature: {rendered}"
        );
    }

    /// A signature member removed from a SIGNED package is the other outcome
    /// the line above must not be confused with — an error, not a verdict.
    #[tokio::test]
    async fn a_removed_signature_is_an_error_rather_than_unsigned() {
        let received = received(true);
        let stripped = fixture::rewrite(&received.package, |name, bytes| {
            (name != signing::SIGNATURE_ENTRY).then(|| bytes.to_vec())
        });
        let error = signing_error(execute(&stripped, &received.key).await);
        assert!(
            matches!(error, SigningError::SignatureMissing { .. }),
            "{error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("was removed after it was written")
        );
    }

    /// #5563 closure condition 2, first of four the archive itself can express:
    /// a member whose bytes changed after the manifest was signed.
    #[tokio::test]
    async fn a_tampered_member_is_refused_with_its_own_line() {
        let received = received(true);
        let tampered = fixture::rewrite(&received.package, |name, bytes| {
            Some(if name == "README.md" {
                b"read me differently".to_vec()
            } else {
                bytes.to_vec()
            })
        });
        let error = signing_error(execute(&tampered, &received.key).await);
        match &error {
            SigningError::ContentMismatch { entry, .. } => assert_eq!(entry, "README.md"),
            other => panic!("expected a content mismatch, got {other:?}"),
        }
        assert!(error.to_string().contains("was altered after the package"));
    }

    /// A member the signed manifest lists and the archive no longer holds.
    #[tokio::test]
    async fn a_removed_member_is_refused_with_its_own_line() {
        let received = received(true);
        let short = fixture::rewrite(&received.package, |name, bytes| {
            (name != "README.md").then(|| bytes.to_vec())
        });
        let error = signing_error(execute(&short, &received.key).await);
        match &error {
            SigningError::MemberMissing { entry, .. } => assert_eq!(entry, "README.md"),
            other => panic!("expected a missing member, got {other:?}"),
        }
        assert!(error.to_string().contains("is not in the package"));
    }

    /// A member nobody signed, added to the archive afterwards.
    #[tokio::test]
    async fn an_added_member_is_refused_with_its_own_line() {
        let received = received(true);
        let extra =
            fixture::with_extra_member(&received.package, "reports/b/report.md", b"planted");
        let error = signing_error(execute(&extra, &received.key).await);
        match &error {
            SigningError::MemberUnexpected { entry, .. } => {
                assert_eq!(entry, "reports/b/report.md");
            }
            other => panic!("expected an unexpected member, got {other:?}"),
        }
        assert!(error.to_string().contains("it was added"));
    }

    /// A key that is not the one the package was signed with — the closure
    /// condition's "wrong public key fails closed, never passes".
    #[tokio::test]
    async fn a_key_that_did_not_sign_the_package_is_refused() {
        let received = received(true);
        let other = received.dir.path().join("someone-else.pub");
        std::fs::write(&other, EngagementKey::generate().public_hex()).expect("write the key");
        let error = signing_error(execute(&received.package, &other).await);
        assert!(
            matches!(error, SigningError::SignatureInvalid { .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("does not verify"));
    }

    /// A second central-directory record under a name already in the archive.
    /// The parser hides it; the check refuses the whole file.
    #[tokio::test]
    async fn a_duplicate_directory_record_is_refused_with_its_own_line() {
        let received = received(true);
        let forged = fixture::with_duplicate_record(&received.package, "README.md", b"read MF");
        let error = signing_error(execute(&forged, &received.key).await);
        match &error {
            SigningError::DuplicateMember { entry, .. } => assert_eq!(entry, "README.md"),
            other => panic!("expected a duplicate member, got {other:?}"),
        }
        assert!(error.to_string().contains("extracts differently"));
    }

    /// A decoy end-of-central-directory record planted in the archive comment,
    /// which the parser takes and the raw walk does not. The two views then
    /// disagree about the member set, and disagreement is a refusal.
    ///
    /// `DirectoryMalformed` is the one refusal this seam cannot reach: an
    /// archive whose directory cannot be framed at all is refused by the zip
    /// parser inside `open`, one step before the raw walk runs, so it arrives
    /// as `Archive`. `super::super::signing::signing_tests` covers it against
    /// `zip_directory::member_names` directly.
    #[tokio::test]
    async fn a_directory_the_parser_and_the_walk_disagree_on_is_refused() {
        let received = received(true);
        let doctored = fixture::with_decoy_directory(&received.package);
        let error = signing_error(execute(&doctored, &received.key).await);
        match &error {
            SigningError::DirectoryMismatch { reason, .. } => {
                assert!(reason.contains("collapsed"), "{reason}");
            }
            other => panic!("expected a directory disagreement, got {other:?}"),
        }
        assert!(error.to_string().contains("disagree about the members"));
    }

    /// The key file is the operator's input, so its absence is named as a file
    /// they can go and find rather than as a verification failure.
    #[tokio::test]
    async fn a_missing_key_file_is_refused_before_the_package_is_read() {
        let received = received(true);
        let absent = received.dir.path().join("not-here.pub");
        match execute(&received.package, &absent).await {
            Err(AuditError::Read { path, .. }) => assert_eq!(path, absent),
            other => panic!("expected a read failure, got {other:?}"),
        }
    }

    /// A key file holding something that is not a key fails closed too.
    #[tokio::test]
    async fn a_key_file_that_is_not_a_key_is_refused() {
        let received = received(true);
        let bad = received.dir.path().join("junk.pub");
        std::fs::write(&bad, "not a key").expect("write the file");
        let error = signing_error(execute(&received.package, &bad).await);
        assert!(
            matches!(error, SigningError::KeyMalformed { .. }),
            "{error:?}"
        );
    }
}
