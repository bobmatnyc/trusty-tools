//! Tests for the per-release highlights and the doctor notice (#11003).
//!
//! Why: the notice must show once per release, and a store failure must
//! neither fail doctor nor count the release as seen.
//! What: the embedded files' shape, version selection, rendering, and the
//! store and notice decision over hermetic temp dirs.
//! Test: this file.

use super::*;
use crate::test_support::hermetic_temp_dir;

fn highlights(version: &str, text: &'static str) -> Highlights {
    Highlights {
        version: Version::parse(version).expect("test version parses"),
        text,
    }
}

const SAMPLE: &str = "# What's new\n\n## One\na\nb\nTry: `tm x`\n\n## Two\nc\nd\ne\nTry: `tm y`\n";

/// Every row parses, and every file holds 1+ features of 3–5 lines each, the
/// last line naming a `tm` command to try (the issue's curated shape).
#[test]
fn every_release_row_parses_and_is_well_formed() {
    assert_eq!(
        embedded().len(),
        RELEASES.len(),
        "a release version failed to parse"
    );
    for h in embedded() {
        let feats = features(h.text);
        assert!(!feats.is_empty(), "{} has no `## ` feature", h.version);
        for f in feats {
            let n = f.lines.len();
            assert!(
                (3..=5).contains(&n),
                "{} / {}: {n} lines, want 3-5",
                h.version,
                f.title
            );
            let last = f.lines[n - 1];
            assert!(
                last.starts_with("Try: `tm ") && last.ends_with('`'),
                "{} / {}: last line must be `Try: `tm …``, got {last:?}",
                h.version,
                f.title
            );
        }
    }
}

/// A file under `src/assets/whats-new/` with no [`RELEASES`] row would never
/// ship; a row naming no file would not compile, so only this direction needs
/// a test.
#[test]
fn every_highlights_file_is_registered() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/whats-new");
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .expect("highlights dir is readable")
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".md")
                .map(str::to_owned)
        })
        .collect();
    on_disk.sort();
    let mut rows: Vec<String> = RELEASES.iter().map(|(v, _)| (*v).to_owned()).collect();
    rows.sort();
    assert_eq!(on_disk, rows);
}

/// The built crate version always has highlights to show, its own or older.
#[test]
fn the_built_version_has_highlights() {
    let v = Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version parses");
    assert!(for_version(&v).is_some());
}

#[test]
fn for_version_picks_the_newest_release_not_after_the_installed_one() {
    let first = embedded().into_iter().next().expect("one release");
    assert_eq!(for_version(&first.version), Some(first.clone()));
    let mut later = first.version.clone();
    later.patch += 1;
    assert_eq!(for_version(&later), Some(first.clone()));
    let earlier = Version::new(0, 0, 1);
    assert_eq!(for_version(&earlier), None);
}

#[test]
fn features_splits_on_level_two_headings() {
    let feats = features(SAMPLE);
    assert_eq!(feats.len(), 2);
    assert_eq!(feats[0].title, "One");
    assert_eq!(feats[0].lines, vec!["a", "b", "Try: `tm x`"]);
    assert_eq!(feats[1].lines.len(), 4);
}

#[test]
fn render_prints_the_exact_release() {
    let (v, text) = RELEASES[0];
    assert_eq!(render(v), text);
}

#[test]
fn render_names_the_older_release_it_falls_back_to() {
    let first = embedded().into_iter().next().expect("one release");
    let mut later = first.version.clone();
    later.patch += 1;
    let out = render(&later.to_string());
    assert!(out.starts_with(&format!(
        "No highlights were curated for {later}; showing {}.",
        first.version
    )));
    assert!(out.ends_with(first.text));
}

#[test]
fn render_says_so_when_no_release_applies() {
    assert_eq!(
        render("0.0.1"),
        "No curated highlights ship with trusty-mpm 0.0.1.\n"
    );
    assert_eq!(
        render("not-a-version"),
        "No curated highlights ship with trusty-mpm not-a-version.\n"
    );
}

#[test]
fn mark_seen_round_trips_through_last_seen() {
    let dir = hermetic_temp_dir();
    let store = SeenStore::in_root(dir.path());
    assert_eq!(store.last_seen().expect("absent is ok"), None);
    store.mark_seen(&Version::new(1, 2, 3)).expect("write");
    assert_eq!(store.last_seen().expect("read"), Some("1.2.3".to_owned()));
    let raw = std::fs::read_to_string(store.path()).expect("file");
    assert!(raw.contains("\"schema_version\": 1"), "{raw}");
}

#[test]
fn last_seen_refuses_another_schema_version() {
    let dir = hermetic_temp_dir();
    let store = SeenStore::in_root(dir.path());
    std::fs::write(
        store.path(),
        r#"{"schema_version":2,"last_seen_version":"9.9.9"}"#,
    )
    .expect("seed");
    assert!(matches!(
        store.last_seen(),
        Err(StoreError::Schema { found: 2, .. })
    ));
}

#[test]
fn doctor_notice_shows_once_then_stays_quiet() {
    let dir = hermetic_temp_dir();
    let h = highlights("1.2.3", SAMPLE);
    let first = doctor_notice(Ok(SeenStore::in_root(dir.path())), Some(&h));
    assert_eq!(
        first.line.as_deref(),
        Some("New in trusty-mpm 1.2.3: 2 highlights — run `tm whats-new`")
    );
    assert!(first.warning.is_none(), "{:?}", first.warning);
    let second = doctor_notice(Ok(SeenStore::in_root(dir.path())), Some(&h));
    assert!(second.line.is_none() && second.warning.is_none());
    let newer = highlights("1.3.0", SAMPLE);
    let third = doctor_notice(Ok(SeenStore::in_root(dir.path())), Some(&newer));
    assert!(third.line.is_some(), "a new release is announced again");
}

/// #11003: a store holding a newer version (a downgrade, or the other binary
/// sharing it) yields no line and is not written.
#[test]
fn doctor_notice_stays_quiet_for_an_older_build_and_never_writes_back() {
    let dir = hermetic_temp_dir();
    let store = SeenStore::in_root(dir.path());
    store.mark_seen(&Version::new(1, 3, 0)).expect("seed");
    let older = highlights("1.2.3", SAMPLE);
    let out = doctor_notice(Ok(store.clone()), Some(&older));
    assert!(out.line.is_none() && out.warning.is_none());
    assert_eq!(store.last_seen().expect("read"), Some("1.3.0".to_owned()));
}

/// #11003: two builds taking turns announce once in total.
#[test]
fn doctor_notice_shows_once_across_alternating_builds() {
    let dir = hermetic_temp_dir();
    let (old, new) = (highlights("1.2.3", SAMPLE), highlights("1.3.0", SAMPLE));
    let shown: Vec<bool> = [&new, &old, &new, &old]
        .into_iter()
        .map(|h| {
            doctor_notice(Ok(SeenStore::in_root(dir.path())), Some(h))
                .line
                .is_some()
        })
        .collect();
    assert_eq!(shown, [true, false, false, false]);
}

/// A failed write is reported and NOT recorded, so the notice shows again.
#[test]
fn doctor_notice_shows_again_when_the_write_fails() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = hermetic_temp_dir();
    // A read-only root: the store reads as absent, and the write is refused.
    let root = dir.path().join("root");
    std::fs::create_dir(&root).expect("root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let h = highlights("1.2.3", SAMPLE);
    let runs: Vec<NoticeOutcome> = (0..2)
        .map(|_| doctor_notice(Ok(SeenStore::in_root(&root)), Some(&h)))
        .collect();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("restore");
    for out in runs {
        assert!(out.line.is_some(), "the notice still shows");
        assert!(
            matches!(out.warning, Some(StoreError::Write { .. })),
            "{:?}",
            out.warning
        );
    }
    assert!(!root.join(STORE_FILE).exists(), "nothing was recorded");
}

#[test]
fn doctor_notice_does_not_overwrite_an_unreadable_store() {
    let dir = hermetic_temp_dir();
    let store = SeenStore::in_root(dir.path());
    std::fs::write(store.path(), b"not json").expect("seed");
    let h = highlights("1.2.3", SAMPLE);
    let out = doctor_notice(Ok(store.clone()), Some(&h));
    assert!(out.line.is_some());
    assert!(matches!(out.warning, Some(StoreError::Parse { .. })));
    assert_eq!(std::fs::read(store.path()).expect("file"), b"not json");
}

#[test]
fn doctor_notice_reports_an_unresolved_store() {
    let h = highlights("1.2.3", SAMPLE);
    let out = doctor_notice(
        Err(StoreError::Unresolved("broken install.toml".into())),
        Some(&h),
    );
    assert!(out.line.is_some());
    assert!(matches!(out.warning, Some(StoreError::Unresolved(_))));
    let none = doctor_notice(Err(StoreError::Unresolved("x".into())), None);
    assert!(none.line.is_none() && none.warning.is_none());
}
