//! trusty-mpm's one-test-at-a-time integration target (#8345).
//!
//! Why: these modules rewrite process-global state — `$HOME`, `$PATH`, other
//! environment variables, the working directory — for the length of a test.
//! Each used to be its own test binary so nothing else could observe that
//! state; several held exactly one test for the same reason. In the parallel
//! `integration` target a narrowed `$PATH` makes an unrelated test's
//! `git` spawn fail, and `#[serial]` cannot help, because it only orders the
//! tests that carry it. Serializing the whole target keeps that guarantee — no
//! other test runs while one of these holds the process — at one link instead
//! of fourteen.
//! What: [`run_one_test_at_a_time`] sets `RUST_TEST_THREADS=1` before `main`,
//! which is what libtest reads when no `--test-threads` flag is passed. Passing
//! `--test-threads=N` by hand overrides it and reintroduces the races. Under
//! `cargo nextest` every test is its own process and the setting is moot.
//! Test: every test in the modules below. Run one former binary with a module
//! filter: `cargo test -p trusty-mpm --test env_serial session_manager_mvp::`.

// #8545: `common` arms the home-write fence before `main`.
mod common;

mod auth_cost;
mod daemon_dual_serve;
// #9121: a test daemon's environment carries no inherited secret.
mod daemon_env_isolation;
mod inproject_cold_start;
mod inproject_git_failure;
mod mcp_spawn_gate;
mod pane_launch_line_8233;
mod prompt_refusal_before_post_8286;
mod scratch_home_tmux_gate;
mod scratch_root_tmux_gate;
mod session_manager_mvp;
mod session_new_requires_a_local_path;
mod standalone_isolation;
mod supervisor_floor_host;
mod test_session_lifecycle;
mod worktree_disk_usage_gate;
mod worktree_request_fail_closed;

/// Force libtest onto one test thread for this target (#8345).
///
/// Unconditional, so an operator's exported `RUST_TEST_THREADS` cannot re-enable
/// parallelism here.
#[ctor::ctor]
fn run_one_test_at_a_time() {
    // SAFETY: a constructor runs before `main`, while the process has one
    // thread, so no reader can race this write.
    unsafe { std::env::set_var("RUST_TEST_THREADS", "1") };
}
