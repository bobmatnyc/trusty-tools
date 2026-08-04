//! Command handler modules for the `tm` binary.
//!
//! Why: splitting each subcommand group into its own file keeps every handler
//! file well under the 500-line cap and makes the handler surface easy to
//! navigate.
//! What: re-exports handler modules — `auth`, `compress`, `daemon`,
//! `hook_rewrite`, `install`, `launch`, `managed`, `managed_route`, `meta`,
//! `misc`, `project`, `services`, `session`, `slack`, `supervisor`, `telegram`.
//! Two client-side `tm doctor` probes also live here rather than in the daemon's
//! `run_doctor`, because each needs state the responding daemon cannot report on
//! itself: `doctor_stale` (this binary's own `CARGO_PKG_VERSION`, #2332) and
//! `doctor_orphan` (whether launchd owns the process that answered, #4230).
//! Test: each module has its own unit tests; integration coverage lives in
//! `tests.rs`.

pub(crate) mod agent;
pub(crate) mod auth;
pub(crate) mod banner;
pub(crate) mod compress;
pub(crate) mod daemon;
pub(crate) mod delete;
// #4230: the client-side orphan-daemon check — the daemon's own `run_doctor`
// cannot detect that the process answering it is the unsupervised one.
pub(crate) mod doctor_fix_skills;
pub(crate) mod doctor_orphan;
pub(crate) mod doctor_stale;
pub(crate) mod first_run;
pub(crate) mod generate;
pub(crate) mod guided;
pub(crate) mod guided_autostart;
pub(crate) mod guided_inplace;
pub(crate) mod guided_launch;
pub(crate) mod guided_resolver;
pub(crate) mod guided_resume;
pub(crate) mod hook_payload;
pub(crate) mod hook_rewrite;
pub(crate) mod hooks;
pub(crate) mod install;
// #4605: the unmanaged-bundled-skill reporter and `--reconcile-skills` path,
// split out of `install` for the 500-SLOC production cap.
pub(crate) mod install_skills;
pub(crate) mod issue;
pub(crate) mod launch;
pub(crate) mod launchd_probe;
pub(crate) mod managed;
// #2919: merged-PR reclaim-pass rendering, split out of `managed` for the cap.
pub(crate) mod managed_merged_prs;
pub(crate) mod managed_root;
pub(crate) mod managed_route;
pub(crate) mod managed_workspace;
pub(crate) mod manager;
pub(crate) mod mcp;
pub(crate) mod meta;
pub(crate) mod misc;
pub(crate) mod pane_identity;
pub(crate) mod picker_delete;
pub(crate) mod pm_guard;
pub(crate) mod pm_guard_bash;
pub(crate) mod pm_guard_budget;
pub(crate) mod pm_guard_cost;
pub(crate) mod pm_guard_deny_by_default;
pub(crate) mod pm_guard_fanout;
pub(crate) mod pm_guard_routing;
pub(crate) mod project;
pub(crate) mod projects;
pub(crate) mod prune;
pub(crate) mod push_guard;
pub(crate) mod reconcile_worktrees;
pub(crate) mod rename;
pub(crate) mod repair;
pub(crate) mod serve_stdio;
pub(crate) mod services;
pub(crate) mod sessctl;
pub(crate) mod session;
pub(crate) mod session_picker;
pub(crate) mod session_picker_prune;
pub(crate) mod session_picker_rename;
pub(crate) mod session_picker_render;
pub(crate) mod slack;
pub(crate) mod sm_serve;
pub(crate) mod spawn_disclaimed;
pub(crate) mod standalone;
pub(crate) mod statusline;
pub(crate) mod supervisor;
pub(crate) mod sync_assets;
pub(crate) mod telegram;
pub(crate) mod ticket;
pub(crate) mod tmux_attach;
pub(crate) mod watch;
