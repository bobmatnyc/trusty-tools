//! The complexity-absence claim check (#6691).
//!
//! Why: the graded report's Code Quality paragraph said no complexity
//! distribution was provided, four lines above the deterministic table that
//! rendered A 68618 / B 5241 / C 1836 / D 720 / F 733. The first fix keyed on
//! the two words that report happened to use, so every paraphrase of the same
//! false claim — "complexity could not be measured", "no complexity metrics
//! were computed" — still shipped. What separates an absence claim from a claim
//! about the distribution's SHAPE is the negation and the supply verb, not the
//! word "distribution", so the word pair is gone.
//!
//! It also summed the buckets report-wide. A CAST estate report covers several
//! applications, and one of them genuinely having no metrics makes "App B's
//! complexity was not available" TRUE — a sentence the report-wide sum deleted
//! anyway, under a note quoting App A's count. Counts are per application here,
//! and a sentence is refuted only by the applications it is about.
//! What: [`ComplexityFacts`] holds one `(application, functions)` pair per
//! repository, read from the same buckets `reporter_codesec::bucket_summary`
//! renders into the `cq_complexity` cell. [`drop_false_absence`] cuts every
//! sentence the facts refute and records one [`Correction`] per cut.
//! Test: `synthesize_grounding_tests.rs`.

use super::model::ReportModel;
use super::synthesize_grounding::Correction;
use super::synthesize_grounding_text::{contains_name, remove_sentence, sentences};
use super::synthesize_grounding_vocab::{ABSENCE_NEGATIONS, COMPLEXITY_WORD, DATA_SUPPLY_WORDS};

/// The complexity measurement each application contributed, as the Code Quality
/// table renders it.
///
/// Why: both halves of the check need the same numbers the table shows — the
/// gate that decides whether a sentence is false, and the note that says what
/// refuted it.
/// What: one entry per repository, `(display name, functions across its
/// buckets)`; `0` for a repository whose analyze lane contributed nothing.
/// Test: `synthesize_grounding_tests::a_per_application_absence_claim_survives_when_that_app_has_none`.
#[derive(Debug, Default, Clone)]
pub(super) struct ComplexityFacts {
    per_app: Vec<(String, u64)>,
}

impl ComplexityFacts {
    /// Read the per-application counts off the model.
    pub(super) fn from_model(model: &ReportModel) -> Self {
        ComplexityFacts {
            per_app: model
                .repositories
                .iter()
                .map(|r| {
                    let functions = r.metrics.as_ref().map_or(0, |m| {
                        m.complexity.buckets.iter().map(|b| b.count).sum::<u64>()
                    });
                    (r.name.clone(), functions)
                })
                .collect(),
        }
    }

    /// The measured function count that refutes this sentence, or `None` when
    /// the sentence is true for the applications it is about.
    ///
    /// Why: an absence claim naming one application is a claim about THAT
    /// application, and only its own measurement can contradict it. A claim
    /// naming none is report-wide, so it stands as long as any application
    /// genuinely measured nothing.
    /// What: `lower_sentence` is already lowercased. Applications are matched by
    /// name on word boundaries ([`contains_name`]); `Some(total)` over the
    /// matched set when every one of them measured a bucket, else `None`.
    /// Test: `synthesize_grounding_tests::{a_no_complexity_data_claim_is_dropped_when_the_table_renders_one,
    /// a_per_application_absence_claim_survives_when_that_app_has_none,
    /// a_report_wide_absence_claim_survives_when_one_application_has_none}`.
    fn refuted_by(&self, lower_sentence: &str) -> Option<u64> {
        let named: Vec<u64> = self
            .per_app
            .iter()
            .filter(|(name, _)| contains_name(lower_sentence, &name.to_lowercase()))
            .map(|(_, n)| *n)
            .collect();
        let scope = if named.is_empty() {
            self.per_app.iter().map(|(_, n)| *n).collect()
        } else {
            named
        };
        if scope.is_empty() || scope.contains(&0) {
            return None;
        }
        Some(scope.iter().sum())
    }
}

/// Remove every sentence claiming the run was given no complexity data that the
/// rendered table refutes.
///
/// Why: feeding the distribution into the digest is the derivation half of
/// #6691; this is the mechanical half, so the contradiction cannot ship even
/// when the model writes past its input.
/// What: a sentence qualifies when it carries [`COMPLEXITY_WORD`], an
/// [`ABSENCE_NEGATIONS`] word and a [`DATA_SUPPLY_WORDS`] word — the last two
/// together are what leave a claim about the distribution's shape alone. It is
/// then cut only if [`ComplexityFacts::refuted_by`] says the applications it is
/// about did measure one. Returns the surviving text and one note per cut.
/// Test: `synthesize_grounding_tests.rs`.
pub(super) fn drop_false_absence(facts: &ComplexityFacts, text: &str) -> (String, Vec<Correction>) {
    let mut out = text.to_string();
    let mut notes = Vec::new();
    for sentence in sentences(text) {
        let lower = sentence.to_lowercase();
        if !lower.contains(COMPLEXITY_WORD) {
            continue;
        }
        if !ABSENCE_NEGATIONS.iter().any(|n| lower.contains(n)) {
            continue;
        }
        if !DATA_SUPPLY_WORDS.iter().any(|w| lower.contains(w)) {
            continue;
        }
        let Some(measured) = facts.refuted_by(&lower) else {
            continue;
        };
        out = remove_sentence(&out, sentence);
        // The distribution is the report's own measurement, not any one §5.x
        // finding, so there is no subject to number.
        notes.push(Correction {
            text: format!(
                "a sentence claiming no complexity data was provided was dropped — the Code \
                 Quality table renders one over {measured} functions"
            ),
            subject: None,
        });
    }
    (out.trim().to_string(), notes)
}
