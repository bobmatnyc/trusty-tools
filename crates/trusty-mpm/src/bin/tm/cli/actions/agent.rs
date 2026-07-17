//! `tm agent` — inspect the deployed agent roster's declared skills (DOC-42).
//!
//! Why: `docs/specs/agent-bundled-skills.md` §SPEC-AGENTSKILLS-04 requires
//! that `skills:` declarations and their 3-tier resolution be visible from
//! the CLI, so an operator debugging a missing-skill error can see, per
//! agent, which skills it declares and whether each one resolves.
//! What: [`AgentAction`] — `list` (every deployed agent, one line each) and
//! `show <name>` (one agent's full metadata plus per-skill resolution).
//! Test: `cli_parses_agent_list`, `cli_parses_agent_show` in `tests.rs`.

use clap::Subcommand;

/// Actions for the `agent` subcommand.
///
/// Why: a roster-wide summary and a single-agent deep-dive are the two
/// distinct read shapes an operator needs — mirroring `tm catalog ls` (list)
/// versus a hypothetical per-item show.
/// What: `List` prints every deployed agent with its declared skills; `Show`
/// prints one agent's full frontmatter plus each declared skill's resolved
/// tier (or `NOT FOUND`).
/// Test: `cli_parses_agent_list`, `cli_parses_agent_show`,
/// `cli_parses_agent_list_json`.
#[derive(Debug, Subcommand)]
pub(crate) enum AgentAction {
    /// List every deployed agent with its declared skills.
    List {
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Show one agent's metadata and per-skill resolution.
    Show {
        /// Agent name (the deployed file's stem, e.g. `code-critic`).
        name: String,
        /// Output as JSON instead of formatted text.
        #[arg(long)]
        json: bool,
    },
}
