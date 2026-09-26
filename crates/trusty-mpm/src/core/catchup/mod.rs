//! Re-export shim — catch-up engine now lives in `trusty-common` (PR1, #1762).
//!
//! Why: the orchestrator was extracted to `trusty_common::catchup` so both
//! trusty-mpm and trusty-code can share it. This shim re-exports the public
//! surface so all existing `crate::core::catchup::*` call sites compile
//! without change.
//! What: pub-use the entire `trusty_common::catchup` surface. Sub-modules
//! (`git`, `palace`, `state`, `session_finder`, `mpm_session`, `mpm_registry`)
//! are also re-exported by name so sub-module qualified paths still resolve.
//! Test: `cargo test -p trusty-mpm` — exercises re-exported types.
// CUTOVER BRIDGE — remove post-migration (#1762)

pub use trusty_common::catchup::*;
pub use trusty_common::catchup::{
    git, mpm_registry, mpm_session, palace, resolve, session_finder, state,
};

/// [`run_catchup_blocking`] with the watermark's state root named (#8545).
///
/// Why: `run_catchup_blocking` advances the watermark under the process home, so
/// a test driving session prep under a temp home still wrote
/// `~/.trusty-mpm/projects/<palace>/catchup-state.json`.
/// What: the same thread-plus-runtime shape, running [`run_catchup_in`] against
/// `state_root` (`None` keeps the home-relative default). An advancing call
/// first passes the destination through `core::home_write_fence`.
/// Test: `tests::catchup_blocking_in_writes_the_watermark_under_the_named_root`.
pub fn run_catchup_blocking_in(
    opts: CatchupOptions,
    advance_watermark: bool,
    state_root: Option<std::path::PathBuf>,
) -> String {
    if advance_watermark {
        let root = state_root.clone().or_else(|| {
            dirs::home_dir().map(|h| crate::core::paths::FrameworkPaths::under(h).root)
        });
        if let Some(root) = root {
            crate::core::home_write_fence::check(&root.join("projects"));
        }
    }
    let joined = std::thread::spawn(move || {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt.block_on(run_catchup_in(
                &opts,
                advance_watermark,
                state_root.as_deref(),
            )),
            Err(e) => {
                eprintln!("catchup: could not build tokio runtime: {e}");
                String::new()
            }
        }
    })
    .join();
    joined.unwrap_or_else(|_| {
        eprintln!("catchup: catch-up thread panicked; skipping");
        String::new()
    })
}

#[cfg(test)]
mod tests {
    /// #8545: an advancing run writes its watermark under the named root.
    #[test]
    fn catchup_blocking_in_writes_the_watermark_under_the_named_root() {
        let project = crate::test_support::hermetic_temp_dir();
        let state = crate::test_support::hermetic_temp_dir();
        let opts = super::CatchupOptions {
            project_dir: project.path().to_path_buf(),
            memory_socket: state.path().join("no-such.sock"),
            include_git: false,
            include_palace: false,
            git_limit: 1,
            drawer_limit: 1,
            full: false,
        };
        super::run_catchup_blocking_in(opts, true, Some(state.path().to_path_buf()));
        let written: Vec<_> = std::fs::read_dir(state.path().join("projects"))
            .expect("the watermark's projects dir")
            .flatten()
            .map(|e| e.path().join("catchup-state.json"))
            .collect();
        assert_eq!(written.len(), 1, "{written:?}");
        assert!(written[0].is_file(), "{}", written[0].display());
    }
}
