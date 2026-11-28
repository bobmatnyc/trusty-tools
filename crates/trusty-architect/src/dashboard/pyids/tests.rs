//! The retire failure branches (#12029 P2d, the fail-open check).

use std::sync::Arc;

use super::*;
use crate::pyval::get;

fn ctx<'a>(dir: &Path, env: &'a dyn Fn(&str) -> Option<String>) -> Ctx<'a> {
    Ctx {
        cwd: dir.to_path_buf(),
        default_root: join(&dir.to_string_lossy(), "owner-queue"),
        env,
        schema_dir: dir.join("schemas"),
        briefs: brief_ids::Paths {
            repo: dir.to_path_buf(),
        },
        clock: Arc::new(FakeClock::new(1_795_784_400.0)),
    }
}

fn put(dir: &Path, id: &str, body: &str) {
    let items = dir.join("owner-queue/items");
    std::fs::create_dir_all(&items).expect("items dir");
    std::fs::write(items.join(format!("{id}.json")), body).expect("doc");
}

/// 2026-11-10T12:00:00Z.
const NOW: At = At {
    unix: 1_794_312_000.0,
};

#[test]
fn a_failed_archive_skips_the_item_and_keeps_it() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tmp");
    let env = |_: &str| None;
    let c = ctx(tmp.path(), &env);
    put(
        tmp.path(),
        "D01",
        r#"{"status": "done", "done_at": "2026-10-01T00:00:00Z", "version": 1}"#,
    );
    // The archive directory cannot be created: the parent is read-only.
    let lock = std::fs::Permissions::from_mode(0o555);
    std::fs::set_permissions(tmp.path(), lock).expect("chmod");
    let mut stderr = String::new();
    let got = retire_apply(&c, None, true, Some(NOW), "D", &mut stderr);
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755))
        .expect("chmod back");
    assert_eq!(got, Ok(vec![("D".to_owned(), 1, 0, 1, 1154)]));
    assert!(
        stderr.starts_with("dashboard_ids retire: items/D01 skipped: [Errno 13]"),
        "{stderr}"
    );
    assert!(
        tmp.path().join("owner-queue/items/D01.json").exists(),
        "an unarchived item is never deleted"
    );
}

#[test]
fn a_refused_delete_leaves_the_archived_item_in_place() {
    let tmp = tempfile::tempdir().expect("tmp");
    let env = |_: &str| None;
    let c = ctx(tmp.path(), &env);
    put(tmp.path(), "D01", r#"{"status": "done", "version": 3}"#);
    let store = c.store(None, None, c.clock.clone()).expect("store");
    let snap = snapshot(&store).expect("snapshot");
    let doc = &coll(&snap, "items")[0];
    let mut stale = row("items", doc, "x".into());
    if let PyValue::Dict(p) = &mut stale {
        p[2].1 = PyValue::Int(PyInt::Small(2));
    }
    let archive = join(&tmp.path().to_string_lossy(), "owner-queue-archive");
    let got = archive_then_delete(&store, &archive, &stale, "2026-11-10T12:00:00Z");
    assert!(matches!(got, Err(PyStoreError::Conflict(_))), "{got:?}");
    assert!(tmp.path().join("owner-queue/items/D01.json").exists());
    let line = std::fs::read_to_string(join(&archive, RETIRED_LOG)).expect("archive");
    assert_eq!(line.lines().count(), 1);
    assert_eq!(get(&stale, "id"), Some(&PyValue::Str("D01".into())));
}

#[test]
fn a_bad_counter_digit_writes_nothing() {
    let tmp = tempfile::tempdir().expect("tmp");
    let env = |_: &str| None;
    let c = ctx(tmp.path(), &env);
    let got = record_issued(&c, "D", "D0O", None);
    assert_eq!(
        got,
        Err(IdsError::Value("not two base-34 digits: '0O'".into()))
    );
    assert!(!Path::new(&counters_path(&c, None)).exists());
}
