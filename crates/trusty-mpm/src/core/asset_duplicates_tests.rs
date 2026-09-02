//! Tests for [`super`] — same-stem duplicate detection inside one tier (#6649).

use super::*;
use tempfile::TempDir;

/// Write an empty agent-shaped file.
fn touch(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), "x").unwrap();
}

#[test]
fn a_file_beside_a_directory_of_the_same_stem_is_a_duplicate() {
    let tmp = TempDir::new().unwrap();
    touch(tmp.path(), "qa.md");
    std::fs::create_dir(tmp.path().join("qa")).unwrap();

    let found = scan_duplicate_stems(tmp.path()).unwrap();
    assert_eq!(found.len(), 1, "one colliding stem: {found:?}");
    assert_eq!(found[0].stem, "qa");
    assert_eq!(found[0].paths.len(), 2, "both entries named: {found:?}");
}

#[test]
fn case_variant_stems_are_a_duplicate() {
    let tmp = TempDir::new().unwrap();
    touch(tmp.path(), "qa.md");
    // A case-insensitive filesystem collapses these into one file, so the
    // second write is a no-op there and the assertion below adapts.
    std::fs::write(tmp.path().join("QA.md"), "y").unwrap();

    let found = scan_duplicate_stems(tmp.path()).unwrap();
    let entries = std::fs::read_dir(tmp.path()).unwrap().count();
    if entries == 1 {
        // Case-insensitive volume: there is genuinely one file, so there is
        // genuinely no duplicate to report.
        assert!(found.is_empty(), "one file cannot collide: {found:?}");
        return;
    }
    assert_eq!(found.len(), 1, "case variants collide: {found:?}");
    assert_eq!(found[0].stem, "qa");
}

#[test]
fn a_clean_tier_has_no_duplicates() {
    let tmp = TempDir::new().unwrap();
    touch(tmp.path(), "qa.md");
    touch(tmp.path(), "engineer.md");
    std::fs::create_dir(tmp.path().join("tm-workflow")).unwrap();

    assert!(scan_duplicate_stems(tmp.path()).unwrap().is_empty());
}

#[test]
fn dot_entries_never_form_a_group() {
    let tmp = TempDir::new().unwrap();
    touch(tmp.path(), ".trusty-mpm-manifest.json");
    touch(tmp.path(), ".trusty-mpm-manifest.json.tmp");
    // Two dot-files whose stems would otherwise be compared; bookkeeping, not
    // assets.
    assert!(scan_duplicate_stems(tmp.path()).unwrap().is_empty());
}

#[test]
fn a_disabled_quarantine_sibling_is_not_a_duplicate() {
    // #4448 leaves `<name>.md.disabled` where it moved `<name>.md` from. That
    // sibling must never read as a second claim on the name.
    let tmp = TempDir::new().unwrap();
    touch(tmp.path(), "qa.md");
    touch(tmp.path(), "qa.md.disabled");

    assert!(
        scan_duplicate_stems(tmp.path()).unwrap().is_empty(),
        "an inert `.md.disabled` sibling is not a second claim on the name"
    );
}

#[test]
fn an_absent_tier_scans_clean() {
    let tmp = TempDir::new().unwrap();
    let absent = tmp.path().join("never-provisioned");
    assert!(scan_duplicate_stems(&absent).unwrap().is_empty());
}

#[test]
#[cfg(unix)]
fn an_unreadable_tier_is_an_error_not_an_empty_scan() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("locked");
    std::fs::create_dir(&dir).unwrap();
    touch(&dir, "qa.md");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();

    // Running as root defeats the permission bit entirely, so establish
    // whether the mode took before asserting on it.
    let mode_took = std::fs::read_dir(&dir).is_err();
    let scanned = scan_duplicate_stems(&dir);
    // Restore before asserting so the temp dir can be cleaned up either way.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    if !mode_took {
        return;
    }
    assert!(
        scanned.is_err(),
        "an unreadable tier must report the failure, never an empty scan"
    );
}

#[test]
fn named_stems_summarise_the_remainder() {
    let found: Vec<DuplicateStem> = ["a", "b", "c"]
        .iter()
        .map(|s| DuplicateStem {
            stem: (*s).to_string(),
            paths: vec![PathBuf::from(s), PathBuf::from(format!("{s}.md"))],
        })
        .collect();

    assert_eq!(name_duplicates(&found, 5), "a, b, c");
    assert_eq!(name_duplicates(&found, 2), "a, b (+1 more)");
    assert_eq!(name_duplicates(&[], 5), "");
}
