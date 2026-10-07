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
        assert!(msg.contains(INSTALL_HINT), "{msg}");
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

/// A first-use fetch that fails as an unreachable network does.
fn unreachable_fetch(_: &Path) -> Result<Option<UpdateOutcome>, CacheError> {
    Err(CacheError::Network {
        url: "https://api.github.com".to_owned(),
        reason: "network is unreachable".to_owned(),
        tag: None,
        fallback: Fallback::None,
    })
}

/// #9396: after a failed fetch, a composition within the window does not
/// fetch again, and says why; after the window it does. The memo never
/// serves content: a bundle installed meanwhile resolves at once.
#[test]
fn a_failed_fetch_is_not_retried_within_the_window() {
    use crate::content::first_use::{FailureMemo, resolve_or_fetch_with};
    let memo = FailureMemo::new(Duration::from_secs(60));
    let cache = tempfile::tempdir().unwrap();
    let t0 = std::time::Instant::now();
    let calls = AtomicUsize::new(0);
    let failing = |c: &Path| {
        calls.fetch_add(1, Ordering::SeqCst);
        unreachable_fetch(c)
    };
    let first = resolve_or_fetch_with(cache.path(), DevOverride::Off, failing, &memo, t0)
        .expect_err("unreachable");
    assert!(
        first.to_string().contains("network is unreachable"),
        "{first}"
    );
    let again = resolve_or_fetch_with(
        cache.path(),
        DevOverride::Off,
        |_: &Path| -> Result<Option<UpdateOutcome>, CacheError> {
            panic!("retried inside the window")
        },
        &memo,
        t0 + Duration::from_secs(30),
    )
    .expect_err("still not installed");
    assert!(again.is_not_installed(), "{again:?}");
    let msg = again.to_string();
    assert!(msg.contains("network is unreachable"), "{msg}");
    assert!(msg.contains("did not retry"), "{msg}");
    assert!(msg.contains("tm content update"), "{msg}");

    let later = t0 + Duration::from_secs(61);
    resolve_or_fetch_with(cache.path(), DevOverride::Off, failing, &memo, later)
        .expect_err("still unreachable");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "retried after the window");

    let src = tempfile::tempdir().unwrap();
    let bytes = bundle(A, 1, A.as_bytes());
    install_from_file(cache.path(), &bundle_file(src.path(), A, &bytes, true)).expect("pin A");
    let content = resolve_or_fetch_with(
        cache.path(),
        DevOverride::Off,
        |_: &Path| -> Result<Option<UpdateOutcome>, CacheError> { panic!("a lock is present") },
        &memo,
        later,
    )
    .expect("the installed bundle serves inside the window");
    assert_eq!(installed_tag(&content), A);
}

/// #9396: a fetch run from a task on a one-worker runtime hands the worker
/// off, so another task still runs while the fetch blocks. Without that the
/// releasing task starves until the fetch gives up.
#[test]
fn a_fetch_inside_a_runtime_leaves_the_worker_free() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("runtime");
    let cache = tempfile::tempdir().unwrap();
    let path = cache.path().to_path_buf();
    let (entered_tx, entered_rx) = channel::<()>();
    let (release_tx, release_rx) = channel::<()>();
    let released = runtime.block_on(async move {
        let fetching = tokio::spawn(async move {
            let mut released = false;
            let _ = resolve_or_fetch_in(&path, DevOverride::Off, |c| {
                let _ = entered_tx.send(());
                released = release_rx.recv_timeout(Duration::from_secs(5)).is_ok();
                unreachable_fetch(c)
            });
            released
        });
        // The fetch now blocks the only worker; this task needs one to run.
        entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the fetch started");
        tokio::spawn(async move {
            let _ = release_tx.send(());
        });
        fetching.await.expect("fetch task")
    });
    assert!(
        released,
        "the releasing task never ran while the fetch blocked"
    );
}

/// Lays out a tm-managed workspace `<home>/trusty-mpm-projects/<owner>/<repo>`
/// holding `.git`; a `trusty-tools` repo also gets a stale checkout's layout
/// (every class directory, a `[workspace]` manifest, no `BASE-AGENT.md`).
fn managed_workspace(home: &Path, owner: &str, repo: &str) -> std::path::PathBuf {
    let root = home.join("trusty-mpm-projects").join(owner).join(repo);
    std::fs::create_dir_all(root.join(".git")).expect(".git");
    if repo == "trusty-tools" {
        for (_, rel) in trusty_common::content::DEV_CLASS_SOURCES {
            std::fs::create_dir_all(root.join(rel)).expect("class dir");
        }
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("manifest");
        std::fs::write(root.join("content/agents/engineer.md"), "e\n").expect("agent");
    } else {
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"w\"\n").expect("manifest");
    }
    root
}

/// #9396: from a tm-managed workspace that is not trusty-tools, an empty
/// cache is `NotInstalled` naming the remedy (the first-use fetch failing
/// leaves the same answer), and once a bundle is installed into the cache
/// the very next resolution serves it — no restart, no re-init. A stale
/// trusty-tools base clone serves its own tree, and its error names the
/// clone and `git pull`, never `tm content update`.
#[test]
fn a_managed_workspace_resolves_the_cache_once_a_bundle_lands() {
    use crate::content::first_use::FETCH_OVERRIDE;
    use crate::core::content_source::{Fetch, resolve_for_in};
    use trusty_agents_common::agent_content::{AgentRoster, REMEDY};

    let home = tempfile::tempdir().unwrap();
    let cache = home.path().join(".trusty-mpm/content");
    std::fs::create_dir_all(&cache).unwrap();
    let project = managed_workspace(home.path(), "acme", "widget");
    let stale = managed_workspace(home.path(), "bobmatnyc", "trusty-tools");

    let err =
        resolve_for_in(Some(&project), None, Some(&cache), Fetch::Never).expect_err("empty cache");
    assert!(err.is_not_installed(), "{err:?}");
    assert!(err.to_string().contains(REMEDY), "{err}");
    FETCH_OVERRIDE.with(|f| f.set(Some(unreachable_fetch)));
    let fetched = resolve_for_in(Some(&project), None, Some(&cache), Fetch::OnFirstUse);
    FETCH_OVERRIDE.with(|f| f.set(None));
    let err = fetched.expect_err("the fetch failed");
    assert!(err.is_not_installed(), "{err:?}");
    assert!(err.to_string().contains("tm content update"), "{err}");

    let src = tempfile::tempdir().unwrap();
    let bytes = bundle(A, 1, A.as_bytes());
    install_from_file(&cache, &bundle_file(src.path(), A, &bytes, true)).expect("pin A");
    for fetch in [Fetch::Never, Fetch::OnFirstUse] {
        let content = resolve_for_in(Some(&project), None, Some(&cache), fetch)
            .expect("the installed bundle serves");
        assert_eq!(installed_tag(&content), A);
    }

    let content = resolve_for_in(Some(&stale), None, Some(&cache), Fetch::Never)
        .expect("the clone serves its working tree");
    let msg = AgentRoster::load(&content)
        .expect_err("no BASE-AGENT.md in the stale clone")
        .to_string();
    assert!(msg.contains(&stale.display().to_string()), "{msg}");
    assert!(msg.contains("git pull"), "{msg}");
    assert!(!msg.contains("tm content update"), "{msg}");
}
