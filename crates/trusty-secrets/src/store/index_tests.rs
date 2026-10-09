//! Unit tests for [`super::NamesIndex`] and the platform helpers it uses.
//! Every test runs in a temp directory.
//!
//! Test: itself.

use std::path::Path;
use std::time::Duration;

use tempfile::TempDir;

use super::platform;
use super::*;
use crate::api::methods::SetOutcome;
use crate::api::{SecretKey, SecretsError, VaultName};

fn vault() -> VaultName {
    VaultName::new("trusty/acme/web").unwrap()
}

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).unwrap()
}

fn fixture() -> (TempDir, NamesIndex) {
    let tmp = TempDir::new().unwrap();
    let index = NamesIndex::at(tmp.path().join("index"));
    (tmp, index)
}

/// Every `.tmp` scratch file left beside `path`.
fn scratch_files(path: &Path) -> Vec<String> {
    std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect()
}

/// Why: the index lists which secrets exist; other local users must not read
/// it. File 0600, directory 0700.
/// Test: itself.
#[cfg(unix)]
#[test]
fn index_files_are_0600_in_a_0700_directory() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, index) = fixture();
    index.upsert(&vault(), &key("API_KEY"), 12, 1).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&index.path_for(&vault())), 0o600);
    assert_eq!(mode(index.root()), 0o700);
    assert_eq!(mode(&platform::lock_path(&index.path_for(&vault()))), 0o600);

    // A pre-existing, group-readable root is narrowed on the next write.
    std::fs::set_permissions(index.root(), std::fs::Permissions::from_mode(0o755)).unwrap();
    index.upsert(&vault(), &key("OTHER"), 3, 2).unwrap();
    assert_eq!(mode(index.root()), 0o700);
}

/// Why: two vaults must never share a file, and the file name must stay one
/// directory deep.
/// Test: itself.
#[test]
fn index_file_names_are_flat_and_distinct() {
    let (_tmp, index) = fixture();
    let owner = VaultName::new("trusty/acme").unwrap();
    let project = index.path_for(&vault());
    assert_eq!(project.parent().unwrap(), index.root());
    assert_eq!(
        project.file_name().unwrap().to_str().unwrap(),
        "trusty%2Facme%2Fweb.json"
    );
    assert_ne!(index.path_for(&owner), project);
}

/// Why: the index is the listable record and must hold names and metadata
/// only. The file has no field that can hold a value.
/// Test: itself.
#[test]
fn index_rows_round_trip_without_plaintext() {
    let (_tmp, index) = fixture();
    assert!(
        index.list(&vault()).unwrap().is_empty(),
        "missing file is empty"
    );
    assert_eq!(
        index.upsert(&vault(), &key("B_KEY"), 40, 100).unwrap(),
        SetOutcome::New
    );
    assert_eq!(
        index.upsert(&vault(), &key("A_KEY"), 8, 200).unwrap(),
        SetOutcome::New
    );
    let rows = index.list(&vault()).unwrap();
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["A_KEY", "B_KEY"]);
    assert_eq!((rows[1].length, rows[1].updated_at), (40, 100));
    assert!(!rows[1].agents_may_use, "the flag defaults OFF");
    assert_eq!(
        index.get(&vault(), &key("A_KEY")).unwrap().unwrap().length,
        8
    );
    assert!(index.get(&vault(), &key("NONE")).unwrap().is_none());

    let raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(index.path_for(&vault())).unwrap()).unwrap();
    let row = &raw["keys"]["B_KEY"];
    let mut fields: Vec<&str> = row
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    // #9070: the agents flag is the backend's; the index never writes it.
    assert_eq!(fields, ["length", "updated_at"]);
}

/// Why: #9070 — the index is a file any same-uid process can edit, so it
/// never holds the agents flag. A pre-#9070 file carrying
/// `"agents_may_use": true` still loads, reads OFF, and loses the field on
/// the next write; the deprecated index setter cannot turn it ON.
/// Test: itself.
#[test]
#[allow(deprecated)]
fn index_never_holds_the_agents_flag() {
    let (_tmp, index) = fixture();
    index.upsert(&vault(), &key("API_KEY"), 10, 1).unwrap();
    let path = index.path_for(&vault());
    let legacy = r#"{"version":1,"vault":"trusty/acme/web","keys":{"API_KEY":{"length":10,"updated_at":1,"agents_may_use":true}}}"#;
    std::fs::write(&path, legacy).unwrap();

    let row = index.get(&vault(), &key("API_KEY")).unwrap().unwrap();
    assert!(!row.agents_may_use, "a legacy ON flag reads OFF");
    assert!(!index.list(&vault()).unwrap()[0].agents_may_use);

    let err = index
        .set_agents_may_use(&vault(), &key("API_KEY"), true)
        .unwrap_err();
    assert!(matches!(err, SecretsError::Unsupported { .. }), "{err:?}");
    index
        .set_agents_may_use(&vault(), &key("API_KEY"), false)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        legacy,
        "never written"
    );
    let err = index
        .set_agents_may_use(&vault(), &key("MISSING"), false)
        .unwrap_err();
    assert!(matches!(err, SecretsError::NotFound { .. }), "{err:?}");

    index.upsert(&vault(), &key("API_KEY"), 20, 2).unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        !raw.contains("agents_may_use"),
        "the next write drops it: {raw}"
    );
}

/// Why: fail closed — a corrupt index must be an error on read AND on write,
/// and must never be replaced by an empty one. Downgrading `read_file`'s error
/// arm to "empty index" fails every case here.
/// Test: itself.
#[test]
fn index_corrupt_file_fails_closed_and_is_never_reset() {
    let cases = [
        ("garbage", "{ not json".to_string()),
        (
            "version",
            r#"{"version":2,"vault":"trusty/acme/web","keys":{}}"#.to_string(),
        ),
        (
            "vault",
            r#"{"version":1,"vault":"trusty/other/x","keys":{}}"#.to_string(),
        ),
        (
            "value field",
            r#"{"version":1,"vault":"trusty/acme/web","keys":{"K":{"length":1,"updated_at":1,"value":"x"}}}"#
                .to_string(),
        ),
        (
            "bad key",
            r#"{"version":1,"vault":"trusty/acme/web","keys":{"a b":{"length":1,"updated_at":1}}}"#
                .to_string(),
        ),
    ];
    for (label, body) in cases {
        let (_tmp, index) = fixture();
        std::fs::create_dir_all(index.root()).unwrap();
        let path = index.path_for(&vault());
        std::fs::write(&path, &body).unwrap();

        let err = index.list(&vault()).unwrap_err();
        assert!(
            matches!(err, SecretsError::IndexCorrupt { .. }),
            "{label}: {err:?}"
        );
        let err = index.upsert(&vault(), &key("NEW"), 1, 1).unwrap_err();
        assert!(
            matches!(err, SecretsError::IndexCorrupt { .. }),
            "{label}: {err:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            body,
            "{label}: a corrupt index must be left byte-identical"
        );
    }
}

/// Why: a crash mid-write must not leave truncated JSON at the live path.
/// The inode changes only when a new file is renamed into place; a failed
/// write leaves the previous index byte-identical and no scratch file behind.
/// Test: itself.
#[test]
fn index_write_publishes_by_rename() {
    let (_tmp, index) = fixture();
    let path = index.path_for(&vault());
    index.upsert(&vault(), &key("FIRST"), 1, 1).unwrap();
    assert!(scratch_files(&path).is_empty());

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let first = std::fs::metadata(&path).unwrap().ino();
        index.upsert(&vault(), &key("SECOND"), 1, 2).unwrap();
        assert_ne!(first, std::fs::metadata(&path).unwrap().ino());

        let before = std::fs::read_to_string(&path).unwrap();
        // 0500: the scratch create fails; the lock sidecar already exists.
        std::fs::set_permissions(index.root(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = index.upsert(&vault(), &key("THIRD"), 1, 3);
        std::fs::set_permissions(index.root(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            matches!(result, Err(SecretsError::Io { .. })),
            "a failed scratch write must surface: {result:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert!(scratch_files(&path).is_empty());
    }
}

/// Why: separate processes write one index; without the lock, interleaved
/// read/read/write/write loses a name. Each writer here has its own
/// `NamesIndex`, so only the file lock can serialise them.
/// Test: itself.
#[test]
fn index_concurrent_writers_never_lose_a_name() {
    const WRITERS: usize = 8;
    const ROUNDS: usize = 6;
    let (_tmp, index) = fixture();
    for round in 0..ROUNDS {
        std::thread::scope(|scope| {
            for writer in 0..WRITERS {
                let mine = NamesIndex::at(index.root());
                scope.spawn(move || {
                    mine.upsert(&vault(), &key(&format!("K_{round}_{writer}")), 1, 1)
                        .expect("a concurrent upsert must not fail");
                });
            }
        });
    }
    let mut expected: Vec<String> = (0..ROUNDS)
        .flat_map(|r| (0..WRITERS).map(move |w| format!("K_{r}_{w}")))
        .collect();
    expected.sort();
    let names: Vec<String> = index
        .list(&vault())
        .unwrap()
        .into_iter()
        .map(|m| m.name.to_string())
        .collect();
    assert_eq!(names, expected);
}

/// Why: a wedged holder must produce an error naming the lock, never a write
/// that skipped it.
/// Test: itself.
#[test]
fn index_lock_timeout_fails_closed() {
    let (_tmp, index) = fixture();
    index.upsert(&vault(), &key("FIRST"), 1, 1).unwrap();
    let path = index.path_for(&vault());
    let before = std::fs::read_to_string(&path).unwrap();

    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(platform::lock_path(&path))
        .unwrap();
    let mut rw = fd_lock::RwLock::new(held);
    let _guard = rw.try_write().unwrap();

    let short = index.clone().with_lock_timeout(Duration::from_millis(60));
    let err = short.upsert(&vault(), &key("SECOND"), 1, 2).unwrap_err();
    assert!(matches!(err, SecretsError::LockTimeout { .. }), "{err:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}
