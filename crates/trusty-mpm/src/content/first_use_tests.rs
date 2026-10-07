//! Tests for `content::first_use` (#9396): tm installs the content release on
//! first use, once, never over a present lock, and fails closed offline.
//!
//! A child of `bundle_cache_tests` so it reuses that file's in-memory
//! [`FakeSource`]; nothing here touches the network.

use std::sync::atomic::{AtomicUsize, Ordering};

use trusty_common::content::ContentSource;

use super::*;
use crate::content::first_use::resolve_or_fetch_in;

/// A [`ReleaseSource`] that counts the listings and asset reads it serves.
struct Counting<'a> {
    inner: &'a FakeSource,
    listings: AtomicUsize,
    assets: AtomicUsize,
}

impl<'a> Counting<'a> {
    fn new(inner: &'a FakeSource) -> Self {
        Self {
            inner,
            listings: AtomicUsize::new(0),
            assets: AtomicUsize::new(0),
        }
    }

    fn listings(&self) -> usize {
        self.listings.load(Ordering::SeqCst)
    }

    fn assets(&self) -> usize {
        self.assets.load(Ordering::SeqCst)
    }
}

impl ReleaseSource for Counting<'_> {
    fn asset_url(&self, tag: &str, file: &str) -> String {
        self.inner.asset_url(tag, file)
    }

    fn asset(&self, tag: &str, file: &str, max: u64) -> Result<Option<Vec<u8>>, FetchError> {
        self.assets.fetch_add(1, Ordering::SeqCst);
        self.inner.asset(tag, file, max)
    }

    fn content_tags(&self) -> Result<Vec<String>, FetchError> {
        self.listings.fetch_add(1, Ordering::SeqCst);
        self.inner.content_tags()
    }

    fn release(&self, tag: &str) -> Result<Release, FetchError> {
        self.inner.release(tag)
    }
}

/// The installed tag `content` is served from; panics on any other source.
fn installed_tag(content: &trusty_common::content::ResolvedContent) -> String {
    match content.source() {
        ContentSource::Installed { tag, .. } => tag.clone(),
        other => panic!("expected the installed bundle, got {other:?}"),
    }
}

/// Every `*.tar.gz` stored in `cache`.
fn stored_bundles(cache: &Path) -> Vec<String> {
    std::fs::read_dir(cache)
        .expect("read cache")
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|name| name.ends_with(".tar.gz"))
        .collect()
}

/// #9396: with no lock, the first resolution fetches the release `tm content
/// update` picks (the newest), pins it and serves it; the second one reads
/// the pin and fetches nothing.
#[test]
fn missing_lock_fetches_the_release_once() {
    let cache = tempfile::tempdir().unwrap();
    let mut fake = FakeSource::default();
    fake.publish(A);
    fake.publish(B);
    let source = Counting::new(&fake);
    for _ in 0..2 {
        let content = resolve_or_fetch_in(cache.path(), DevOverride::Off, |c| {
            install_if_missing(c, &source)
        })
        .expect("fetched, verified and served");
        assert_eq!(installed_tag(&content), B);
    }
    assert_eq!(source.listings(), 1, "one release listing in total");
    assert_eq!(source.assets(), 2, "one bundle and one sidecar in total");
    assert_eq!(pinned(cache.path()).tag(), B);
    resolves_to(cache.path(), B);
}

/// #9396: a present lock never triggers a fetch — a valid one is served, and
/// one that does not parse is an error left untouched for `tm content update`.
#[test]
fn present_lock_never_fetches() {
    let (src, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let bytes = bundle(A, 1, A.as_bytes());
    install_from_file(cache.path(), &bundle_file(src.path(), A, &bytes, true)).expect("pin A");
    let mut fake = FakeSource::default();
    fake.publish(B);
    let calls = AtomicUsize::new(0);
    let fetch = |c: &Path| {
        calls.fetch_add(1, Ordering::SeqCst);
        install_if_missing(c, &fake)
    };
    let content = resolve_or_fetch_in(cache.path(), DevOverride::Off, fetch).expect("pinned A");
    assert_eq!(installed_tag(&content), A, "the present pin is served");

    let broken = tempfile::tempdir().unwrap();
    let lock = broken.path().join(LOCK_FILE_NAME);
    std::fs::write(&lock, "not = [valid").unwrap();
    let fetch = |c: &Path| {
        calls.fetch_add(1, Ordering::SeqCst);
        install_if_missing(c, &fake)
    };
    let err = resolve_or_fetch_in(broken.path(), DevOverride::Off, fetch)
        .expect_err("an unreadable lock is an error");
    assert!(!err.is_not_installed(), "{err:?}");
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), "not = [valid");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a present lock never fetches"
    );
}

/// #9396: offline, or with bytes that fail their sidecar, the first use fails
/// closed: no lock, no stored bundle, and the error names `tm content update`
/// and the offline `--from` install.
#[test]
fn missing_lock_offline_fails_closed_naming_update_and_from() {
    let offline = FakeSource {
        offline: true,
        ..FakeSource::default()
    };
    let mut tampered = FakeSource::default();
    let bytes = bundle(A, 1, A.as_bytes());
    tampered.put(A, &bytes);
    tampered
        .assets
        .insert(format!("{A}/{A}.tar.gz.sha256"), sidecar(A, b"other bytes"));
    for source in [&offline, &tampered] {
        let cache = tempfile::tempdir().unwrap();
        let err = resolve_or_fetch_in(cache.path(), DevOverride::Off, |c| {
            install_if_missing(c, source)
        })
        .expect_err("nothing verified to serve");
        assert!(err.is_not_installed(), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("run `tm content update`"), "{msg}");
        assert!(
            msg.contains("tm content install --from <bundle.tar.gz>"),
            "{msg}"
        );
        assert!(!cache.path().join(LOCK_FILE_NAME).exists(), "no lock");
        assert!(stored_bundles(cache.path()).is_empty(), "no stored bundle");
    }
}

/// #9396: two first uses racing on an empty cache both serve the one pin the
/// first wrote; the second re-checks under the update lock and fetches
/// nothing.
#[test]
fn concurrent_first_use_leaves_one_valid_lock() {
    let cache = tempfile::tempdir().unwrap();
    let mut fake = FakeSource::default();
    fake.publish(B);
    let (entered_tx, entered_rx) = channel();
    let (release_tx, release_rx) = channel();
    fake.gate = Some(Gate {
        file: format!("{B}.tar.gz"),
        entered: entered_tx,
        release: Mutex::new(release_rx),
        timeout: Duration::from_secs(5),
    });
    let source = Counting::new(&fake);
    let first_use = || {
        resolve_or_fetch_in(cache.path(), DevOverride::Off, |c| {
            install_if_missing(c, &source)
        })
        .map(|content| installed_tag(&content))
    };
    std::thread::scope(|s| {
        let first = s.spawn(first_use);
        // The first is mid-fetch, holding the update lock, with no lock file
        // written yet: the second resolves "not installed" and must wait.
        let reached = entered_rx.recv_timeout(Duration::from_secs(10));
        let second = s.spawn(first_use);
        std::thread::sleep(Duration::from_millis(200));
        let _ = release_tx.send(());
        let first = first.join().expect("join");
        let second = second.join().expect("join");
        reached.expect("the first use reached its fetch");
        assert_eq!(first.expect("first use"), B);
        assert_eq!(second.expect("second use"), B);
    });
    assert_eq!(source.listings(), 1, "only one first use fetched");
    assert_eq!(pinned(cache.path()).tag(), B);
    resolves_to(cache.path(), B);
}
