//! The line `trusty-audit verify` prints (#5563).
//!
//! Why: a check whose result a human has to interpret is not a check. The
//! operator gets one verdict word, the fingerprint of the key that produced it,
//! and how much of the package it covers — and, for a signed package, the limit
//! on what that proves, in the same words `super::render_package` uses when the
//! package is written.
//!
//! What: [`render`], one arm per [`Verdict`]. Its own file because
//! `super`'s is at the 500-SLOC production cap.
//! Test: `crate::session::verify::verify_tests::a_signed_package_verifies_through_the_session_seam`,
//! `crate::session::verify::verify_tests::an_unsigned_package_has_its_own_exit_code`.

use crate::package::signing::Verdict;
use crate::session::verify::VerifyReport;

/// The verdict as the CLI prints it.
///
/// Test: see the module docs.
pub(super) fn render(report: &VerifyReport) -> String {
    let package = report.package.display();
    match &report.verdict {
        Verdict::Signed {
            key_fingerprint,
            files,
        } => format!(
            "Verified: {package}\n  \
             signed with engagement key {key_fingerprint} — tamper-evident in transit, \
             not proof about the sender\n  \
             {}, every one hashing to what the signed manifest records\n",
            super::count_of(*files, "member", "members")
        ),
        // #5563: an unsigned package is what a rewritten manifest also produces,
        // so this line says what is NOT known rather than reporting a weaker
        // pass. `Outcome::exit_code` gives it a status of its own to match.
        Verdict::Unsigned { files } => format!(
            "UNSIGNED: {package}\n  \
             the package carries a manifest and no signature, so nothing in it is \
             authenticated — an engagement with no [signing] key produces this, and so \
             does anyone who rewrites the manifest of one that had\n  \
             {} listed, none of them checked against a key\n",
            super::count_of(*files, "member", "members")
        ),
    }
}
