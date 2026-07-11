//! `tm projects` command group (DOC-35 §3.1/§10.8, #2115/#2381).
//!
//! Why: the deterministic CLI half of the project control plane. The verb tree is
//! large (four registry verbs + two nested CRUD subtrees), so it is split per
//! subtree — `registry` (list/register/show/status), `deliverables`, and
//! `milestones` — each a sibling file well under the 500-SLOC cap, with this
//! `mod.rs` a thin dispatcher plus the shared clap-arg → domain-type conversions.
//! What: [`projects`] routes a [`ProjectsAction`] to the right subtree handler;
//! the `convert` submodule maps the CLI value enums to `trusty_mpm::deliverable`
//! domain types so `cli.rs` stays free of a domain dependency.
//! Test: `cli_parses_projects_*` (parse) in `tests_projects.rs`; the per-subtree
//! rendering/serialization tests live in each submodule.

pub(crate) mod convert;
pub(crate) mod deliverables;
pub(crate) mod milestones;
pub(crate) mod registry;

use crate::cli::ProjectsAction;

/// Dispatch a `tm projects <action>` invocation to its subtree handler.
///
/// Why: `main.rs` stays a thin bootstrap; all `projects` routing lives here.
/// What: matches the top-level action and forwards to the registry verb handlers
/// or the deliverables/milestones subtree dispatchers, threading the CLI's shared
/// `(reqwest::Client, url)` pair through to the typed `DaemonClient` methods.
/// Test: exercised end-to-end by the daemon integration tests; parse coverage in
/// `tests_projects.rs`.
pub(crate) async fn projects(
    client: &reqwest::Client,
    url: &str,
    action: ProjectsAction,
) -> anyhow::Result<()> {
    match action {
        ProjectsAction::List { json, tag } => registry::list(client, url, json, tag).await,
        ProjectsAction::Register {
            name,
            repo_url,
            default_branch,
            description,
            tags,
            stack_hint,
            gh_user,
        } => {
            registry::register(
                client,
                url,
                registry::RegisterInput {
                    name,
                    repo_url,
                    default_branch,
                    description,
                    tags,
                    stack_hint,
                    gh_user,
                },
            )
            .await
        }
        ProjectsAction::Show { name, json } => registry::show(client, url, &name, json).await,
        ProjectsAction::Status { name, json } => registry::status(client, url, &name, json).await,
        ProjectsAction::Deliverables { action } => {
            deliverables::dispatch(client, url, action).await
        }
        ProjectsAction::Milestones { action } => milestones::dispatch(client, url, action).await,
    }
}
