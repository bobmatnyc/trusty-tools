//! Per-test-process allowlist fixture, shared by every module whose tests
//! register or restore an index.
//!
//! Why (#767): the gate refuses any root it does not recognise, so a test that
//! registers an index must be able to approve its fixture root — WITHOUT
//! reading or writing the developer's real `~/.config/trusty-search/`. It also
//! has to stay a real gate: a root no test approved is refused exactly as in
//! production, which is what `create_index_refuses_unlisted_root` relies on.
//!
//! What: [`paths`] resolves both members of the union to per-process fixture
//! files under `$HOME/.trusty-search-test-roots/`; the project-registry member
//! points at a path that never exists so a test's verdict cannot depend on
//! which projects the developer has registered with `tm`. [`approve`] adds a
//! root, serialising the read-modify-write behind a lock.
//!
//! Test: used by `service::server::test_support`, `commands::start::tests_4846`,
//! and the `SearchAppState` test-build default.

use std::path::{Path, PathBuf};

use super::{AllowlistConfig, AllowlistEntry, AllowlistPaths};

/// Serialises the read-modify-write on the shared fixture file.
///
/// Why: `cargo test` runs multi-threaded and every caller appends to ONE file,
/// so two concurrent callers would read the same base version and one approval
/// would be lost — surfacing later as a root that mysteriously fails the gate.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Base directory for test fixtures: `$HOME/.trusty-search-test-roots`.
///
/// Why anchored at `$HOME` rather than `$TMPDIR`: the hard denylist refuses
/// `/tmp` and `/var/folders` outright, so a fixture root there would be refused
/// before the allowlist is ever consulted.
pub(crate) fn base_dir() -> PathBuf {
    let home = dirs::home_dir().expect("HOME must be set to run trusty-search tests");
    let base = home.join(".trusty-search-test-roots");
    std::fs::create_dir_all(&base).expect("create ~/.trusty-search-test-roots");
    base
}

/// The allowlist file this test process reads. Scoped by pid so two test
/// binaries running at once cannot interleave writes.
pub(crate) fn allowlist_file() -> PathBuf {
    base_dir().join(format!("allowlist-{}.toml", std::process::id()))
}

/// A project-registry path that never exists, so the project member of the
/// union contributes nothing during tests.
pub(crate) fn project_paths_file() -> PathBuf {
    base_dir().join("no-project-registry.json")
}

/// The [`AllowlistPaths`] a test-built `SearchAppState` uses by default.
pub(crate) fn paths() -> AllowlistPaths {
    AllowlistPaths::default()
        .with_allowlist(allowlist_file())
        .with_project_paths(project_paths_file())
}

/// Approve `path` in this process's fixture allowlist.
///
/// Writes the entry directly rather than through
/// [`super::add_to_allowlist`], which pre-checks the denylist:
/// `create_index_allows_sensitive_path_when_opted_in` legitimately needs an
/// approved root under `/var/folders`. That cannot manufacture a false pass —
/// the gate applies the denylist itself, so a denylisted entry here is still
/// refused.
pub(crate) fn approve(path: &Path) {
    let _guard = WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let file = allowlist_file();
    let mut cfg = AllowlistConfig::load_from(&file).expect("load test allowlist");
    cfg.upsert(AllowlistEntry {
        path: path.to_path_buf(),
        name: None,
        exclude: Vec::new(),
        extensions: Vec::new(),
        skip_kg: false,
    });
    cfg.save_to(&file).expect("write test allowlist");
}

/// A real, unique, RAII-cleaned directory that `SENSITIVE_PATH_PREFIXES`
/// denies no matter what `$TMPDIR` is, plus its canonical path.
///
/// Why (#9527): `tempfile::tempdir()` follows `$TMPDIR`, which is only denied
/// when it happens to sit under a prefix (`/tmp/`, `/var/folders`). Under EVO's
/// forced `TMPDIR=~/evo/tmp/<key>` it is not, so a test asserting the prefix
/// refusal got no refusal. The denylist is a fixed list on purpose; the
/// fixture is what must be explicit.
/// What: creates `/tmp/<prefix>XXXXXX` (a unique name per call) and returns the
/// guard with `fs::canonicalize` of it (`/private/tmp/…` on macOS, also denied).
/// Test: `create_index_still_rejects_sensitive_path_by_default`,
/// `allow_sensitive_path_relaxes_only_the_prefix_denylist`.
pub(crate) fn denylisted_root(prefix: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/tmp")
        .expect("create a directory under /tmp");
    let canonical = std::fs::canonicalize(dir.path()).expect("canonicalize denylisted root");
    assert!(
        super::is_denied(&canonical).is_some(),
        "fixture {} must be denylisted",
        canonical.display()
    );
    (dir, canonical)
}
