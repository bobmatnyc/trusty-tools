//! Tests for the shared non-canonical-tier ownership classifier (#4442).
//!
//! Why: this predicate is consumed by two surfaces (`tm doctor`'s `asset_tier`
//! probe and the #4448 quarantine), so every branch is pinned here once rather
//! than re-tested at each consumer. Two of these guard the module's stated
//! invariants directly: identity is the frontmatter `name`, and a user-owned
//! ledger entry survives a name collision.
//! What: pure `classify_tier_resident` / `agent_identity` branches plus
//! `audit_agent_tier` against staged temp directories — including the
//! load-bearing pair (a project-tier stub on a bundled name is REPORTED; a
//! project's own agent is NOT).
//! Test: this file.

use std::collections::BTreeSet;

use super::*;
use crate::agents::manifest::{AgentManifest, ManifestEntry, Origin, checksum};

/// Build a name set from string literals.
fn roster(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| (*s).to_owned()).collect()
}

/// A minimal agent document declaring `name`.
fn doc(name: &str) -> String {
    format!("---\nname: {name}\nrole: engineer\n---\n\nBody.\n")
}

/// Write a manifest into `dir` claiming `filename` with `origin`.
fn track(dir: &std::path::Path, filename: &str, origin: Origin) {
    let mut manifest = AgentManifest::default();
    manifest.managed.insert(
        filename.to_owned(),
        ManifestEntry {
            source_chain: vec![],
            checksum: checksum("whatever"),
            deployed_at: "2026-07-31T00:00:00Z".to_owned(),
            origin,
        },
    );
    manifest.save(dir).unwrap();
}

// ---------------------------------------------------------------------------
// agent_identity — invariant 1: identity is the frontmatter `name`.
// ---------------------------------------------------------------------------

#[test]
fn identity_prefers_the_frontmatter_name() {
    // The loader keys on `name:`, so a file named anything can BE rust-engineer.
    assert_eq!(
        agent_identity(&doc("rust-engineer"), "helper.md"),
        "rust-engineer"
    );
}

#[test]
fn identity_falls_back_to_the_stem() {
    // No frontmatter at all: the loader resolves such a file by its stem.
    assert_eq!(agent_identity("just prose\n", "helper.md"), "helper");
    assert_eq!(agent_identity("", "qa.md"), "qa");
}

#[test]
fn identity_ignores_a_blank_name() {
    // A declared-but-empty name is not an identity; fall back to the stem.
    assert_eq!(agent_identity("---\nname:   \n---\n", "qa.md"), "qa");
}

#[test]
fn identity_strips_only_one_md_suffix() {
    // `strip_suffix`, not `trim_end_matches`: a repeated suffix must survive.
    assert_eq!(agent_identity("", "weird.md.md"), "weird.md");
}

// ---------------------------------------------------------------------------
// classify_tier_resident
// ---------------------------------------------------------------------------

#[test]
fn classify_bundled_name_shadows() {
    // A name tm ships outranks the canonical tier wherever else it appears.
    let bundled = roster(&["rust-engineer", "qa"]);
    assert_eq!(
        classify_tier_resident("rust-engineer", TierOwnership::Untracked, &bundled),
        TierResidentClass::ShadowsBundled
    );
}

#[test]
fn classify_user_owned_survives_a_bundled_name_collision() {
    // INVARIANT 2. The ledger positively proves the operator owns this file.
    // A bundled-name collision must NOT override that — `retract_framework_agents`
    // preserves this tier, so quarantining it under #4448 would delete the
    // operator's own agent. Fails against a bool-flattened ownership input.
    let bundled = roster(&["qa"]);
    assert_eq!(
        classify_tier_resident("qa", TierOwnership::UserOwned, &bundled),
        TierResidentClass::Custom
    );
    assert!(!TierResidentClass::Custom.is_tm_owned());
}

#[test]
fn classify_manifest_framework_owned_is_stranded() {
    // Retired/renamed bundled agent: no longer in the roster, but the ledger
    // proves tm wrote it, so no deploy will ever refresh it again.
    let bundled = roster(&["qa"]);
    assert_eq!(
        classify_tier_resident("legacy-agent", TierOwnership::FrameworkOwned, &bundled),
        TierResidentClass::StrandedFrameworkOwned
    );
}

#[test]
fn classify_unknown_name_is_custom() {
    // The exclusion seam: a project's own agent is never tm's to touch.
    let bundled = roster(&["qa"]);
    assert_eq!(
        classify_tier_resident("acme-internal", TierOwnership::Untracked, &bundled),
        TierResidentClass::Custom
    );
}

#[test]
fn classify_prefers_shadowing_over_stranded() {
    // Shadowing is a property of the NAME, not of the ledger — a tm-tracked
    // file whose name is still bundled reports as shadowing, the stronger,
    // actionable verdict.
    let bundled = roster(&["qa"]);
    assert_eq!(
        classify_tier_resident("qa", TierOwnership::FrameworkOwned, &bundled),
        TierResidentClass::ShadowsBundled
    );
}

#[test]
fn tm_owned_is_true_for_exactly_the_two_owned_classes() {
    assert!(TierResidentClass::ShadowsBundled.is_tm_owned());
    assert!(TierResidentClass::StrandedFrameworkOwned.is_tm_owned());
    assert!(!TierResidentClass::Custom.is_tm_owned());
}

#[test]
fn ownership_of_reads_the_manifest_entry() {
    // The three states must come from one place, or a caller re-derives a bool.
    let mut manifest = AgentManifest::default();
    for (file, origin) in [
        ("bundled.md", Origin::Bundled),
        ("mine.md", Origin::User),
        ("pulled.md", Origin::Registry),
    ] {
        manifest.managed.insert(
            file.to_owned(),
            ManifestEntry {
                source_chain: vec![],
                checksum: checksum("x"),
                deployed_at: "2026-07-31T00:00:00Z".to_owned(),
                origin,
            },
        );
    }
    assert_eq!(
        ownership_of(&manifest, "bundled.md"),
        TierOwnership::FrameworkOwned
    );
    assert_eq!(ownership_of(&manifest, "mine.md"), TierOwnership::UserOwned);
    // Registry is conservatively user-owned — the operator pulled it.
    assert_eq!(
        ownership_of(&manifest, "pulled.md"),
        TierOwnership::UserOwned
    );
    assert_eq!(
        ownership_of(&manifest, "absent.md"),
        TierOwnership::Untracked
    );
}

// ---------------------------------------------------------------------------
// bundled_agent_names
// ---------------------------------------------------------------------------

#[test]
fn bundled_names_use_the_frontmatter_name() {
    // The bundle really does ship files whose stem and name differ
    // (`BASE-AGENT.md` declares `name: base-agent`), so the roster must be
    // keyed the way the loader keys it.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("BASE-AGENT.md"), doc("base-agent")).unwrap();
    std::fs::write(tmp.path().join("qa.md"), doc("qa")).unwrap();
    std::fs::write(tmp.path().join("nameless.md"), "prose only").unwrap();
    std::fs::write(tmp.path().join("notes.txt"), "x").unwrap();

    assert_eq!(
        bundled_agent_names(tmp.path()),
        roster(&["base-agent", "qa", "nameless"])
    );
}

#[test]
fn bundled_names_missing_dir_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(bundled_agent_names(&tmp.path().join("nope")).is_empty());
}

// ---------------------------------------------------------------------------
// audit_agent_tier
// ---------------------------------------------------------------------------

#[test]
fn audit_reports_a_shadowing_stub() {
    // #4408 reproduced: a 32-byte project-tier stub sitting on a bundled name.
    // The scan must find it — this is the case the whole probe exists for.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("rust-engineer.md"), doc("rust-engineer")).unwrap();

    let found = audit_agent_tier(tmp.path(), &roster(&["rust-engineer"]));
    assert_eq!(found.len(), 1, "found: {found:?}");
    assert_eq!(found[0].name, "rust-engineer");
    assert_eq!(found[0].class, TierResidentClass::ShadowsBundled);
    assert_eq!(found[0].path, tmp.path().join("rust-engineer.md"));
}

#[test]
fn audit_flags_a_renamed_file_that_declares_a_bundled_name() {
    // The MISS a stem-keyed predicate produces: the file is called `helper.md`
    // but the harness resolves it as `rust-engineer`, so it shadows.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("helper.md"), doc("rust-engineer")).unwrap();

    let found = audit_agent_tier(tmp.path(), &roster(&["rust-engineer"]));
    assert_eq!(found.len(), 1, "found: {found:?}");
    assert_eq!(found[0].name, "rust-engineer");
    assert_eq!(found[0].path, tmp.path().join("helper.md"));
}

#[test]
fn audit_ignores_a_bundled_filename_that_declares_a_custom_name() {
    // The FALSE POSITIVE a stem-keyed predicate produces: the filename collides
    // but the harness resolves this as `acme-custom`, which shadows nothing.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("rust-engineer.md"), doc("acme-custom")).unwrap();

    assert!(audit_agent_tier(tmp.path(), &roster(&["rust-engineer"])).is_empty());
}

#[test]
fn audit_ignores_a_custom_agent() {
    // The noise guard: a project agent tm never authored must produce NOTHING,
    // even sitting in the same directory as a real shadowing file.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("acme-internal.md"), doc("acme-internal")).unwrap();
    std::fs::write(tmp.path().join("qa.md"), doc("qa")).unwrap();

    let found = audit_agent_tier(tmp.path(), &roster(&["qa"]));
    assert_eq!(found.len(), 1, "found: {found:?}");
    assert_eq!(found[0].name, "qa");
}

#[test]
fn audit_ignores_a_user_owned_entry_on_a_bundled_name() {
    // INVARIANT 2, end to end: the operator's own `qa.md`, tracked `Origin::User`,
    // on a name tm also ships. Reporting it would hand #4448 a file to rename
    // out from under its author.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("qa.md"), doc("qa")).unwrap();
    track(tmp.path(), "qa.md", Origin::User);

    assert!(audit_agent_tier(tmp.path(), &roster(&["qa"])).is_empty());
}

#[test]
fn audit_reports_a_stranded_framework_entry() {
    // Tracked framework-owned but no longer bundled: reported, because no
    // deploy will refresh it and no operator knows it is there.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("legacy-agent.md"), doc("legacy-agent")).unwrap();
    track(tmp.path(), "legacy-agent.md", Origin::Bundled);

    let found = audit_agent_tier(tmp.path(), &roster(&["qa"]));
    assert_eq!(found.len(), 1, "found: {found:?}");
    assert_eq!(found[0].class, TierResidentClass::StrandedFrameworkOwned);
}

#[test]
fn audit_ignores_a_user_owned_manifest_entry() {
    // A tracked USER-origin file outside the roster stays invisible too.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("mine.md"), doc("mine")).unwrap();
    track(tmp.path(), "mine.md", Origin::User);

    assert!(audit_agent_tier(tmp.path(), &roster(&["qa"])).is_empty());
}

#[test]
fn audit_missing_dir_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(audit_agent_tier(&tmp.path().join("nope"), &roster(&["qa"])).is_empty());
}

#[test]
fn audit_tolerates_a_corrupt_manifest() {
    // A corrupt ledger must not silence the roster-based verdict — that would
    // be a presence-only check reporting green through a real shadowing.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("qa.md"), doc("qa")).unwrap();
    std::fs::write(
        tmp.path().join(crate::agents::manifest::MANIFEST_FILE),
        "{ not json",
    )
    .unwrap();

    let found = audit_agent_tier(tmp.path(), &roster(&["qa"]));
    assert_eq!(found.len(), 1, "found: {found:?}");
    assert_eq!(found[0].class, TierResidentClass::ShadowsBundled);
}

#[test]
fn audit_is_sorted_by_name() {
    // Stable output so doctor messages and quarantine logs are diffable.
    let tmp = tempfile::tempdir().unwrap();
    for name in ["qa", "engineer", "rust-engineer"] {
        std::fs::write(tmp.path().join(format!("{name}.md")), doc(name)).unwrap();
    }

    let found = audit_agent_tier(tmp.path(), &roster(&["qa", "engineer", "rust-engineer"]));
    let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["engineer", "qa", "rust-engineer"]);
}
