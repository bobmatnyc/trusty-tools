//! PM effort scoring — the EFFORT tier of the Activity / Work / Effort model
//! for tickets (issue #3915, epic #3914).
//!
//! Why: `fact_pm_work` (#3916) answers whether a ticket is real management
//! labor. It does not say how much complexity producing it required, so a
//! one-paragraph bug and a decomposed platform epic still count the same.
//! This module scores that complexity, and the score is persisted to
//! `fact_pm_effort` (`core::db::pm_effort`).
//!
//! What: [`score`] is a deterministic function of one ticket's item type,
//! age, child count, description length, comment count, transition count and
//! story points. Every input is already in the database — there is no LLM
//! tier in v1, matching `core::pm_work`.
//!
//! Test: `tests` in `tests.rs`.
//!
//! # Two guards the raw formula does not provide
//!
//! **Recency.** Issue #3915's motivating case is three epics authored on
//! 2026-07-09 that had zero children at scoring time — too new to have been
//! decomposed, not simple. A ticket whose type accrues children and that is
//! younger than [`thresholds::RECENCY_FLOOR_DAYS`] is recorded as
//! [`ScoreStatus::DeferredRecent`] with NO score rather than a low one. See
//! [`score`].
//!
//! **Meaningfulness.** Only tickets `fact_pm_work` marks meaningful are
//! scored at all. That gate lives in the loader
//! ([`crate::core::db::load_effort_candidates`]) rather than here, because it
//! is a question about a different table's verdict, not about this ticket's
//! complexity.
//!
//! # Versioning
//!
//! Every persisted row records [`FORMULA_VERSION`]. The numbers in
//! [`thresholds`] are v1 constants: a retune ships as a NEW version string,
//! never as an edit of the values there.

pub mod extract;

use std::fmt;

/// Weight set version baked into every persisted `fact_pm_effort` row.
///
/// Why: issue #3915 marks every threshold and weight "TBD, refine with
/// product", so v1 will be retuned. An already-stored score must keep naming
/// the weight set that produced it.
/// What: the string `"pm-effort-1"`, written to
/// `fact_pm_effort.formula_version`.
/// Test: `scorer_reports_the_v1_formula_version`.
pub const FORMULA_VERSION: &str = "pm-effort-1";

/// v1 weights, caps and thresholds (`formula_version = "pm-effort-1"`).
///
/// Why: issue #3915 states the scoring formula as an example and marks the
/// bucket boundaries "TBD by data distribution". Everything tunable
/// therefore lives in this one block, so a retune is a reviewable diff of
/// this module plus a bump of [`FORMULA_VERSION`] — never a scattered edit
/// that silently changes what already-stored rows meant. Nothing outside
/// this block is tunable.
/// What: the additive weights, their per-input caps, the score range, the
/// bucket boundaries, the recency floor, and the story-point field
/// spellings.
/// Test: `weights_sum_to_the_documented_score_ceiling`,
/// `bucket_boundaries_are_the_documented_thresholds`.
pub mod thresholds {
    /// Floor of the v1 score range. Every scored ticket gets at least this,
    /// because a meaningful ticket is by definition non-zero labor.
    pub const BASE_SCORE: f64 = 1.0;

    /// Lower bound of the stored `effort_score`, equal to [`BASE_SCORE`].
    pub const MIN_SCORE: f64 = 1.0;

    /// Upper bound of the stored `effort_score`. Issue #3915 asks for a
    /// 1–50 range so a reader cannot mistake a PM score for an engineering
    /// one (`fact_commit_effort` runs 2.5–45). The two remain incommensurable
    /// units regardless — see #3917.
    pub const MAX_SCORE: f64 = 50.0;

    /// Score added per direct child of a decomposed ticket.
    pub const CHILD_WEIGHT: f64 = 2.0;
    /// Children beyond this count add nothing: past ten, child count says
    /// more about decomposition style than about complexity.
    pub const CHILD_COUNT_CAP: u32 = 10;

    /// Description words that buy one point of score.
    pub const BODY_WORDS_PER_POINT: f64 = 40.0;
    /// Ceiling on the description term, so a pasted stack trace cannot
    /// dominate the score.
    pub const BODY_POINTS_CAP: f64 = 12.0;

    /// Score added per comment on the ticket.
    pub const COMMENT_WEIGHT: f64 = 0.8;
    /// Comments beyond this count add nothing.
    pub const COMMENT_COUNT_CAP: u32 = 10;

    /// Score added per recorded status transition.
    pub const TRANSITION_WEIGHT: f64 = 0.5;
    /// Transitions beyond this count add nothing.
    pub const TRANSITION_COUNT_CAP: u32 = 12;

    /// Score added per story point, when story points are present at all.
    pub const STORY_POINT_WEIGHT: f64 = 0.4;
    /// Ceiling on the story-point term. Deliberately small: the field is 76%
    /// NULL on the source instance (issue #3915), so letting it move the
    /// score much would systematically advantage the minority of tickets
    /// that carry it.
    pub const STORY_POINT_CAP: f64 = 3.0;
    /// Smallest story-point value treated as real. Below this is a
    /// placeholder, not an estimate.
    pub const STORY_POINTS_MIN: f64 = 0.5;
    /// Largest story-point value treated as real. Above this the field is
    /// being used for something other than an estimate — the same custom
    /// field ID means something different in another project.
    pub const STORY_POINTS_MAX: f64 = 40.0;

    /// Lowest score in the MEDIUM bucket; below it is LOW.
    pub const BUCKET_MEDIUM_MIN: f64 = 15.0;
    /// Lowest score in the HIGH bucket.
    pub const BUCKET_HIGH_MIN: f64 = 30.0;

    /// A ticket of an age-floored type younger than this many days is not
    /// scored. Issue #3915's acceptance criterion, stated verbatim: "only
    /// score epics ≥7 days old".
    pub const RECENCY_FLOOR_DAYS: i64 = 7;

    /// Item types whose complexity accrues after creation, lowercased.
    ///
    /// These are the types that get DECOMPOSED — their `epic_children_count`
    /// input is zero on the day they are filed and grows as the team breaks
    /// them down. Scoring one immediately reads "not yet decomposed" as
    /// "simple", which is the recency bias the floor exists to prevent. A
    /// bug or task has no such input, so the floor would only delay its
    /// score for nothing.
    pub const AGE_FLOORED_ITEM_TYPES: &[&str] = &["epic", "feature", "initiative"];

    /// Per-project story-point custom-field IDs observed on the source JIRA
    /// instance (issue #3915). Tried in order after the name-keyed
    /// spellings.
    pub const STORY_POINT_FIELD_IDS: &[&str] = &[
        "customfield_10004",
        "customfield_10016",
        "customfield_13001",
        "customfield_13737",
    ];

    /// Story-point field spellings matched by name rather than ID, compared
    /// after lowercasing and dropping non-alphanumeric characters. The first
    /// two mirror `JiraClient::get_story_point_field`'s live discovery;
    /// `estimate` is Linear's spelling of the same quantity.
    pub const STORY_POINT_FIELD_NAMES: &[&str] = &["storypoints", "storypointestimate", "estimate"];
}

/// T-shirt bucket for a scored ticket.
///
/// Why: the consumer (cto-reports) groups by bucket rather than by raw
/// score, the same way `fact_commit_effort` is read through its T-shirt bin.
/// What: a three-valued enum stored as text in
/// `fact_pm_effort.effort_bucket`, NULL when the ticket was not scored.
/// Test: `effort_bucket_round_trips_through_its_wire_string`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffortBucket {
    /// Score below [`thresholds::BUCKET_MEDIUM_MIN`].
    Low,
    /// Score in `[BUCKET_MEDIUM_MIN, BUCKET_HIGH_MIN)`.
    Medium,
    /// Score at or above [`thresholds::BUCKET_HIGH_MIN`].
    High,
}

impl EffortBucket {
    /// The value stored in `fact_pm_effort.effort_bucket`.
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
        }
    }

    /// Parse a stored `fact_pm_effort.effort_bucket` value.
    ///
    /// Returns `None` for an unrecognised string — a row written by a newer
    /// `formula_version` — so callers decide whether to skip or fail.
    #[must_use]
    pub fn from_wire_str(s: &str) -> Option<Self> {
        match s {
            "LOW" => Some(Self::Low),
            "MEDIUM" => Some(Self::Medium),
            "HIGH" => Some(Self::High),
            _ => None,
        }
    }

    /// The bucket a v1 score falls in.
    #[must_use]
    pub fn of_score(score: f64) -> Self {
        if score >= thresholds::BUCKET_HIGH_MIN {
            Self::High
        } else if score >= thresholds::BUCKET_MEDIUM_MIN {
            Self::Medium
        } else {
            Self::Low
        }
    }
}

impl fmt::Display for EffortBucket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

/// Whether a ticket was scored, and if not, why not.
///
/// Why: a deferred ticket must be distinguishable from a genuinely simple
/// one. Storing the deferral as a status with a NULL score is what stops a
/// consumer averaging "too early to tell" in as a zero — issue #3915's
/// stated failure mode.
/// What: stored as text in `fact_pm_effort.score_status`; every row carries
/// one, so a consumer can filter without a COALESCE.
/// Test: `score_status_round_trips_through_its_wire_string`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScoreStatus {
    /// The ticket carries an `effort_score` and an `effort_bucket`.
    Scored,
    /// The ticket is inside the recency floor: too new for its complexity to
    /// have materialized. Score and bucket are NULL.
    DeferredRecent,
}

impl ScoreStatus {
    /// The value stored in `fact_pm_effort.score_status`.
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::Scored => "SCORED",
            Self::DeferredRecent => "DEFERRED_RECENT",
        }
    }

    /// Parse a stored `fact_pm_effort.score_status` value; `None` when
    /// unrecognised.
    #[must_use]
    pub fn from_wire_str(s: &str) -> Option<Self> {
        match s {
            "SCORED" => Some(Self::Scored),
            "DEFERRED_RECENT" => Some(Self::DeferredRecent),
            _ => None,
        }
    }
}

impl fmt::Display for ScoreStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

/// The five measured quantities the v1 formula reads, as stored on the row.
///
/// Separate from [`PmEffortInput`] so the persistence layer can carry the
/// inputs and the score together without restating each field.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EffortCounts {
    /// `work_items` rows naming this ticket as their parent.
    pub epic_children: u32,
    /// Words of plain-text description.
    pub description_words: u32,
    /// `fact_jira_comment_detail` rows for this ticket.
    pub comments: u32,
    /// `fact_ticket_transitions` rows for this ticket.
    pub transitions: u32,
    /// Story points, when the payload carried a plausible value. `None` is
    /// the common case (76% NULL on the source instance) and costs the
    /// ticket nothing beyond that one term — see [`score`].
    pub story_points: Option<f64>,
}

/// Everything the v1 scorer reads about one ticket.
#[derive(Debug, Clone, Copy)]
pub struct PmEffortInput<'a> {
    /// `work_items.item_type`, matched case-insensitively against
    /// [`thresholds::AGE_FLOORED_ITEM_TYPES`].
    pub item_type: &'a str,
    /// Whole days between the ticket's creation and the scoring run. `None`
    /// when the payload carried no parseable creation timestamp, in which
    /// case the recency floor cannot be applied and the ticket is scored.
    pub age_days: Option<i64>,
    /// The measured inputs.
    pub counts: EffortCounts,
}

/// Which inputs contributed a non-zero term to a score.
///
/// Why: issue #3915 requires that a missing story-point value degrade the
/// formula rather than zero it. A consumer therefore cannot assume two
/// scores were built from the same signals, and needs the row to say which
/// ones were.
/// What: one flag per formula term, serialized to
/// `fact_pm_effort.inputs_present` by [`InputsPresent::to_wire_string`].
/// A flag is set when the term's contribution is greater than zero — an
/// epic with no children and a ticket whose children could not be resolved
/// are both recorded as not having contributed a child term, because
/// neither moved the score.
/// Test: `inputs_present_names_only_contributing_terms`,
/// `a_ticket_with_no_signal_records_no_inputs`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputsPresent {
    /// The child-count term contributed.
    pub children: bool,
    /// The description-length term contributed.
    pub description: bool,
    /// The comment-count term contributed.
    pub comments: bool,
    /// The transition-count term contributed.
    pub transitions: bool,
    /// The story-point term contributed.
    pub story_points: bool,
}

impl InputsPresent {
    /// Serialize to the comma-separated `inputs_present` column value.
    ///
    /// Order is fixed (never the order the flags were set) so two runs over
    /// unchanged data produce byte-identical rows. `"NONE"` when nothing
    /// contributed, so the column is never empty.
    #[must_use]
    pub fn to_wire_string(self) -> String {
        let mut parts: Vec<&'static str> = Vec::with_capacity(5);
        if self.children {
            parts.push("CHILDREN");
        }
        if self.description {
            parts.push("DESCRIPTION");
        }
        if self.comments {
            parts.push("COMMENTS");
        }
        if self.transitions {
            parts.push("TRANSITIONS");
        }
        if self.story_points {
            parts.push("STORY_POINTS");
        }
        if parts.is_empty() {
            return "NONE".to_string();
        }
        parts.join(",")
    }
}

/// One ticket's effort verdict.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PmEffortScore {
    /// Whether the ticket was scored at all.
    pub status: ScoreStatus,
    /// The score, in `[MIN_SCORE, MAX_SCORE]`, rounded to two decimals.
    /// `None` exactly when `status` is not [`ScoreStatus::Scored`].
    pub effort_score: Option<f64>,
    /// The bucket the score falls in; `None` alongside a `None` score.
    pub effort_bucket: Option<EffortBucket>,
    /// Which terms contributed. All false for a deferred ticket.
    pub inputs_present: InputsPresent,
}

/// Whether `item_type` names a ticket type whose complexity accrues after
/// creation, and which the recency floor therefore guards.
///
/// Test: `the_recency_floor_applies_only_to_decomposable_types`.
#[must_use]
pub fn is_age_floored_type(item_type: &str) -> bool {
    let lower = item_type.trim().to_lowercase();
    thresholds::AGE_FLOORED_ITEM_TYPES.contains(&lower.as_str())
}

/// A story-point value the scorer will use, or `None`.
///
/// Why: the field is sourced from four per-project custom-field IDs and is
/// 76% NULL, so an out-of-range or nonsensical value is a routine
/// occurrence rather than a corrupt row. Issue #3915's acceptance criterion
/// is "no crash on inconsistent data".
/// What: rejects NaN, infinities, and anything outside
/// `[STORY_POINTS_MIN, STORY_POINTS_MAX]`, which the scorer then treats
/// identically to an absent value.
/// Test: `implausible_story_points_are_treated_as_absent`.
#[must_use]
pub fn plausible_story_points(points: f64) -> Option<f64> {
    (points.is_finite()
        && (thresholds::STORY_POINTS_MIN..=thresholds::STORY_POINTS_MAX).contains(&points))
    .then_some(points)
}

/// Score one ticket's PM effort under `formula_version = "pm-effort-1"`.
///
/// Why: see the module docs — the EFFORT tier of #3914.
/// What: the recency floor is applied first, then the score is the sum of
/// [`thresholds::BASE_SCORE`] and five independently capped terms, clamped
/// to `[MIN_SCORE, MAX_SCORE]` and rounded to two decimals:
///
/// | Term | Weight | Cap |
/// |---|---|---|
/// | children | [`thresholds::CHILD_WEIGHT`] each | [`thresholds::CHILD_COUNT_CAP`] children |
/// | description | 1 per [`thresholds::BODY_WORDS_PER_POINT`] words | [`thresholds::BODY_POINTS_CAP`] |
/// | comments | [`thresholds::COMMENT_WEIGHT`] each | [`thresholds::COMMENT_COUNT_CAP`] comments |
/// | transitions | [`thresholds::TRANSITION_WEIGHT`] each | [`thresholds::TRANSITION_COUNT_CAP`] transitions |
/// | story points | [`thresholds::STORY_POINT_WEIGHT`] each | [`thresholds::STORY_POINT_CAP`] |
///
/// The terms are additive and independent, which is what makes a missing
/// input DEGRADE the score rather than zero it: an absent or implausible
/// story-point value simply drops its term, and the other four still
/// produce a score. [`PmEffortScore::inputs_present`] records which terms
/// actually fired so a consumer never has to assume.
///
/// Test: `a_substantive_epic_scores_in_the_high_bucket`,
/// `missing_story_points_degrade_rather_than_zero_the_score`,
/// `a_recent_epic_is_deferred_rather_than_scored_low`,
/// `each_term_is_capped_independently`,
/// `the_score_never_leaves_the_documented_range`.
#[must_use]
pub fn score(input: &PmEffortInput<'_>) -> PmEffortScore {
    if is_age_floored_type(input.item_type)
        && input
            .age_days
            .is_some_and(|days| days < thresholds::RECENCY_FLOOR_DAYS)
    {
        return PmEffortScore {
            status: ScoreStatus::DeferredRecent,
            effort_score: None,
            effort_bucket: None,
            inputs_present: InputsPresent::default(),
        };
    }

    let counts = input.counts;
    let children =
        capped_count(counts.epic_children, thresholds::CHILD_COUNT_CAP) * thresholds::CHILD_WEIGHT;
    let description = (f64::from(counts.description_words) / thresholds::BODY_WORDS_PER_POINT)
        .min(thresholds::BODY_POINTS_CAP);
    let comments =
        capped_count(counts.comments, thresholds::COMMENT_COUNT_CAP) * thresholds::COMMENT_WEIGHT;
    let transitions = capped_count(counts.transitions, thresholds::TRANSITION_COUNT_CAP)
        * thresholds::TRANSITION_WEIGHT;
    let story_points = counts
        .story_points
        .and_then(plausible_story_points)
        .map_or(0.0, |p| {
            (p * thresholds::STORY_POINT_WEIGHT).min(thresholds::STORY_POINT_CAP)
        });

    let raw =
        thresholds::BASE_SCORE + children + description + comments + transitions + story_points;
    let clamped = round2(raw.clamp(thresholds::MIN_SCORE, thresholds::MAX_SCORE));

    PmEffortScore {
        status: ScoreStatus::Scored,
        effort_score: Some(clamped),
        effort_bucket: Some(EffortBucket::of_score(clamped)),
        inputs_present: InputsPresent {
            children: children > 0.0,
            description: description > 0.0,
            comments: comments > 0.0,
            transitions: transitions > 0.0,
            story_points: story_points > 0.0,
        },
    }
}

/// `count`, capped at `cap`, as an `f64`.
fn capped_count(count: u32, cap: u32) -> f64 {
    f64::from(count.min(cap))
}

/// Round to two decimals so a re-run over unchanged inputs stores a
/// byte-identical `effort_score` and the idempotency contract holds.
fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
