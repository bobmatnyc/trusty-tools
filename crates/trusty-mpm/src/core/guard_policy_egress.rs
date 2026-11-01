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
const ARCHITECT: &[Role] = &[Role::Architect];

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

pub static EGRESS_FAMILY: GuardPolicy = policy(
    "GP-043",
    "egress-family",
    "No call to a messaging or mail family (Slack, Gmail, Notion, chat) outside its allowlist.",
    "#8454, #10387",
    ALL,
    true,
    OUTWARD_ACTION,
);

pub static EGRESS_BASH: GuardPolicy = policy(
    "GP-044",
    "egress-bash",
    "No Bash command that reaches a messaging or mail endpoint.",
    "#8454",
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
    "An Architect egress allow stands only once its audit line is written.",
    "#10447",
    ARCHITECT,
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
