//! #9918: the chained palace-format migration, 0 → 1 → 2, at a writer open.
//!
//! Why: an upgrade that skipped a format bump left every format-0 palace
//! refused `FormatTooOld`. A writer open now runs each step from the stored
//! format, in ascending order, behind one verified backup.
//! What: `Seams::after` logs each step with the mirror on disk after it, or
//! stops the chain between its two steps; fixtures from format_migration_tests.rs.
//! Test: this file.

use super::tests::{
    Fixture, Writer, assert_migrated, build_v0, build_with, complete_backups, has_triple_key,
    hash_tree, legacy_triple_key, marker, mirror, open, schema_row, write_format_0_palace_with,
};
use super::v2_tests::store_error;
use super::{Seams, Step, open_gate_with};
use crate::engine::store::concurrent_open::OpenIntent;
use crate::engine::store::format_v0_fixture::FORMAT_0_DRAWERS;
use crate::engine::store::format_v1_fixture::write_format_1_palace_with;
use crate::engine::store::kg_redb::migrate::STEPS;
use crate::engine::store::kg_store::KG_SCHEMA_TRIPLE_KEY;
use crate::engine::store::palace_format::{MIGRATION_RECORD, PALACE_FORMAT_SUPPORTED};
use crate::engine::store::palace_store::PalaceStoreError;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

/// One seam call: the step just finished and the mirror on disk after it.
type Logged = (Step, Option<u64>);

thread_local! {
    static DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static LOG: RefCell<Vec<Logged>> = const { RefCell::new(Vec::new()) };
    /// Fail at the first `Step::Kg`: after the 0 → 1 step committed.
    static FAIL_FIRST_KG: Cell<bool> = const { Cell::new(false) };
}

fn log_step(step: Step) -> std::io::Result<()> {
    let m = DIR.with(|d| d.borrow().as_deref().and_then(mirror));
    LOG.with(|l| l.borrow_mut().push((step, m)));
    if step == Step::Kg && FAIL_FIRST_KG.with(|f| f.replace(false)) {
        return Err(std::io::Error::other("injected crash between chain steps"));
    }
    Ok(())
}

/// Run the writer gate of `fx` with the logging seam; the result and the log.
fn logged_writer_gate(fx: &Fixture, fail_first_kg: bool) -> (anyhow::Result<()>, Vec<Logged>) {
    DIR.with(|d| *d.borrow_mut() = Some(fx.dir().to_path_buf()));
    LOG.with(|l| l.borrow_mut().clear());
    FAIL_FIRST_KG.with(|f| f.set(fail_first_kg));
    let seams = Seams {
        after: log_step,
        ..Seams::default()
    };
    let r = open_gate_with(fx.dir(), fx.palace.id.as_str(), OpenIntent::Writer, seams);
    FAIL_FIRST_KG.with(|f| f.set(false));
    (r, LOG.with(RefCell::take))
}

/// AC1: a format-0 palace migrates 0 → 1 → 2 at its first writer open, in
/// ascending order, behind one backup of the pre-migration bytes; drawers and
/// triples are unchanged and no legacy triple key is left.
#[test]
fn a_format_0_palace_migrates_to_2_at_first_writer_open() {
    let v0 = build_v0("v0-chain");
    let before = hash_tree(v0.dir());
    let (r, log) = logged_writer_gate(&v0, false);
    r.expect("the writer gate migrates a format-0 palace");
    let want = [
        (Step::Backup, None),
        (Step::Record, None),
        (Step::Mirror, Some(1)),
        (Step::Kg, Some(1)),
        (Step::Mirror, Some(2)),
        (Step::Kg, Some(2)),
    ];
    assert_eq!(log, want, "one backup, then each step in ascending order");

    let backups = complete_backups(&v0);
    assert_eq!(backups.len(), 1, "one backup: {backups:?}");
    let name = backups[0].file_name().expect("name").to_string_lossy();
    assert!(name.starts_with("0-to-2-"), "{name}");
    let kg = Path::new("kg.redb");
    assert_eq!(
        hash_tree(&backups[0]).get(kg),
        before.get(kg),
        "pre-migration"
    );

    let handle = open(&v0, OpenIntent::Writer).expect("the migrated palace opens");
    assert_eq!(handle.drawers.read().len(), FORMAT_0_DRAWERS.len());
    assert!(!handle.drawer_load_degraded);
    drop(handle);
    assert_migrated(&v0);
    assert!(!has_triple_key(
        &v0.kg(),
        &legacy_triple_key("alice", "knows")
    ));
}

/// AC5: every format below the supported one has a fixture and a `kg.redb`
/// step, and each migrates to the supported format at a writer open. A format
/// bump without a fixture or a step fails the length checks.
#[test]
fn every_historical_format_migrates_to_current() {
    const FIXTURES: [Writer; 2] = [write_format_0_palace_with, write_format_1_palace_with];
    assert_eq!(FIXTURES.len(), PALACE_FORMAT_SUPPORTED as usize);
    assert_eq!(STEPS.len(), FIXTURES.len(), "one kg.redb step per fixture");
    for (format, write) in FIXTURES.into_iter().enumerate() {
        let fx = build_with(&format!("history-{format}"), write, |_| {});
        assert_eq!(marker(&fx.kg()).unwrap_or(0) as usize, format, "fixture");
        drop(open(&fx, OpenIntent::Writer).unwrap_or_else(|e| panic!("format {format}: {e:#}")));
        assert_migrated(&fx);
    }
}

/// AC2: a crash between the 0 → 1 and 1 → 2 steps leaves a format-1 palace
/// (marker, mirror and triple key at 1, legacy keys rewritten) with the record
/// and the one backup on disk. A read-only open refuses it; the next writer
/// open runs only the 1 → 2 step, behind the same backup.
#[test]
fn a_fault_between_chain_steps_leaves_a_resumable_palace() {
    let v0 = build_v0("v0-chain-fault");
    let (r, _) = logged_writer_gate(&v0, true);
    let err = r.expect_err("the injected crash stops the chain");
    let failed = matches!(
        store_error(&err),
        Some(PalaceStoreError::FormatMigrationFailed { .. })
    );
    assert!(failed, "{err:#}");
    assert_eq!(marker(&v0.kg()), Some(1), "the 0 -> 1 step committed");
    assert_eq!(mirror(v0.dir()), Some(1));
    assert_eq!(schema_row(&v0.kg(), KG_SCHEMA_TRIPLE_KEY), Some(1));
    assert!(!has_triple_key(
        &v0.kg(),
        &legacy_triple_key("alice", "knows")
    ));
    assert!(v0.dir().join(MIGRATION_RECORD).exists(), "the record stays");
    let backups = complete_backups(&v0);
    assert_eq!(backups.len(), 1, "{backups:?}");

    let ro = open(&v0, OpenIntent::ReadOnlyClient)
        .err()
        .expect("refused");
    let behind = matches!(
        store_error(&ro),
        Some(PalaceStoreError::FormatNeedsMigration { .. })
    );
    assert!(behind, "{ro:#}");

    let (r, log) = logged_writer_gate(&v0, false);
    r.expect("the next writer open resumes the chain");
    let want = [(Step::Mirror, Some(2)), (Step::Kg, Some(2))];
    assert_eq!(log, want, "only the 1 -> 2 step runs");
    assert_migrated(&v0);
    assert_eq!(complete_backups(&v0), backups, "the backup is reused");
}
