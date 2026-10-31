//! The entries of the guard policy table (#10641 PR1).
//!
//! Why: one entry per audit label a guard refusal records today, each with
//! its written source. PR1 records current behaviour only: `applies` and
//! `absolute` describe what the guard does now, not what PR2/PR3 will change.
//! What: one `pub static` per entry, built by `super::policy`, and
//! [`POLICIES`], the table in id order. Ids are stable and never reused.
//! Per ruling D29 item 5, an entry that binds the Architect names the
//! Architect core hard limit it implements (`architect_limit`, a member name
//! of `crate::core::architect_core::ARCHITECT_CORE`).
//! Test: `guard_policy_tests.rs`.

use super::egress::*;
use super::{GuardPolicy, Role, policy};

const ALL: &[Role] = &[Role::Pm, Role::Architect, Role::Agent];
const PM_AGENT: &[Role] = &[Role::Pm, Role::Agent];
const PM_ARCHITECT: &[Role] = &[Role::Pm, Role::Architect];
const PM: &[Role] = &[Role::Pm];
const AGENT: &[Role] = &[Role::Agent];

// The Architect core hard limits an entry cites (`content/instructions/architect/core.md`).
const GUARD_STAYS_ON: Option<&str> = Some("the guard stays on");
const SECRET_LOCATIONS: Option<&str> = Some("secret locations only");
const EXACT_PANE_TARGETS: Option<&str> = Some("exact pane targets");
const CHECKPOINT_FIRST: Option<&str> = Some("checkpoint first");
const SHARED_TARGETS: Option<&str> = Some("shared target directories");

/// A refusal whose audit label has no entry; still a refusal (#10641).
pub static POLICY_LOOKUP_MISS: GuardPolicy = policy(
    "GP-000",
    "policy-lookup-miss",
    "A refusal named no policy-table entry, so the guard denies rather than allows.",
    "#10641 (owner ruling D29)",
    ALL,
    true,
    GUARD_STAYS_ON,
);

pub static UNREADABLE_PAYLOAD: GuardPolicy = policy(
    "GP-001",
    "unreadable-payload",
    "A PreToolUse payload that is empty, unreadable or not a JSON object is denied.",
    "#7975",
    ALL,
    true,
    GUARD_STAYS_ON,
);

pub static UNCLASSIFIABLE_PAYLOAD: GuardPolicy = policy(
    "GP-002",
    "unclassifiable-payload",
    "A payload without a string tool_name and an object tool_input is denied.",
    "#7975",
    ALL,
    false,
    GUARD_STAYS_ON,
);

pub static GUARD_FAILURE: GuardPolicy = policy(
    "GP-003",
    "guard-failure",
    "A guard run that panics or errors before deciding refuses the call, bar the recovery allowlist.",
    "#10600, #10641",
    ALL,
    true,
    GUARD_STAYS_ON,
);

pub static UNCLASSIFIABLE_COMMAND: GuardPolicy = policy(
    "GP-004",
    "unclassifiable-command",
    "A Bash command the guard cannot lex (ANSI-C quoting, unbalanced quotes) is denied.",
    "#6660, #9001",
    ALL,
    true,
    GUARD_STAYS_ON,
);

pub static DESTRUCTIVE_DELETE: GuardPolicy = policy(
    "GP-005",
    "destructive-delete",
    "No delete of a filesystem root, home, the repo root, a .git directory or a worktree root.",
    "#4031",
    ALL,
    true,
    GUARD_STAYS_ON,
);

pub static TRUST_ANCHOR_WRITE: GuardPolicy = policy(
    "GP-006",
    "trust-anchor-write",
    "No write to a guard trust anchor (launch records, user config); the Architect has a narrow exemption.",
    "#8878 D4",
    ALL,
    true,
    GUARD_STAYS_ON,
);

pub static SECRET_FILE_READ: GuardPolicy = policy(
    "GP-007",
    "secret-file-read",
    "No command or Read that prints a secret file or a credential value.",
    "#7266, #8879, #8939",
    ALL,
    true,
    SECRET_LOCATIONS,
);

pub static KUZU_STORE_READ: GuardPolicy = policy(
    "GP-008",
    "kuzu-store-read",
    "No direct read of a kuzu memory store.",
    "#8625 (ruling B10)",
    ALL,
    true,
    SECRET_LOCATIONS,
);

pub static KUZU_IMPORT_PYTHON: GuardPolicy = policy(
    "GP-009",
    "kuzu-import-python",
    "No trusty-memory kuzu import with --python.",
    "#9794 (ruling Q1)",
    ALL,
    true,
    SECRET_LOCATIONS,
);

pub static D4_FLOOR: GuardPolicy = policy(
    "GP-010",
    "d4-floor",
    "No exfiltration upload, raw disk tool, force-push or anchor write outside the Architect.",
    "#8878 D4",
    PM_AGENT,
    true,
    None,
);

pub static ARCHITECT_PANE: GuardPolicy = policy(
    "GP-011",
    "architect-pane",
    "No tmux verb aimed at the Architect's pane from any other session.",
    "#8902",
    PM_AGENT,
    true,
    None,
);

pub static TMUX_TARGET_UNRESOLVED: GuardPolicy = policy(
    "GP-012",
    "tmux-target-unresolved",
    "A tmux target must resolve exactly to an existing pane, window or session.",
    "#9001",
    ALL,
    true,
    EXACT_PANE_TARGETS,
);

pub static FLEET_RELAUNCH: GuardPolicy = policy(
    "GP-013",
    "fleet-relaunch",
    "No hooked session runs tm fleet relaunch, the Architect included.",
    "#10033",
    ALL,
    true,
    CHECKPOINT_FIRST,
);

pub static READ_ONLY_DISPATCH: GuardPolicy = policy(
    "GP-014",
    "read-only-dispatch",
    "A read-only dispatch runs only allowlisted read shapes.",
    "#8439",
    AGENT,
    false,
    None,
);

pub static WORKTREE_ADD: GuardPolicy = policy(
    "GP-015",
    "worktree-add",
    "No git worktree add under a temp root or past the disk threshold.",
    "#3977, #7497",
    ALL,
    false,
    GUARD_STAYS_ON,
);

pub static SECRET_FILE_COPY: GuardPolicy = policy(
    "GP-016",
    "secret-file-copy",
    "No shell copy or move of a secret-shaped file into a worktree.",
    "#7122",
    ALL,
    false,
    SECRET_LOCATIONS,
);

pub static MAIN_CHECKOUT_DESTRUCTIVE: GuardPolicy = policy(
    "GP-017",
    "main-checkout-destructive",
    "No command that destroys uncommitted work in a main checkout.",
    "ADR-0037",
    PM_AGENT,
    false,
    None,
);

pub static HEAD_SWITCH: GuardPolicy = policy(
    "GP-018",
    "head-switch",
    "No HEAD switch in a main checkout over another session's uncommitted edit.",
    "#8572",
    ALL,
    false,
    GUARD_STAYS_ON,
);

pub static MAIN_CHECKOUT_COMMIT: GuardPolicy = policy(
    "GP-019",
    "main-checkout-commit",
    "No commit in a main checkout beyond a docs-and-config set.",
    "ADR-0044, ADR-0048, ADR-0049",
    PM_AGENT,
    false,
    None,
);

pub static DOCS_COMMIT: GuardPolicy = policy(
    "GP-020",
    "docs-commit",
    "No docs commit in a main checkout another live writer stands in.",
    "ADR-0049",
    PM_AGENT,
    false,
    None,
);

pub static HEAD_MOVE: GuardPolicy = policy(
    "GP-021",
    "head-move",
    "No merge or rebase of a main checkout another live writer stands in.",
    "ADR-0048 decision 10, ADR-0053, #9127",
    PM_AGENT,
    false,
    None,
);

pub static LINKED_WORKTREE_HEAD_MOVE: GuardPolicy = policy(
    "GP-022",
    "linked-worktree-head-move",
    "No HEAD move into a linked worktree a live agent holds.",
    "#8161",
    PM_AGENT,
    false,
    None,
);

pub static MAIN_CHECKOUT_WRITE: GuardPolicy = policy(
    "GP-023",
    "main-checkout-write",
    "No edit-tool write into a main checkout outside the write boundary.",
    "ADR-0044, ADR-0048",
    PM_AGENT,
    false,
    None,
);

pub static PROTECTED_PATH_WRITE: GuardPolicy = policy(
    "GP-024",
    "protected-path-write",
    "No write to settings, .mcp.json, git config or hooks, or hook-run paths.",
    "#8928",
    PM_AGENT,
    false,
    None,
);

pub static SUBAGENT_FANOUT: GuardPolicy = policy(
    "GP-025",
    "subagent-fanout",
    "A subagent never dispatches further subagents.",
    "#4784",
    AGENT,
    false,
    None,
);

pub static ENTER_WORKTREE: GuardPolicy = policy(
    "GP-026",
    "enter-worktree",
    "A worktree-pinned agent never switches worktrees.",
    "#7172",
    AGENT,
    false,
    None,
);

pub static WORKTREE_REMOVE: GuardPolicy = policy(
    "GP-027",
    "worktree-remove",
    "An agent never removes a worktree; the PM does.",
    "#5791, ADR-0057",
    AGENT,
    false,
    None,
);

pub static WORKTREE_REMOVE_RECHECK: GuardPolicy = policy(
    "GP-028",
    "worktree-remove-recheck",
    "A version-control removal proceeds only once clean, pushed, merged and unowned are proven.",
    "ADR-0057, #7889",
    AGENT,
    false,
    None,
);

pub static RESUME_WORKTREE: GuardPolicy = policy(
    "GP-029",
    "resume-worktree",
    "No SendMessage that resumes an agent whose isolated worktree is gone.",
    "#8004",
    PM_ARCHITECT,
    false,
    GUARD_STAYS_ON,
);

pub static DISPATCH_CONTENT: GuardPolicy = policy(
    "GP-030",
    "dispatch-content",
    "A dispatch is classified against the installed content roster; with none it is refused.",
    "#9011 (owner ruling 09(a))",
    PM_ARCHITECT,
    false,
    GUARD_STAYS_ON,
);

pub static WORKTREE_GRANT: GuardPolicy = policy(
    "GP-031",
    "worktree-grant",
    "A writer dispatched from a main checkout gets a worktree or is refused.",
    "ADR-0048",
    PM_ARCHITECT,
    false,
    GUARD_STAYS_ON,
);

pub static WORKTREE_GRANT_DISPATCH: GuardPolicy = policy(
    "GP-032",
    "worktree-grant-dispatch",
    "A granted dispatch is refused while another live writer holds the checkout.",
    "ADR-0048, #5769",
    PM_ARCHITECT,
    false,
    GUARD_STAYS_ON,
);

pub static DISPATCH: GuardPolicy = policy(
    "GP-033",
    "dispatch",
    "No second file-mutating agent in a working directory that already has one.",
    "#4480, #5923",
    PM_ARCHITECT,
    false,
    GUARD_STAYS_ON,
);

pub static AGENT_COST: GuardPolicy = policy(
    "GP-034",
    "agent-cost",
    "A subagent past its context ceiling is stopped.",
    "#4837",
    AGENT,
    false,
    None,
);

pub static BUILD_LEASE: GuardPolicy = policy(
    "GP-035",
    "build-lease",
    "A heavy build runs under tm build-lease or not at all.",
    "#8261",
    ALL,
    false,
    SHARED_TARGETS,
);

pub static PERSONA_DENY_BY_DEFAULT: GuardPolicy = policy(
    "GP-036",
    "persona-deny-by-default",
    "Opt-in: a subagent whose persona failed is refused guarded tools.",
    "#2231",
    AGENT,
    false,
    None,
);

pub static FILE_CHANGE_BUDGET: GuardPolicy = policy(
    "GP-037",
    "file-change-budget",
    "A PM's direct file changes are capped per turn; past the cap it delegates.",
    "#2918 (prohibitions P1, P5)",
    PM,
    false,
    None,
);

pub static PM_TOOL_POLICY: GuardPolicy = policy(
    "GP-038",
    "pm-tool-policy",
    "A PM delegates builds, tests, network calls and source edits.",
    "#1977 (prohibitions P1-P11)",
    PM,
    false,
    None,
);

/// #10641: the in-process watchdog's refusal; the recovery allowlist
/// (read-only tools, `tm doctor`, `tm install`, `cargo install trusty-mpm`)
/// still runs.
pub static GUARD_TIMEOUT: GuardPolicy = policy(
    "GP-049",
    "guard-timeout",
    "A guard run that has not decided by its 8 s deadline refuses the call, bar the recovery allowlist.",
    "#10600, #10641",
    ALL,
    true,
    GUARD_STAYS_ON,
);

/// Every entry once, in id order.
pub static POLICIES: &[&GuardPolicy] = &[
    &POLICY_LOOKUP_MISS,
    &UNREADABLE_PAYLOAD,
    &UNCLASSIFIABLE_PAYLOAD,
    &GUARD_FAILURE,
    &UNCLASSIFIABLE_COMMAND,
    &DESTRUCTIVE_DELETE,
    &TRUST_ANCHOR_WRITE,
    &SECRET_FILE_READ,
    &KUZU_STORE_READ,
    &KUZU_IMPORT_PYTHON,
    &D4_FLOOR,
    &ARCHITECT_PANE,
    &TMUX_TARGET_UNRESOLVED,
    &FLEET_RELAUNCH,
    &READ_ONLY_DISPATCH,
    &WORKTREE_ADD,
    &SECRET_FILE_COPY,
    &MAIN_CHECKOUT_DESTRUCTIVE,
    &HEAD_SWITCH,
    &MAIN_CHECKOUT_COMMIT,
    &DOCS_COMMIT,
    &HEAD_MOVE,
    &LINKED_WORKTREE_HEAD_MOVE,
    &MAIN_CHECKOUT_WRITE,
    &PROTECTED_PATH_WRITE,
    &SUBAGENT_FANOUT,
    &ENTER_WORKTREE,
    &WORKTREE_REMOVE,
    &WORKTREE_REMOVE_RECHECK,
    &RESUME_WORKTREE,
    &DISPATCH_CONTENT,
    &WORKTREE_GRANT,
    &WORKTREE_GRANT_DISPATCH,
    &DISPATCH,
    &AGENT_COST,
    &BUILD_LEASE,
    &PERSONA_DENY_BY_DEFAULT,
    &FILE_CHANGE_BUDGET,
    &PM_TOOL_POLICY,
    &EGRESS_UNREADABLE_PAYLOAD,
    &EGRESS_UNCLASSIFIABLE_PAYLOAD,
    &EGRESS_UNNAMED_RESOURCE_SERVER,
    &EGRESS_MALFORMED_MCP_NAME,
    &EGRESS_FAMILY,
    &EGRESS_BASH,
    &EGRESS_CHANNEL_TAINTED,
    &EGRESS_POLICY_NOT_READY,
    &EGRESS_ARCHITECT_AUDIT_UNWRITABLE,
    &EGRESS_GUARD_PANIC,
    &GUARD_TIMEOUT,
];
