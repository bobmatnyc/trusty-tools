//! Tests for the #10571 refresh of unedited seeds and the bundle provenance.

use super::super::Step;
use super::super::seed::refresh;
use super::{Fixture, seed, seeded_files};

/// Mode bits of `path`.
fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// The shipped row for `dest`.
fn shipped(dest: &str) -> &'static str {
    seeded_files()
        .into_iter()
        .find(|f| f.dest == dest)
        .unwrap_or_else(|| panic!("{dest} is not seeded"))
        .contents
}

/// An earlier shipped copy (known to the injected history) is replaced by the
/// current one, and a script keeps mode 0755.
#[test]
fn a_refresh_updates_an_unedited_earlier_copy() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    let dest = "scripts/dashboard-serve.py";
    let old = b"print('an earlier shipped dashboard-serve')\n";
    std::fs::write(dir.join(dest), old).unwrap();
    let old_hash = refresh::sha256_hex(old);
    let history = |d: &str, b: &[u8]| d == dest && refresh::sha256_hex(b) == old_hash;
    let steps = super::super::seed::deploy_with(&dir, seed(), &history).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join(dest)).unwrap(),
        shipped(dest)
    );
    assert!(
        steps.contains(&Step::Changed(format!(
            "updated {dest} (an unedited earlier shipped copy)"
        ))),
        "{steps:?}"
    );
    assert_eq!(mode(&dir.join(dest)), 0o755);
    let again = super::super::seed::deploy_with(&dir, seed(), &history).unwrap();
    assert!(again.contains(&Step::Unchanged(dest.to_owned())));
}

/// Bytes the history does not list are an edit: kept, and reported as kept.
#[test]
fn a_refresh_keeps_an_edited_copy_and_reports_it() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    let dest = "scripts/datg.py";
    let old = b"print('an earlier shipped datg')\n";
    let edited = b"print('the owner edited datg')\n";
    std::fs::write(dir.join(dest), edited).unwrap();
    let old_hash = refresh::sha256_hex(old);
    let history = |d: &str, b: &[u8]| d == dest && refresh::sha256_hex(b) == old_hash;
    let steps = super::super::seed::deploy_with(&dir, seed(), &history).unwrap();
    assert_eq!(std::fs::read(dir.join(dest)).unwrap(), edited);
    assert!(
        steps.iter().any(|s| matches!(s, Step::Skipped(t)
            if t.starts_with(dest) && t.contains("left as is") && t.contains("custom copy kept"))),
        "{steps:?}"
    );
}

/// The real history knows every current shipped copy, so the next release can
/// refresh it, and it knows the copies shipped before this change.
#[test]
fn every_embedded_seed_is_in_the_shipped_history() {
    let rows: Vec<_> = refresh::history().collect();
    let mut checked = 0;
    for file in seeded_files() {
        if file.dest == "CLAUDE.md" || file.dest.starts_with(".claude/skills/") {
            continue; // content-sourced; the launch deploy owns the skills' refresh
        }
        let hash = refresh::sha256_hex(file.contents.as_bytes());
        assert!(
            rows.contains(&(file.dest.as_str(), hash.as_str())),
            "{}: current bytes missing from seed_history.tsv; run \
             scripts/architect_bundle_meta.py history",
            file.dest
        );
        checked += 1;
    }
    assert!(checked >= 100, "checked only {checked} rows");
    // A pre-#10571 install holds an earlier datg.py; the refresh must know it.
    let datg = rows.iter().filter(|(d, _)| *d == "scripts/datg.py").count();
    assert!(datg >= 2, "only {datg} datg.py version(s) in the history");
}

/// `init` and `status` both name the prototype commit and the content manifest.
#[test]
fn init_and_status_report_the_bundle_provenance() {
    let fx = Fixture::new();
    let report = fx.init().unwrap();
    assert!(
        report.steps.iter().any(|s| matches!(s, Step::Unchanged(t)
            if t.starts_with("bundle: prototype 23da8a9, export manifest sha256 ")
                && t.ends_with(" files"))),
        "{}",
        report.render()
    );
    let status = fx.status();
    assert!(status.bundle.ok, "{}", status.bundle.detail);
    assert!(status.bundle.detail.contains("23da8a9"));
    assert!(status.render().contains("bundle"));
    std::fs::remove_file(fx.dir().join(refresh::PROVENANCE_DEST)).unwrap();
    let missing = fx.status().bundle;
    assert!(!missing.ok && missing.detail.contains("tm fleet init"));
}

/// The stamp's per-file sha256 matches the sub-project content it describes.
#[test]
fn the_bundle_provenance_matches_the_shipped_content() {
    let root = trusty_common::test_harness::test_repo_root().expect("resolve the checkout");
    let sub = root.join("python/trusty-architect");
    let text = shipped(refresh::PROVENANCE_DEST);
    let v: serde_json::Value = serde_json::from_str(text).unwrap();
    let files = v["files"].as_object().expect("files map");
    assert!(files.len() > 100);
    for (rel, hash) in files {
        let bytes = std::fs::read(sub.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert_eq!(
            refresh::sha256_hex(&bytes),
            hash.as_str().unwrap(),
            "{rel}: stale; run scripts/architect_bundle_meta.py provenance"
        );
    }
}
