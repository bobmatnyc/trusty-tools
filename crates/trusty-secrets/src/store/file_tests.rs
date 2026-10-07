//! Tests for [`super::FileBackend`] (#9326): modes, owners, symlinks, the
//! key encoding, and that no value reaches the index, an error or `Debug`.
//! Every path is under a `TempDir`; nothing touches `~/.trusty-tools`.
//!
//! Test: itself.

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;

use super::*;
use crate::store::{NamesIndex, SecretStore};

const VALUE: &str = "sk-fake-file-9326-0123456789abcdef";

fn vault() -> VaultName {
    VaultName::new("trusty/acme/web").unwrap()
}

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).unwrap()
}

struct Fx {
    tmp: TempDir,
    backend: FileBackend,
}

impl Fx {
    fn root(&self) -> PathBuf {
        self.backend.root().to_path_buf()
    }

    fn vault_dir(&self) -> PathBuf {
        self.root().join(vault().file_stem())
    }

    fn value_path(&self, name: &str) -> PathBuf {
        self.vault_dir().join(file_name(&key(name)).unwrap())
    }
}

fn fx() -> Fx {
    let tmp = TempDir::new().unwrap();
    let backend = FileBackend::at(tmp.path().join("values"));
    Fx { tmp, backend }
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Assert `err` is a refusal naming `path`, and that it carries no value.
fn assert_refused(err: SecretsError, path: &Path) {
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains(VALUE), "{shown}");
    match err {
        SecretsError::StorageRefused { path: refused, .. } => assert_eq!(refused, path),
        other => panic!("expected StorageRefused for {}: {other:?}", path.display()),
    }
}

/// Why: one pure rule judges every path; the owner arm cannot be exercised
/// on disk without a second account.
/// Test: itself.
#[test]
fn file_verdict_table() {
    let me = 501;
    let file = |mode, owner| Observed {
        symlink: false,
        dir: false,
        file: true,
        mode,
        owner,
    };
    let dir = |mode, owner| Observed {
        symlink: false,
        dir: true,
        file: false,
        mode,
        owner,
    };
    let link = Observed {
        symlink: true,
        dir: false,
        file: false,
        mode: 0o777,
        owner: me,
    };
    let cases = [
        (file(0o100600, me), Kind::File, None),
        (file(0o100400, me), Kind::File, None),
        (
            file(0o100640, me),
            Kind::File,
            Some("grants permissions beyond 0600"),
        ),
        (
            file(0o100604, me),
            Kind::File,
            Some("grants permissions beyond 0600"),
        ),
        (
            file(0o100700, me),
            Kind::File,
            Some("grants permissions beyond 0600"),
        ),
        (
            file(0o104600, me),
            Kind::File,
            Some("grants permissions beyond 0600"),
        ),
        (
            file(0o100600, 0),
            Kind::File,
            Some("is not owned by the current user"),
        ),
        (dir(0o040700, me), Kind::File, Some("is not a regular file")),
        (link, Kind::File, Some("is a symbolic link")),
        (dir(0o040700, me), Kind::Dir, None),
        (
            dir(0o040750, me),
            Kind::Dir,
            Some("grants permissions beyond 0700"),
        ),
        (
            dir(0o041700, me),
            Kind::Dir,
            Some("grants permissions beyond 0700"),
        ),
        (
            dir(0o040700, 502),
            Kind::Dir,
            Some("is not owned by the current user"),
        ),
        (file(0o100600, me), Kind::Dir, Some("is not a directory")),
        (link, Kind::Dir, Some("is a symbolic link")),
    ];
    for (observed, kind, want) in cases {
        assert_eq!(
            verdict(observed, kind, me),
            want,
            "{observed:?} as {kind:?}"
        );
    }
}

/// Why: #9326 AC1 — set, get, overwrite, list and delete work on files, and
/// delete removes the file.
/// Test: itself.
#[test]
fn file_backend_round_trips_and_lists_names() {
    let fx = fx();
    let b = &fx.backend;
    assert!(b.get(&vault(), &key("A")).unwrap().is_none());
    assert!(b.list_names(&vault()).unwrap().is_empty());
    assert!(!b.delete(&vault(), &key("A")).unwrap());

    b.set(&vault(), &key("A"), &SecretValue::new("first"))
        .unwrap();
    b.set(&vault(), &key("A"), &SecretValue::new(VALUE))
        .unwrap();
    b.set(&vault(), &key("b.c-d"), &SecretValue::new("x"))
        .unwrap();
    assert_eq!(b.get(&vault(), &key("A")).unwrap().unwrap().expose(), VALUE);
    assert_eq!(b.list_names(&vault()).unwrap(), [key("A"), key("b.c-d")]);

    assert!(b.delete(&vault(), &key("A")).unwrap());
    assert!(!fx.value_path("A").exists(), "delete removes the file");
    assert!(b.get(&vault(), &key("A")).unwrap().is_none());
    assert_eq!(b.list_names(&vault()).unwrap(), [key("b.c-d")]);
    assert_eq!(b.id(), BackendId::file());
}

/// Why: #9326 AC2 — the root and vault directories are 0700 and each value
/// file 0600, set at creation; no temp file is left behind.
/// Test: itself.
#[test]
fn file_backend_creates_0700_dirs_and_0600_files() {
    let fx = fx();
    fx.backend
        .set(&vault(), &key("A"), &SecretValue::new(VALUE))
        .unwrap();
    assert_eq!(mode(&fx.root()), 0o700);
    assert_eq!(mode(&fx.vault_dir()), 0o700);
    assert_eq!(mode(&fx.value_path("A")), 0o600);
    let entries: Vec<_> = std::fs::read_dir(fx.vault_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries, [std::ffi::OsString::from("%a")]);
}

/// Why: #9326 AC2 — a file or directory wider than 0600/0700 is refused on
/// every operation with an error naming the path; nothing is read. A
/// narrower file (0400) is fine.
/// Red when the mode arm of `verdict` is removed.
/// Test: itself.
#[test]
fn file_backend_refuses_wrong_modes() {
    let fx = fx();
    let b = &fx.backend;
    b.set(&vault(), &key("A"), &SecretValue::new(VALUE))
        .unwrap();
    let file = fx.value_path("A");

    chmod(&file, 0o640);
    assert_refused(b.get(&vault(), &key("A")).unwrap_err(), &file);
    assert_refused(b.list_names(&vault()).unwrap_err(), &file);
    assert_refused(b.delete(&vault(), &key("A")).unwrap_err(), &file);
    assert!(file.exists(), "a refused delete leaves the file");
    chmod(&file, 0o400);
    assert_eq!(b.get(&vault(), &key("A")).unwrap().unwrap().expose(), VALUE);
    chmod(&file, 0o600);

    for dir in [fx.vault_dir(), fx.root()] {
        chmod(&dir, 0o750);
        assert_refused(b.get(&vault(), &key("A")).unwrap_err(), &dir);
        assert_refused(b.list_names(&vault()).unwrap_err(), &dir);
        assert_refused(b.delete(&vault(), &key("A")).unwrap_err(), &dir);
        let err = b
            .set(&vault(), &key("A"), &SecretValue::new("new"))
            .unwrap_err();
        assert_refused(err, &dir);
        chmod(&dir, 0o700);
    }
    assert_eq!(b.get(&vault(), &key("A")).unwrap().unwrap().expose(), VALUE);
}

/// Why: #9326 — a symlink at the value path, the vault directory or the
/// root is refused, never followed.
/// Test: itself.
#[test]
fn file_backend_refuses_symlinks() {
    let fx = fx();
    let b = &fx.backend;
    b.set(&vault(), &key("A"), &SecretValue::new(VALUE))
        .unwrap();
    let elsewhere = fx.tmp.path().join("elsewhere");
    std::fs::write(&elsewhere, "planted").unwrap();
    chmod(&elsewhere, 0o600);
    let link = fx.value_path("B");
    symlink(&elsewhere, &link).unwrap();
    assert_refused(b.get(&vault(), &key("B")).unwrap_err(), &link);
    assert_refused(b.delete(&vault(), &key("B")).unwrap_err(), &link);
    assert_refused(b.list_names(&vault()).unwrap_err(), &link);
    std::fs::remove_file(&link).unwrap();

    let real = fx.tmp.path().join("real");
    std::fs::rename(fx.vault_dir(), &real).unwrap();
    symlink(&real, fx.vault_dir()).unwrap();
    assert_refused(b.get(&vault(), &key("A")).unwrap_err(), &fx.vault_dir());
    let err = b
        .set(&vault(), &key("A"), &SecretValue::new("x"))
        .unwrap_err();
    assert_refused(err, &fx.vault_dir());
    std::fs::remove_file(fx.vault_dir()).unwrap();
    std::fs::rename(&real, fx.vault_dir()).unwrap();

    let real_root = fx.tmp.path().join("real-root");
    std::fs::rename(fx.root(), &real_root).unwrap();
    symlink(&real_root, fx.root()).unwrap();
    assert_refused(b.get(&vault(), &key("A")).unwrap_err(), &fx.root());
    let err = b
        .set(&vault(), &key("A"), &SecretValue::new("x"))
        .unwrap_err();
    assert_refused(err, &fx.root());
}

/// Why: macOS volumes are case-insensitive by default, so `API` and `api`
/// must not share a file; a key too long for a file name is refused.
/// Test: itself.
#[test]
fn file_backend_keeps_keys_differing_only_in_case_apart() {
    let fx = fx();
    let b = &fx.backend;
    b.set(&vault(), &key("API"), &SecretValue::new("upper"))
        .unwrap();
    b.set(&vault(), &key("api"), &SecretValue::new("lower"))
        .unwrap();
    assert_eq!(
        b.get(&vault(), &key("API")).unwrap().unwrap().expose(),
        "upper"
    );
    assert_eq!(
        b.get(&vault(), &key("api")).unwrap().unwrap().expose(),
        "lower"
    );
    assert_eq!(b.list_names(&vault()).unwrap(), [key("API"), key("api")]);

    for name in ["A", "a_B.c-D9", "_x", "Zz"] {
        let encoded = file_name(&key(name)).unwrap();
        assert_eq!(key_from_file_name(&encoded), Some(key(name)), "{encoded}");
    }
    for bad in ["A", "%", "%A", "a%", "%%a", ".tmp.1.2.3", "a/b"] {
        assert_eq!(key_from_file_name(bad), None, "{bad}");
    }
    let long = key(&"K".repeat(200));
    let err = b.set(&vault(), &long, &SecretValue::new("x")).unwrap_err();
    assert!(matches!(err, SecretsError::InvalidKey { .. }), "{err:?}");
}

/// Why: #9326 AC5 — no refusal, decode failure or `Debug` output carries
/// the value, even when the file holding it is the one refused.
/// Red when the UTF-8 error quotes the stored bytes.
/// Test: itself.
#[test]
fn file_errors_and_debug_never_carry_the_value() {
    let fx = fx();
    let b = &fx.backend;
    b.set(&vault(), &key("A"), &SecretValue::new(VALUE))
        .unwrap();
    let file = fx.value_path("A");

    let mut bytes = VALUE.as_bytes().to_vec();
    bytes.push(0xff);
    std::fs::write(&file, &bytes).unwrap();
    let err = b.get(&vault(), &key("A")).unwrap_err();
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains(VALUE), "{shown}");
    assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");

    chmod(&file, 0o644);
    assert_refused(b.get(&vault(), &key("A")).unwrap_err(), &file);

    let store = SecretStore::new(Arc::new(b.clone()), NamesIndex::at(fx.tmp.path().join("i")));
    let shown = format!("{b:?} {store:?}");
    assert!(!shown.contains(VALUE), "{shown}");
}

/// Every byte under `dir`, recursively, as lossy text.
fn all_text_under(dir: &Path) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            text.push_str(&all_text_under(&path));
        } else {
            text.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
        }
    }
    text
}

/// Why: #9326 AC1 — through [`SecretStore`], the value lands in the value
/// file only; the names-only index holds the name and length, never the
/// value.
/// Red when an index row records the value.
/// Test: itself.
#[test]
fn file_store_never_writes_a_value_to_the_index() {
    let fx = fx();
    let index_root = fx.tmp.path().join("index");
    let store = SecretStore::new(Arc::new(fx.backend.clone()), NamesIndex::at(&index_root));
    store
        .set(&vault(), &key("A"), &SecretValue::new(VALUE))
        .unwrap();

    let rows = store.list(&vault()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].length, VALUE.chars().count());
    let index_text = all_text_under(&index_root);
    assert!(index_text.contains("\"A\""), "{index_text}");
    assert!(!index_text.contains(VALUE), "{index_text}");
    assert!(all_text_under(&fx.root()).contains(VALUE));

    assert!(store.delete(&vault(), &key("A")).unwrap().removed);
    assert!(store.list(&vault()).unwrap().is_empty());
    assert!(!fx.value_path("A").exists());
}

/// A temp file named for writer `pid`, holding `VALUE`, at 0600.
fn plant_temp(fx: &Fx, pid: &str) -> PathBuf {
    let path = fx.vault_dir().join(format!(".tmp.{pid}.1.0"));
    std::fs::write(&path, VALUE).unwrap();
    chmod(&path, 0o600);
    path
}

/// Why: #9326 — a crash between `create_new` and `rename` leaves a plaintext
/// temp file; `delete` must not report the key gone while that copy stays.
/// A temp file whose writer is this process (alive) is kept.
/// Red when `delete` skips the orphan sweep.
/// Test: itself.
#[test]
fn file_delete_removes_an_orphaned_temp_file() {
    let fx = fx();
    let b = &fx.backend;
    b.set(&vault(), &key("A"), &SecretValue::new(VALUE))
        .unwrap();
    // 2_000_000_000 exceeds every pid_max, so no process holds it.
    let orphan = plant_temp(&fx, "2000000000");
    let unparsed = plant_temp(&fx, "x");
    let live = plant_temp(&fx, &std::process::id().to_string());

    assert!(b.delete(&vault(), &key("A")).unwrap());
    assert!(!orphan.exists() && !unparsed.exists());
    assert!(live.exists(), "a live writer's temp file is kept");
    std::fs::remove_file(&live).unwrap();
    let left = all_text_under(&fx.vault_dir());
    assert!(!left.contains(VALUE), "{left}");

    // set and list sweep too; an orphan that fails its checks is an error.
    let orphan = plant_temp(&fx, "2000000000");
    b.set(&vault(), &key("B"), &SecretValue::new("x")).unwrap();
    assert!(!orphan.exists());
    let orphan = plant_temp(&fx, "2000000000");
    assert_eq!(b.list_names(&vault()).unwrap(), [key("B")]);
    assert!(!orphan.exists());
    let wide = plant_temp(&fx, "2000000000");
    chmod(&wide, 0o644);
    assert_refused(b.delete(&vault(), &key("B")).unwrap_err(), &wide);
    assert!(wide.exists(), "a refused orphan is not unlinked");
}
