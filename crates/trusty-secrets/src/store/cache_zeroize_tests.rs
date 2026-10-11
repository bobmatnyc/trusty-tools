//! A cached value's bytes are zeroed before its buffer is freed (#7520 S2).
//!
//! Why: owner ruling Dd8 — a value the session cache drops must not stay in
//! freed heap memory, where a later allocation or a core file can show it.
//! What: this unit-test binary's global allocator hands out zeroed buffers
//! and, when a small byte buffer is freed, counts it if it still holds
//! [`MARKER`]. Values the test itself holds are leaked with `mem::forget`,
//! so only the cache's own copies are ever freed. Only align-1 buffers of at
//! most 256 bytes are read: they hold bytes only (no padding), and the zeroed
//! allocation means none is uninitialized.
//! Test: `cache_zeroizes_every_value_it_drops`,
//! `cached_value_type_zeroizes_on_drop`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::mem::forget;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use super::*;

/// The text every test value starts with; no other test uses it.
const MARKER: &[u8] = b"zeroize-canary-7520-";

/// Freed buffers that still held [`MARKER`].
static FREED_WITH_MARKER: AtomicUsize = AtomicUsize::new(0);

/// [`System`], zeroing each allocation and counting marked frees.
struct MarkerAlloc;

// SAFETY: every call forwards to `System` with the same layout and pointer.
unsafe impl GlobalAlloc for MarkerAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's layout contract passes through unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if layout.align() == 1 && (MARKER.len()..=256).contains(&layout.size()) {
            // SAFETY: `ptr` is a live block of `layout.size()` bytes, zeroed
            // at allocation, and align 1 means it holds no padding.
            let bytes = unsafe { std::slice::from_raw_parts(ptr, layout.size()) };
            if bytes.windows(MARKER.len()).any(|w| w == MARKER) {
                FREED_WITH_MARKER.fetch_add(1, Ordering::SeqCst);
            }
        }
        // SAFETY: as above; `ptr` came from `alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: MarkerAlloc = MarkerAlloc;

/// A marked value, built in one exact-size allocation so no partly written
/// copy is freed on the way.
fn marked(tag: &str) -> Result<Option<SecretValue>, SecretsError> {
    let mut text = String::with_capacity(MARKER.len() + tag.len());
    text.push_str(std::str::from_utf8(MARKER).unwrap_or_default());
    text.push_str(tag);
    Ok(Some(SecretValue::new(text)))
}

/// An unmarked value.
fn plain() -> Result<Option<SecretValue>, SecretsError> {
    Ok(Some(SecretValue::new("plain")))
}

fn opened(account: &str) -> OpenedUnder {
    OpenedUnder {
        settings: Some(CliSettings {
            account: Some(account.to_owned()),
            ..CliSettings::default()
        }),
        file: None,
    }
}

/// Why: #7520 S2 (ruling Dd8) — a value leaves the cache by expiry, by
/// eviction, by a settings change, by replacement, and with the whole cache
/// at server exit; each path must zero its bytes.
/// Red while entries hold a plain `SecretValue`.
/// Test: itself.
#[test]
fn cache_zeroizes_every_value_it_drops() {
    let (instant, wall) = (Instant::now(), SystemTime::now());
    let secs = Arc::new(AtomicU64::new(0));
    let (a, b) = (Arc::clone(&secs), Arc::clone(&secs));
    let cache = SecretCache::with_clocks(
        Duration::from_secs(5),
        Arc::new(move || instant + Duration::from_secs(a.load(Ordering::SeqCst))),
        Arc::new(move || wall + Duration::from_secs(b.load(Ordering::SeqCst))),
    );
    let op = BackendId::onepassword();
    let web = VaultName::new("trusty/acme/web").unwrap();
    let key = |name: &str| SecretKey::new(name).unwrap();
    let (under, other) = (opened("a"), opened("b"));
    let freed = || FREED_WITH_MARKER.load(Ordering::SeqCst);

    forget(
        cache.read_through(&op, Some(&under), &web, &key("EXPIRES"), || {
            marked("expiry")
        }),
    );
    secs.store(5, Ordering::SeqCst);
    forget(cache.read_through(&op, Some(&under), &web, &key("OTHER"), plain));
    assert_eq!(freed(), 0, "an expired value was freed unzeroed");

    forget(cache.read_through(&op, Some(&under), &web, &key("EVICTED"), || marked("evict")));
    cache.evict(&web, &key("EVICTED"));
    assert_eq!(freed(), 0, "an evicted value was freed unzeroed");

    forget(cache.read_through(&op, Some(&under), &web, &key("SWITCH"), || marked("switch")));
    forget(cache.read_through(&op, Some(&other), &web, &key("OTHER"), plain));
    assert_eq!(
        freed(),
        0,
        "a value dropped on a settings change was freed unzeroed"
    );

    // A read that finds its slot filled by a nested read replaces that entry.
    forget(
        cache.read_through(&op, Some(&other), &web, &key("RACE"), || {
            forget(cache.read_through(&op, Some(&other), &web, &key("RACE"), || marked("inner")));
            marked("outer")
        }),
    );
    assert_eq!(freed(), 0, "a replaced value was freed unzeroed");

    assert!(cache.count() >= 1, "the cache holds a value to drop");
    drop(cache);
    assert_eq!(freed(), 0, "a value held at cache drop was freed unzeroed");
}

/// Why: #7520 S2 — the entry type itself zeroes on drop, so no new removal
/// path can skip it.
/// Test: itself.
#[test]
fn cached_value_type_zeroizes_on_drop() {
    fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}
    assert_zeroize_on_drop::<Held>();
}
