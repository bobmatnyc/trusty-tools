//! The checkout's reviewer prompts, supplied to an in-process review (#9015).
//!
//! Why: an in-process review cannot rely on the cwd checkout (EVO syncs no
//! `.git`, #9619) or on an installed bundle, so the model-eval driver hands
//! the pipeline the checkout's prompts through `ReviewOptions::with_prompts`.
//! What: [`checkout_prompts`] reads them through the resolver from a staged
//! checkout copy built from `support/content.rs`.
//! Test: `perfect_recording_catches_every_defect` (tests/model_eval.rs).

use trusty_common::content::{DevOverride, ResolveOptions, resolve};
use trusty_review::pipeline::ReviewerPrompts;

/// The checkout's prompts, read through the resolver from a staged checkout.
pub fn checkout_prompts() -> ReviewerPrompts {
    let root = tempfile::tempdir().expect("tempdir");
    for (_, rel) in trusty_common::content::DEV_CLASS_SOURCES {
        std::fs::create_dir_all(root.path().join(rel)).expect("class dir");
    }
    std::fs::create_dir_all(root.path().join(".git")).expect(".git");
    std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n").expect("Cargo.toml");
    for (path, bytes) in crate::content::repo_prompts() {
        let disk = root.path().join("content").join(path);
        std::fs::create_dir_all(disk.parent().expect("parent")).expect("dir");
        std::fs::write(disk, bytes).expect("prompt");
    }
    let at = DevOverride::At(root.path().to_path_buf());
    let content = resolve(&ResolveOptions::new(root.path(), at)).expect("staged checkout");
    ReviewerPrompts::from_content(&content).expect("the checkout's prompts load")
}
