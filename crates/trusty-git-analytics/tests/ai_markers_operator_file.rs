//! A marker file added at runtime changes what `detect` catches (#5414).
//!
//! Why: the unit tests in `collect::ai_markers` exercise the marker-set
//! builder against an explicit path. That leaves the last link unproven — that
//! the SHIPPED entry point, `detect`, with no argument naming a file, actually
//! reads one. This test asserts exactly that, and it is the regression proof
//! for #5414: it uses only public API that existed in tga 2.15.0, so it
//! compiles against the pre-#5414 tree and fails there, where a house footer
//! nobody hardcoded scores `AgenticMode::None`.
//! What: writes a marker file, points `TGA_AI_MARKERS` at it, then classifies
//! a commit carrying a footer that appears nowhere in `BUILTIN`.
//! Test: this file.
//!
//! ONE test per binary, deliberately: the marker set is a process-global
//! `OnceLock` and `TGA_AI_MARKERS` is process-global, so a sibling test in the
//! same binary would race for both. The rejected-file half lives in
//! `ai_markers_bad_file.rs` for the same reason.

use tga::collect::ai_markers::{detect, detection_disclosure, CommitSignals};

#[test]
fn a_house_footer_added_at_runtime_is_detected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ai-markers.yaml");
    std::fs::write(
        &path,
        "markers:\n\
         \x20 - tool: acme-autopilot\n\
         \x20   mode: full_agentic\n\
         \x20   scope: message\n\
         \x20   pattern: '(?i)Generated\\s+with\\s+ACME\\s+Autopilot'\n\
         \x20 - tool: acme-ide\n\
         \x20   mode: ide_assisted\n\
         \x20   scope: email\n\
         \x20   pattern: '(?i)^autopilot@acme\\.example$'\n",
    )
    .expect("writes the marker file");

    // #6405: this file is a one-test binary on purpose — the process has no
    // other thread reading the environment, so this set_var races nothing.
    // The lazily-cached marker load is what needs its own process.
    std::env::set_var("TGA_AI_MARKERS", &path);

    // Nothing in BUILTIN knows this footer; before #5414 nothing could.
    let footer = "feat: ship the widget\n\n--\nGenerated with ACME Autopilot 2.1";
    let d = detect(&CommitSignals::from_message(footer));
    assert_eq!(
        d.tool,
        Some("acme-autopilot"),
        "an operator marker must be able to name a tool the crate has never heard of"
    );
    assert_eq!(
        d.mode,
        tga::collect::ai_attribution::AgenticMode::FullAgentic
    );

    // The email scope is configurable too, not only message footers.
    let by_email = CommitSignals {
        message: "fix: tighten the bound",
        author_email: "autopilot@acme.example",
        committer_email: "human@acme.example",
    };
    let d = detect(&by_email);
    assert_eq!(d.tool, Some("acme-ide"));
    assert_eq!(
        d.mode,
        tga::collect::ai_attribution::AgenticMode::IdeAssisted
    );

    // A commit with no marker is still unmarked — the file adds, it does not
    // widen everything.
    assert_eq!(
        detect(&CommitSignals::from_message("chore: bump deps")).mode,
        tga::collect::ai_attribution::AgenticMode::None
    );

    // The run's disclosure has to say the set was extended, and with how much:
    // an acquirer reading a share cannot otherwise tell this run apart from a
    // stock one.
    let disclosure = detection_disclosure();
    assert!(
        disclosure.contains("2 operator marker(s) loaded from"),
        "{disclosure}"
    );
    assert!(disclosure.contains("acme-autopilot"), "{disclosure}");
    assert!(disclosure.contains("no markers emitted"), "{disclosure}");
}
