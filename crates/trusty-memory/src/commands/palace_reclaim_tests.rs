//! Tests for `palace reclaim` (#9140): the dry run lists every class, leaves
//! recent or populated palaces alone, matches fixture turns by prompt, and
//! writes nothing under the palace root.

use super::*;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};
use trusty_common::memory_core::{Palace, PalaceId, PalaceRegistry};

const DAY: i64 = 86_400;

/// Create palace `name` under `root` holding `drawers`, then drop every handle.
fn seed_palace(root: &Path, name: &str, drawers: &[Drawer]) -> PathBuf {
    let registry = PalaceRegistry::with_max_open(4);
    let palace = Palace {
        id: PalaceId::new(name),
        name: name.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: root.join(name),
    };
    let handle = registry
        .create_palace(root, palace)
        .unwrap_or_else(|e| panic!("create_palace({name}): {e:#}"));
    for d in drawers {
        handle
            .kg
            .store()
            .upsert_drawer(d)
            .unwrap_or_else(|e| panic!("upsert_drawer: {e:#}"));
    }
    drop(handle);
    registry.remove(&PalaceId::new(name));
    root.join(name)
}

fn drawer(content: &str, tags: &[&str]) -> Drawer {
    let mut d = Drawer::new(uuid::Uuid::new_v4(), content);
    d.tags = tags.iter().map(|t| t.to_string()).collect();
    d
}

/// Backdate `palace.json` and `last_used` so the palace reads as idle.
fn backdate(dir: &Path, now: i64, days: i64) {
    let then = now - days * DAY;
    let when = SystemTime::UNIX_EPOCH + Duration::from_secs(then as u64);
    std::fs::File::options()
        .write(true)
        .open(dir.join("palace.json"))
        .and_then(|f| f.set_modified(when))
        .expect("backdate palace.json");
    crate::palace_last_used::write(dir, then as u64).expect("write last_used");
}

/// Every file under `root` with its length and mtime.
fn fingerprint(root: &Path) -> BTreeMap<PathBuf, (u64, SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).expect("read_dir").flatten() {
            let m = std::fs::symlink_metadata(e.path()).expect("meta");
            if m.is_dir() {
                stack.push(e.path());
            } else {
                out.insert(e.path(), (m.len(), m.modified().expect("mtime")));
            }
        }
    }
    out
}

fn count(report: &ReclaimReport, class: ReclaimClass) -> usize {
    report.items.iter().filter(|i| i.class == class).count()
}

/// Why (#9140 AC 1, 5): the dry run must list every class the issue names,
/// with the fixture turn picked out of a palace that also holds a real turn,
/// and must not change a byte under the palace root.
/// Test: itself.
#[test]
fn reclaim_scan_lists_every_class_and_writes_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let now = chrono::Utc::now().timestamp();

    let empty = seed_palace(root, "old-empty", &[]);
    std::fs::write(
        empty.join("index.usearch.redb.v2-incompatible"),
        b"0123456789",
    )
    .expect("write incompatible");
    backdate(&empty, now, 40);

    let fixture = drawer("User: say hi\n\nAssistant: hello", &["turn", "session:s1"]);
    let busy = seed_palace(
        root,
        "busy",
        &[
            fixture.clone(),
            drawer("User: say hi to Bob\n\nAssistant: ok", &["turn"]),
            drawer("User: say hi\n\nAssistant: untagged", &["note"]),
        ],
    );
    std::fs::write(busy.join("kg.redb.pre-compact.bak"), b"backup").expect("write bak");
    backdate(&busy, now, 40);

    std::fs::create_dir(root.join("t-tmpabc")).expect("orphan dir");
    std::fs::write(root.join("t-tmpabc/recall.redb"), b"orphan").expect("orphan file");
    std::fs::write(root.join("uds_addr"), "/nonexistent/m1-9140.sock").expect("uds_addr");
    std::fs::write(root.join("activity.redb.v2-incompatible"), b"old").expect("root file");

    let before = fingerprint(root);
    let report = scan(root, now).expect("scan");
    assert_eq!(fingerprint(root), before, "the dry run must write nothing");

    assert!(report.unreadable.is_empty(), "{:?}", report.unreadable);
    assert_eq!(count(&report, ReclaimClass::IncompatibleFile), 2);
    assert_eq!(count(&report, ReclaimClass::KgBackup), 1);
    assert_eq!(count(&report, ReclaimClass::EmptyPalace), 1);
    assert_eq!(count(&report, ReclaimClass::OrphanDir), 1);
    assert_eq!(count(&report, ReclaimClass::StaleUdsAddr), 1);
    assert_eq!(count(&report, ReclaimClass::FixtureDrawer), 1);
    let listed = report
        .items
        .iter()
        .find(|i| i.class == ReclaimClass::FixtureDrawer)
        .and_then(|i| i.drawer_id.clone());
    assert_eq!(listed, Some(fixture.id.to_string()));

    // The incompatible file inside the empty palace is counted once.
    let empty_bytes = tree_bytes(&empty);
    let expected = empty_bytes
        + tree_bytes(&busy.join("kg.redb.pre-compact.bak"))
        + tree_bytes(&root.join("t-tmpabc"))
        + tree_bytes(&root.join("uds_addr"))
        + 3;
    assert_eq!(report.unique_bytes(), expected);
    let text = report.render_text();
    assert!(text.contains("DRY RUN — nothing was deleted"), "{text}");
    assert!(
        text.contains("summary class=FixtureDrawer count=1"),
        "{text}"
    );
}

/// Why (#9140 AC 3): an empty palace used or created in the last 30 days, or
/// any palace with drawers, is never listed as empty.
/// Test: itself.
#[test]
fn reclaim_scan_keeps_recent_and_nonempty_palaces() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let now = chrono::Utc::now().timestamp();

    seed_palace(root, "fresh-empty", &[]);
    let used = seed_palace(root, "used-empty", &[]);
    backdate(&used, now, 40);
    crate::palace_last_used::write(&used, (now - 2 * DAY) as u64).expect("recent stamp");
    let full = seed_palace(root, "old-full", &[drawer("a real decision", &[])]);
    backdate(&full, now, 400);

    let report = scan(root, now).expect("scan");
    assert_eq!(
        count(&report, ReclaimClass::EmptyPalace),
        0,
        "{}",
        report.render_text()
    );
}

/// Why (#9140 AC 5): a real turn carries the `turn` tag too, so the prompt
/// decides; only an exact fixture prompt is listed.
/// Test: itself.
#[test]
fn fixture_turns_match_only_the_fixture_prompt_set() {
    for (want, content, tags) in [
        (true, "User: say hi\n\nAssistant: hi", &["turn"][..]),
        (
            true,
            "User: find where auth lives\n\nAssistant: src/auth",
            &["turn"][..],
        ),
        (
            true,
            "User: say hi again\n\nAssistant: hi",
            &["turn", "x"][..],
        ),
        (false, "User: say hi\n\nAssistant: hi", &["note"][..]),
        (false, "User: say hi please\n\nAssistant: hi", &["turn"][..]),
        (false, "say hi", &["turn"][..]),
        (false, "User: Say hi\n\nAssistant: hi", &["turn"][..]),
    ] {
        assert_eq!(
            is_fixture_turn(&drawer(content, tags)),
            want,
            "{content:?} tags={tags:?}"
        );
    }
}
