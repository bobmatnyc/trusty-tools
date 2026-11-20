//! [`LockedBuf`]: one secret value in page-aligned, `mlock`'d memory (#10524).
//!
//! Why: an unlock grant keeps Keychain and CLI-backed values in the server's
//! memory for up to 8 h. Those pages must not be written to swap, and the
//! bytes must be zeroed when the value is let go.
//! What: each buffer is its own page-aligned, page-sized allocation, so one
//! buffer's `munlock` never unlocks a page another buffer still needs (a
//! lock is per page, not counted). The page is locked before the value is
//! copied in; a refused lock frees the empty page and yields `None`, so the
//! caller does not cache the value. Drop zeroes every byte, unlocks and
//! frees. Off unix nothing is ever locked, so nothing is ever cached.
//! Test: `locked_buf_refused_lock_caches_nothing`,
//! `locked_buf_round_trips_and_zeroes_on_drop`.

use std::alloc::{self, Layout};
use std::fmt;
use std::ptr::NonNull;

use zeroize::Zeroize;

/// Locks `len` bytes at `addr` in RAM; `true` on success. The seam a test
/// uses to refuse the lock.
pub(crate) type MemLock = fn(*const u8, usize) -> bool;

/// The production [`MemLock`]: `mlock(2)`.
#[cfg(unix)]
pub(crate) fn os_mlock(addr: *const u8, len: usize) -> bool {
    // SAFETY: `addr..addr+len` is one live allocation this module owns.
    unsafe { libc::mlock(addr.cast(), len) == 0 }
}

/// Off unix there is no lock, so nothing is cached (fail closed).
#[cfg(not(unix))]
pub(crate) fn os_mlock(_addr: *const u8, _len: usize) -> bool {
    false
}

/// The page size, or `None` when the OS will not say.
fn page_size() -> Option<usize> {
    #[cfg(unix)]
    {
        // SAFETY: `sysconf` reads a constant; no memory is passed.
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        usize::try_from(size).ok().filter(|size| *size > 0)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// One value in locked memory; zeroed, unlocked and freed on drop.
///
/// Why: see the module docs.
/// What: `Debug` prints the length only.
/// Test: `locked_buf_round_trips_and_zeroes_on_drop`.
pub(crate) struct LockedBuf {
    ptr: NonNull<u8>,
    layout: Layout,
    len: usize,
}

// SAFETY: the buffer owns its allocation outright and has no interior
// mutability; `&LockedBuf` only reads.
unsafe impl Send for LockedBuf {}
// SAFETY: as above.
unsafe impl Sync for LockedBuf {}

impl fmt::Debug for LockedBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LockedBuf").field("len", &self.len).finish()
    }
}

impl LockedBuf {
    /// `value` copied into a fresh locked page run, or `None` when the
    /// allocation or `lock` fails; nothing is copied before the lock holds.
    pub(crate) fn new(value: &str, lock: MemLock) -> Option<Self> {
        let page = page_size()?;
        let size = value.len().max(1).div_ceil(page).checked_mul(page)?;
        let layout = Layout::from_size_align(size, page).ok()?;
        // SAFETY: `layout` has a non-zero size.
        let ptr = NonNull::new(unsafe { alloc::alloc_zeroed(layout) })?;
        if !lock(ptr.as_ptr(), size) {
            // #10524: a refused lock caches nothing; the page holds no value.
            // SAFETY: allocated just above with `layout`.
            unsafe { alloc::dealloc(ptr.as_ptr(), layout) };
            return None;
        }
        // SAFETY: `size >= value.len()` and the regions cannot overlap.
        unsafe { std::ptr::copy_nonoverlapping(value.as_ptr(), ptr.as_ptr(), value.len()) };
        Some(Self {
            ptr,
            layout,
            len: value.len(),
        })
    }

    /// The value.
    pub(crate) fn as_str(&self) -> &str {
        // SAFETY: the first `len` bytes were copied from a `&str` and are
        // never written again until drop.
        unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(self.ptr.as_ptr(), self.len))
        }
    }
}

impl Drop for LockedBuf {
    fn drop(&mut self) {
        let size = self.layout.size();
        // SAFETY: the whole allocation is owned by `self` and still live.
        let bytes = unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), size) };
        bytes.zeroize();
        #[cfg(unix)]
        // SAFETY: as above. A failed unlock (never locked, under a test
        // lock) changes nothing; the pages are freed next either way.
        unsafe {
            libc::munlock(self.ptr.as_ptr().cast(), size);
        }
        // SAFETY: allocated in `new` with `self.layout`.
        unsafe { alloc::dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refuse(_: *const u8, _: usize) -> bool {
        false
    }

    /// Why: #10524 AC2 — a value whose page cannot be locked is not cached.
    /// Test: itself.
    #[test]
    fn locked_buf_refused_lock_caches_nothing() {
        assert!(LockedBuf::new("v", refuse).is_none());
    }

    /// Why: #10524 — the buffer holds the value exactly, and `Debug` never
    /// shows it.
    /// Test: itself; zeroing on drop is pinned by
    /// `grant_cache_zeroizes_on_drop` in `cache_zeroize_tests.rs`.
    #[cfg(unix)]
    #[test]
    fn locked_buf_round_trips_and_zeroes_on_drop() {
        let buf = LockedBuf::new("s3cret-value", os_mlock).expect("mlock in a test");
        assert_eq!(buf.as_str(), "s3cret-value");
        assert!(!format!("{buf:?}").contains("s3cret"));
        assert!(LockedBuf::new("", os_mlock).is_some_and(|empty| empty.as_str().is_empty()));
    }
}
