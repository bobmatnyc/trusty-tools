//! The Gaps & Caveats phrases a `trusty-review` report states and its readers
//! key on (#6784).
//!
//! Why: `trusty-review` writes a report's Gaps & Caveats list, and
//! `trusty-audit` reads that list back off the report's JSON twin to say, in the
//! bundle index, whether the static-analysis lane ran. The two crates must not
//! depend on each other — DOC-67 §5 puts the seam at a FILE, not a Cargo edge —
//! so the phrase they agree on was spelled as a literal on each side. It drifted
//! immediately: `trusty-review` has TWO total-collapse paths, and the second one
//! led with "trusty-analyze data unavailable" while the reader matched
//! "trusty-analyze lane DID NOT RUN". A client-build collapse therefore rendered
//! as a lane that RAN, and undercounted the index's dead-lane tally.
//! What: `ANALYZE_LANE_DEAD_HEADLINE`, which every writer leads its line with,
//! and `analyze_lane_is_dead`, the one predicate every reader applies to a
//! report's `gaps` list. Zero dependencies — a `&str` and a `bool`.
//!
//! This is the [`crate::env_vars`] pattern applied to report prose rather than
//! to environment-variable names, and for the same reason that module gives: a
//! literal in each crate is the drift it exists to stop. Version skew across the
//! file boundary is unchanged and still real — a `trusty-audit` built against an
//! older `trusty-common` holds whatever value it was compiled with. What this
//! removes is two spellings inside ONE build.
//! Test: `the_headline_is_stable`, `a_dead_lane_line_is_recognised`,
//! `a_live_lane_is_not_recognised_as_dead`.

/// The phrase every total analyze-lane collapse leads its Gaps & Caveats line
/// with (#6784, #6811).
///
/// Why: see the module doc — this is the one string the writer emits and the
/// reader matches, so a report and the index that summarises it cannot disagree
/// about whether static analysis ran.
/// What: the literal phrase, with no trailing punctuation. A writer formats
/// `"{ANALYZE_LANE_DEAD_HEADLINE} — <its own detail>"`; a reader tests
/// containment through [`analyze_lane_is_dead`], never with its own literal.
/// Test: `the_headline_is_stable`.
pub const ANALYZE_LANE_DEAD_HEADLINE: &str = "trusty-analyze lane DID NOT RUN";

/// Whether a report's Gaps & Caveats list says its analyze lane assessed
/// nothing (#6784).
///
/// Why: containment, not equality — each writer appends its own detail after the
/// headline (which repository count collapsed, or that the client never built),
/// and a reader that matched whole lines would recognise neither.
/// What: true when any gap line contains [`ANALYZE_LANE_DEAD_HEADLINE`]. A
/// partially degraded lane leads with a different phrase and reads false, which
/// is correct: some applications WERE assessed.
/// Test: `a_dead_lane_line_is_recognised`, `a_live_lane_is_not_recognised_as_dead`.
pub fn analyze_lane_is_dead<'a>(gaps: impl IntoIterator<Item = &'a str>) -> bool {
    gaps.into_iter()
        .any(|gap| gap.contains(ANALYZE_LANE_DEAD_HEADLINE))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the literal so a rename here can never silently change what every
    /// writer emits and every reader matches.
    #[test]
    fn the_headline_is_stable() {
        assert_eq!(
            ANALYZE_LANE_DEAD_HEADLINE,
            "trusty-analyze lane DID NOT RUN"
        );
    }

    /// The predicate matches a line that CONTAINS the headline, because every
    /// writer appends its own detail after it.
    #[test]
    fn a_dead_lane_line_is_recognised() {
        let line =
            format!("{ANALYZE_LANE_DEAD_HEADLINE} — 0 of 59 application(s) assessed, 59 failed.");
        assert!(analyze_lane_is_dead([line.as_str()]));
        assert!(analyze_lane_is_dead(["unrelated gap", line.as_str()]));
    }

    /// A partially degraded lane, and an empty list, are not a dead lane.
    #[test]
    fn a_live_lane_is_not_recognised_as_dead() {
        assert!(!analyze_lane_is_dead(std::iter::empty()));
        assert!(!analyze_lane_is_dead([
            "trusty-analyze lane partially degraded — 58 of 59 application(s) assessed, 1 failed."
        ]));
    }
}
