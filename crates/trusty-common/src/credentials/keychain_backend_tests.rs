//! Unit tests for [`super::KeychainBackend`] (issue #7521).
//!
//! Why: isolated in a sibling file (declared via `#[path =
//! "keychain_backend_tests.rs"]` in `keychain_backend.rs`) so the production
//! module stays well inside the 500-SLOC cap.
//! What: every test but the `#[ignore]` round-trip runs against an injected
//! [`MemoryKeyStore`] and a temp-dir index — **no test here touches a real OS
//! keychain**, matching `keyring_store.rs`'s own acceptance criterion.
//! Test: itself.

use std::sync::Arc;

use tempfile::TempDir;

use super::*;
use crate::credentials::MemoryKeyStore;

const GROUP: &str = "bobmatnyc/trusty-tools";
/// Stand-in value; asserted absent from every rendered/Debug surface.
const FAKE_VALUE: &str = "fake-value-8f2c1d";

/// A backend bound to a fresh temp index and an inspectable memory store.
fn fixture() -> (TempDir, Arc<MemoryKeyStore>, KeychainBackend) {
    let tmp = TempDir::new().unwrap();
    let store = Arc::new(MemoryKeyStore::new());
    let backend = KeychainBackend::with_store(
        GROUP,
        Arc::clone(&store) as Arc<dyn KeyStore>,
        index_path_at(tmp.path(), GROUP),
    )
    .unwrap();
    (tmp, store, backend)
}

/// Why: DOC-74 §6.3 fixes the naming — service `trusty/<owner>/<repo>`,
/// account the bare KEY. Two backends must match it exactly or slice 2's
/// vaults cannot find slice 1's entries.
/// Test: itself.
#[test]
fn keychain_backend_namespaces_by_group_and_key() {
    let (_tmp, store, backend) = fixture();
    assert_eq!(
        keychain_service_name(GROUP),
        "trusty/bobmatnyc/trusty-tools"
    );
    assert_eq!(backend.service(), "trusty/bobmatnyc/trusty-tools");
    assert_eq!(backend.group(), GROUP);

    backend.set("API_KEY", FAKE_VALUE).unwrap();
    // The account is the BARE key — the group lives in the service name, not
    // in the account string.
    assert_eq!(store.list(), vec!["API_KEY".to_string()]);
    assert_eq!(store.get("API_KEY").as_deref(), Some(FAKE_VALUE));
    assert_eq!(backend.get("API_KEY").unwrap().as_deref(), Some(FAKE_VALUE));
}

/// Why: `list` must enumerate from the index, never from the backing store
/// (which cannot enumerate) and never by reading values back.
/// Test: itself.
#[test]
fn keychain_backend_list_reads_index_names_only() {
    let (_tmp, _store, backend) = fixture();
    backend.set("B_KEY", FAKE_VALUE).unwrap();
    backend.set("A_KEY", FAKE_VALUE).unwrap();

    assert_eq!(
        backend.list().unwrap(),
        vec!["A_KEY".to_string(), "B_KEY".to_string()]
    );

    let raw = std::fs::read_to_string(backend.index_path()).unwrap();
    assert!(raw.contains("A_KEY"), "index must hold names: {raw}");
    assert!(
        !raw.contains(FAKE_VALUE),
        "index must never hold a value: {raw}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(backend.index_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "expected 0600 index, got {mode:o}");
    }
}

/// Why: a removal that dropped only one of the two records would either
/// orphan a keychain entry or make `list` report a key that is gone.
/// Test: itself.
#[test]
fn keychain_backend_remove_deletes_entry_and_index_row() {
    let (_tmp, store, backend) = fixture();
    backend.set("KEEP", FAKE_VALUE).unwrap();
    backend.set("DROP", FAKE_VALUE).unwrap();

    backend.remove("DROP").unwrap();

    assert_eq!(backend.list().unwrap(), vec!["KEEP".to_string()]);
    assert_eq!(store.list(), vec!["KEEP".to_string()]);
    assert!(store.get("DROP").is_none());
    // Removing an absent key is not an error (KeyStore::unset's contract).
    backend.remove("DROP").unwrap();
}

/// Why: QA regression class from PR #2427 — `{:?}` of any credential-holding
/// type must never render a value. Here the property is structural (no field
/// can hold one), and this test pins it against a vault that HAS a value.
/// Test: itself.
#[test]
fn keychain_backend_debug_never_contains_a_value() {
    let (_tmp, _store, backend) = fixture();
    backend.set("API_KEY", FAKE_VALUE).unwrap();
    let rendered = format!("{backend:?}");
    assert!(
        !rendered.contains(FAKE_VALUE),
        "Debug output leaked a value: {rendered}"
    );
    assert!(rendered.contains("trusty/bobmatnyc/trusty-tools"));
}

/// Every `.tmp` scratch file left in the index's directory.
fn leftover_scratch_files(index: &std::path::Path) -> Vec<String> {
    let dir = index.parent().expect("the index always has a parent");
    std::fs::read_dir(dir)
        .expect("the index directory exists after a write")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect()
}

/// Why: a crash mid-write would leave truncated JSON at the final path, and
/// `read_index` (correctly) refuses that forever — a vault that lists nothing
/// and refuses every add. The write must therefore be PUBLISHED by rename, not
/// copied into the live file, and a failed write must leave the previous index
/// intact.
///
/// The inode assertion is what makes this a rename test rather than a
/// "temp file plus copy" test: `rename(2)` moves a new inode onto the name, so
/// the published index's inode changes; any copy-into-place — the shape that
/// CAN be observed half-written — reuses the destination inode and fails here.
/// Test: itself.
#[test]
fn keychain_backend_index_write_publishes_by_rename() {
    let (_tmp, _store, backend) = fixture();
    backend.set("FIRST", FAKE_VALUE).unwrap();
    assert_eq!(backend.list().unwrap(), vec!["FIRST".to_string()]);
    assert!(
        leftover_scratch_files(backend.index_path()).is_empty(),
        "no scratch file may survive a successful write: {:?}",
        leftover_scratch_files(backend.index_path())
    );

    #[cfg(unix)]
    let first_inode = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(backend.index_path()).unwrap().ino()
    };

    backend.set("SECOND", FAKE_VALUE).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let second_inode = std::fs::metadata(backend.index_path()).unwrap().ino();
        assert_ne!(
            first_inode, second_inode,
            "the index must be published by rename (a fresh inode), not written \
             or copied over the live file"
        );
    }

    let before = std::fs::read_to_string(backend.index_path()).unwrap();

    // Make the scratch write fail without naming it: creating a file needs
    // WRITE permission on the directory, so a 0500 directory refuses the
    // scratch create while leaving the already-created lock sidecar openable.
    let dir = backend.index_path().parent().unwrap().to_path_buf();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(
            backend.set("THIRD", FAKE_VALUE).is_err(),
            "a failed scratch write must surface, never be swallowed"
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    assert_eq!(
        std::fs::read_to_string(backend.index_path()).unwrap(),
        before,
        "a failed write must leave the previous index byte-identical"
    );
    assert_eq!(
        backend.list().unwrap(),
        vec!["FIRST".to_string(), "SECOND".to_string()]
    );
    assert!(
        leftover_scratch_files(backend.index_path()).is_empty(),
        "a failed write may not leave a scratch file behind: {:?}",
        leftover_scratch_files(backend.index_path())
    );
}

/// Why: `set`/`remove` are read-modify-write cycles over one shared file, run
/// by whatever `tm secrets` processes the operator happens to start at once.
/// Without cross-process serialisation two writers interleave
/// read/read/write/write and one name is lost — with its keychain entry still
/// present, so `list` under-reports what is stored — or they publish a
/// half-written index that `read_index` then refuses forever.
///
/// Each writer gets its OWN [`KeychainBackend`] and its own store, so nothing
/// is shared in-process but the index file itself: an in-struct `Mutex` would
/// not make this pass, and neither does a sequential run of the same calls.
/// Test: itself.
#[test]
fn keychain_backend_concurrent_writers_never_lose_a_name() {
    const WRITERS: usize = 8;
    const ROUNDS: usize = 6;

    let tmp = TempDir::new().unwrap();
    let index = index_path_at(tmp.path(), GROUP);
    let open = |path: std::path::PathBuf| {
        KeychainBackend::with_store(
            GROUP,
            Arc::new(MemoryKeyStore::new()) as Arc<dyn KeyStore>,
            path,
        )
        .unwrap()
    };

    for round in 0..ROUNDS {
        std::thread::scope(|scope| {
            for writer in 0..WRITERS {
                let path = index.clone();
                scope.spawn(move || {
                    open(path)
                        .set(&format!("KEY_{round}_{writer}"), FAKE_VALUE)
                        .expect("a concurrent add must not fail");
                });
            }
        });
    }

    let names = open(index.clone())
        .list()
        .expect("the index must still parse after concurrent writers");
    let mut expected: Vec<String> = (0..ROUNDS)
        .flat_map(|round| (0..WRITERS).map(move |writer| format!("KEY_{round}_{writer}")))
        .collect();
    expected.sort();
    assert_eq!(
        names, expected,
        "every concurrently added name must survive; {} of {} present",
        names.len(),
        expected.len()
    );
}

/// Why: the group is a path segment under `~/.trusty-tools/...`; a `..`
/// segment would place the index outside it.
/// Test: itself.
#[test]
fn keychain_backend_rejects_a_traversing_group() {
    assert!(validate_group("../../etc").is_err());
    assert!(validate_group("").is_err());
    assert!(validate_group("owner//repo").is_err());
    assert!(validate_group("owner/repo").is_ok());

    let tmp = TempDir::new().unwrap();
    let err = KeychainBackend::with_store(
        "../escape",
        Arc::new(MemoryKeyStore::new()) as Arc<dyn KeyStore>,
        tmp.path().join("x.json"),
    );
    assert!(err.is_err(), "a traversing group must be refused");
}

/// Why: a blank or whitespace-bearing key is unaddressable in both the
/// keychain and the index.
/// Test: itself.
#[test]
fn keychain_backend_rejects_a_blank_key() {
    let (_tmp, store, backend) = fixture();
    assert!(backend.set("", FAKE_VALUE).is_err());
    assert!(backend.set("has space", FAKE_VALUE).is_err());
    assert!(backend.set("has/slash", FAKE_VALUE).is_err());
    assert!(
        store.list().is_empty(),
        "a refused key must never reach the store"
    );
}

/// Why: every other test here runs against a double, so exactly one test
/// proves the composition works against the real macOS Keychain. Ignored by
/// default — run with `--include-ignored` on a machine with a keychain.
/// What: add → list → get → remove in a throwaway group, cleaning up both the
/// entry and the index file it wrote.
/// Test: itself (`cargo test -p trusty-common --features keyring-store --
/// --include-ignored keychain_backend_real_roundtrip_add_list_remove`).
#[test]
#[ignore = "touches the real OS keychain; run explicitly with --include-ignored"]
fn keychain_backend_real_roundtrip_add_list_remove() {
    if !KeychainBackend::keychain_reachable() {
        eprintln!("keychain unreachable on this host; nothing to prove");
        return;
    }
    let group = "trusty-tools-test/ignored-roundtrip";
    let backend = KeychainBackend::new(group).unwrap();
    backend.set("TM_SECRETS_ROUNDTRIP", FAKE_VALUE).unwrap();

    assert_eq!(
        backend.list().unwrap(),
        vec!["TM_SECRETS_ROUNDTRIP".to_string()]
    );
    assert_eq!(
        backend.get("TM_SECRETS_ROUNDTRIP").unwrap().as_deref(),
        Some(FAKE_VALUE)
    );

    backend.remove("TM_SECRETS_ROUNDTRIP").unwrap();
    assert!(backend.list().unwrap().is_empty());
    assert!(backend.get("TM_SECRETS_ROUNDTRIP").unwrap().is_none());
    let _ = std::fs::remove_file(backend.index_path());
}
