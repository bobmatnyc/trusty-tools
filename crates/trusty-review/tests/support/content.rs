//! The checkout's trusty-review content, staged for integration tests (#9015).
//!
//! Why: since #9015 the reviewer prompts, report and review templates and the
//! shipped voice are runtime content — the bundle pinned under
//! `<home>/.trusty-mpm/content`, or a checkout `TRUSTY_REVIEW_CONTENT_CHECKOUT`
//! names. An in-process test has no installed bundle on EVO (and EVO syncs no
//! `.git`, #9619), and a spawned binary pinned to a scratch `$HOME` has
//! neither, so this stages what `tm content install` would have.
//! What: [`repo_content`] reads every file of the checkout's
//! `content/instructions/trusty-review/`; [`stage_bundle`] writes a verified
//! bundle of it (minus one file, or with one emptied, to stage content older
//! than or broken against this binary) into `<home>/.trusty-mpm/content`;
//! [`staged_content`] resolves such a bundle in memory for an in-process
//! caller; [`fake_checkout`] lays the files out as a trusted checkout.
//! Test: `stale_or_absent_content_refuses_run_naming_the_remedy`
//! (tests/stale_content_e2e.rs); `support/prompts.rs` builds on it.

#![allow(dead_code)] // each test crate that includes this uses part of it

use std::path::{Path, PathBuf};
use std::sync::Arc;

use trusty_common::content::{
    ContentLock, DevOverride, LOCK_FILE_NAME, ResolveOptions, ResolvedContent, resolve,
};
use trusty_common::integrity::Sha256Digest;

/// The bundle directory every trusty-review content file lives under.
pub const CONTENT_DIR: &str = "instructions/trusty-review";

/// The bundle path of the shipped voice package.
pub const VOICE_PATH: &str = "instructions/trusty-review/voices/duetto/voice.toml";

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

/// Every file of the checkout's `content/instructions/trusty-review/` as
/// `(bundle path, bytes)`, sorted, read from the runtime checkout (#9298).
pub fn repo_content() -> Vec<(String, Vec<u8>)> {
    let repo = trusty_common::test_harness::test_repo_root().expect("resolve the checkout");
    let mut out = Vec::new();
    collect(&repo.join("content"), Path::new(CONTENT_DIR), &mut out);
    out.sort();
    out
}

/// Appends every regular file under `base/rel` to `out`, keyed by `rel`.
fn collect(base: &Path, rel: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    for entry in std::fs::read_dir(base.join(rel)).expect("read a content dir") {
        let entry = entry.expect("dir entry");
        let child = rel.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            collect(base, &child, out);
        } else {
            let bytes = std::fs::read(base.join(&child)).expect("read a content file");
            let key = child.to_str().expect("utf8 path").replace('\\', "/");
            out.push((key, bytes));
        }
    }
}

/// How [`stage_bundle`] alters the checkout's content.
#[derive(Debug, Clone, Copy)]
pub enum Staged<'a> {
    /// Every file as the checkout has it.
    Current,
    /// Without this bundle path: a release older than #9015.
    Without(&'a str),
    /// With this bundle path emptied.
    Emptied(&'a str),
}

/// Installs a verified bundle of the content into `<home>/.trusty-mpm/content`.
pub fn stage_bundle(home: &Path, staged: Staged<'_>) {
    stage_bundle_in(&home.join(".trusty-mpm").join("content"), staged);
}

/// The checkout's content, verified and resolved in memory; for an in-process
/// caller such as `TemplateLoader::with_content`.
pub fn staged_content() -> Arc<ResolvedContent> {
    let cache = tempfile::tempdir().expect("cache");
    stage_bundle_in(cache.path(), Staged::Current);
    let options = ResolveOptions::new(cache.path(), DevOverride::Off);
    Arc::new(resolve(&options).expect("the staged bundle resolves"))
}

/// Lays `files` (bundle path, bytes) out under `root` as a trusted
/// trusty-tools checkout: every class directory, `.git` and a workspace
/// `Cargo.toml`.
pub fn fake_checkout(root: &Path, files: &[(String, Vec<u8>)]) {
    for (_, rel) in trusty_common::content::DEV_CLASS_SOURCES {
        std::fs::create_dir_all(root.join(rel)).expect("class dir");
    }
    std::fs::create_dir_all(root.join(".git")).expect(".git");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("Cargo.toml");
    for (path, bytes) in files {
        let disk = root.join("content").join(path);
        std::fs::create_dir_all(disk.parent().expect("parent")).expect("dir");
        std::fs::write(disk, bytes).expect("content file");
    }
}

/// [`stage_bundle`] into the cache directory `cache`.
fn stage_bundle_in(cache: &Path, staged: Staged<'_>) {
    const TAG: &str = "content-v0.0.1";
    let mut entries: Vec<(String, Vec<u8>)> = vec![(
        "bundle-manifest.toml".to_string(),
        format!("tag = \"{TAG}\"\nschema_major = 1\n").into_bytes(),
    )];
    for (path, bytes) in repo_content() {
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
    std::fs::create_dir_all(cache).expect("content cache");
    let lock = ContentLock::new(TAG, Sha256Digest::of_bytes(&bytes)).expect("lock");
    std::fs::write(cache.join(lock.bundle_file_name()), &bytes).expect("bundle");
    lock.store(&cache.join(LOCK_FILE_NAME)).expect("store lock");
}
