//! Value provenance labels for rendered report fields (#2342.4 / wave 2).
//!
//! Why: owner feedback was that "not stated in source data" was everywhere and
//! the report read as an empty template.  The fix is to label where each value
//! came from instead of blanking it: a reader must be able to tell a repository
//! `measured` figure from a manifest `declared` one from an LLM `inferred`
//! judgement, and `not stated` is reserved for the genuinely unknowable.  A
//! compact, consistent marker keeps tables readable while making provenance
//! auditable.
//! What: [`Provenance`] enumerates the four kinds; [`Provenance::tag`] renders a
//! compact trailing superscript marker appended to a value, and [`tag`] is a
//! convenience that suffixes a value string.  [`LEGEND`] is the one-line key
//! rendered once near the top of the report so the markers are self-explaining.
//! Test: `provenance` inline tests assert the marker text and the tag helper.

/// The origin of a rendered value.
///
/// Why: every substantive value in the report carries one of these so provenance
/// is explicit rather than implied; the guardrail and the omit-empty pass both
/// key off the distinction between real data and a genuine gap.
/// What: `Measured` = computed from the repository; `Declared` = supplied via the
/// manifest, analyst input, or an external metrics JSON; `Inferred` = LLM
/// judgement grounded in repo evidence; `NotStated` = genuinely unknowable.
/// Test: `provenance::tests::tags_are_stable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Computed directly from the repository (scan-derived figures).
    Measured,
    /// Supplied via manifest, analyst input, or an external metrics JSON.
    Declared,
    /// LLM judgement, grounded in cited repository evidence.
    Inferred,
    /// Genuinely unknowable from the available inputs (e.g. deal client name).
    NotStated,
}

/// The compact trailing marker for `Measured` provenance.
pub const MEASURED_TAG: &str = " ⁽ᵐ⁾";
/// The compact trailing marker for `Declared` provenance.
pub const DECLARED_TAG: &str = " ⁽ᵈ⁾";
/// The compact trailing marker for `Inferred` provenance.
pub const INFERRED_TAG: &str = " ⁽ⁱ⁾";

/// The one-line provenance legend rendered once near the top of the report.
///
/// Why: the superscript markers must be self-explaining without external docs.
/// What: a single markdown line keyed to the three visible markers (`NotStated`
/// has no marker — those values are omitted and listed under Gaps & Caveats).
/// Test: `reporter_tests.rs` asserts the legend is present in the rendered head.
pub const LEGEND: &str = "> Provenance: ⁽ᵐ⁾ measured (computed from the repository) · ⁽ᵈ⁾ declared (manifest / metrics input) · ⁽ⁱ⁾ inferred (LLM, evidence-grounded). Genuinely unknowable fields are omitted and listed under Gaps & Caveats.";

impl Provenance {
    /// The compact trailing marker for this provenance (empty for `NotStated`).
    ///
    /// Why: rendering appends this to a value so the origin travels with it in
    /// every table cell and prose line without a separate column everywhere.
    /// What: returns one of the `*_TAG` constants, or `""` for `NotStated`.
    /// Test: `provenance::tests::tags_are_stable`.
    pub fn tag(self) -> &'static str {
        match self {
            Provenance::Measured => MEASURED_TAG,
            Provenance::Declared => DECLARED_TAG,
            Provenance::Inferred => INFERRED_TAG,
            Provenance::NotStated => "",
        }
    }
}

/// Suffix a value string with its provenance marker.
///
/// Why: the single place the reporter turns a bare value + provenance into the
/// rendered cell text, keeping the marker format consistent everywhere.
/// What: returns `value` with the provenance tag appended; `NotStated` returns
/// `value` unchanged (such fields are dropped by the omit-empty pass anyway).
/// Test: `provenance::tests::tag_suffixes_value`.
pub fn tag(value: impl Into<String>, provenance: Provenance) -> String {
    let mut s = value.into();
    s.push_str(provenance.tag());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the markers are asserted on by rendering tests, so they must be stable.
    /// What: pins each marker and the `NotStated` empty case.
    /// Test: this test itself.
    #[test]
    fn tags_are_stable() {
        assert_eq!(Provenance::Measured.tag(), " ⁽ᵐ⁾");
        assert_eq!(Provenance::Declared.tag(), " ⁽ᵈ⁾");
        assert_eq!(Provenance::Inferred.tag(), " ⁽ⁱ⁾");
        assert_eq!(Provenance::NotStated.tag(), "");
    }

    /// Why: the suffix helper is used for every measured/declared value.
    /// What: asserts the marker is appended and `NotStated` is a no-op.
    /// Test: this test itself.
    #[test]
    fn tag_suffixes_value() {
        assert_eq!(tag("8200", Provenance::Measured), "8200 ⁽ᵐ⁾");
        assert_eq!(tag("acme", Provenance::NotStated), "acme");
    }
}
