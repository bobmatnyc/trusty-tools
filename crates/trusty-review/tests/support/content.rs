//! The checkout's reviewer prompts, staged for integration tests (#9015).
//!
//! Why: since #9015 the reviewer prompts are runtime content — a checkout
//! above the cwd, else the bundle pinned under `<home>/.trusty-mpm/content`.
//! An in-process test cannot rely on the cwd checkout (EVO syncs no `.git`,
//! #9619), and a spawned binary pinned to a scratch `$HOME` and cwd has
//! neither, so this stages what `tm content install` would have.
//! What: [`repo_prompts`] reads the checkout's prompt files; [`stage_bundle`]
//! writes a verified bundle of the
//! prompts (minus one, or with one emptied, to stage content older than or
//! broken against this binary) into `<home>/.trusty-mpm/content`.
//! Test: `stale_or_absent_content_refuses_run_naming_the_remedy`
//! (tests/stale_content_e2e.rs); `support/prompts.rs` builds on it.

#![allow(dead_code)] // each test crate that includes this uses part of it

use std::path::{Path, PathBuf};

use trusty_common::content::{ContentLock, LOCK_FILE_NAME};
use trusty_common::integrity::Sha256Digest;

/// The bundle path of the stock prompt. Spelled out, not taken from the
/// library, so a renamed path fails here and the file needs nothing from
/// `trusty_review` (it also compiles against the pre-#9015 crate).
pub const STOCK_PATH: &str = "instructions/trusty-review/prompts/system_prompt_stock.md";

/// The bundle path of the coverage-gating prompt.
pub const COVERAGE_GATING_PATH: &str =
    "instructions/trusty-review/prompts/system_prompt_coverage_gating.md";

/// The checkout's two prompts as `(bundle path, bytes)`, read from the runtime
/// checkout (#9298), never the compile-time build path.
pub fn repo_prompts() -> Vec<(String, Vec<u8>)> {
    let repo = trusty_common::test_harness::test_repo_root().expect("resolve the checkout");
    [STOCK_PATH, COVERAGE_GATING_PATH]
        .iter()
        .map(|path| {
            let disk: PathBuf = repo.join("content").join(path);
            let bytes = std::fs::read(&disk).expect("read a checkout prompt");
            ((*path).to_owned(), bytes)
        })
        .collect()
}

/// How [`stage_bundle`] alters the checkout's prompts.
#[derive(Debug, Clone, Copy)]
pub enum Staged<'a> {
    /// Both prompts as the checkout has them.
    Current,
    /// Without this bundle path: a release older than #9015.
    Without(&'a str),
    /// With this bundle path emptied.
    Emptied(&'a str),
}

/// Installs a verified bundle of the prompts into `<home>/.trusty-mpm/content`.
pub fn stage_bundle(home: &Path, staged: Staged<'_>) {
    const TAG: &str = "content-v0.0.1";
    let mut entries: Vec<(String, Vec<u8>)> = vec![(
        "bundle-manifest.toml".to_string(),
        format!("tag = \"{TAG}\"\nschema_major = 1\n").into_bytes(),
    )];
    for (path, bytes) in repo_prompts() {
        match staged {
            Staged::Without(skip) if skip == path => continue,
            Staged::Emptied(empty) if empty == path => entries.push((path, Vec::new())),
            _ => entries.push((path, bytes)),
        }
    }
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    for (path, data) in &entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, path, data.as_slice())
            .expect("append");
    }
    let bytes = tar.into_inner().expect("tar").finish().expect("gzip");
    let cache = home.join(".trusty-mpm").join("content");
    std::fs::create_dir_all(&cache).expect("content cache");
    let lock = ContentLock::new(TAG, Sha256Digest::of_bytes(&bytes)).expect("lock");
    std::fs::write(cache.join(lock.bundle_file_name()), &bytes).expect("bundle");
    lock.store(&cache.join(LOCK_FILE_NAME)).expect("store lock");
}
