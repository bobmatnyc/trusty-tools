//! Objective observations attached to a candidate row.
//!
//! Why: the report deliberately emits **no suggested classification**, and this
//! module is where that restraint is enforced. ADR-0028 §C4 measured the
//! alternative directly: content-classifying the 654 `resume-target` drawers
//! gives 71.4% point-in-time but 25.5% standing, and `standing-instruction` is
//! overloaded in the opposite direction — drawer `f59fb536` carries it while its
//! content is a one-shot action. The ADR's conclusion is that "any design that
//! infers tier from the existing tags inherits this ambiguity". A verdict column
//! built on those tags would be wrong for a quarter of the rows while looking
//! exactly as confident as the right ones, and the human doing the triage would
//! have no way to tell which quarter. So each signal states a fact the reader
//! can check against the row beside it, and stops there.
//!
//! What: six checks, each true or false from the drawer row and the scanned log
//! window alone. They are listed on the row so a reader can see *why* something
//! is near the top; they never combine into a score and never order the output —
//! injection frequency does that.
//!
//! Test: `tags_are_reported_verbatim`, `date_stamp_only_scans_the_opening`,
//! `weight_retained_needs_both_age_and_weight`, `no_signal_implies_empty_list`,
//! `predates_log_window_fires_only_outside_coverage`.

use chrono::{DateTime, Utc};
use trusty_common::memory_core::decay::DecayConfig;
use trusty_common::memory_core::palace::Drawer;

/// Characters of content scanned for an opening date stamp.
///
/// Why: the point-in-time drawers §C7 names put their date in the headline
/// ("SESSION CHECKPOINT 2026-07-16 — …"). Scanning the whole body would instead
/// match any drawer that happens to cite a date anywhere, which is most of them.
const DATE_SCAN_CHARS: usize = 120;

/// Age past which [`Signal::WeightRetained`] can fire.
const WEIGHT_RETAINED_MIN_AGE_DAYS: f32 = 14.0;

/// Fraction of base importance still retained for [`Signal::WeightRetained`].
const WEIGHT_RETAINED_FRACTION: f32 = 0.8;

/// A checkable observation about a drawer. Never a conclusion about its tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// Carries one of the tags §C4 found spanning both tiers. Reported because
    /// the reader will want to see it, not because it decides anything.
    Tagged(String),
    /// The first [`DATE_SCAN_CHARS`] characters contain a `YYYY-MM-DD` date.
    DateStamped,
    /// `importance` is at the 1.0 ceiling — the §C5 privilege dial, fully open.
    MaxImportance,
    /// No `expires_at` is set, so nothing retires this drawer.
    NoExpiry,
    /// Old enough to be stale, yet the 90-day half-life still leaves most of its
    /// weight — the §C7 arithmetic, evaluated for this specific row.
    WeightRetained,
    /// The drawer was created before the scanned log window opens, so part of
    /// its life is unmeasured.
    ///
    /// Why this is its own signal: a 0-injection row means two opposite things
    /// that warrant opposite decisions. Without this marker, "nobody retrieves
    /// this, it costs nothing, leave it" and "the logs do not reach back far
    /// enough to know" are indistinguishable on the page, and a reader would
    /// have to compare each row's age against the coverage header by hand.
    PredatesLogWindow,
}

impl Signal {
    /// Short label for the table column.
    pub fn label(&self) -> String {
        match self {
            Signal::Tagged(t) => format!("tag:{t}"),
            Signal::DateStamped => "date-stamped".to_string(),
            Signal::MaxImportance => "importance=1.0".to_string(),
            Signal::NoExpiry => "no-expiry".to_string(),
            Signal::WeightRetained => "weight-retained".to_string(),
            Signal::PredatesLogWindow => "predates-log-window".to_string(),
        }
    }
}

/// Tags worth surfacing, in the order §C4's census reports them.
const REPORTED_TAGS: [&str; 4] = [
    "status",
    "resume-target",
    "standing-instruction",
    "bob-decision",
];

/// Collect every signal true of `drawer`.
///
/// Why: gathering them in one pass keeps the "observation, not verdict" rule in
/// a single reviewable place.
/// What: checks the four reported tags, the opening date stamp, the importance
/// ceiling, the absence of an expiry, the retained-weight condition, and whether
/// the drawer predates `window_start` — the earliest hook-log entry scanned, or
/// `None` when no log was read at all.
/// Test: `tags_are_reported_verbatim`, `no_signal_implies_empty_list`,
/// `predates_log_window_fires_only_outside_coverage`.
pub fn observe(drawer: &Drawer, age_days: f32, window_start: Option<DateTime<Utc>>) -> Vec<Signal> {
    let mut out = Vec::new();
    for tag in REPORTED_TAGS {
        if drawer.tags.iter().any(|t| t == tag) {
            out.push(Signal::Tagged(tag.to_string()));
        }
    }
    if opens_with_date(drawer.content()) {
        out.push(Signal::DateStamped);
    }
    if drawer.importance >= 1.0 {
        out.push(Signal::MaxImportance);
    }
    if drawer.expires_at.is_none() {
        out.push(Signal::NoExpiry);
    }
    let decayed = DecayConfig::default().effective_importance(drawer.importance, age_days, 0.0);
    if age_days >= WEIGHT_RETAINED_MIN_AGE_DAYS
        && drawer.importance > 0.0
        && decayed / drawer.importance >= WEIGHT_RETAINED_FRACTION
    {
        out.push(Signal::WeightRetained);
    }
    // #4891: only meaningful when a window exists. With no log scanned at all,
    // every count is 0 for a different reason, and the report says that once at
    // the top rather than tagging all 2,287 rows with it.
    if window_start.is_some_and(|start| drawer.created_at < start) {
        out.push(Signal::PredatesLogWindow);
    }
    out
}

/// True when the opening of `content` carries a `YYYY-MM-DD` date.
///
/// What: scans the first [`DATE_SCAN_CHARS`] characters for four digits, a
/// dash, two digits, a dash, two digits. Deliberately shape-only — it does not
/// validate the date, because a malformed date in a headline is still a date
/// stamp for the reader's purposes.
/// Test: `date_stamp_only_scans_the_opening`.
fn opens_with_date(content: &str) -> bool {
    let head: Vec<char> = content.chars().take(DATE_SCAN_CHARS).collect();
    head.windows(10).any(|w| {
        w[0..4].iter().all(char::is_ascii_digit)
            && w[4] == '-'
            && w[5..7].iter().all(char::is_ascii_digit)
            && w[7] == '-'
            && w[8..10].iter().all(char::is_ascii_digit)
    })
}
