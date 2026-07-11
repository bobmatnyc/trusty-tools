//! Command handler modules for the `tm` binary.
//!
//! Why: splitting each subcommand group into its own file keeps every handler
//! file well under the 500-line cap and makes the handler surface easy to
//! navigate.
//! What: re-exports handler modules — `auth`, `compress`, `daemon`,
//! `hook_rewrite`, `install`, `launch`, `managed`, `managed_route`, `meta`,
//! `misc`, `project`, `services`, `session`, `slack`, `supervisor`, `telegram`.
//! Test: each module has its own unit tests; integration coverage lives in
//! `tests.rs`.

pub(crate) mod auth;
pub(crate) mod banner;
pub(crate) mod compress;
pub(crate) mod daemon;
pub(crate) mod delete;
pub(crate) mod first_run;
pub(crate) mod guided;
pub(crate) mod guided_autostart;
pub(crate) mod guided_inplace;
pub(crate) mod guided_launch;
pub(crate) mod guided_launch_sse;
pub(crate) mod guided_resume;
pub(crate) mod hook_rewrite;
pub(crate) mod install;
pub(crate) mod issue;
pub(crate) mod launch;
pub(crate) mod managed;
pub(crate) mod managed_root;
pub(crate) mod managed_route;
pub(crate) mod mcp;
pub(crate) mod meta;
pub(crate) mod misc;
pub(crate) mod picker_delete;
pub(crate) mod pm_guard;
pub(crate) mod pm_guard_bash;
pub(crate) mod pm_guard_deny_by_default;
pub(crate) mod project;
pub(crate) mod projects;
pub(crate) mod prune;
pub(crate) mod repair;
pub(crate) mod serve_stdio;
pub(crate) mod services;
pub(crate) mod sessctl;
pub(crate) mod session;
pub(crate) mod session_picker;
pub(crate) mod slack;
pub(crate) mod sm_serve;
pub(crate) mod standalone;
pub(crate) mod statusline;
pub(crate) mod supervisor;
pub(crate) mod telegram;
pub(crate) mod ticket;
pub(crate) mod tmux_attach;
pub(crate) mod watch;
