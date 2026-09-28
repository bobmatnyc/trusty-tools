//! #8311: session prep advances the catch-up watermark under `fw.root`.
//!
//! Why: the watermark followed the injected user home, and a `None` home fell
//! back to the process home — so a test naming only its framework root still
//! wrote `<home>/.trusty-mpm/projects/<palace>/catchup-state.json`, the
//! directories that reached 42k in the operator's real state dir.
//! What: runs the real launch with a framework root and a separate home, both
//! temp dirs, for a `None` and a `Some` home, and asserts the watermark lands
//! under the framework root and nothing lands under the home.
//! Test: this is the test module.

use super::tests::EnvVarGuard;
use super::*;

/// The `[catchup]` section with auto-inject on and both network-free sources
/// off, so the watermark write runs without dialling a live trusty-memory.
const CATCHUP_OFFLINE: &str = "[catchup]\nauto = true\ninclude_git = false\n\
include_palace = false\ngit_limit = 1\ndrawer_limit = 1\n";

/// Every `catchup-state.json` under `<root>/projects/*/`.
fn watermarks_under(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root.join("projects"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path().join("catchup-state.json"))
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
#[serial_test::serial]
fn session_prep_writes_the_catchup_watermark_under_the_framework_root() {
    for inject_home in [false, true] {
        // `$HOME` points at a temp dir too, so a regression lands there — where
        // this test sees it — and never in the operator's real home.
        let home = crate::test_support::hermetic_temp_dir();
        let _home_env = EnvVarGuard::set("HOME", home.path());
        let fw_base = crate::test_support::hermetic_temp_dir();
        let project = crate::test_support::hermetic_temp_dir();
        let fw = FrameworkPaths::under(fw_base.path());
        std::fs::create_dir_all(&fw.root).unwrap();
        std::fs::write(fw.root.join("config.toml"), CATCHUP_OFFLINE).unwrap();

        let named_home = inject_home.then(|| home.path());
        crate::core::session_launch::prepare_session_with_memory_reachable(
            &fw,
            project.path(),
            named_home,
            false,
        )
        .expect("prep succeeds");

        let written = watermarks_under(&fw.root);
        assert_eq!(
            written.len(),
            1,
            "home injected: {inject_home}; one watermark under {}: {written:?}",
            fw.root.display()
        );
        let home_state = home.path().join(".trusty-mpm").join("projects");
        assert!(
            !home_state.exists(),
            "home injected: {inject_home}; the watermark must not fall back to \
             the home: {}",
            home_state.display()
        );
    }
}
