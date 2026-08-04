//! `InstallPolicy` behavior tests (issue #3381).
//!
//! Why: split out of `tests_behavior_a.rs` to stay under the 500-SLOC
//! production-file cap — this basename ends in `_tests.rs` so it gets the
//! 1500-SLOC test-file cap instead.
//! What: pins the real two-way contract `install_to()`/`install_one()` now
//! honor: `InstallPolicy::Overwrite` (framework-owned) always refreshes;
//! `InstallPolicy::SeedOnce` (user-owned) writes once and is never clobbered
//! without `--force`.
//! Test: this file.

use crate::commands::install::{install_one, install_to};

#[test]
fn overwrite_artifact_refreshes_modified_file_without_force() {
    // A framework-owned (`InstallPolicy::Overwrite`) artifact must be
    // refreshed on a PLAIN `tm install` — no `--force` needed — since that is
    // what lets an upgrade actually deliver new framework assets.
    // `optimizer.toml` is a real `ALL` entry and is `Overwrite`.
    let dir = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(dir.path());
    let optimizer = paths.optimizer_config();
    std::fs::create_dir_all(&paths.hooks).unwrap();
    std::fs::write(&optimizer, "custom").unwrap();

    let report = install_to(&paths, false).unwrap();
    assert!(
        report
            .iter()
            .any(|l| l.contains("optimizer.toml") && l.contains("refreshed")),
        "expected a refreshed line for optimizer.toml, got: {report:?}"
    );
    assert_ne!(
        std::fs::read_to_string(&optimizer).unwrap(),
        "custom",
        "Overwrite artifact must replace a modified file even without --force"
    );
}

#[test]
fn seed_once_artifact_is_not_clobbered_without_force() {
    // A user-owned (`InstallPolicy::SeedOnce`) artifact must NOT be touched
    // by a plain `tm install` once it exists on disk. No shipped artifact
    // currently uses `SeedOnce`, so this exercises the policy via a
    // directly-constructed fixture rather than a fake bundled entry.
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("user-owned.md");
    std::fs::write(&dest, "the user's own edits").unwrap();

    let artifact = trusty_mpm::core::bundle::BundledArtifact {
        rel_path: "user-owned.md",
        contents: "shipped default content",
        install: trusty_mpm::core::bundle::InstallPolicy::SeedOnce,
    };

    let line = install_one(&dest, &artifact, false).unwrap();
    assert!(line.contains("preserved"), "unexpected report line: {line}");
    assert_eq!(
        std::fs::read_to_string(&dest).unwrap(),
        "the user's own edits"
    );
}

#[test]
fn seed_once_artifact_force_resets_to_shipped_default() {
    // `--force` is the escape hatch that lets a user reset a seed-once
    // artifact back to the shipped default.
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("user-owned.md");
    std::fs::write(&dest, "the user's own edits").unwrap();

    let artifact = trusty_mpm::core::bundle::BundledArtifact {
        rel_path: "user-owned.md",
        contents: "shipped default content",
        install: trusty_mpm::core::bundle::InstallPolicy::SeedOnce,
    };

    let line = install_one(&dest, &artifact, true).unwrap();
    assert!(
        line.contains("reset to shipped default"),
        "unexpected report line: {line}"
    );
    assert_eq!(
        std::fs::read_to_string(&dest).unwrap(),
        "shipped default content"
    );
}

#[test]
fn bundle_install_pass_never_touches_the_compiled_prompt() {
    // Why (#4752): the defect was two writers on one path — a bundled stub and
    // the compiled prompt. This pins that the bundle-artifact pass (`install_to`,
    // which walks `bundle::ALL`) cannot write the compiled prompt's file, so the
    // only thing that ever puts bytes there is an explicit compiled write.
    //
    // FIXTURE NOTE: the compiled path is pre-seeded with a sentinel. Asserting
    // the sentinel SURVIVES `install_to` is what exercises the guard — an
    // absence check would pass trivially on a fresh temp dir even if a bundled
    // artifact did target that name.
    let dir = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(dir.path());
    let compiled = paths.instructions_compiled();
    std::fs::create_dir_all(compiled.parent().unwrap()).unwrap();
    const SENTINEL: &str = "SENTINEL-NOT-WRITTEN-BY-THE-BUNDLE-PASS";
    std::fs::write(&compiled, SENTINEL).unwrap();

    install_to(&paths, true).unwrap();

    assert_eq!(
        std::fs::read_to_string(&compiled).unwrap(),
        SENTINEL,
        "no bundled artifact may write {}",
        compiled.display()
    );

    // And the explicit compiled write — the ONE writer — replaces it with the
    // full assembled prompt, never a stub (regression guard for #383).
    trusty_mpm::core::instruction_pipeline::install_system_prompt_to(&compiled).unwrap();
    assert_eq!(
        std::fs::read_to_string(&compiled).unwrap(),
        trusty_mpm::core::instruction_pipeline::assemble_system_prompt()
    );
}
