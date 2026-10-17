//! Test-only content sources for trusty-review (#9015).
//!
//! Why: the prompts, templates and voice left the crate directory, so a unit
//! test that builds a review or a report reads the checkout's `content/`.
//! Resolving it from a checkout needs the checkout's `.git`, which EVO does not
//! sync (#9619); a staged bundle keeps the resolver in the path without it. A
//! test of a fail-closed arm names its own source instead.
//! What: [`resolve_override`] is the hook [`super::resolve_for_process`] calls
//! under `cfg(test)`: the source [`OverrideGuard`] set on this thread, else the
//! staged bundle of the checkout's content ([`staged`]).
//! Test: `the_repository_prompts_load_byte_identical`, and the pipeline arm
//! `run_review_without_prompts_fails_closed_before_any_fetch_or_model_call`.

use std::cell::RefCell;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use trusty_common::content::{
    ContentLock, DevOverride, LOCK_FILE_NAME, ResolveOptions, ResolvedContent, resolve,
};
use trusty_common::integrity::Sha256Digest;

use super::{CONTENT_DIR, PromptContentError};

/// What [`OverrideGuard`] makes this thread resolve.
#[derive(Clone)]
enum Override {
    /// Resolve these options on each call.
    Options(ResolveOptions),
    /// Serve this content.
    Content(Arc<ResolvedContent>),
}

thread_local! {
    /// The source a test named for this thread, if any.
    static OVERRIDE: RefCell<Option<Override>> = const { RefCell::new(None) };
}

/// The content [`super::resolve_for_process`] returns on this thread.
pub(crate) fn resolve_override() -> Option<Result<Arc<ResolvedContent>, PromptContentError>> {
    let named = OVERRIDE.with(|o| o.borrow().clone());
    Some(match named {
        Some(Override::Options(options)) => resolve(&options)
            .map(Arc::new)
            .map_err(PromptContentError::from),
        Some(Override::Content(content)) => Ok(content),
        None => Ok(staged()),
    })
}

/// Makes [`super::resolve_for_process`] on this thread resolve a named source
/// until the guard drops. Use it from a current-thread runtime, so the
/// pipeline runs on the thread that set it.
pub(crate) struct OverrideGuard;

impl OverrideGuard {
    /// Sets `options` as this thread's source.
    pub(crate) fn set(options: ResolveOptions) -> Self {
        OVERRIDE.with(|o| *o.borrow_mut() = Some(Override::Options(options)));
        Self
    }

    /// Sets `content` as this thread's source.
    pub(crate) fn content(content: Arc<ResolvedContent>) -> Self {
        OVERRIDE.with(|o| *o.borrow_mut() = Some(Override::Content(content)));
        Self
    }

    /// A source with no checkout and no installed bundle under `cache`.
    pub(crate) fn not_installed(cache: &Path) -> Self {
        Self::set(ResolveOptions::new(cache, DevOverride::Off))
    }
}

impl Drop for OverrideGuard {
    fn drop(&mut self) {
        OVERRIDE.with(|o| *o.borrow_mut() = None);
    }
}

/// Builds a trusted checkout at `root` holding `files` (repo-relative path,
/// bytes), with every dev class directory present.
pub(crate) fn fake_checkout(root: &Path, files: &[(String, Vec<u8>)]) {
    for (_, rel) in trusty_common::content::DEV_CLASS_SOURCES {
        std::fs::create_dir_all(root.join(rel)).expect("class dir");
    }
    std::fs::create_dir_all(root.join(".git")).expect(".git");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("Cargo.toml");
    for (rel, body) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, body).expect("file");
    }
}

/// Every file of the checkout's `content/instructions/trusty-review/` as
/// `(bundle path, bytes)`, read from the runtime checkout (#9298), never the
/// compile-time build path.
pub(crate) fn repo_content_files() -> Vec<(String, Vec<u8>)> {
    let root = trusty_common::test_harness::test_repo_root()
        .expect("resolve the trusty-tools checkout (set TRUSTY_TEST_REPO_ROOT)");
    let mut out = Vec::new();
    collect(&root.join("content"), Path::new(CONTENT_DIR), &mut out);
    out.sort();
    out
}

/// Appends every regular file under `base/rel` to `out`, keyed by `rel`.
fn collect(base: &Path, rel: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let dir = std::fs::read_dir(base.join(rel)).expect("read a content dir");
    for entry in dir {
        let entry = entry.expect("dir entry");
        let child = rel.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            collect(base, &child, out);
        } else {
            let bytes = std::fs::read(base.join(&child)).expect("read a content file");
            let key = child
                .to_str()
                .expect("utf8 content path")
                .replace('\\', "/");
            out.push((key, bytes));
        }
    }
}

/// `files` as installed content: a verified bundle, resolved into memory.
pub(crate) fn installed(files: &[(String, Vec<u8>)]) -> Arc<ResolvedContent> {
    const TAG: &str = "content-v0.0.1";
    let manifest = format!("tag = \"{TAG}\"\nschema_major = 1\n").into_bytes();
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    let entries = std::iter::once(("bundle-manifest.toml", manifest.as_slice()))
        .chain(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())));
    for (path, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, path, data).expect("append");
    }
    let bytes = tar.into_inner().expect("tar").finish().expect("gzip");
    let cache = tempfile::tempdir().expect("cache");
    let lock = ContentLock::new(TAG, Sha256Digest::of_bytes(&bytes)).expect("lock");
    std::fs::write(cache.path().join(lock.bundle_file_name()), &bytes).expect("bundle");
    lock.store(&cache.path().join(LOCK_FILE_NAME))
        .expect("store lock");
    let options = ResolveOptions::new(cache.path(), DevOverride::Off);
    Arc::new(resolve(&options).expect("the staged bundle resolves"))
}

/// The checkout's content without bundle path `path`: a stale release.
pub(crate) fn installed_without(path: &str) -> Arc<ResolvedContent> {
    let mut files = repo_content_files();
    let before = files.len();
    files.retain(|(p, _)| p != path);
    assert_eq!(files.len() + 1, before, "`{path}` is not a content file");
    installed(&files)
}

/// The checkout's content with bundle path `path` replaced by `body`.
pub(crate) fn installed_with(path: &str, body: &[u8]) -> Arc<ResolvedContent> {
    let mut files = repo_content_files();
    let file = files.iter_mut().find(|(p, _)| p == path);
    file.expect("a content file").1 = body.to_vec();
    installed(&files)
}

/// The checkout's content as an installed bundle; staged once per process.
pub(crate) fn staged() -> Arc<ResolvedContent> {
    static STAGED: OnceLock<Arc<ResolvedContent>> = OnceLock::new();
    Arc::clone(STAGED.get_or_init(|| installed(&repo_content_files())))
}
