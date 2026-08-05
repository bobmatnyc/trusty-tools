//! Tests for `source_scan` — the shared "what is a skill here?" enumeration.
//!
//! Why: #4949 was a scan that recognised exactly one layout and reported
//! nothing about what it declined. Every case below pins one half of that:
//! the layouts that must be recognised, and the names that must come back as
//! rejects instead of vanishing.
//! What: covers the flat, directory, and hybrid layouts, hidden-entry
//! filtering, recursive extras, and the malformed-directory reject.
//! Test: this file IS the test module for `source_scan`; run with
//! `cargo test -p trusty-agents-common -- skills::source_scan`.

use super::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn scan_finds_flat_skill() {
    // The pre-#4949 layout must keep working unchanged.
    let src = TempDir::new().unwrap();
    fs::write(src.path().join("tm-doctor.md"), "# Doctor\n").unwrap();

    let scan = scan_skill_sources(src.path()).unwrap();

    assert_eq!(scan.skills.len(), 1);
    assert_eq!(scan.skills[0].stem, "tm-doctor");
    assert_eq!(scan.skills[0].entry, src.path().join("tm-doctor.md"));
    assert!(scan.skills[0].extras.is_empty());
    assert!(scan.rejected.is_empty());
}

#[test]
fn scan_finds_directory_shaped_skill() {
    // #4949: `<stem>/SKILL.md` is a skill. Before the fix the top-level
    // `is_file()` test rejected the directory and the scan returned nothing.
    let src = TempDir::new().unwrap();
    let skill = src.path().join("duetto-design-system");
    fs::create_dir_all(skill.join("references")).unwrap();
    fs::write(skill.join("SKILL.md"), "# Duetto\n").unwrap();
    fs::write(skill.join("metadata.json"), "{}\n").unwrap();
    fs::write(skill.join("references").join("tokens.md"), "# Tokens\n").unwrap();

    let scan = scan_skill_sources(src.path()).unwrap();

    assert_eq!(scan.skills.len(), 1, "got {:?}", scan);
    let found = &scan.skills[0];
    assert_eq!(found.stem, "duetto-design-system");
    assert_eq!(found.entry, skill.join("SKILL.md"));
    // Every carried file travels — not just `references/*.md`. `SKILL.md`
    // itself is the entry point, never an extra.
    let rels: Vec<&str> = found.extras.iter().map(|e| e.rel.as_str()).collect();
    assert_eq!(rels, vec!["metadata.json", "references/tokens.md"]);
    assert!(scan.rejected.is_empty());
}

#[test]
fn scan_carries_non_markdown_extras() {
    // `cto-kb-ingest` ships `scripts/ingest.sh`. A `.md`-only extras filter
    // would fix the `is_file()` half of #4949 and leave a second silent drop.
    let src = TempDir::new().unwrap();
    let skill = src.path().join("cto-kb-ingest");
    fs::create_dir_all(skill.join("scripts")).unwrap();
    fs::write(skill.join("SKILL.md"), "# Ingest\n").unwrap();
    fs::write(skill.join("scripts").join("ingest.sh"), "#!/bin/sh\n").unwrap();

    let scan = scan_skill_sources(src.path()).unwrap();

    let rels: Vec<&str> = scan.skills[0]
        .extras
        .iter()
        .map(|e| e.rel.as_str())
        .collect();
    assert_eq!(rels, vec!["scripts/ingest.sh"]);
}

#[test]
fn scan_companion_dir_is_not_a_reject() {
    // The bundled hybrid layout: a flat `tm-capabilities.md` beside a
    // `tm-capabilities/references/` companion. The directory carries no
    // SKILL.md, but it is not malformed — warning about it would fire on
    // every bundled skill on every deploy.
    let src = TempDir::new().unwrap();
    fs::write(src.path().join("tm-capabilities.md"), "# Caps\n").unwrap();
    fs::create_dir_all(src.path().join("tm-capabilities").join("references")).unwrap();
    fs::write(
        src.path()
            .join("tm-capabilities")
            .join("references")
            .join("cli.md"),
        "# CLI\n",
    )
    .unwrap();

    let scan = scan_skill_sources(src.path()).unwrap();

    assert_eq!(scan.skills.len(), 1);
    assert_eq!(scan.skills[0].stem, "tm-capabilities");
    assert_eq!(scan.skills[0].entry, src.path().join("tm-capabilities.md"));
    let rels: Vec<&str> = scan.skills[0]
        .extras
        .iter()
        .map(|e| e.rel.as_str())
        .collect();
    assert_eq!(rels, vec!["references/cli.md"]);
    assert!(
        scan.rejected.is_empty(),
        "companion dir must not be rejected: {:?}",
        scan.rejected
    );
}

#[test]
fn scan_rejects_dir_without_skill_md() {
    // A directory that looks like a skill but has no entry point cannot be
    // deployed. It must come back named, not disappear.
    let src = TempDir::new().unwrap();
    fs::create_dir_all(src.path().join("broken-skill")).unwrap();
    fs::write(src.path().join("broken-skill").join("notes.md"), "x\n").unwrap();

    let scan = scan_skill_sources(src.path()).unwrap();

    assert!(scan.skills.is_empty());
    assert_eq!(scan.rejected, vec!["broken-skill".to_string()]);
}

#[test]
fn scan_ignores_hidden_entries() {
    // `.git` and friends are neither skills nor malformed skills.
    let src = TempDir::new().unwrap();
    fs::create_dir_all(src.path().join(".git")).unwrap();
    fs::write(src.path().join(".hidden.md"), "x\n").unwrap();
    let skill = src.path().join("real");
    fs::create_dir_all(skill.join(".cache")).unwrap();
    fs::write(skill.join("SKILL.md"), "# Real\n").unwrap();
    fs::write(skill.join(".DS_Store"), "junk").unwrap();
    fs::write(skill.join(".cache").join("x.md"), "junk").unwrap();

    let scan = scan_skill_sources(src.path()).unwrap();

    assert_eq!(scan.skills.len(), 1);
    assert_eq!(scan.skills[0].stem, "real");
    assert!(
        scan.skills[0].extras.is_empty(),
        "{:?}",
        scan.skills[0].extras
    );
    assert!(scan.rejected.is_empty());
}

#[test]
fn scan_extras_are_recursive_and_sorted() {
    // Deterministic ordering keeps deploy output and manifest writes stable.
    let src = TempDir::new().unwrap();
    let skill = src.path().join("deep");
    fs::create_dir_all(skill.join("a").join("b")).unwrap();
    fs::write(skill.join("SKILL.md"), "# Deep\n").unwrap();
    fs::write(skill.join("z.txt"), "z\n").unwrap();
    fs::write(skill.join("a").join("m.md"), "m\n").unwrap();
    fs::write(skill.join("a").join("b").join("n.md"), "n\n").unwrap();

    let scan = scan_skill_sources(src.path()).unwrap();

    let rels: Vec<&str> = scan.skills[0]
        .extras
        .iter()
        .map(|e| e.rel.as_str())
        .collect();
    assert_eq!(rels, vec!["a/b/n.md", "a/m.md", "z.txt"]);
}

#[test]
fn scan_missing_source_is_empty() {
    let src = TempDir::new().unwrap();
    let scan = scan_skill_sources(&src.path().join("nope")).unwrap();
    assert_eq!(scan, SourceScan::default());
}
