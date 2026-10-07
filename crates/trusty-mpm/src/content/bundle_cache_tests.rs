//! Tests for the content cache writer, the status view, and the GitHub
//! release source (#8378 PR-C; ADR-0064 PHASE_3, #8974 test plan cases 1-5,
//! 10 and 13).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use trusty_common::content::{
    ContentError, ContentLock, DevOverride, LOCK_FILE_NAME, ResolveOptions, SUPPORTED_SCHEMA_MAJOR,
    resolve,
};
use trusty_common::integrity::Sha256Digest;

use super::*;
use crate::content::status::content_status;

const A: &str = "content-v0.1.0";
const B: &str = "content-v0.2.0";

/// A gzip tar laid out the way `scripts/package_content.sh` writes one.
fn bundle(tag: &str, schema_major: u32, body: &[u8]) -> Vec<u8> {
    bundle_with_manifest(
        &format!("tag = \"{tag}\"\nschema_major = {schema_major}\n"),
        body,
    )
}

fn bundle_with_manifest(manifest: &str, body: &[u8]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    for (path, data) in [
        ("bundle-manifest.toml", manifest.as_bytes()),
        ("skills/tm/SKILL.md", body),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, path, data).expect("append");
    }
    tar.into_inner().expect("tar").finish().expect("gzip")
}

fn sidecar(tag: &str, bytes: &[u8]) -> Vec<u8> {
    format!("{}  {tag}.tar.gz\n", Sha256Digest::of_bytes(bytes)).into_bytes()
}

/// Holds `asset()` for one file until released, or until `timeout`.
struct Gate {
    file: String,
    entered: Sender<()>,
    release: Mutex<Receiver<()>>,
    timeout: Duration,
}

/// An in-memory release host. Every tag with an asset is a release; `drafts`
/// and `prereleases` mark some of them the way GitHub does: `releases/tags`
/// answers 404 for a draft and flags a pre-release.
#[derive(Default)]
struct FakeSource {
    assets: HashMap<String, Vec<u8>>,
    drafts: HashSet<String>,
    prereleases: HashSet<String>,
    offline: bool,
    gate: Option<Gate>,
}

impl FakeSource {
    fn publish(&mut self, tag: &str) -> Vec<u8> {
        let bytes = bundle(tag, 1, tag.as_bytes());
        self.put(tag, &bytes);
        bytes
    }

    fn put(&mut self, tag: &str, bytes: &[u8]) {
        self.assets
            .insert(format!("{tag}/{tag}.tar.gz"), bytes.to_vec());
        self.assets
            .insert(format!("{tag}/{tag}.tar.gz.sha256"), sidecar(tag, bytes));
    }
}

impl ReleaseSource for FakeSource {
    fn asset_url(&self, tag: &str, file: &str) -> String {
        format!("fake://{tag}/{file}")
    }

    fn asset(&self, tag: &str, file: &str, _max: u64) -> Result<Option<Vec<u8>>, FetchError> {
        if self.offline {
            return Err(FetchError {
                url: self.asset_url(tag, file),
                reason: "network is unreachable".into(),
                status: None,
            });
        }
        if let Some(gate) = self.gate.as_ref().filter(|g| g.file == file) {
            let _ = gate.entered.send(());
            let _ = gate
                .release
                .lock()
                .expect("gate")
                .recv_timeout(gate.timeout);
        }
        Ok(self.assets.get(&format!("{tag}/{file}")).cloned())
    }

    fn content_tags(&self) -> Result<Vec<String>, FetchError> {
        if self.offline {
            return Err(FetchError {
                url: "fake://tags".into(),
                reason: "network is unreachable".into(),
                status: None,
            });
        }
        let mut tags: Vec<String> = self
            .assets
            .keys()
            .filter_map(|k| k.split('/').next().map(str::to_owned))
            .collect();
        tags.sort();
        tags.dedup();
        Ok(tags)
    }

    fn release(&self, tag: &str) -> Result<Release, FetchError> {
        if self.drafts.contains(tag) {
            return Err(FetchError {
                url: format!("fake://releases/tags/{tag}"),
                reason: "HTTP 404 Not Found".into(),
                status: Some(404),
            });
        }
        Ok(Release {
            tag: tag.to_owned(),
            draft: false,
            prerelease: self.prereleases.contains(tag),
        })
    }
}

fn pinned(cache: &Path) -> ContentLock {
    ContentLock::load(&cache.join(LOCK_FILE_NAME)).expect("lock")
}

fn resolves_to(cache: &Path, tag: &str) {
    let resolved = resolve(&ResolveOptions::new(cache, DevOverride::Off)).expect("resolve");
    let body = resolved.read("skills/tm/SKILL.md").expect("read");
    assert_eq!(body, tag.as_bytes(), "the served bundle is {tag}");
}

/// Writes `<dir>/<tag>.tar.gz` and, when `with_sidecar`, its sidecar.
fn bundle_file(dir: &Path, tag: &str, bytes: &[u8], with_sidecar: bool) -> std::path::PathBuf {
    let path = dir.join(format!("{tag}.tar.gz"));
    std::fs::write(&path, bytes).expect("bundle");
    if with_sidecar {
        std::fs::write(
            dir.join(format!("{tag}.tar.gz.sha256")),
            sidecar(tag, bytes),
        )
        .expect("sidecar");
    }
    path
}

#[test]
fn install_from_a_file_pins_its_tag_and_sha256() {
    let (src, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let bytes = bundle(A, 1, A.as_bytes());
    let file = bundle_file(src.path(), A, &bytes, true);
    let out = install_from_file(cache.path(), &file).expect("install");
    assert_eq!(out.action, UpdateAction::Installed);
    let lock = pinned(cache.path());
    assert_eq!(lock.tag(), A);
    assert_eq!(lock.sha256(), &Sha256Digest::of_bytes(&bytes));
    resolves_to(cache.path(), A);
}

#[test]
fn install_refuses_a_bundle_that_does_not_match_its_sidecar() {
    let (src, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let file = bundle_file(src.path(), A, &bundle(A, 1, b"x"), false);
    std::fs::write(
        src.path().join(format!("{A}.tar.gz.sha256")),
        sidecar(A, b"other bytes"),
    )
    .unwrap();
    let err = install_from_file(cache.path(), &file).expect_err("mismatch");
    assert!(
        matches!(err, CacheError::ChecksumMismatch { .. }),
        "{err:?}"
    );
    assert!(
        !cache.path().join(LOCK_FILE_NAME).exists(),
        "nothing pinned"
    );
}

#[test]
fn install_without_a_sidecar_is_refused() {
    let (src, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let file = bundle_file(src.path(), A, &bundle(A, 1, b"x"), false);
    let err = install_from_file(cache.path(), &file).expect_err("no sidecar");
    assert!(matches!(err, CacheError::SidecarMissing { .. }), "{err:?}");
    assert!(
        !cache.path().join(LOCK_FILE_NAME).exists(),
        "nothing pinned"
    );
}

/// #8974 case 10: a newer schema major is refused and the cache is untouched.
#[test]
fn install_refuses_a_newer_schema_major_and_leaves_the_cache_untouched() {
    let (src, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = bundle(A, 1, A.as_bytes());
    install_from_file(cache.path(), &bundle_file(src.path(), A, &a, true)).expect("A");
    let b = bundle(B, SUPPORTED_SCHEMA_MAJOR + 1, B.as_bytes());
    let err = install_from_file(cache.path(), &bundle_file(src.path(), B, &b, true))
        .expect_err("newer schema");
    assert!(
        matches!(
            err,
            CacheError::Content(ContentError::UnsupportedSchema { .. })
        ),
        "{err:?}"
    );
    assert_eq!(pinned(cache.path()).tag(), A);
    assert!(!cache.path().join(format!("{B}.tar.gz")).exists());
    resolves_to(cache.path(), A);
}

#[test]
fn update_with_no_lock_installs_the_latest_release() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    src.publish(A);
    src.publish(B);
    src.publish("content-v0.3.0-rc.1");
    let out = update(cache.path(), &src, None).expect("update");
    assert_eq!((out.tag.as_str(), out.action), (B, UpdateAction::Installed));
    resolves_to(cache.path(), B);
}

/// Owner ruling (Bob item 207): a no-flag update installs the newest
/// published release and re-pins to it; a second run with nothing newer
/// writes nothing.
#[test]
fn update_without_a_ref_moves_the_pin_to_the_newest_release() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    src.publish(A);
    update(cache.path(), &src, Some(A)).expect("pin A");
    src.publish(B);
    let out = update(cache.path(), &src, None).expect("update");
    assert_eq!((out.tag.as_str(), out.action), (B, UpdateAction::Installed));
    assert_eq!(pinned(cache.path()).tag(), B);
    resolves_to(cache.path(), B);
    let again = update(cache.path(), &src, None).expect("again");
    assert_eq!(
        (again.tag.as_str(), again.action),
        (B, UpdateAction::AlreadyCurrent)
    );
}

/// "Latest" is a published release. A pre-release with the highest version
/// is passed over for the next one down. A draft has no release GitHub will
/// show (a 404), so a draft tag at the top fails the update and the pin stays.
#[test]
fn update_never_installs_a_draft_or_a_pre_release() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    src.publish(A);
    src.publish("content-v0.8.0");
    src.prereleases.insert("content-v0.8.0".to_owned());
    let out = update(cache.path(), &src, None).expect("update");
    assert_eq!(out.tag, A);
    assert_eq!(pinned(cache.path()).tag(), A);

    src.publish("content-v0.9.0");
    src.drafts.insert("content-v0.9.0".to_owned());
    let err = update(cache.path(), &src, None).expect_err("a draft is a 404");
    assert!(matches!(err, CacheError::Network { .. }), "{err:?}");
    assert_eq!(pinned(cache.path()).tag(), A);
}

/// The sidecar is required: a release that ships a bundle without one is
/// refused, and nothing is pinned or stored.
#[test]
fn update_refuses_a_release_without_a_sidecar() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    src.publish(A);
    src.assets.remove(&format!("{A}/{A}.tar.gz.sha256"));
    let err = update(cache.path(), &src, None).expect_err("no sidecar");
    match &err {
        CacheError::SidecarNotPublished { tag, fallback, .. } => {
            assert_eq!((tag.as_str(), fallback), (A, &Fallback::None));
        }
        other => panic!("expected SidecarNotPublished, got {other:?}"),
    }
    // #9396: the manual install, for the concrete tag.
    assert!(
        err.to_string()
            .contains(&format!("tm content install --from <dir>/{A}.tar.gz")),
        "{err}"
    );
    assert!(!cache.path().join(LOCK_FILE_NAME).exists());
    assert!(!cache.path().join(format!("{A}.tar.gz")).exists());
}

/// ADR-0064 PHASE_3 (iv) on the update path: a newer `schema_major` is
/// `UnsupportedSchema`, a missing one is `BundleCorrupt`; the pin stays.
#[test]
fn update_refuses_a_newer_or_missing_schema_major_and_keeps_the_pin() {
    let newer = format!(
        "tag = \"{B}\"\nschema_major = {}\n",
        SUPPORTED_SCHEMA_MAJOR + 1
    );
    let missing = format!("tag = \"{B}\"\n");
    for (manifest, want_corrupt) in [(newer, false), (missing, true)] {
        let cache = tempfile::tempdir().unwrap();
        let mut src = FakeSource::default();
        src.publish(A);
        update(cache.path(), &src, Some(A)).expect("pin A");
        src.put(B, &bundle_with_manifest(&manifest, B.as_bytes()));
        let err = update(cache.path(), &src, None).expect_err("refused");
        match (&err, want_corrupt) {
            (CacheError::Content(ContentError::UnsupportedSchema { .. }), false) => {}
            (CacheError::Content(ContentError::BundleCorrupt { reason, .. }), true) => {
                assert!(reason.contains("no schema_major"), "{reason}");
            }
            (other, _) => panic!("{manifest:?}: unexpected {other:?}"),
        }
        assert_eq!(pinned(cache.path()).tag(), A);
        assert!(!cache.path().join(format!("{B}.tar.gz")).exists());
        resolves_to(cache.path(), A);
    }
}

#[test]
fn update_refuses_a_bundle_whose_sha256_differs_from_the_sidecar() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    src.publish(A);
    src.assets
        .insert(format!("{A}/{A}.tar.gz"), bundle(A, 1, b"tampered"));
    let err = update(cache.path(), &src, Some(A)).expect_err("mismatch");
    assert!(
        matches!(err, CacheError::ChecksumMismatch { .. }),
        "{err:?}"
    );
    assert!(
        !cache.path().join(LOCK_FILE_NAME).exists(),
        "nothing pinned"
    );
    assert!(!cache.path().join(format!("{A}.tar.gz")).exists());
}

/// #8974 case 5: a missing tag is named; the verified cache keeps serving.
#[test]
fn update_to_a_tag_missing_upstream_is_a_named_error() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    src.publish(A);
    update(cache.path(), &src, Some(A)).expect("pin A");
    let err = update(cache.path(), &src, Some("content-v9.9.9")).expect_err("missing");
    match &err {
        CacheError::TagNotFound { tag, fallback, .. } => {
            assert_eq!(tag, "content-v9.9.9");
            assert_eq!(fallback, &Fallback::Cached(A.to_owned()));
        }
        other => panic!("expected TagNotFound, got {other:?}"),
    }
    assert_eq!(pinned(cache.path()).tag(), A);
    resolves_to(cache.path(), A);

    // With no verified cache, the same miss fails closed and names the fix.
    std::fs::remove_file(cache.path().join(format!("{A}.tar.gz"))).unwrap();
    let mut gone = FakeSource::default();
    gone.publish(B);
    let err = update(cache.path(), &gone, Some(A)).expect_err("pinned tag gone");
    assert!(
        matches!(&err, CacheError::TagNotFound { fallback, .. } if *fallback == Fallback::None),
        "{err:?}"
    );
    assert!(err.to_string().contains(INSTALL_HINT), "{err}");
    assert_eq!(pinned(cache.path()).tag(), A, "an explicit ref never moves");
}

/// #8974 case 1: offline with no cache, tm names the install command.
#[test]
fn update_offline_with_no_cache_names_the_install_command() {
    let cache = tempfile::tempdir().unwrap();
    let src = FakeSource {
        offline: true,
        ..FakeSource::default()
    };
    let err = update(cache.path(), &src, None).expect_err("offline");
    assert!(matches!(err, CacheError::Network { .. }), "{err:?}");
    assert!(err.to_string().contains(INSTALL_HINT), "{err}");
    assert!(!cache.path().join(LOCK_FILE_NAME).exists());
}

/// A tag-listing failure is an error, never "already current": the update
/// could not learn the newest release. The verified pin stays in use.
#[test]
fn update_offline_with_a_verified_cache_fails_and_keeps_the_pin() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    src.publish(A);
    update(cache.path(), &src, Some(A)).expect("pin A");
    src.offline = true;
    let err = update(cache.path(), &src, None).expect_err("listing failed");
    match &err {
        CacheError::Network { url, fallback, .. } => {
            assert_eq!(url, "fake://tags");
            assert_eq!(fallback, &Fallback::Cached(A.to_owned()));
        }
        other => panic!("expected Network, got {other:?}"),
    }
    assert_eq!(pinned(cache.path()).tag(), A);
    let err = update(cache.path(), &src, Some(B)).expect_err("offline");
    assert!(
        err.to_string().contains("is verified and stays in use"),
        "{err}"
    );
    resolves_to(cache.path(), A);
}

#[test]
fn update_repairs_a_missing_pinned_bundle_and_refuses_a_republished_tag() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    src.publish(A);
    update(cache.path(), &src, Some(A)).expect("pin A");
    let bundle_path = cache.path().join(format!("{A}.tar.gz"));
    std::fs::remove_file(&bundle_path).unwrap();
    let out = update(cache.path(), &src, None).expect("repair");
    assert_eq!(out.action, UpdateAction::Repaired);
    resolves_to(cache.path(), A);

    std::fs::remove_file(&bundle_path).unwrap();
    src.put(A, &bundle(A, 1, b"republished"));
    let err = update(cache.path(), &src, None).expect_err("republished");
    assert!(matches!(err, CacheError::PinConflict { .. }), "{err:?}");
}

/// The bundle is stored before the lock swap: a failed bundle write leaves
/// the previous pin in force and resolvable.
#[test]
fn a_failed_bundle_write_leaves_the_previous_pin_in_force() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    src.publish(A);
    src.publish(B);
    update(cache.path(), &src, Some(A)).expect("pin A");
    // A non-empty directory where B's bundle goes: the rename onto it fails.
    let blocker = cache.path().join(format!("{B}.tar.gz"));
    std::fs::create_dir_all(blocker.join("occupied")).unwrap();
    let err = update(cache.path(), &src, Some(B)).expect_err("write fails");
    assert!(matches!(err, CacheError::Io { .. }), "{err:?}");
    assert_eq!(pinned(cache.path()).tag(), A, "the lock was not swapped");
    resolves_to(cache.path(), A);
}

/// #8974 case 13: a no-flag update that is mid-fetch cannot overwrite the pin
/// a concurrent `--content-ref` writes; the file lock serialises them.
#[test]
fn concurrent_updates_serialise_on_the_file_lock() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    src.publish(A);
    src.publish(B);
    update(cache.path(), &src, Some(A)).expect("pin A");
    // The no-flag update below moves to B, and pauses while fetching it. The
    // `--content-ref A` behind it must wait, then find B pinned and restore A;
    // without the lock it would see A verified, write nothing, and lose to B.
    let (entered_tx, entered_rx) = channel();
    let (release_tx, release_rx) = channel();
    src.gate = Some(Gate {
        file: format!("{B}.tar.gz"),
        entered: entered_tx,
        release: Mutex::new(release_rx),
        timeout: Duration::from_secs(2),
    });
    std::thread::scope(|s| {
        let first = s.spawn(|| update(cache.path(), &src, None));
        entered_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the first update reached its fetch");
        let second = s.spawn(|| {
            let out = update(cache.path(), &src, Some(A));
            let _ = release_tx.send(());
            out
        });
        first.join().expect("join").expect("first update");
        let second = second.join().expect("join").expect("second update");
        assert_eq!(second.action, UpdateAction::Installed, "A was re-pinned");
    });
    assert_eq!(pinned(cache.path()).tag(), A, "the last explicit pin wins");
    resolves_to(cache.path(), A);
}

#[test]
fn status_reports_a_verified_bundle() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    let bytes = src.publish(A);
    update(cache.path(), &src, Some(A)).expect("pin A");
    let status = content_status(cache.path(), None);
    assert_eq!(status.source_label(), "bundle");
    assert!(status.serves());
    let text = status.lines().join("\n");
    assert!(
        text.contains(&Sha256Digest::of_bytes(&bytes).to_string()),
        "{text}"
    );
    assert!(text.contains(&format!("installed: {A}")), "{text}");
    assert!(text.contains("binary: trusty-mpm "), "{text}");
}

#[test]
fn status_with_nothing_installed_names_the_fix() {
    let cache = tempfile::tempdir().unwrap();
    let status = content_status(cache.path(), Some(cache.path()));
    assert_eq!(status.source_label(), "none");
    assert!(!status.serves() && status.not_installed());
    assert!(status.lines().join("\n").contains(INSTALL_HINT));
}

/// Critic M2, after #9012 dropped the last built-in content: `tm content
/// status` with nothing installed exits non-zero. A broken install never
/// exits 0.
#[test]
fn status_with_nothing_installed_exits_non_zero() {
    let cache = tempfile::tempdir().unwrap();
    let status = content_status(cache.path(), Some(cache.path()));
    assert!(!status.exits_ok());

    let mut src = FakeSource::default();
    src.publish(A);
    update(cache.path(), &src, Some(A)).expect("pin A");
    std::fs::write(cache.path().join(format!("{A}.tar.gz")), b"tampered").unwrap();
    let broken = content_status(cache.path(), None);
    assert!(!broken.exits_ok(), "a broken install never exits 0");
}

#[test]
fn status_reports_a_tampered_bundle_as_unhealthy() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    src.publish(A);
    update(cache.path(), &src, Some(A)).expect("pin A");
    std::fs::write(cache.path().join(format!("{A}.tar.gz")), b"tampered").unwrap();
    let status = content_status(cache.path(), None);
    assert!(!status.serves() && !status.not_installed());
    assert!(matches!(
        status.installed,
        Err(ContentError::ChecksumMismatch { .. })
    ));
    assert!(status.lines().join("\n").contains("UNHEALTHY"));
}

/// Serves `routes` (path -> status, body) over HTTP/1.1 on a loopback port,
/// one request per connection, and returns the base URL. A route with no
/// query string answers that path with any query. The thread lives until the
/// test process exits.
fn serve(routes: Vec<(&'static str, u16, &'static str)>) -> String {
    let owned = routes
        .into_iter()
        .map(|(path, status, body)| route(path, status, body))
        .collect();
    serve_recording(owned).0
}

/// One served route: path, status, body.
type Route = (String, u16, Vec<u8>);

/// Every request a [`serve_recording`] server saw: (path, raw headers).
type Seen = std::sync::Arc<Mutex<Vec<(String, String)>>>;

/// [`serve`] with owned routes and byte bodies, and a log of each request's
/// path and headers.
fn serve_recording(routes: Vec<Route>) -> (String, Seen) {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let seen = Seen::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.expect("accept");
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("request");
            let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
            let mut headers = String::new();
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).expect("header");
                if header.trim().is_empty() {
                    break;
                }
                headers.push_str(&header);
            }
            let empty = Vec::new();
            let bare = path.split('?').next().unwrap_or("");
            let (status, body) = routes
                .iter()
                .find(|(p, _, _)| *p == path || *p == bare)
                .map_or((404, &empty), |(_, s, b)| (*s, b));
            log.lock().expect("log").push((path, headers));
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(body);
        }
    });
    (base, seen)
}

#[test]
fn github_source_reads_a_404_as_absent() {
    let base = serve(vec![("/dl/content-v0.1.0/present", 200, "bytes")]);
    let src = GithubReleases::with_bases(&format!("{base}/dl"), &base).expect("client");
    assert_eq!(
        src.asset("content-v0.1.0", "present", 64).expect("200"),
        Some(b"bytes".to_vec())
    );
    assert_eq!(
        src.asset("content-v0.1.0", "absent", 64).expect("404"),
        None
    );
}

/// One `git/matching-refs` entry, as JSON.
fn ref_entry(tag: &str) -> String {
    format!(
        r#"{{"ref":"refs/tags/{tag}","node_id":"n","url":"u","object":{{"sha":"s","type":"commit","url":"u"}}}}"#
    )
}

/// A JSON array of ref entries for `tags`.
fn refs(tags: &[&str]) -> String {
    let entries: Vec<String> = tags.iter().map(|t| ref_entry(t)).collect();
    format!("[{}]", entries.join(","))
}

/// One `releases/tags/{tag}` response, as JSON.
fn release_json(tag: &str, draft: bool, prerelease: bool) -> String {
    format!(
        r#"{{"tag_name":"{tag}","draft":{draft},"prerelease":{prerelease},"name":"x","assets":[]}}"#
    )
}

/// The one tag-listing request; the server answers it whatever the query.
const REFS: &str = "/git/matching-refs/tags/content-v";

fn route(path: &str, status: u16, body: impl Into<Vec<u8>>) -> Route {
    (path.to_owned(), status, body.into())
}

/// A source over `base`: assets under `/dl`, the API at the root.
fn github(base: &str) -> GithubReleases {
    GithubReleases::with_bases(&format!("{base}/dl"), base).expect("client")
}

/// #9036: matching-refs ignores `per_page` and `page` and answers every
/// query with the whole list, so one unpaged request reads it all. The
/// server here does the same with 120 refs; a client that pages re-reads
/// them until its page cap and fails.
#[test]
fn github_source_lists_every_content_tag_in_one_request() {
    let owned: Vec<String> = (0..120).map(|i| format!("content-v0.0.{i}")).collect();
    let tags: Vec<&str> = owned.iter().map(String::as_str).collect();
    let (base, seen) = serve_recording(vec![route(REFS, 200, refs(&tags))]);
    let listed = github(&base).content_tags().expect("tags");
    let paths: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(p, _)| p.clone())
        .collect();
    assert_eq!(paths, vec![REFS.to_owned()], "exactly one unpaged request");
    assert_eq!(listed, owned);
}

/// The listing is read whole or not at all: one over the size cap is
/// refused, never truncated to the tags that fit, and an update pins nothing.
#[test]
fn github_source_refuses_a_tag_listing_over_its_size_cap() {
    let cap = usize::try_from(crate::content::release_source::MAX_LISTING_BYTES).unwrap();
    let mut body = refs(&[A]).into_bytes();
    body.resize(cap + 1, b' ');
    let (base, _) = serve_recording(vec![route(REFS, 200, body)]);
    let err = github(&base).content_tags().expect_err("over the cap");
    assert!(err.reason.contains("over"), "{err:?}");
    let cache = tempfile::tempdir().unwrap();
    let err = update(cache.path(), &github(&base), None).expect_err("over the cap");
    assert!(matches!(err, CacheError::Network { .. }), "{err:?}");
    assert!(!cache.path().join(LOCK_FILE_NAME).exists());
}

/// A release body that is not JSON, a release for another tag, and a ref
/// outside `refs/tags/` are each a failure naming what was wrong, and an
/// update reading any of them pins nothing.
#[test]
fn github_source_refuses_a_malformed_or_mismatched_answer() {
    let release_route = format!("/releases/tags/{A}");
    let bad_ref = format!(r#"[{{"ref":"refs/heads/{A}","object":{{"sha":"s"}}}}]"#);
    let cases: [(&str, Vec<Route>, &str); 3] = [
        (
            "release body not JSON",
            vec![
                route(REFS, 200, refs(&[A])),
                route(&release_route, 200, "<html>busy</html>"),
            ],
            "unexpected response",
        ),
        (
            "tag_name mismatch",
            vec![
                route(REFS, 200, refs(&[A])),
                route(&release_route, 200, release_json(B, false, false)),
            ],
            "asked for release",
        ),
        (
            "ref outside refs/tags/",
            vec![route(REFS, 200, bad_ref)],
            "unexpected ref",
        ),
    ];
    for (case, routes, reason) in cases {
        let (base, _) = serve_recording(routes);
        let src = github(&base);
        let err = src
            .content_tags()
            .and_then(|_| src.release(A))
            .expect_err(case);
        assert!(err.reason.contains(reason), "{case}: {err:?}");
        let cache = tempfile::tempdir().unwrap();
        let err = update(cache.path(), &src, None).expect_err(case);
        assert!(matches!(err, CacheError::Network { .. }), "{case}: {err:?}");
        assert!(!cache.path().join(LOCK_FILE_NAME).exists(), "{case}");
    }
}

/// #9036: an empty or whitespace-only `GITHUB_TOKEN` falls through to
/// `GH_TOKEN`, and a blank `GH_TOKEN` is unset. Reads a table, not the process environment.
#[test]
fn an_empty_github_token_falls_through_to_gh_token() {
    let cases = [
        (Some(""), Some("gh"), Some("gh")),
        (None, Some("gh"), Some("gh")),
        (Some("g"), Some("gh"), Some("g")),
        (Some("g"), None, Some("g")),
        (Some("  "), Some("gh"), Some("gh")),
        (Some("\n"), Some("gh"), Some("gh")),
        (Some(" g \n"), Some("gh"), Some("g")),
        (Some(""), Some(""), None),
        (Some("  "), Some("\n"), None),
        (Some(""), None, None),
        (None, None, None),
    ];
    for (github_token, gh_token, want) in cases {
        let got = crate::content::release_source::github_token_from(|name| match name {
            "GITHUB_TOKEN" => github_token.map(str::to_owned),
            "GH_TOKEN" => gh_token.map(str::to_owned),
            other => panic!("unexpected variable {other}"),
        });
        assert_eq!(
            got.as_deref(),
            want,
            "GITHUB_TOKEN={github_token:?} GH_TOKEN={gh_token:?}"
        );
    }
}

/// Critic M3: a rate-limited (403) or unavailable (503) listing, or release
/// lookup, is a failure that names the status; an update reads it as
/// `Network`, never as `NoReleases`, and pins nothing.
#[test]
fn github_source_reads_a_rate_limited_or_5xx_response_as_a_failure() {
    for (status, label) in [(403, "403"), (503, "503")] {
        let (base, _) = serve_recording(vec![route(REFS, status, r#"{"message":"no"}"#)]);
        let src = github(&base);
        let err = src.content_tags().expect_err("error status");
        assert!(err.reason.contains(label), "{status}: {err:?}");
        let cache = tempfile::tempdir().unwrap();
        let err = update(cache.path(), &src, None).expect_err("no listing");
        assert!(
            matches!(err, CacheError::Network { .. }),
            "{status}: {err:?}"
        );
        assert!(!cache.path().join(LOCK_FILE_NAME).exists());

        let (base, _) = serve_recording(vec![
            route(REFS, 200, refs(&[A])),
            route(
                &format!("/releases/tags/{A}"),
                status,
                r#"{"message":"no"}"#,
            ),
        ]);
        let src = github(&base);
        let err = src.release(A).expect_err("error status");
        assert!(err.reason.contains(label), "{status}: {err:?}");
        let err = update(cache.path(), &src, None).expect_err("no release");
        assert!(
            matches!(err, CacheError::Network { .. }),
            "{status}: {err:?}"
        );
        assert!(!cache.path().join(LOCK_FILE_NAME).exists());
    }
}

/// The repository always exists, so a 404 listing, or a tag with no release,
/// is a failure, not "none".
#[test]
fn github_source_reads_a_404_tag_listing_as_a_failure() {
    let (base, _) = serve_recording(vec![]);
    let err = github(&base).content_tags().expect_err("404");
    assert!(err.reason.contains("404"), "{err:?}");
    let (base, _) = serve_recording(vec![route(REFS, 200, refs(&[A]))]);
    let cache = tempfile::tempdir().unwrap();
    let err = update(cache.path(), &github(&base), None).expect_err("no release for the tag");
    assert!(matches!(err, CacheError::Network { .. }), "{err:?}");
}

#[test]
fn github_source_reads_a_non_404_error_status_as_a_failure() {
    let base = serve(vec![("/dl/content-v0.1.0/broken", 500, "oops")]);
    let src = GithubReleases::with_bases(&format!("{base}/dl"), &base).expect("client");
    let err = src
        .asset("content-v0.1.0", "broken", 64)
        .expect_err("a 500 is not an asset");
    assert!(err.reason.contains("HTTP 500"), "{err:?}");
    assert!(err.url.ends_with("/content-v0.1.0/broken"), "{err:?}");
}

#[test]
fn github_source_refuses_a_tag_listing_that_is_not_json() {
    let (base, _) = serve_recording(vec![route(REFS, 200, "<html>rate limited</html>")]);
    let err = github(&base).content_tags().expect_err("not JSON");
    assert!(err.reason.starts_with("unexpected response"), "{err:?}");
}

/// An empty matching-refs list is an error (`NoReleases`), never "current".
#[test]
fn update_with_no_content_tags_is_an_error() {
    let (base, _) = serve_recording(vec![route(REFS, 200, "[]")]);
    let cache = tempfile::tempdir().unwrap();
    let err = update(cache.path(), &github(&base), None).expect_err("empty list");
    assert!(matches!(err, CacheError::NoReleases), "{err:?}");
    assert!(!cache.path().join(LOCK_FILE_NAME).exists());
}

/// A tag that is not a content version is refused, not skipped.
#[test]
fn update_refuses_a_listing_with_an_unparsable_tag() {
    let (base, _) = serve_recording(vec![route(REFS, 200, refs(&[A, "content-vfoo"]))]);
    let cache = tempfile::tempdir().unwrap();
    let err = update(cache.path(), &github(&base), None).expect_err("unparsable");
    match &err {
        CacheError::UnparsableTag { tag, .. } => assert_eq!(tag, "content-vfoo"),
        other => panic!("expected UnparsableTag, got {other:?}"),
    }
    assert!(!cache.path().join(LOCK_FILE_NAME).exists());
}

/// A release with no bundle, or a bundle with no `.sha256` sidecar, is an
/// error that pins nothing.
#[test]
fn update_through_github_refuses_a_release_without_a_bundle_or_a_sidecar() {
    let listing = [
        route(REFS, 200, refs(&[A])),
        route(
            &format!("/releases/tags/{A}"),
            200,
            release_json(A, false, false),
        ),
    ];
    let (base, _) = serve_recording(listing.to_vec());
    let cache = tempfile::tempdir().unwrap();
    let err = update(cache.path(), &github(&base), None).expect_err("no bundle");
    assert!(matches!(err, CacheError::TagNotFound { .. }), "{err:?}");

    let bytes = bundle(A, 1, b"x");
    let mut routes = listing.to_vec();
    routes.push(route(&format!("/dl/{A}/{A}.tar.gz"), 200, bytes));
    let (base, _) = serve_recording(routes);
    let err = update(cache.path(), &github(&base), None).expect_err("no sidecar");
    assert!(
        matches!(err, CacheError::SidecarNotPublished { .. }),
        "{err:?}"
    );
    assert!(!cache.path().join(LOCK_FILE_NAME).exists());
}

/// #9036: finding the newest release costs two API requests however many
/// content tags exist. Before: every page of `/releases` (about 6.4 MB in 4
/// requests); the listing was a function of the total release count.
#[test]
fn update_finds_the_newest_release_in_two_api_requests() {
    let newest = "content-v0.10.0";
    let owned: Vec<String> = (0..60).map(|i| format!("content-v0.1.{i}")).collect();
    let mut tags: Vec<&str> = owned.iter().map(String::as_str).collect();
    tags.extend(["content-v0.9.0", newest]);
    let bytes = bundle(newest, 1, b"x");
    let (base, seen) = serve_recording(vec![
        route(REFS, 200, refs(&tags)),
        route(
            &format!("/releases/tags/{newest}"),
            200,
            release_json(newest, false, false),
        ),
        route(&format!("/dl/{newest}/{newest}.tar.gz"), 200, bytes.clone()),
        route(
            &format!("/dl/{newest}/{newest}.tar.gz.sha256"),
            200,
            sidecar(newest, &bytes),
        ),
    ]);
    let cache = tempfile::tempdir().unwrap();
    let out = update(cache.path(), &github(&base), None).expect("update");
    assert_eq!(out.tag, newest);
    let api: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(path, _)| path.clone())
        .filter(|path| !path.starts_with("/dl/"))
        .collect();
    assert_eq!(
        api,
        vec![REFS.to_owned(), format!("/releases/tags/{newest}")]
    );
}

/// The token header goes on API calls when a token is set and nowhere when
/// it is not; asset downloads never carry it.
#[test]
fn github_source_sends_the_token_only_when_one_is_set() {
    let routes = || {
        vec![
            route(REFS, 200, refs(&[A])),
            route(
                &format!("/releases/tags/{A}"),
                200,
                release_json(A, false, false),
            ),
            route(&format!("/dl/{A}/file"), 200, "bytes"),
        ]
    };
    let drive = |token: Option<&str>| {
        let (base, seen) = serve_recording(routes());
        let src = github(&base).with_token(token.map(str::to_owned));
        src.content_tags().expect("tags");
        src.release(A).expect("release");
        src.asset(A, "file", 64).expect("asset");
        seen.lock().unwrap().clone()
    };
    let auth = |headers: &str| {
        headers
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
            .map(str::to_owned)
    };
    for (path, headers) in drive(Some("ghp_TOKEN123")) {
        let found = auth(&headers).map(|l| l.split_once(':').unwrap().1.trim().to_owned());
        if path.starts_with("/dl/") {
            assert_eq!(found, None, "{path}");
        } else {
            assert_eq!(found.as_deref(), Some("Bearer ghp_TOKEN123"), "{path}");
        }
    }
    let unauthenticated = drive(None);
    assert_eq!(unauthenticated.len(), 3);
    for (path, headers) in unauthenticated {
        assert_eq!(auth(&headers), None, "{path}");
    }
    // An empty or whitespace-only token is unset.
    for blank in ["", "  ", "\n"] {
        for (path, headers) in drive(Some(blank)) {
            assert_eq!(auth(&headers), None, "{blank:?} {path}");
        }
    }
}

/// The token is never in `Debug` of the source, in a `FetchError`, or in a
/// `CacheError` (`Debug` or `Display`), on an HTTP failure or a token that
/// cannot be sent.
#[test]
fn a_token_never_appears_in_debug_or_error_output() {
    let token = "ghp_SECRETVALUE";
    let (base, _) = serve_recording(vec![route(REFS, 403, r#"{"message":"rate limit"}"#)]);
    let src = github(&base).with_token(Some(token.to_owned()));
    assert!(!format!("{src:?}").contains(token), "{src:?}");
    let fetch_err = src.content_tags().expect_err("403");
    let cache = tempfile::tempdir().unwrap();
    let cache_err = update(cache.path(), &src, None).expect_err("403");
    let unsendable = github(&base).with_token(Some(format!("{token}\nx")));
    let header_err = unsendable.content_tags().expect_err("bad header value");
    assert!(
        header_err.reason.contains("not a valid header"),
        "{header_err:?}"
    );
    let text = format!("{fetch_err:?} {cache_err:?} {cache_err} {header_err:?} {unsendable:?}");
    assert!(!text.contains("SECRETVALUE"), "{text}");
}

/// The newest release is chosen by semver: `0.10.0` beats `0.9.0`, which a
/// string comparison ranks higher. The fake lists tags in string order, so
/// first, last and string-max all pick a wrong tag.
#[test]
fn latest_release_compares_versions_not_strings() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    for tag in ["content-v0.1.0", "content-v0.9.0", "content-v0.10.0"] {
        src.publish(tag);
    }
    let out = update(cache.path(), &src, None).expect("update");
    assert_eq!(out.tag, "content-v0.10.0");

    let pinned_old = tempfile::tempdir().unwrap();
    update(pinned_old.path(), &src, Some("content-v0.9.0")).expect("pin 0.9.0");
    let out = update(pinned_old.path(), &src, None).expect("update");
    assert_eq!(out.tag, "content-v0.10.0", "0.10.0 is newer than 0.9.0");
}

#[test]
fn update_with_only_prereleases_published_has_no_release() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    src.publish("content-v0.3.0-rc.1");
    let err = update(cache.path(), &src, None).expect_err("no release");
    assert!(matches!(err, CacheError::NoReleases), "{err:?}");
    assert!(!cache.path().join(LOCK_FILE_NAME).exists());
}

#[test]
fn install_refuses_an_unparseable_sidecar() {
    let (src, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let file = bundle_file(src.path(), A, &bundle(A, 1, b"x"), false);
    std::fs::write(
        src.path().join(format!("{A}.tar.gz.sha256")),
        "not-a-digest  content-v0.1.0.tar.gz\n",
    )
    .unwrap();
    let err = install_from_file(cache.path(), &file).expect_err("bad sidecar");
    assert!(matches!(err, CacheError::Sidecar { .. }), "{err:?}");
    assert!(!cache.path().join(LOCK_FILE_NAME).exists());
}

#[test]
fn install_refuses_a_bundle_that_names_no_tag() {
    let (src, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let bytes = bundle(A, 1, b"x");
    let file = src.path().join("bundle.tar.gz");
    std::fs::write(&file, &bytes).unwrap();
    std::fs::write(
        src.path().join("bundle.tar.gz.sha256"),
        format!("{}  bundle.tar.gz\n", Sha256Digest::of_bytes(&bytes)),
    )
    .unwrap();
    let err = install_from_file(cache.path(), &file).expect_err("no tag");
    assert!(matches!(err, CacheError::UnknownTag { .. }), "{err:?}");
    assert!(!cache.path().join(LOCK_FILE_NAME).exists());
}

#[test]
fn install_refuses_an_oversized_sidecar() {
    let (src, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let bytes = bundle(A, 1, b"x");
    let file = bundle_file(src.path(), A, &bytes, false);
    let mut line = sidecar(A, &bytes);
    line.resize(MAX_SIDECAR_BYTES as usize + 1, b'\n');
    std::fs::write(src.path().join(format!("{A}.tar.gz.sha256")), line).unwrap();
    let err = install_from_file(cache.path(), &file).expect_err("oversized");
    match err {
        CacheError::TooLarge { len, cap, .. } => {
            assert_eq!(cap, MAX_SIDECAR_BYTES);
            assert!(len > cap);
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

/// A broken lock is not cleared by a no-flag update, which re-reads it.
#[test]
fn status_remedy_for_a_broken_lock_names_an_explicit_ref() {
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(cache.path().join(LOCK_FILE_NAME), "not = [toml").unwrap();
    let status = content_status(cache.path(), None);
    assert!(
        matches!(status.installed, Err(ContentError::LockInvalid { .. })),
        "{:?}",
        status.installed
    );
    let text = status.lines().join("\n");
    assert!(text.contains("--content-ref"), "{text}");
    assert!(text.contains(INSTALL_HINT), "{text}");
    assert!(!text.contains("fetch and pin the newest release"), "{text}");
}

/// A too-new schema is not cleared by re-fetching the same release.
#[test]
fn status_remedy_for_a_newer_schema_names_an_upgrade_or_an_older_pin() {
    let cache = tempfile::tempdir().unwrap();
    let bytes = bundle(A, SUPPORTED_SCHEMA_MAJOR + 1, A.as_bytes());
    std::fs::write(cache.path().join(format!("{A}.tar.gz")), &bytes).unwrap();
    ContentLock::new(A, Sha256Digest::of_bytes(&bytes))
        .expect("lock")
        .store(&cache.path().join(LOCK_FILE_NAME))
        .expect("store");
    let status = content_status(cache.path(), None);
    assert!(
        matches!(
            status.installed,
            Err(ContentError::UnsupportedSchema { .. })
        ),
        "{:?}",
        status.installed
    );
    let text = status.lines().join("\n");
    assert!(text.contains("upgrade tm"), "{text}");
    assert!(text.contains("--content-ref"), "{text}");
    assert!(!text.contains("fetch and pin the newest release"), "{text}");
}

#[test]
fn github_source_reports_an_unreachable_host() {
    // Bind then drop, so nothing listens on the port.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("port")
        .port();
    let base = format!("http://127.0.0.1:{port}");
    let src = GithubReleases::with_bases(&base, &base).expect("client");
    let err = src.asset(A, "x", 64).expect_err("unreachable");
    assert!(err.url.starts_with(&base), "{err:?}");
}

/// #9396: a pin written by another writer while a first use waited on the
/// update lock is kept: the first use fetches nothing and writes nothing.
#[test]
fn install_if_missing_keeps_a_lock_written_while_it_waited() {
    let cache = tempfile::tempdir().unwrap();
    let mut src = FakeSource::default();
    src.publish(B);
    let a = bundle(A, 1, A.as_bytes());
    let holder = UpdateGuard::acquire(cache.path()).expect("hold the update lock");
    std::thread::scope(|s| {
        let waiter = s.spawn(|| install_if_missing(cache.path(), &src));
        // Give the waiter time to block on the lock; the outcome does not
        // depend on it, because the check runs under the lock either way.
        std::thread::sleep(Duration::from_millis(200));
        commit(
            cache.path(),
            A,
            &a,
            &Sha256Digest::of_bytes(&a),
            None,
            "the other writer",
        )
        .expect("the other writer pins A");
        drop(holder);
        let out = waiter.join().expect("join").expect("no error");
        assert_eq!(out, None, "the first use must not replace a present pin");
    });
    assert_eq!(pinned(cache.path()).tag(), A, "the present pin wins");
    resolves_to(cache.path(), A);
}

#[path = "first_use_tests.rs"]
mod first_use_tests;

#[path = "retry_tests.rs"]
mod retry_tests;
