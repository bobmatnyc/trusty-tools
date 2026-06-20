//! # Spec References
//!
//! - [`SPEC-HARNESS-UNDERSTANDING-01~draft`](../../docs/specs/harness-understanding.md)
//!
//! Canonical harness-understanding instructions shared between trusty-mpm SM and
//! future t-code overseer (DOC-21).
//!
//! Why: The harness mental model, per-harness signals, and intervention decision
//!      protocol must live in one place so both the SM prompt (trusty-mpm) and a
//!      future t-code overseer can consume the same authoritative content. This
//!      module exposes it as static `&str` slices compiled in at build time.
//! What: Four structured accessors — `agnostic()`, `mpm_session_manager()`,
//!       `tcode()`, `overseer()` — plus a `harness_understanding()` convenience
//!       that concatenates all four sections for consumers that want the full doc.
//! Test: `harness_doc::tests` verifies non-empty content, canonical marker
//!       presence (`✻`, `__HARNESS_EVENT__`), and the structured-accessor sum
//!       equals the full-doc output.

/// Harness-agnostic mental model: session lifecycle, pane/IO model, prompt
/// shapes, completion/error signals, observability, and the WHEN-TO-INTERVENE
/// decision protocol with deterministic LLM-free fallback.
///
/// Why: Every consumer (SM, overseer, future tooling) needs the same foundation
///      — session states, idle/working/error patterns, and when to act.
/// What: Returns the full text of `HARNESS_AGNOSTIC.md`, compiled in at build
///       time via `include_str!`.
/// Test: `agnostic_non_empty`, `agnostic_contains_claude_code_glyph`,
///       `agnostic_contains_harness_event`.
pub fn agnostic() -> &'static str {
    include_str!("assets/harness_understanding/HARNESS_AGNOSTIC.md")
}

/// trusty-mpm session-manager specifics: how OBSERVE/VERIFY consume the
/// harness model, managed states, adopt semantics, chat-core verb mapping,
/// RawObservation/Summary two-tier, and the override convention.
///
/// Why: The SM prompt needs SM-specific wiring on top of the agnostic model —
///      the two-tier observation model, the override path, and chat-core verbs.
/// What: Returns the full text of `HARNESS_MPM_SM.md`.
/// Test: `mpm_sm_non_empty`, `mpm_sm_contains_raw_observation`.
pub fn mpm_session_manager() -> &'static str {
    include_str!("assets/harness_understanding/HARNESS_MPM_SM.md")
}

/// tcode-specific signals: task banners, `__HARNESS_EVENT__` NDJSON lines,
/// agent-delegation patterns, and diff/edit confirmation output.
///
/// Why: tcode emits structured NDJSON events that differ from Claude Code's
///      pane-text signals; both consumers need the tcode specifics.
/// What: Returns the full text of `HARNESS_TCODE.md`.
/// Test: `tcode_non_empty`, `tcode_contains_harness_event`.
pub fn tcode() -> &'static str {
    include_str!("assets/harness_understanding/HARNESS_TCODE.md")
}

/// Forward-looking t-code-as-overseer contract: the `Overseer` trait +
/// `HarnessSource::Code` filter seam, event-to-decision mapping, and
/// `__HARNESS_EVENT__` as structured overseer input.
///
/// Why: The overseer seam is already defined (`Overseer` trait,
///      `HarnessSource::Code`); this section codifies the behavioral contract
///      so the first t-code overseer implementer has a spec to build against.
/// What: Returns the full text of `HARNESS_OVERSEER.md`.
/// Test: `overseer_non_empty`, `overseer_contains_flag_for_human`.
pub fn overseer() -> &'static str {
    include_str!("assets/harness_understanding/HARNESS_OVERSEER.md")
}

/// Full harness-understanding document: all four sections concatenated in
/// order (agnostic → mpm_session_manager → tcode → overseer), separated by
/// `\n\n---\n\n`.
///
/// Why: SM prompt assembly and tests need the full body; structured accessors
///      satisfy consumers that only need one section.
/// What: Concatenates `agnostic()`, `mpm_session_manager()`, `tcode()`, and
///       `overseer()` with a `\n\n---\n\n` separator.
/// Test: `full_doc_contains_all_markers`, `full_doc_sum_of_parts`.
pub fn harness_understanding() -> String {
    [agnostic(), mpm_session_manager(), tcode(), overseer()]
        .iter()
        .map(|s| s.trim())
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agnostic_non_empty() {
        assert!(!agnostic().trim().is_empty());
    }

    #[test]
    fn agnostic_contains_claude_code_glyph() {
        // The ✻ glyph is the canonical Claude Code working signal (DOC-21 §5.1)
        assert!(
            agnostic().contains('✻'),
            "agnostic section must document the ✻ glyph"
        );
    }

    #[test]
    fn agnostic_contains_harness_event() {
        // __HARNESS_EVENT__ is the canonical tcode NDJSON event prefix (DOC-21 §5.2)
        assert!(
            agnostic().contains("__HARNESS_EVENT__"),
            "agnostic section must document __HARNESS_EVENT__ prefix"
        );
    }

    #[test]
    fn mpm_sm_non_empty() {
        assert!(!mpm_session_manager().trim().is_empty());
    }

    #[test]
    fn mpm_sm_contains_raw_observation() {
        assert!(
            mpm_session_manager().contains("RawObservation")
                || mpm_session_manager().contains("raw observation")
                || mpm_session_manager().contains("raw pane"),
            "SM section must reference the RawObservation/raw-pane two-tier model"
        );
    }

    #[test]
    fn tcode_non_empty() {
        assert!(!tcode().trim().is_empty());
    }

    #[test]
    fn tcode_contains_harness_event() {
        assert!(
            tcode().contains("__HARNESS_EVENT__"),
            "tcode section must document __HARNESS_EVENT__ NDJSON prefix"
        );
    }

    #[test]
    fn overseer_non_empty() {
        assert!(!overseer().trim().is_empty());
    }

    #[test]
    fn overseer_contains_flag_for_human() {
        assert!(
            overseer().contains("FlagForHuman") || overseer().contains("flag_for_human"),
            "overseer section must reference FlagForHuman escalation"
        );
    }

    #[test]
    fn full_doc_contains_all_markers() {
        let doc = harness_understanding();
        assert!(doc.contains('✻'), "full doc must contain ✻ glyph");
        assert!(
            doc.contains("__HARNESS_EVENT__"),
            "full doc must contain __HARNESS_EVENT__"
        );
        assert!(
            doc.contains("FlagForHuman") || doc.contains("flag_for_human"),
            "full doc must contain FlagForHuman"
        );
    }

    #[test]
    fn full_doc_sum_of_parts() {
        // The full doc is a join of the four parts; every part's content must
        // appear somewhere in the full doc.
        let doc = harness_understanding();
        // Take first 50 chars of each trimmed part as a unique anchor
        let parts = [agnostic(), mpm_session_manager(), tcode(), overseer()];
        for part in &parts {
            let anchor: String = part.trim().chars().take(50).collect();
            assert!(
                doc.contains(anchor.trim()),
                "full doc missing content from part starting: {anchor:?}"
            );
        }
    }
}
