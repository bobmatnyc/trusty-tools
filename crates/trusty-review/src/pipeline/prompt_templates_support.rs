//! Test-only sources for the reviewer prompts (#9015).
//!
//! Why: the prompts left the crate directory, so a unit test that builds a
//! review reads the checkout's `content/`. Resolving it from the cwd needs the
//! checkout's `.git`, which EVO does not sync (#9619); a staged copy keeps the
//! resolver in the path without it. A test of the fail-closed arms names its
//! own source instead.
//! What: [`load_override`] is the hook [`ReviewerPrompts::load`] calls under
//! `cfg(test)`: the source [`OverrideGuard`] set on this thread, else the
//! staged checkout copy ([`prompts`]).
//! Test: `the_repository_prompts_load_byte_identical`, and the pipeline arm
//! `run_review_without_prompts_fails_closed_before_any_fetch_or_model_call`.

use std::cell::RefCell;
use std::path::Path;
use std::sync::OnceLock;

use trusty_common::content::{DevOverride, ResolveOptions, resolve};

use super::{COVERAGE_GATING_FILE, PROMPTS_DIR, PromptContentError, ReviewerPrompts, STOCK_FILE};

thread_local! {
    /// The source a test named for this thread, if any.
    static OVERRIDE: RefCell<Option<ResolveOptions>> = const { RefCell::new(None) };
}

/// The prompts [`ReviewerPrompts::load`] returns on this thread.
pub(crate) fn load_override() -> Option<Result<ReviewerPrompts, PromptContentError>> {
    let named = OVERRIDE.with(|o| o.borrow().clone());
    Some(match named {
        Some(options) => resolve(&options)
            .map_err(PromptContentError::from)
            .and_then(|content| ReviewerPrompts::from_content(&content)),
        None => Ok(prompts().clone()),
    })
}

/// Makes [`ReviewerPrompts::load`] on this thread resolve `options` until the
/// guard drops. Use it from a current-thread runtime, so the pipeline runs on
/// the thread that set it.
pub(crate) struct OverrideGuard;

impl OverrideGuard {
    /// Sets `options` as this thread's source.
    pub(crate) fn set(options: ResolveOptions) -> Self {
        OVERRIDE.with(|o| *o.borrow_mut() = Some(options));
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

/// The checkout's two prompt files as `(repo-relative path, bytes)`, read from
/// the runtime checkout (#9298), never the compile-time build path.
pub(crate) fn repo_prompt_files() -> Vec<(String, Vec<u8>)> {
    let root = trusty_common::test_harness::test_repo_root()
        .expect("resolve the trusty-tools checkout (set TRUSTY_TEST_REPO_ROOT)");
    [STOCK_FILE, COVERAGE_GATING_FILE]
        .iter()
        .map(|file| {
            let rel = format!("content/{PROMPTS_DIR}/{file}");
            let bytes = std::fs::read(root.join(&rel)).expect("read a checkout prompt");
            (rel, bytes)
        })
        .collect()
}

/// The checkout's prompts, read through the resolver from a staged copy;
/// loaded once per test process.
pub(crate) fn prompts() -> &'static ReviewerPrompts {
    static PROMPTS: OnceLock<ReviewerPrompts> = OnceLock::new();
    PROMPTS.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("tempdir");
        fake_checkout(tmp.path(), &repo_prompt_files());
        let options = ResolveOptions::new(tmp.path(), DevOverride::At(tmp.path().to_path_buf()));
        let content = resolve(&options).expect("the staged checkout resolves");
        ReviewerPrompts::from_content(&content).expect("the checkout's prompts load")
    })
}
