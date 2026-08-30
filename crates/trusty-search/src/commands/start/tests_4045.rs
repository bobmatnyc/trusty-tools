//! Registry-level single-registration-per-corpus coverage, driven through the
//! warm-boot discovery scan (#4045, guarding #2305 / #2336 / #3929).
//!
//! Why: #2305 and #2336 attached the "one redb file, one index id" guard to the
//! two runtime mutation entry points, `create_index` and `relocate_index`.
//! #3929 then re-created the same double-registration through the warm-boot
//! colocated discovery scan, which those two guards do not sit on: 188 of 222
//! indexes failed to open their corpus with `DatabaseAlreadyOpen` on a single
//! restart. The invariant was right and its enforcement was in the wrong place,
//! so every existing test kept passing. #4045 asks for the invariant to be
//! pinned where a NEW path cannot dodge it — on the registry that boot
//! populates, rather than on the helper a given path happens to call.
//!
//! What: boots `restore_indexes` with the colocated discovery scan ENABLED over
//! two tracked roots whose `.trusty-search/index.redb` is one hard-linked file,
//! then reads `state.registry` and asserts no two registered handles resolve to
//! the same corpus file. The assertion is over the whole registry, so it holds
//! for entries arriving from any collection path — legacy rows, discovery, or a
//! path added later.
//!
//! The hard link is what makes this a DISCOVERY test rather than another
//! root-path test: the two roots canonicalize to different absolute paths, so
//! the pre-#3929 path-only dedup key sees two distinct corpora and lets both
//! register. Only the file-identity key collapses them, which is the same
//! mount-alias shape the reporter hit on EFS.
//!
//! Test: `warm_boot_registers_one_corpus_under_exactly_one_id`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::core::registry::IndexRegistry;
use crate::core::Embedder;
use crate::service::colocated_storage::COLOCATED_DIR_NAME;
use crate::service::SearchAppState;
use trusty_common::embedder::MockEmbedder;

/// Fixture allowlist under `dir` approving `roots` (#767).
///
/// Why: warm-boot drops entries whose root the allowlist does not approve, so
/// without this every entry would be filtered out before dedup ran and the
/// registry assertion would pass vacuously. Injecting an explicit fixture also
/// keeps the test off the developer's real config.
/// What: writes a one-off `allowlist.toml` naming each root and returns the
/// `AllowlistPaths` pointing at it.
/// Test: used by `warm_boot_registers_one_corpus_under_exactly_one_id`.
fn approving(dir: &Path, roots: &[&Path]) -> crate::allowlist::AllowlistPaths {
    let paths = crate::allowlist::AllowlistPaths::default()
        .with_allowlist(dir.join("test-allowlist.toml"))
        .with_project_paths(dir.join("no-projects.json"));
    let cfg = crate::allowlist::AllowlistConfig {
        entries: roots
            .iter()
            .map(|p| crate::allowlist::AllowlistEntry {
                path: p.to_path_buf(),
                name: None,
                exclude: Vec::new(),
                extensions: Vec::new(),
                skip_kg: false,
            })
            .collect(),
    };
    cfg.save_to(&paths.allowlist_file()).unwrap();
    paths
}

/// `(device, inode)` of `root`'s colocated redb corpus, or `None` when the file
/// does not exist or cannot be stat'd.
///
/// Why: this is the identity the invariant is stated over — two registry
/// handles pointing at one physical `.redb` are the double-open that produces
/// `DatabaseAlreadyOpen`, however differently their `root_path` strings read.
/// What: stats `<root>/.trusty-search/index.redb` read-only; never creates the
/// file or its parent.
/// Test: used by `warm_boot_registers_one_corpus_under_exactly_one_id`.
#[cfg(unix)]
fn corpus_identity(root: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let redb = root.join(COLOCATED_DIR_NAME).join("index.redb");
    let meta = std::fs::metadata(redb).ok()?;
    Some((meta.dev(), meta.ino()))
}

/// Create a colocated index root under `base` named `name`, with a real
/// `.trusty-search/index.redb` file.
///
/// Why: the discovery scan keys off the `.trusty-search/` directory, and the
/// file-identity dedup key needs a corpus file that can actually be stat'd.
/// What: creates the directory pair and writes a small placeholder corpus file.
/// The bytes are not a valid redb database on purpose — warm-boot then records
/// `corpus_open_failed` and REGISTERS the index anyway, which is exactly the
/// state #3929 reported (222 registered, 188 corpus-failed). A valid database
/// would make the second registration fail its open for a reason the test is
/// not about.
/// Test: used by `warm_boot_registers_one_corpus_under_exactly_one_id`.
fn make_colocated_root(base: &Path, name: &str) -> std::path::PathBuf {
    let root = base.join(name);
    std::fs::create_dir_all(root.join(COLOCATED_DIR_NAME)).unwrap();
    std::fs::write(
        root.join(COLOCATED_DIR_NAME).join("index.redb"),
        b"not-a-redb",
    )
    .unwrap();
    root
}

/// Why (#4045, ask B): assert that no corpus file is registered under two index
/// ids, and drive the assertion through the warm-boot DISCOVERY path rather
/// than through `create_index` / `relocate_index`. #3929 is the evidence that a
/// guard on the mutation entry points does not cover discovery: the scan
/// registered each root a second time under a freshly-derived id against the
/// same `.redb`, and redb is single-open, so 188 of 222 corpora failed to open.
///
/// What: two tracked roots sharing one hard-linked `index.redb`, an empty
/// `indexes.toml`, and `no_auto_discover = false` so the colocated scan is the
/// ONLY thing that produces entries. After `restore_indexes`, the registry must
/// hold exactly one handle per corpus file.
///
/// Against the pre-#3929 dedup key — canonicalized `root_path` only, no file
/// identity — the two roots read as two distinct corpora, both register, and
/// this test fails naming the two ids that share an inode.
/// Test: this test.
#[cfg(unix)]
#[tokio::test]
#[serial_test::serial]
async fn warm_boot_registers_one_corpus_under_exactly_one_id() {
    let data_tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();

    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path());
        std::env::set_var("TRUSTY_DISABLE_WATCHER", "1");
    }

    // Two roots, one corpus file. The hard link gives them a shared
    // `(device, inode)` while their canonical paths stay distinct — the
    // mount-alias shape from the #3929 report, reproduced locally.
    let primary = make_colocated_root(work.path(), "repo-primary");
    let alias = work.path().join("repo-alias");
    std::fs::create_dir_all(alias.join(COLOCATED_DIR_NAME)).unwrap();
    std::fs::hard_link(
        primary.join(COLOCATED_DIR_NAME).join("index.redb"),
        alias.join(COLOCATED_DIR_NAME).join("index.redb"),
    )
    .unwrap();

    assert_eq!(
        corpus_identity(&primary),
        corpus_identity(&alias),
        "fixture precondition: the two roots must share one corpus file"
    );
    assert_ne!(
        primary.canonicalize().unwrap(),
        alias.canonicalize().unwrap(),
        "fixture precondition: the two roots must canonicalize differently, or \
         the path-only dedup key would collapse them and this test would pass \
         without exercising file identity"
    );

    // Snapshot corpus identity while the hard link is still intact — see the
    // assertion block below for why a post-boot stat cannot stand in for this.
    // Keyed by the CANONICAL root: a registered handle carries the canonical
    // form (`/private/var/…` on macOS), which the fixture's own path is not.
    let pre_boot_identity: HashMap<std::path::PathBuf, (u64, u64)> = [&primary, &alias]
        .into_iter()
        .filter_map(|root| {
            let id = corpus_identity(root)?;
            Some((root.canonicalize().ok()?, id))
        })
        .collect();

    crate::service::roots_registry::upsert_root(primary.clone()).unwrap();
    crate::service::roots_registry::upsert_root(alias.clone()).unwrap();

    // `indexes.toml` stays empty: every entry this boot registers comes from
    // the discovery scan, which is the path #3929 came in through.
    crate::service::persistence::save_index_registry(&[]).unwrap();

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(16));
    let state = SearchAppState::new(IndexRegistry::new())
        .with_allowlist_paths(approving(data_tmp.path(), &[&primary, &alias]));

    // `no_auto_discover = false` — the colocated discovery scan runs.
    super::restore::restore_indexes(&state, &embedder, false).await;

    let handles = state.registry.list_handles();

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::remove_var("TRUSTY_DISABLE_WATCHER");
    }

    // The invariant, stated over the whole registry rather than over the
    // entries one particular collection path produced.
    //
    // Identity comes from the PRE-BOOT snapshot, not from a fresh stat: a boot
    // that fails to open the placeholder corpus replaces the file, so the two
    // roots no longer share an inode afterwards and a post-boot stat can no
    // longer see the collision it is looking for.
    let mut by_corpus: HashMap<(u64, u64), Vec<String>> = HashMap::new();
    for handle in &handles {
        let canonical = handle
            .root_path
            .canonicalize()
            .unwrap_or_else(|_| handle.root_path.clone());
        let identity = pre_boot_identity
            .get(&canonical)
            .copied()
            .or_else(|| corpus_identity(&handle.root_path));
        if let Some(identity) = identity {
            by_corpus
                .entry(identity)
                .or_default()
                .push(handle.id.to_string());
        }
    }
    let doubled: Vec<_> = by_corpus.iter().filter(|(_, ids)| ids.len() > 1).collect();
    assert!(
        doubled.is_empty(),
        "warm boot registered one redb corpus under more than one index id — redb \
         is single-open, so every id after the first fails with DatabaseAlreadyOpen \
         (#2305 / #2336 / #3929). Collisions: {doubled:?}"
    );

    // The scan saw two roots; exactly one of them may end up registered.
    assert_eq!(
        handles.len(),
        1,
        "the discovery scan found two roots backed by one corpus file, so exactly \
         one index may be registered; got {:?}",
        handles
            .iter()
            .map(|h| (h.id.to_string(), h.root_path.clone()))
            .collect::<Vec<_>>()
    );
}
