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
// #8985: `disk_survey` through a built daemon's `/rpc`.
mod disk_survey_mcp_smoke;
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
mod no_content_single_error;
mod orphan_gc_sweep;
mod pid_registry_sweep;
mod project_registry_routes;
mod project_status_route;
mod projects_json_concurrency;
mod proxy_routes;
mod push_guard_hook;
mod relocated_interactive_config_4181;
mod resume_unresumable_mapping;
// #6288 S3a: the built daemon serves its socket only and holds no TCP listener.
mod daemon_no_tcp_6288_s3;
// #9121: `tm daemon --sandbox` refuses secret-carrying envs, never printing values.
mod sandbox_daemon_9121;
// #9557: a sandbox daemon reaches only its own tmux server.
mod sandbox_tmux_9557;
mod services_integration;
mod session_control_api;
mod session_lifecycle;
mod session_manager_slots;
mod sm_e2e_smoke;
mod spawned_tm_home_isolation;
mod tm_build_lease;
mod tm_cli_socket;
// #6288 client cutover: moved commands reach the socket only, failing closed.
mod tm_client_socket_cutover_6288;
mod tm_compress_pipe;
mod tm_doctor_network;
mod tm_doctor_standalone;
// #9822: `tm architect <verb>` against the Python goldens, fake tmux.
mod tm_architect_verbs;
// #8436: `tm fleet init|status` through the binary.
mod tm_fleet;
mod tm_guided_default_explicit_url;
mod tm_hook_delegation_payload;
// #8454 S2: the trust anchors through the egress hook, pm-guard off.
mod tm_hook_egress_anchor_8454;
// #8454: `tm hook --egress-guard`, the outbound-channel family deny.
mod tm_hook_egress_guard_8454;
// #9923: the egress guard taints a hook-less id under a live mark.
mod tm_hook_egress_lazy_taint_9923;
mod tm_hook_idle_parking;
mod tm_hook_notification_8392;
// #8531: `tm hook` SessionStart over the socket, HTTP only on Unreachable.
mod tm_hook_pm_guard;
mod tm_hook_pm_guard_absolute_write_7228;
mod tm_hook_pm_guard_architect_envfile_8939;
mod tm_hook_pm_guard_architect_pane_8902;
mod tm_hook_pm_guard_build_lease;
mod tm_hook_pm_guard_credential_print;
mod tm_hook_pm_guard_deny_capture;
mod tm_hook_pm_guard_false_positives;
// #10033: `tm fleet relaunch` denied for every hooked session, under each bypass.
mod tm_hook_pm_guard_fleet_relaunch_10033;
mod tm_hook_pm_guard_head_switch;
mod tm_hook_pm_guard_heredoc_continuation_9180;
mod tm_hook_pm_guard_heredoc_delimiter_9150;
// #8625: the kuzu-store deny under each bypass and for the Architect.
mod tm_hook_pm_guard_kuzu_store_8625;
// #9720: a `<<`-dense command is judged inside the hook timeout.
mod tm_hook_pm_guard_architect_8453;
mod tm_hook_pm_guard_heredoc_density_9720;
mod tm_hook_pm_guard_operator_checkouts;
mod tm_hook_pm_guard_pem_consumers;
// #8928 step 7: the Write/Edit protected-path check.
mod tm_hook_pm_guard_protected_paths_8928;
mod tm_hook_pm_guard_refusing_daemon_9526;
mod tm_hook_pm_guard_secret_batch;
// #9791: `file -f`/`-m` on a secret file under each bypass and for the Architect.
mod tm_hook_pm_guard_secret_file_list_9791;
mod tm_hook_pm_guard_stdin_7975;
mod tm_hook_pm_guard_tmux_target_9001;
mod tm_hook_pm_guard_trust_anchor_8878;
mod tm_hook_pm_guard_unquoted_heredoc_expansion;
mod tm_hook_pretooluse_rewrite;
mod tm_hook_session_end_8980;
mod tm_hook_session_start_8531;
// #8433 P0 (D09): a tainted session's new id reads tainted at SessionStart.
mod tm_hook_session_start_taint_8433;
mod tm_ls_state_colors;
// #9621: `tm secrets exec` on a fake socket, and live on a real server.
mod tm_secrets_exec;
mod tm_secrets_exec_live;
// #9072: `tm secrets resolve` against a real server.
mod tm_secrets_resolve;
mod tm_session_disk_cli;
mod tm_sessions_alias_notice;
mod tm_sessions_instructions_diagnostics;
// #8994: `tm worktrees --json` on a scratch HOME.
mod tm_worktrees_cli;
mod trusty_mpm_alias;
mod workspace_serial_test_features;
