//! #10094: `palace migrate`'s engine half — plan, migrate, verify.
//!
//! Why: the command must reuse the daemon's chain (same backup, same resume),
//! write nothing on a dry run or a verify, and catch a migration that lost a
//! row or left a marker behind.
//! What: format-0 and format-1 fixtures under `<tmp>/palaces/<id>`, driven
//! through [`plan`], [`migrate_with`] and `verify`, with the chain's own fault
//! seam for the resume case.
//! Test: this file.

use super::super::format_migration::{Seams, Step};
use super::super::format_upgrade_verify::{FormatStatus, MarkerCheck, backup_root_of, verify};
use super::{UpgradeSeams, migrate, migrate_with, plan};
use crate::engine::store::format_v0_fixture::{
    FORMAT_0_DRAWERS, FORMAT_0_TRIPLES, write_format_0_palace,
};
use crate::engine::store::format_v1_fixture::write_format_1_palace;
use crate::engine::store::kg_store::{KG_SCHEMA, KgSchemaMarker, encode_value};
use crate::engine::store::palace_format::{KG_SCHEMA_PALACE_FORMAT, MIGRATION_RECORD};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A palace written by `write` under `<tmp>/palaces/<id>`.
pub(crate) fn build(
    id: &str,
    write: fn(&Path, &str) -> anyhow::Result<()>,
) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("palaces").join(id);
    write(&dir, id).expect("write fixture palace");
    (tmp, dir)
}

/// Every file under `root`: its bytes and modification time.
pub(crate) fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("read_dir") {
            let p = e.expect("entry").path();
            let meta = std::fs::metadata(&p).expect("stat");
            if meta.is_dir() {
                out.insert(p.clone(), (Vec::new(), meta.modified().expect("mtime")));
                stack.push(p);
            } else {
                let bytes = std::fs::read(&p).expect("read");
                out.insert(p, (bytes, meta.modified().expect("mtime")));
            }
        }
    }
    out
}

/// Write only the `kg_schema.palace_format` row, as a hand conversion might.
fn hand_stamp_format(dir: &Path, format: u32) {
    let db = trusty_common::redb_cache::create_palace_db(&dir.join("kg.redb")).expect("kg");
    let wtx = db.begin_write().expect("txn");
    {
        let mut t = wtx.open_table(KG_SCHEMA).expect("kg_schema");
        let bytes = encode_value(&KgSchemaMarker {
            schema_version: format,
        })
        .expect("encode");
        t.insert(KG_SCHEMA_PALACE_FORMAT, bytes.as_slice())
            .expect("stamp");
    }
    wtx.commit().expect("commit");
}

/// Set `palace.json`'s `format_version`, keeping every other field.
fn set_mirror(dir: &Path, format: u32) {
    let path = dir.join("palace.json");
    let mut v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
    v["format_version"] = serde_json::json!(format);
    std::fs::write(&path, serde_json::to_vec_pretty(&v).expect("encode")).expect("write");
}

/// Format-0 and format-1 palaces migrate through the chain behind one backup;
/// the report names it, the counts are unchanged and every check passes.
#[test]
fn palace_migrate_takes_a_backup_and_reports_it() {
    type Write = fn(&Path, &str) -> anyhow::Result<()>;
    for (from, write) in [
        (0, write_format_0_palace as Write),
        (1, write_format_1_palace),
    ] {
        let id = format!("cli-v{from}");
        let (_tmp, dir) = build(&id, write);
        let r = migrate(&dir, &id).expect("migrate");
        assert!(r.migrated, "{r:?}");
        assert_eq!(r.from, Some(from));
        assert!(r.problems.is_empty(), "{:?}", r.problems);
        let backup = r.backup.clone().expect("a backup");
        assert!(backup.starts_with(backup_root_of(&dir, &id)), "{backup:?}");
        let name = backup
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with(&format!("{from}-to-2-")), "{name}");
        assert!(backup.join("MANIFEST").is_file());
        assert_eq!(r.before, r.after);
        assert_eq!(r.after.drawers, FORMAT_0_DRAWERS.len() as u64);
        assert_eq!(r.after.triples, FORMAT_0_TRIPLES.len() as u64);
        assert_eq!(r.verify.status, FormatStatus::Current);
        assert_eq!(r.verify.palace_format, MarkerCheck::Present { value: 2 });
        assert_eq!(r.verify.triple_key, MarkerCheck::Present { value: 1 });
        assert_eq!(r.verify.mirror, MarkerCheck::Present { value: 2 });
        assert!(!dir.join(MIGRATION_RECORD).exists());
        assert_eq!(r.verify.backup.as_deref(), Some(backup.as_path()));
        assert_eq!(r.verify.backup_verified, Some(true));
    }
}

/// A palace already at the current format is a no-op success: no backup.
#[test]
fn a_current_palace_is_a_no_op_with_no_backup() {
    let (_tmp, dir) = build("noop", write_format_0_palace);
    let first = migrate(&dir, "noop").expect("first run");
    let root = backup_root_of(&dir, "noop");
    let backups = std::fs::read_dir(&root).expect("backups").count();
    let again = migrate(&dir, "noop").expect("second run");
    assert!(!again.migrated, "{again:?}");
    assert_eq!(again.backup, None);
    assert!(again.problems.is_empty(), "{:?}", again.problems);
    assert_eq!(again.after, first.after);
    assert_eq!(std::fs::read_dir(&root).expect("backups").count(), backups);
}

thread_local! {
    /// Fail the next `Step::Mirror` once.
    static FAIL_MIRROR: Cell<bool> = const { Cell::new(false) };
}

fn fail_mirror_once(step: Step) -> std::io::Result<()> {
    if step == Step::Mirror && FAIL_MIRROR.with(|f| f.replace(false)) {
        return Err(std::io::Error::other("injected crash after the mirror"));
    }
    Ok(())
}

/// A fault mid-chain leaves a resumable palace; the error names the backup
/// and the resume command, and the next run resumes behind that backup.
#[test]
fn an_injected_fault_names_the_backup_and_a_rerun_resumes() {
    let (_tmp, dir) = build("fault", write_format_0_palace);
    FAIL_MIRROR.with(|f| f.set(true));
    let seams = UpgradeSeams {
        chain: Seams {
            after: fail_mirror_once,
            ..Seams::default()
        },
        ..UpgradeSeams::default()
    };
    let err = migrate_with(&dir, "fault", seams).expect_err("the injected fault stops it");
    let shown = format!("{err:#}");
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join(MIGRATION_RECORD)).expect("record"))
            .expect("json");
    let backup = record["backup"].as_str().expect("backup path").to_string();
    assert!(shown.contains(&backup), "{shown}");
    assert!(
        shown.contains("re-run `trusty-memory palace migrate fault`"),
        "{shown}"
    );
    assert!(shown.contains("injected crash"), "{shown}");
    assert_eq!(
        plan(&dir, "fault").expect("plan").status,
        FormatStatus::Interrupted
    );

    let r = migrate(&dir, "fault").expect("the re-run resumes");
    assert!(r.migrated);
    assert_eq!(r.backup.as_deref(), Some(Path::new(&backup)));
    assert!(r.problems.is_empty(), "{:?}", r.problems);
    assert_eq!(r.after.drawers, FORMAT_0_DRAWERS.len() as u64);
}

/// A dry run reports the found and target format, the planned backup, the
/// size and the free space, and writes nothing — not even the backup root.
#[test]
fn dry_run_reports_the_plan_and_writes_nothing() {
    let (tmp, dir) = build("dry", write_format_0_palace);
    let before = snapshot(tmp.path());
    let p = plan(&dir, "dry").expect("plan");
    assert_eq!(snapshot(tmp.path()), before, "a dry run writes nothing");
    assert_eq!(p.status, FormatStatus::NeedsMigration);
    assert_eq!((p.found, p.target), (Some(0), 2));
    let root = backup_root_of(&dir, "dry");
    assert_eq!(p.backup_root, root);
    let planned = p.planned_backup.clone().expect("planned backup");
    assert!(
        planned.starts_with(&format!("{}/0-to-2-", root.display())),
        "{planned}"
    );
    assert!(
        p.estimated_bytes > 0 && p.needed_bytes > p.estimated_bytes,
        "{p:?}"
    );
    assert!(p.free_bytes.is_some_and(|f| f > 0), "{p:?}");
    assert!(p.would_change());
    assert!(!root.exists());
}

/// `--verify` on a palace converted by hand names each gap and writes nothing:
/// a format row with no triple-key row and no mirror; then a mirror that
/// disagrees with the marker.
#[test]
fn verify_reports_the_gaps_of_a_hand_shaped_palace() {
    let (tmp, dir) = build("hand", write_format_0_palace);
    hand_stamp_format(&dir, 2);
    let before = snapshot(tmp.path());
    let r = verify(&dir, "hand").expect("verify");
    assert_eq!(snapshot(tmp.path()), before, "verify writes nothing");
    assert_eq!(r.palace_format, MarkerCheck::Present { value: 2 });
    assert_eq!(r.triple_key, MarkerCheck::Missing);
    assert_eq!(r.mirror, MarkerCheck::Missing);
    assert!(!r.migration_record);
    assert_eq!(r.backup, None);
    let gaps = r.gaps.join("\n");
    assert!(
        gaps.contains("kg_schema triple_key marker is missing"),
        "{gaps}"
    );
    assert!(
        gaps.contains("palace.json format_version mirror is missing"),
        "{gaps}"
    );
    assert!(gaps.contains("disagree"), "{gaps}");

    let (_tmp1, dir1) = build("split", write_format_1_palace);
    set_mirror(&dir1, 2);
    let r = verify(&dir1, "split").expect("verify");
    assert_eq!(r.status, FormatStatus::Refused, "{r:?}");
    assert_eq!(
        r.palace_format,
        MarkerCheck::Mismatched { value: 1, want: 2 }
    );
    assert_eq!(r.mirror, MarkerCheck::Present { value: 2 });
    let gaps = r.gaps.join("\n");
    assert!(gaps.contains("FormatMarkerMismatch"), "{gaps}");
    assert!(
        gaps.contains("kg_schema palace_format marker is mismatched (1, want 2)"),
        "{gaps}"
    );
}

/// On the daemon route the daemon holds the palace's `kg.redb`: the plan reads
/// it as current, `--verify` reads its markers through a copy, and a migrate
/// is a no-op — nothing reads it as "unverified" or takes a backup.
#[test]
fn a_palace_this_process_holds_reads_as_current_through_a_copy() {
    let (_tmp, dir) = build("held", write_format_0_palace);
    let palace = crate::engine::store::PalaceStore::load_palace(&dir).expect("palace.json");
    let handle = crate::engine::retrieval::PalaceHandle::open_with_intent(
        &palace,
        crate::engine::store::OpenIntent::Writer,
    )
    .expect("the writer open migrates and holds kg.redb");
    assert_eq!(
        plan(&dir, "held").expect("plan").status,
        FormatStatus::Current
    );
    let r = verify(&dir, "held").expect("verify");
    assert!(r.read_from_copy, "{r:?}");
    assert!(r.gaps.is_empty(), "{:?}", r.gaps);
    assert_eq!(r.drawers, FORMAT_0_DRAWERS.len() as u64);
    let m = migrate(&dir, "held").expect("migrate");
    assert!(!m.migrated && m.problems.is_empty(), "{m:?}");
    drop(handle);
}
