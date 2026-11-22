// Process-start HOME sandbox for every test binary of this crate (#11632).
//
// Why: a test run once moved the operator's real `~/trusty-agents` because a
// spawned binary inherited the real `$HOME`. A per-test override protects only
// the tests that remember it; this runs before `main`, so every test in the
// binary starts under a throwaway home whatever it forgets.
// What: from the platform initializer (`__mod_init_func` on macOS,
// `.init_array` on Linux) points `HOME` at a fresh `<tmp>/ta-test-home-<pid>`,
// drops `USERPROFILE`, `TA_ASSISTANTS_DIR` and `TAGENT_ASSISTANTS_DIR`, and
// records the path in `TA_TEST_HOME_SANDBOX`. A child test process that
// inherits the marker keeps the environment its parent chose for it. The
// directory is removed at exit. A failure to create it aborts the process
// rather than let tests run against the real home.
// Spliced with `include!` into `src/test_env.rs` (lib tests) and
// `tests/common/mod.rs` (integration tests), so it carries no `use` item and
// no inner doc comment.
// Test: `test_env::tests::test_process_home_is_a_sandbox`,
// `tests/bin_spawn_guard.rs`.

/// Set once the sandbox is in place; see the comment at the top of this file.
const TEST_HOME_SANDBOX_MARKER: &str = "TA_TEST_HOME_SANDBOX";

extern "C" fn sandbox_test_home() {
    if std::env::var_os(TEST_HOME_SANDBOX_MARKER).is_some() {
        return;
    }
    let home = std::env::temp_dir().join(format!("ta-test-home-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&home) {
        eprintln!("test HOME sandbox: cannot create {}: {e}", home.display());
        std::process::abort();
    }
    // SAFETY: this runs from the platform initializer, before `main`, while
    // the process has exactly one thread.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::remove_var("USERPROFILE");
        std::env::remove_var("TA_ASSISTANTS_DIR");
        std::env::remove_var("TAGENT_ASSISTANTS_DIR");
        std::env::set_var(TEST_HOME_SANDBOX_MARKER, &home);
        libc::atexit(remove_sandbox_test_home);
    }
}

extern "C" fn remove_sandbox_test_home() {
    if let Some(home) = std::env::var_os(TEST_HOME_SANDBOX_MARKER) {
        let _ = std::fs::remove_dir_all(home);
    }
}

#[used]
#[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__mod_init_func"))]
#[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
static SANDBOX_TEST_HOME: extern "C" fn() = sandbox_test_home;
