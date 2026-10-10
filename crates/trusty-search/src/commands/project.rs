//! `trusty-search project map|park|unpark|pin|rebuild-map` (#9169 PR-B1).
//!
//! Why: the operator's view of `project-map.toml` — which index answers for
//! each project, and which duplicates are parked.
//! What: `map`, `park`, `unpark` and `pin` call the daemon's
//! `search.project.*` methods over the socket and print the JSON answer.
//! `rebuild-map` runs in this process against the data dir: it moves the
//! current map aside to `project-map.toml.corrupt-<unix>` and writes a fresh
//! migration. It is the one way past a map the daemon refuses to read.
//! Test: `project_map_state_tests.rs` covers the rules these call.

use anyhow::{Context as _, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use trusty_search::service::rpc::project_map::{
    METHOD_PROJECT_MAP, METHOD_PROJECT_PARK, METHOD_PROJECT_PIN, METHOD_PROJECT_UNPARK,
};

use super::daemon_rpc;

/// `trusty-search project <action>`.
#[derive(Args, Debug)]
pub struct ProjectCommand {
    #[command(subcommand)]
    pub action: ProjectAction,
}

/// The project-map actions.
#[derive(Subcommand, Debug)]
pub enum ProjectAction {
    /// Print the project map: each project's pinned index and every parked
    /// index.
    ///
    /// Examples:
    ///   trusty-search project map
    Map,

    /// Park an index: keep its corpus and registration, stop serving,
    /// watching and reindexing it. Only a worktree, an orphaned root or a
    /// same-root sibling parks; a main checkout is refused.
    ///
    /// Examples:
    ///   trusty-search project park apex-feat-x
    Park {
        /// The index id to park.
        index_id: String,
    },

    /// Unpark an index and queue its reindex.
    ///
    /// Examples:
    ///   trusty-search project unpark apex-feat-x
    Unpark {
        /// The index id to unpark.
        index_id: String,
    },

    /// Pin an index as the one its project resolves to.
    ///
    /// Examples:
    ///   trusty-search project pin apex-9a4a584b
    Pin {
        /// The index id to pin.
        index_id: String,
    },

    /// Operator only: move `project-map.toml` aside and rebuild it from the
    /// registry. Pins and parks in the old file are not carried over.
    ///
    /// Examples:
    ///   trusty-search project rebuild-map --yes
    RebuildMap {
        /// Confirm the rebuild; without it nothing changes.
        #[arg(long)]
        yes: bool,
    },
}

/// Run one `trusty-search project` action.
///
/// # Errors
///
/// When the daemon is unreachable or refuses the call, or — for
/// `rebuild-map` — when the registry cannot be read or the map not moved.
pub async fn handle_project(cmd: ProjectCommand) -> Result<()> {
    let (method, params) = match cmd.action {
        ProjectAction::Map => (METHOD_PROJECT_MAP, Value::Null),
        ProjectAction::Park { index_id } => (METHOD_PROJECT_PARK, json!({ "index_id": index_id })),
        ProjectAction::Unpark { index_id } => {
            (METHOD_PROJECT_UNPARK, json!({ "index_id": index_id }))
        }
        ProjectAction::Pin { index_id } => (METHOD_PROJECT_PIN, json!({ "index_id": index_id })),
        ProjectAction::RebuildMap { yes } => return rebuild_map(yes).await,
    };
    let client = daemon_rpc::connect().await?;
    let body = daemon_rpc::call(&client, method, params).await?;
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

/// `project rebuild-map`: quarantine the current map and migrate afresh.
async fn rebuild_map(yes: bool) -> Result<()> {
    anyhow::ensure!(
        yes,
        "rebuild-map moves project-map.toml aside and drops its pins and parks; \
         re-run with --yes to do it"
    );
    let registry = trusty_search::service::persistence::indexes_toml_path()?;
    let runtime = tokio::runtime::Handle::current();
    let (moved, map) = tokio::task::spawn_blocking(move || {
        trusty_search::service::project_map_state::rebuild_map(&registry, Some(runtime))
    })
    .await
    .context("rebuild-map task failed")??;
    if let Some(aside) = moved {
        println!("moved the old map to {}", aside.display());
    }
    println!(
        "rebuilt project map: {} pinned project(s), nothing parked",
        map.projects.len()
    );
    Ok(())
}
