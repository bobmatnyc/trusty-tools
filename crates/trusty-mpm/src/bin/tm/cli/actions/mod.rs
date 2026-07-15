//! Sub-subcommand ("action") enums for every `tm <group> <verb>` command
//! group referenced by [`super::Command`].
//!
//! Why (issue #2603): `cli.rs` grew to 882 SLOC — every command group's
//! action enum was inlined in one file, past the 500-SLOC production cap.
//! This mirrors the #170-#172 precedent (`ctrl/mod.rs`, `runtime/`,
//! `workflow/engine/`): one focused submodule per logical concept, re-exported
//! through a thin facade so every existing `crate::cli::<Type>` reference
//! (`main.rs`, `commands::*`, `tests*.rs`) keeps compiling unchanged.
//! What: declares one submodule per command group and re-exports its public
//! action enum(s)/arg-struct(s) at `cli::actions::*` (and, via `cli/mod.rs`,
//! at `cli::*`). Purely structural — no behavior, signature, or doc-content
//! change versus the pre-split `cli.rs`.
//! Test: unchanged — every `cli_parses_*` test in `tests.rs` /
//! `tests_projects.rs` / `tests_projects_config_tests.rs` / `tests_manager.rs`
//! still exercises these types through the same `crate::cli::<Type>` paths.

mod auth;
mod catalog;
mod coordinator;
mod deliverables;
mod issue;
mod mcp;
mod meta;
mod optimizer;
mod overseer;
mod project;
mod projects;
mod repair;
mod services;
mod sessctl;
mod session;
mod slack;
mod telegram;
mod watch;

pub(crate) use auth::AuthAction;
pub(crate) use catalog::CatalogAction;
pub(crate) use coordinator::CoordinatorAction;
pub(crate) use deliverables::{
    DeliverableKindArg, DeliverableStatusArg, DeliverablesAction, EstimationTierArg,
    MilestonesAction,
};
pub(crate) use issue::IssueCmd;
pub(crate) use mcp::{McpCmd, McpTransportArg};
pub(crate) use meta::MetaAction;
pub(crate) use optimizer::{CliCompressionLevel, OptimizerAction};
pub(crate) use overseer::OverseerAction;
pub(crate) use project::ProjectAction;
pub(crate) use projects::{
    ClearableConfigField, ConfigAction, ProjectsAction, SettableConfigField,
};
pub(crate) use repair::RepairAction;
pub(crate) use services::ServicesAction;
pub(crate) use sessctl::SessctlAction;
pub(crate) use session::SessionAction;
pub(crate) use slack::SlackCmd;
pub(crate) use telegram::TelegramCmd;
pub(crate) use watch::{WatchArgs, WatchCmd};
