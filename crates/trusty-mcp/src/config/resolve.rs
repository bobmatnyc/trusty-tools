//! Layering a global MCP server list with per-consumer overrides (#7452).
//!
//! Why: the owner requirement behind #7451 is that an MCP connector is
//! configurable per assistant AS WELL AS globally. Both trusty-agents and
//! trusty-code need the same answer to "given the shared file and this
//! assistant's own entries, what does this assistant actually connect to",
//! and two independent implementations of that precedence would drift the way
//! the four config shapes did.
//!
//! What: [`resolve`] applies a list of [`McpServerOverride`]s to a global
//! list. Precedence is WHOLESALE, never a field merge: an override that names
//! a server replaces it entirely. A partial merge would make an override's
//! meaning depend on which fields the global entry happened to set, so
//! removing a global `env` key would be impossible to express.
//!
//! Test: `crates/trusty-mcp/tests/mcp_config.rs` — `resolve_set_replaces_wholesale`,
//! `resolve_disable_removes_global_entry`, `resolve_set_reenables_disabled_global`,
//! `resolve_orders_global_first_then_new_additions`.

use std::collections::BTreeMap;

use super::McpServerConfig;

/// One change a consumer's own tier makes to the global list.
///
/// Why: the two operations every consumer needs — "use this instead" and
/// "do not use this at all". Disabling deserves its own variant rather than a
/// `Set` carrying `enabled = false`, because an assistant that merely wants a
/// globally configured server switched off should not have to restate the
/// server's entire transport to say so.
/// What: `Set` supplies a complete replacement (or an addition, when the name
/// is not in the global list); `Disable` removes the global entry by name.
/// Test: `resolve_set_replaces_wholesale`, `resolve_disable_removes_global_entry`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpServerOverride {
    /// Replace, or add, the server with this configuration.
    Set(McpServerConfig),
    /// Drop the global server with this name.
    Disable {
        /// The global server's name.
        name: String,
    },
}

impl McpServerOverride {
    /// The server name this override acts on.
    ///
    /// Why: both variants key on a name, and callers (and [`resolve`]) should
    /// not match to get at it.
    /// Test: `resolve_orders_global_first_then_new_additions`.
    pub fn name(&self) -> &str {
        match self {
            McpServerOverride::Set(config) => &config.name,
            McpServerOverride::Disable { name } => name,
        }
    }
}

/// The effective server list for one consumer.
///
/// Why: one deterministic answer, so an assistant's connector list does not
/// depend on map iteration order or on which crate computed it.
/// What: overrides are folded left to right, so a later override for the same
/// name wins over an earlier one — including a `Set` after a `Disable`, which
/// is how a consumer re-enables something the global tier turned off. The
/// result is then, in order:
///
/// 1. every global server whose name no override disabled, each replaced
///    wholesale by its `Set` override when one exists (the global entry's
///    POSITION is kept, so overriding a server does not move it);
/// 2. every `Set` naming a server absent from the global list, in the order
///    those overrides first appeared.
///
/// A global list carrying a duplicate name keeps only its first occurrence —
/// [`McpConfigFile`](super::McpConfigFile) rejects such a file, so this is the
/// defensive branch for a list assembled in memory.
/// Test: `resolve_set_replaces_wholesale`, `resolve_disable_removes_global_entry`,
/// `resolve_set_reenables_disabled_global`,
/// `resolve_orders_global_first_then_new_additions`,
/// `resolve_later_override_wins_over_earlier`,
/// `resolve_drops_duplicate_global_names`.
pub fn resolve(
    global: &[McpServerConfig],
    overrides: &[McpServerOverride],
) -> Vec<McpServerConfig> {
    // Last write wins per name, which is what makes a Set after a Disable
    // re-enable rather than the fold order mattering to the caller.
    let mut effective: BTreeMap<&str, &McpServerOverride> = BTreeMap::new();
    for over in overrides {
        effective.insert(over.name(), over);
    }

    let mut out: Vec<McpServerConfig> = Vec::with_capacity(global.len() + overrides.len());
    let mut emitted: Vec<&str> = Vec::with_capacity(out.capacity());

    for server in global {
        if emitted.contains(&server.name.as_str()) {
            continue;
        }
        match effective.get(server.name.as_str()) {
            Some(McpServerOverride::Disable { .. }) => {
                // Recorded as emitted so a duplicate global entry under the
                // same name cannot slip past the disable.
                emitted.push(server.name.as_str());
            }
            Some(McpServerOverride::Set(replacement)) => {
                emitted.push(server.name.as_str());
                out.push(replacement.clone());
            }
            None => {
                emitted.push(server.name.as_str());
                out.push(server.clone());
            }
        }
    }

    // Additions keep the override list's own order, not the map's.
    for over in overrides {
        if let McpServerOverride::Set(config) = over {
            if emitted.contains(&config.name.as_str()) {
                continue;
            }
            // `effective` holds the LAST override for this name, which is the
            // one that wins even when an earlier Set appeared first.
            if let Some(McpServerOverride::Set(winner)) = effective.get(config.name.as_str()) {
                emitted.push(config.name.as_str());
                out.push((*winner).clone());
            } else {
                // The last override for this name is a Disable, so the
                // addition never happens.
                emitted.push(config.name.as_str());
            }
        }
    }

    out
}
