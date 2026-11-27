//! The keys live-corpus writers touch while a staged run copies the live
//! corpus into staging (#12064).
//!
//! Why: the carryover copy reads one snapshot of the live corpus (#839). A
//! write that commits after the snapshot and before the swap is missing from
//! staging, and a delete in that span is still in it; the promotion dropped
//! the one and resurrected the other (#10738 window).
//! What: a staged run arms the journal under the indexer read guard, before
//! the copy begins. Every live-corpus writer holds a [`JournalEntry`] for its
//! call; its drop records the files, prefixes or chunk ids it named, so a
//! write that finishes after the arm is recorded whatever its outcome. The
//! swap takes the keys under the indexer write guard, when no writer is in
//! flight, and re-copies those rows from live into staging.
//! Test: `a_write_and_a_delete_in_the_carryover_window_survive_the_promotion`.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use super::CodeIndexer;
use crate::core::path_selection::PathSelection;

/// Keys written to the live corpus since the journal was armed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct JournalKeys {
    pub(crate) files: BTreeSet<String>,
    pub(crate) prefixes: BTreeSet<String>,
    pub(crate) chunk_ids: BTreeSet<String>,
}

impl JournalKeys {
    /// True when no writer finished since the arm.
    pub(crate) fn is_empty(&self) -> bool {
        self.files.is_empty() && self.prefixes.is_empty() && self.chunk_ids.is_empty()
    }

    /// The file and prefix keys as the selections `rows_under` reads.
    pub(crate) fn selections(&self) -> Vec<PathSelection> {
        let mut sels = Vec::with_capacity(2);
        if !self.files.is_empty() {
            sels.push(PathSelection::Files(self.files.clone()));
        }
        if !self.prefixes.is_empty() {
            sels.push(PathSelection::Prefix(self.prefixes.clone()));
        }
        sels
    }

    fn extend(&mut self, other: JournalKeys) {
        self.files.extend(other.files);
        self.prefixes.extend(other.prefixes);
        self.chunk_ids.extend(other.chunk_ids);
    }
}

/// `None` while no staged run copies the corpus; the keys while one does.
#[derive(Debug, Default)]
pub(crate) struct CarryoverJournal(Mutex<Option<JournalKeys>>);

impl CarryoverJournal {
    fn slot(&self) -> std::sync::MutexGuard<'_, Option<JournalKeys>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// An armed journal; dropping it disarms, so a run that returns early
/// leaves nothing collecting.
pub(crate) struct ArmedJournal(Arc<CarryoverJournal>);

impl ArmedJournal {
    /// The keys recorded since the arm; disarms.
    pub(crate) fn take(self) -> JournalKeys {
        self.0.slot().take().unwrap_or_default()
    }
}

impl Drop for ArmedJournal {
    fn drop(&mut self) {
        *self.0.slot() = None;
    }
}

/// One writer's keys, recorded into an armed journal when it drops.
pub(crate) struct JournalEntry<'a> {
    journal: &'a CarryoverJournal,
    keys: JournalKeys,
}

impl Drop for JournalEntry<'_> {
    fn drop(&mut self) {
        if let Some(set) = self.journal.slot().as_mut() {
            set.extend(std::mem::take(&mut self.keys));
        }
    }
}

impl CodeIndexer {
    /// Start collecting live-corpus writes for a staged run's carryover copy.
    pub(crate) fn arm_carryover_journal(&self) -> ArmedJournal {
        *self.carryover_journal.slot() = Some(JournalKeys::default());
        ArmedJournal(Arc::clone(&self.carryover_journal))
    }

    /// Hold for a write to `files`; recorded on drop.
    pub(crate) fn journal_files<I: IntoIterator<Item = String>>(
        &self,
        files: I,
    ) -> JournalEntry<'_> {
        self.journal_entry(JournalKeys {
            files: files.into_iter().collect(),
            ..JournalKeys::default()
        })
    }

    /// Hold for a removal of `sel`; recorded on drop.
    pub(crate) fn journal_selection(&self, sel: &PathSelection) -> JournalEntry<'_> {
        let keys: BTreeSet<String> = sel.scan_starts().map(str::to_string).collect();
        self.journal_entry(match sel {
            PathSelection::Files(_) => JournalKeys {
                files: keys,
                ..JournalKeys::default()
            },
            PathSelection::Prefix(_) => JournalKeys {
                prefixes: keys,
                ..JournalKeys::default()
            },
        })
    }

    /// Hold for a removal of chunk `ids`; recorded on drop.
    pub(crate) fn journal_chunk_ids(&self, ids: &[String]) -> JournalEntry<'_> {
        self.journal_entry(JournalKeys {
            chunk_ids: ids.iter().cloned().collect(),
            ..JournalKeys::default()
        })
    }

    fn journal_entry(&self, keys: JournalKeys) -> JournalEntry<'_> {
        JournalEntry {
            journal: &self.carryover_journal,
            keys,
        }
    }
}
