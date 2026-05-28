//! Native ticketing tools (#132/#133/#243/#246).
//!
//! Why: Expose `TicketingClient` operations as strongly-typed LLM tools so
//! agents don't have to shell out to `gh` / JIRA CLI / Linear CLI. Each tool
//! wraps an `Arc<dyn TicketingClient>` so the provider can be swapped by
//! config without changing tool wiring.
//! What: Six CRUD tools (`crud`), four workflow tools (`workflow`), and two
//! GitHub Actions tools (`actions`). `ticketing_tools` assembles the full set.
//! Test: Construction and minimal error-path tests in `tests`, using a
//! mock `TicketingClient` implementation.

use std::sync::Arc;

use crate::ticketing::TicketingClient;
use crate::ticketing::actions::ActionsClient;
use crate::tools::traits::ToolExecutor;

mod actions;
mod crud;
mod workflow;

pub use actions::{ActionsStatusTool, ActionsTriggerTool};
pub use crud::{
    AddCommentTool, CloseTicketTool, CreateTicketTool, GetTicketTool, ListTicketsTool,
    UpdateTicketTool,
};
pub use workflow::{TicketAssignTool, TicketSearchTool, TicketTagTool, TicketTransitionTool};

/// Build the full set of ticketing + actions tools (#243).
///
/// Why: Centralizes the "what tools does the ticketing agent get" decision
/// so callers (ctrl, PM, ticketing-agent runner) all get the same set
/// without copy/pasting eight `Arc::new`s.
/// What: Returns 10 ticketing tools (always — the 6 originals plus
/// `ticket_tag`, `ticket_assign`, `ticket_transition`, `ticket_search`
/// from #246) plus 2 actions tools when an `actions` client is provided.
/// When `actions` is `None`, only the 10 ticketing tools are returned.
/// Test: `ticketing_tools_count`, `ticketing_tools_count_is_12`.
pub fn ticketing_tools(
    client: Arc<dyn TicketingClient>,
    actions: Option<Arc<dyn ActionsClient>>,
) -> Vec<Arc<dyn ToolExecutor>> {
    let mut out: Vec<Arc<dyn ToolExecutor>> = vec![
        Arc::new(CreateTicketTool(client.clone())),
        Arc::new(GetTicketTool(client.clone())),
        Arc::new(UpdateTicketTool(client.clone())),
        Arc::new(CloseTicketTool(client.clone())),
        Arc::new(ListTicketsTool(client.clone())),
        Arc::new(AddCommentTool(client.clone())),
        Arc::new(TicketTagTool(client.clone())),
        Arc::new(TicketAssignTool(client.clone())),
        Arc::new(TicketTransitionTool(client.clone())),
        Arc::new(TicketSearchTool(client)),
    ];
    if let Some(a) = actions {
        out.push(Arc::new(ActionsTriggerTool { client: a.clone() }));
        out.push(Arc::new(ActionsStatusTool { client: a }));
    }
    out
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
