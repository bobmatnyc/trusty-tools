//! Named interleaving points in the error store's disk path (#8028).
//!
//! Why: the compaction race, the hot-path lock and the read/rotate overlap are
//! all interleavings between two actors. A test that waits on a sleep only
//! catches them by chance. What: [`fire`] marks a point on the calling thread.
//! Under `cfg(test)` a test installs a per-thread hook with [`install`] and runs
//! the competing actor's step at exactly that point; in every other build
//! [`fire`] compiles to nothing.
//! Test: `store_lock_tests::append_releases_the_store_lock_before_touching_disk`,
//! `compaction_tests::compaction_keeps_records_appended_during_it`,
//! `compaction_tests::a_read_overlapping_a_rotation_counts_each_record_once`.

/// A point in the disk path where a test may run a competing actor's step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Point {
    /// In `ErrorStore::append`, just before the rotation and disk write.
    BeforeDiskWrite,
    /// In a legacy compaction, after the tail of the source was read.
    CompactionTailRead,
    /// In a legacy compaction, after the compacted copy was fully written.
    CompactionCopied,
    /// In the reader, after one store file was read and before the next.
    BetweenStoreFiles,
}

#[cfg(test)]
type Hook = Box<dyn FnMut(Point)>;

#[cfg(test)]
thread_local! {
    static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}

/// Run the calling thread's hook, if any, at `point`.
#[inline]
pub(crate) fn fire(point: Point) {
    #[cfg(test)]
    {
        // Taken out while it runs, so a hook that re-enters the store cannot
        // hit a double borrow; it is put back afterwards.
        let hook = HOOK.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook(point);
            HOOK.with(|h| {
                let mut slot = h.borrow_mut();
                if slot.is_none() {
                    *slot = Some(hook);
                }
            });
        }
    }
    #[cfg(not(test))]
    let _ = point;
}

/// Install `hook` for the calling thread; dropping the guard removes it.
#[cfg(test)]
pub(crate) fn install(hook: impl FnMut(Point) + 'static) -> HookGuard {
    HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    HookGuard
}

/// Removes the calling thread's hook on drop, including on a test panic.
#[cfg(test)]
pub(crate) struct HookGuard;

#[cfg(test)]
impl Drop for HookGuard {
    fn drop(&mut self) {
        HOOK.with(|h| *h.borrow_mut() = None);
    }
}
