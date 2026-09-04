//! What a failed `trusty-search` invocation actually said, once its
//! informational chatter is set aside.
//!
//! Why: `trusty-search`'s `main.rs` prints an update-availability notice on
//! stderr before every human-facing subcommand, `index` and `index add`
//! included, whenever crates.io has a newer version. Both of this crate's
//! trusty-search refusal messages used to report the FIRST non-empty stderr
//! line as the reason, so on a machine one release behind the notice landed
//! first and became "the reason" while the real diagnostic was discarded. In a
//! delivered client audit that masked 60 of 61 repositories, and because
//! `crate::grounding` short-circuits the whole evidence-grounding pass on an
//! indexing failure, every one of those reports rendered "not assessed" with
//! nothing recorded to diagnose (#6720).
//!
//! What: [`reason`], the one line both refusal messages quote. It skips blank
//! lines and update-availability notices and returns the first line that is
//! neither. When nothing is left it says so explicitly, and it distinguishes
//! stderr that was empty from stderr that carried only a notice — the second
//! is the state this module exists to make visible, so it must not read as the
//! first.
//!
//! The notice's wording is owned by `trusty_common::update::notice`, which this
//! crate cannot name: `trusty-common`'s `update` module is behind the
//! `update-check` feature and this crate does not enable it. The coupling is
//! held by a test fixture carrying the real emitted text instead.
//!
//! Test: `search_stderr_tests`.

/// The prefix every trusty-* update-availability notice starts with.
///
/// `trusty_common::update::notice` formats `"Update available: {crate} {latest}
/// (you have {current}) — run: cargo install {crate} --locked"`, and the
/// shorter `upgrade`-command variants in `trusty-search` and `trusty-memory`
/// open the same way. Matching the prefix covers all of them without pinning
/// the rest of the sentence.
const UPDATE_NOTICE_PREFIX: &str = "Update available:";

/// Said when a failed invocation wrote nothing to stderr at all.
const NO_REASON: &str = "no reason given";

/// Said when everything a failed invocation wrote was informational.
///
/// Kept distinct from [`NO_REASON`] deliberately: silence and
/// "it only told us about an upgrade" are different facts to whoever reads the
/// gap, and collapsing them would hide the case #6720 was filed about.
const ONLY_A_NOTICE: &str = "no reason given (stderr carried only an update-availability notice)";

/// The one line of a failed `trusty-search` invocation's stderr worth quoting.
///
/// Why: see the module docs — an update-availability notice is not a failure,
/// and reporting it as one silently discards the real diagnostic.
/// What: decodes `stderr` lossily, trims each line, and returns the first that
/// is neither empty nor an update-availability notice. With no such line it
/// returns [`ONLY_A_NOTICE`] if a notice was skipped and [`NO_REASON`]
/// otherwise. Always one line, because both callers embed it in a single-line
/// refusal the recipient reads in a report gap.
/// Test: `search_stderr_tests::{a_real_reason_survives_a_leading_update_notice,
/// a_notice_alone_is_not_a_reason, empty_stderr_is_distinguished_from_a_notice,
/// the_first_real_line_wins_when_nothing_is_informational}`.
pub(crate) fn reason(stderr: &[u8]) -> String {
    let said = String::from_utf8_lossy(stderr);
    let mut skipped_a_notice = false;
    for line in said.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        // #6720: informational, never the failure — skip wherever it lands
        // rather than only in the leading position, so an ordering change in
        // the child does not quietly restore the masking.
        if line.starts_with(UPDATE_NOTICE_PREFIX) {
            skipped_a_notice = true;
            continue;
        }
        return line.to_owned();
    }
    if skipped_a_notice {
        ONLY_A_NOTICE.to_owned()
    } else {
        NO_REASON.to_owned()
    }
}

#[cfg(test)]
mod search_stderr_tests {
    use super::*;

    /// Verbatim from the delivered audit run of 2026-08-25 that #6720 was filed
    /// from: `trusty-search` 0.47.0 with 0.49.1 on crates.io. Held as a literal
    /// because this crate does not enable `trusty-common`'s `update-check`
    /// feature and so cannot call the formatter that produced it.
    const REAL_NOTICE: &str = "Update available: trusty-search 0.49.1 (you have 0.47.0) — run: cargo install \
         trusty-search --locked";

    /// The regression. Before #6720 this returned the notice and the real
    /// diagnostic was lost — across 60 of 61 repositories in one client run.
    #[test]
    fn a_real_reason_survives_a_leading_update_notice() {
        let said = format!("{REAL_NOTICE}\nindexing refused: root is not allowlisted\n");
        assert_eq!(
            reason(said.as_bytes()),
            "indexing refused: root is not allowlisted"
        );
    }

    /// The edge the fix has to answer explicitly: the notice is all there was.
    /// Reporting it would be the original bug; reporting plain silence would
    /// hide that a notice was printed at all.
    #[test]
    fn a_notice_alone_is_not_a_reason() {
        let said = format!("{REAL_NOTICE}\n");
        let reason = reason(said.as_bytes());
        assert!(!reason.contains("Update available"), "{reason}");
        assert_eq!(reason, ONLY_A_NOTICE);
    }

    /// A silent failure and a notice-only failure are different diagnoses.
    #[test]
    fn empty_stderr_is_distinguished_from_a_notice() {
        assert_eq!(reason(b""), NO_REASON);
        assert_eq!(reason(b"   \n\n  \n"), NO_REASON);
        assert_ne!(NO_REASON, ONLY_A_NOTICE);
    }

    /// The unchanged behaviour: with no notice in the way, the first real line
    /// is still the answer, still trimmed, still one line.
    #[test]
    fn the_first_real_line_wins_when_nothing_is_informational() {
        let reason = reason(b"\n  indexing refused: root is not allowlisted\nsecond line\n");
        assert_eq!(reason, "indexing refused: root is not allowlisted");
        assert_eq!(reason.lines().count(), 1);
    }

    /// The notice is skipped wherever it lands, not only first — the child is
    /// free to reorder its own stderr.
    #[test]
    fn a_trailing_notice_is_skipped_too() {
        let said = format!("boom: the daemon refused\n{REAL_NOTICE}\n");
        assert_eq!(reason(said.as_bytes()), "boom: the daemon refused");
    }

    /// The shorter `upgrade`-command wording matches the same prefix.
    #[test]
    fn the_short_upgrade_wording_is_a_notice_too() {
        let said = "Update available: trusty-search 0.49.1 (you have 0.47.0)\n";
        assert_eq!(reason(said.as_bytes()), ONLY_A_NOTICE);
    }

    /// Invalid UTF-8 degrades rather than panicking.
    #[test]
    fn non_utf8_stderr_is_a_reason_not_a_panic() {
        assert_eq!(reason(&[0xff, 0xfe]), "\u{fffd}\u{fffd}");
    }
}
