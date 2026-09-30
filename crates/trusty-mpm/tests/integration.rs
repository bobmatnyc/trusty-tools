//! trusty-mpm's parallel integration target (#8345).
//!
//! Why: every file under `tests/` used to be its own test binary, and linking
//! ~60 of them against the full trusty-mpm dependency graph dominated this
//! crate's CI shard — removing every sleep in the suite saved under two minutes
//! of a 19-26 minute shard. One crate per concern links once.
//! What: each module below is one former test binary, unchanged apart from
//! reaching the shared helper as `crate::common`. libtest runs every test here
//! in parallel, so a module belongs here only if it leaves process-global state
//! alone: no `std::env::set_var`/`remove_var`, no `set_current_dir`. A module
//! that needs either goes in `tests/env_serial.rs`, which runs one test at a
//! time. `common::scratch_home` is the one sanctioned exception — it repoints
//! `$HOME` once per process, never back.
//! Test: `every_integration_target_arms_the_home_write_fence` fails if a file
//! under `tests/` is mounted by neither target, or if a module here mutates the
//! process environment. Run one former binary with a module filter:
//! `cargo test -p trusty-mpm --test integration tm_hook_pm_guard::`.

// #8545: `common` arms the home-write fence before `main`.
mod common;

mod catalog_sync_idempotent;
mod commit_stats_hook_budget;
mod config_mount;
#[cfg(feature = "daemon")]
mod e2e;
mod inproject_hygiene_test;
mod local_spawn;
mod manager_cli_client;
mod manager_inference;
mod manager_routes;
mod manager_routing;
mod memory_verbs_socket;
mod meta_demo_e2e;
mod orphan_gc_sweep;
mod pid_registry_sweep;
mod project_registry_routes;
mod project_status_route;
mod projects_json_concurrency;
mod proxy_routes;
mod push_guard_hook;
mod relocated_interactive_config_4181;
mod resume_unresumable_mapping;
mod services_integration;
mod session_control_api;
mod session_lifecycle;
mod session_manager_slots;
mod sm_e2e_smoke;
mod spawned_tm_home_isolation;
mod tm_build_lease;
mod tm_cli_socket;
mod tm_compress_pipe;
mod tm_doctor_standalone;
// #8436: `tm fleet init|status` through the binary.
mod tm_fleet;
mod tm_guided_default_explicit_url;
mod tm_hook_delegation_payload;
mod tm_hook_idle_parking;
mod tm_hook_notification_8392;
mod tm_hook_pm_guard;
mod tm_hook_pm_guard_architect_envfile_8939;
mod tm_hook_pm_guard_architect_pane_8902;
mod tm_hook_pm_guard_build_lease;
mod tm_hook_pm_guard_credential_print;
mod tm_hook_pm_guard_deny_capture;
mod tm_hook_pm_guard_false_positives;
mod tm_hook_pm_guard_head_switch;
mod tm_hook_pm_guard_pem_consumers;
mod tm_hook_pm_guard_secret_batch;
mod tm_hook_pm_guard_stdin_7975;
mod tm_hook_pm_guard_supervisor_8453;
mod tm_hook_pm_guard_trust_anchor_8878;
mod tm_hook_pretooluse_rewrite;
mod tm_ls_state_colors;
mod tm_session_disk_cli;
mod tm_sessions_alias_notice;
mod tm_sessions_instructions_diagnostics;
mod trusty_mpm_alias;
mod workspace_serial_test_features;
