//! #9274 PR3: the migration's step order, through its fault and space seams.
//!
//! Why: ADR-0067 D3 rules 3 and 4 — the backup's free-space check precedes the
//! copy, the mirror is written before the marker, and a crash between any two
//! steps leaves a palace the next writer open completes.
//! What: `Seams::after` stops the migration after a chosen step exactly as a
//! crash would; the v0 fixture comes from format_migration_tests.rs.
//! Test: this file.

use super::tests::{assert_migrated, build_v0, complete_backups, hash_tree, marker, mirror};
use super::{Seams, Step, open_gate_with};
use crate::memory_core::retrieval::PalaceHandle;
use crate::memory_core::store::concurrent_open::OpenIntent;
use crate::memory_core::store::format_backup::{self, backup_data_root};
use crate::memory_core::store::palace_format::MIGRATION_RECORD;
use crate::memory_core::store::palace_store::PalaceStoreError;
use std::cell::Cell;
use std::path::Path;

thread_local! {
    /// The step after which [`stop_at`] fails.
    static STOP: Cell<Option<Step>> = const { Cell::new(None) };
}

fn stop_at(step: Step) -> std::io::Result<()> {
    if STOP.with(Cell::get) == Some(step) {
        Err(std::io::Error::other("injected crash"))
    } else {
        Ok(())
    }
}

/// AC3/AC4: a crash after each step leaves the palace at a state whose
/// markers say where it stopped — the mirror moves before the marker — and
/// the next ordinary writer open completes it behind the same backup.
#[test]
fn a_fault_after_each_step_leaves_a_palace_the_next_writer_open_completes() {
    for step in [Step::Backup, Step::Record, Step::Mirror, Step::Kg] {
        let v0 = build_v0(&format!("v0-fault-{step:?}"));
        STOP.with(|s| s.set(Some(step)));
        let seams = Seams {
            after: stop_at,
            ..Seams::default()
        };
        let result = open_gate_with(v0.dir(), v0.palace.id.as_str(), OpenIntent::Writer, seams);
        STOP.with(|s| s.set(None));
        let err = result.expect_err("the injected crash stops the migration");
        assert!(
            matches!(
                err.downcast_ref::<PalaceStoreError>(),
                Some(PalaceStoreError::FormatMigrationFailed { .. })
            ),
            "{step:?}: {err:#}"
        );

        let record = v0.dir().join(MIGRATION_RECORD).exists();
        let (want_record, want_mirror, want_marker) = match step {
            Step::Backup => (false, None, None),
            Step::Record => (true, None, None),
            Step::Mirror => (true, Some(1), None),
            Step::Kg => (true, Some(1), Some(1)),
        };
        assert_eq!(record, want_record, "{step:?}: record");
        assert_eq!(mirror(v0.dir()), want_mirror, "{step:?}: mirror");
        assert_eq!(marker(&v0.kg()), want_marker, "{step:?}: marker");
        assert_eq!(complete_backups(&v0).len(), 1, "{step:?}: backup first");

        drop(
            PalaceHandle::open_with_intent(&v0.palace, OpenIntent::Writer)
                .unwrap_or_else(|e| panic!("{step:?}: the next open completes: {e:#}")),
        );
        assert_migrated(&v0);
        assert_eq!(complete_backups(&v0).len(), 1, "{step:?}: backup reused");
    }
}

/// AC2: the free-space check runs before the backup copies a byte; a short
/// filesystem leaves no backup directory, no record and no palace change.
#[test]
fn insufficient_space_aborts_the_migration_before_the_backup() {
    let v0 = build_v0("v0-no-space");
    let before = hash_tree(v0.dir());
    let seams = Seams {
        backup: format_backup::Seams {
            free_space: |_| Ok(Some(0)),
            after_copy: |_| {},
        },
        ..Seams::default()
    };
    let err = open_gate_with(v0.dir(), v0.palace.id.as_str(), OpenIntent::Writer, seams)
        .expect_err("no space refuses the migration");
    assert!(
        matches!(
            err.downcast_ref::<PalaceStoreError>(),
            Some(PalaceStoreError::InsufficientSpace { .. })
        ),
        "{err:#}"
    );
    let left: Vec<_> = std::fs::read_dir(v0.backups())
        .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "the backup started: {left:?}");
    assert!(!v0.dir().join(MIGRATION_RECORD).exists());
    assert_eq!(before, hash_tree(v0.dir()));
}

/// Backups sit beside `palaces/` (ADR-0067 D3 rule 3); in the flat layout
/// they go under a hidden directory `palace reclaim` skips.
#[test]
fn backups_land_beside_the_palaces_directory() {
    let root = Path::new("/data");
    assert_eq!(backup_data_root(&root.join("palaces").join("p")), root);
    assert_eq!(
        backup_data_root(&root.join("p")),
        root.join(".trusty-memory")
    );
}
