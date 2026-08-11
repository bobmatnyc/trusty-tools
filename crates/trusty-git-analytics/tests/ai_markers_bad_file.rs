//! A broken marker file degrades to the builtins, loudly (#5414).
//!
//! Why: the error arm is the half that decides whether configurability is safe
//! to ship. A collect run that aborts because an operator mistyped a regex is
//! worse than one that runs on the shipped set — and a run that silently
//! ignores the file is worse than both, because the resulting agentic share is
//! indistinguishable from a correctly configured one that found nothing.
//! What: points `TGA_AI_MARKERS` at a file whose second entry has an
//! unclosed group, then asserts detection still works, that the valid sibling
//! entry did NOT sneak in, and that `detection_disclosure` names the rejection.
//! Test: this file. See `ai_markers_operator_file.rs` for why it is one test
//! in its own binary.

use tga::collect::ai_attribution::AgenticMode;
use tga::collect::ai_markers::{detect, detection_disclosure, CommitSignals};

#[test]
fn a_rejected_marker_file_does_not_break_detection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ai-markers.yaml");
    std::fs::write(
        &path,
        "markers:\n\
         \x20 - { tool: acme-good, mode: full_agentic, scope: message, pattern: 'ACME Autopilot' }\n\
         \x20 - { tool: acme-broken, mode: full_agentic, scope: message, pattern: '([unclosed' }\n",
    )
    .expect("writes the marker file");

    std::env::set_var("TGA_AI_MARKERS", &path);

    // The shipped markers are unaffected.
    let d = detect(&CommitSignals::from_message(
        "feat: x\n\n🤖🤖🤖 Generated with trusty-mpm",
    ));
    assert_eq!(d.mode, AgenticMode::FullAgentic);
    assert_eq!(d.tool, Some("trusty-mpm"));

    // The file is rejected whole: the valid first entry is not applied either.
    assert_eq!(
        detect(&CommitSignals::from_message(
            "chore: ACME Autopilot ran here"
        ))
        .mode,
        AgenticMode::None,
        "a partially applied marker file is a half-configuration nobody asked for"
    );

    let disclosure = detection_disclosure();
    assert!(disclosure.contains("REJECTED"), "{disclosure}");
    assert!(disclosure.contains("acme-broken"), "{disclosure}");
    assert!(
        disclosure.contains(&path.display().to_string()),
        "the disclosure must name the file so the operator can fix it: {disclosure}"
    );
}
