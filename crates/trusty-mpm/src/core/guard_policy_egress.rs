//! The `tm hook --egress-guard` entries of the guard policy table (#10641 PR1).
//!
//! Why: split from `guard_policy_entries.rs` to keep both under the 500-SLOC
//! cap; the egress guard prints its refusals through the same pm-guard deny
//! builder, so its rules need entries too.
//! What: one `pub static` per egress audit label, built by `super::policy`;
//! `guard_policy_entries.rs` lists them in `POLICIES`.
//! Test: `guard_policy_tests.rs`.

use super::{GuardPolicy, Role, policy};

const ALL: &[Role] = &[Role::Pm, Role::Architect, Role::Agent];
const MAIN_THREADS: &[Role] = &[Role::Pm, Role::Architect];

// The Architect core hard limits an entry cites (`content/instructions/architect/core.md`).
const GUARD_STAYS_ON: Option<&str> = Some("the guard stays on");
const OUTWARD_ACTION: Option<&str> = Some("no unruled outward action");

/// The `tm hook --egress-guard` entries share one source (#8454).
pub static EGRESS_UNREADABLE_PAYLOAD: GuardPolicy = policy(
    "GP-039",
    "egress-unreadable-payload",
    "The egress guard denies a payload it cannot read.",
    "#8454",
    ALL,
    true,
    GUARD_STAYS_ON,
);

pub static EGRESS_UNCLASSIFIABLE_PAYLOAD: GuardPolicy = policy(
    "GP-040",
    "egress-unclassifiable-payload",
    "The egress guard denies a payload it cannot classify.",
    "#8454",
    ALL,
    true,
    GUARD_STAYS_ON,
);

pub static EGRESS_UNNAMED_RESOURCE_SERVER: GuardPolicy = policy(
    "GP-041",
    "egress-unnamed-resource-server",
    "An MCP resource call must name its server.",
    "#8454",
    ALL,
    true,
    GUARD_STAYS_ON,
);

pub static EGRESS_MALFORMED_MCP_NAME: GuardPolicy = policy(
    "GP-042",
    "egress-malformed-mcp-name",
    "An MCP tool name the guard cannot parse is denied.",
    "#8454",
    ALL,
    true,
    GUARD_STAYS_ON,
);

// #11521: a PM or Architect stamped main thread may read, send and write,
// with no launch record (the #11273 record rule was never asked for).
// GP-043 (owner ruling 2026-11-16): a PM's subagents may also read Gmail and
// Google Workspace; every other subagent call is unchanged.
pub static EGRESS_FAMILY: GuardPolicy = policy(
    "GP-043",
    "egress-family",
    "No messaging or mail family call (Slack, Gmail, Notion, chat) from a subagent, an unknown thread or an unstamped session. A main thread with a valid PM or Architect stamp may read, send and write with no launch record and no bind step (#11521); a PM's subagent may read Gmail and Google Workspace. Every allow writes one audit line or denies.",
    "#8454, #10387, #10447, #11194, #11273, #11521",
    ALL,
    true,
    OUTWARD_ACTION,
);

// #11273: inert text naming a family host passes for every role.
pub static EGRESS_BASH: GuardPolicy = policy(
    "GP-044",
    "egress-bash",
    "No Bash command that reaches a messaging or mail endpoint or runs a sender, without the tm launch record.",
    "#8454, #10447, #11273",
    ALL,
    true,
    OUTWARD_ACTION,
);

pub static EGRESS_CHANNEL_TAINTED: GuardPolicy = policy(
    "GP-045",
    "egress-channel-tainted",
    "No outward call from a session whose channel is tainted.",
    "#8433, #8454",
    ALL,
    true,
    OUTWARD_ACTION,
);

pub static EGRESS_POLICY_NOT_READY: GuardPolicy = policy(
    "GP-046",
    "egress-policy-not-ready",
    "A tainted call is denied while the egress policy cannot be loaded.",
    "#9741, #8454",
    ALL,
    true,
    OUTWARD_ACTION,
);

pub static EGRESS_ARCHITECT_AUDIT_UNWRITABLE: GuardPolicy = policy(
    "GP-047",
    "egress-architect-audit-unwritable",
    "A main-session egress allow (Architect send, PM owner-DM send, family read) stands only once its audit line is written.",
    "#10447, #11194, #11273",
    MAIN_THREADS,
    true,
    OUTWARD_ACTION,
);

pub static EGRESS_GUARD_PANIC: GuardPolicy = policy(
    "GP-048",
    "egress-guard-panic",
    "An egress guard run that panics denies the call.",
    "#8454",
    ALL,
    true,
    GUARD_STAYS_ON,
);
