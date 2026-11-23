// Process-start HOME sandbox for every test binary of this crate (#11632).
//
// Why: a test run once moved the operator's real `~/trusty-agents` because a
// spawned binary inherited the real `$HOME`. A per-test override protects only
// the tests that remember it; this runs before `main`, so every test in the
// binary starts under a throwaway home whatever it forgets.
// What: from the platform initializer (`__mod_init_func` on macOS,
// `.init_array` on Linux) points `HOME` at a fresh `<tmp>/ta-test-home-<pid>`,
// removes every variable in `SANDBOX_SCRUB`, and records the path in `TA_TEST_HOME_SANDBOX`. A child test process that
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

/// Every variable that can point this crate, or a library it calls, at a
/// directory or socket outside `$HOME`.
///
/// Why (#11632 review): `HOME` alone is not the whole story. On Linux
/// `dirs::data_dir()` prefers `XDG_DATA_HOME`, and the crate's own resolvers
/// honour the overrides below before `HOME`, so a test run with any of them
/// exported would still write to real directories.
/// What: removed by the process-start hook and by `tests/common::sandboxed`
/// for every spawned binary; both guards assert each one is unset.
/// Found by grepping `env::var`/`var_os`/`env_var` under `src/` plus the
/// `dirs` and `trusty-common` data-dir resolvers.
pub const SANDBOX_SCRUB: &[&str] = &[
    "USERPROFILE",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    "TA_ASSISTANTS_DIR",
    "TAGENT_ASSISTANTS_DIR",
    "TAGENT_CONFIG_DIR",
    "OPEN_MPM_CONFIG_DIR",
    "TAGENT_PROJECT_DIR",
    "OPEN_MPM_PROJECT_DIR",
    "TRUSTY_DATA_DIR_OVERRIDE",
    "KB_KNOWLEDGE_DIR",
    "KB_ROOT",
    "FASTEMBED_CACHE_DIR",
    "TRUSTY_MEMORY_SOCKET",
    // #11550 (via the PR-1 rebase): `trusty_agents_common::health::agents_socket`.
    "TRUSTY_AGENTS_SOCKET",
];

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
        for key in SANDBOX_SCRUB {
            std::env::remove_var(key);
        }
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
