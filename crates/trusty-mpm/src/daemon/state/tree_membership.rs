//! Which delegation records count as writing in a given tree (#8535, #8161).
//!
//! Why: the shared-tree filter keyed each record on the directory its
//! DISPATCHER stood in (`Delegation::cwd`), with one exception for an agent
//! standing in the worktree it was granted (#6556). That key goes wrong in both
//! directions once the harness moves the PM's own cwd into an agent's worktree
//! (#8535). A dispatch made from there is stamped with that worktree even when
//! the harness then puts the agent in a tree of its own, and until the agent's
//! ownership claim lands the record read as a second writer in the PM's
//! directory — the reported deny. And the agent that owns the worktree was never
//! counted in it at all, because its record carries the checkout it was
//! dispatched from. The #8161 consolidation rule asks exactly that second
//! question: is a live agent standing in this parked worktree?
//!
//! What: [`writes_in`], the WHERE half of the filter. Liveness, the session
//! exclusion and the tool-use exclusion stay with the caller in `sessions.rs`.
//! The evidence it prefers is `Delegation::last_agent_cwd` — the cwd of the
//! agent's own latest hook event, which is a statement about the present, not a
//! declaration — resolved to its worktree root lexically, so a subdirectory and
//! its tree compare equal.
//!
//! Test: `a_live_agent_is_counted_in_the_worktree_it_stands_in`,
//! `a_record_stamped_in_a_relocated_pm_cwd_is_not_a_writer_there`,
//! `a_read_only_agent_in_a_linked_worktree_is_not_a_writer`, plus every
//! pre-existing shared-tree test in `super::tests`.

use std::path::Path;

use crate::core::agent::Delegation;
use crate::core::dispatch_isolation::{AgentWriteRisk, agent_write_risk, shares_the_callers_tree};
use crate::core::project_aliases::worktree_root;

/// Does `d` write in the tree `cwd` names?
///
/// Why: see the module doc.
/// What, in order:
///
/// 1. `cwd` is a linked worktree and the agent's last hook ran inside that
///    same worktree: it writes there, unless its bundled definition is
///    positively read-only. Its declared isolation and its dispatcher's
///    directory are irrelevant — it is standing here now (#8161).
/// 2. The record was not stamped at `cwd`: not a writer here.
/// 3. The record was stamped at `cwd` but the agent's last hook ran inside a
///    DIFFERENT linked worktree: not a writer here (#8535). This subsumes
///    #6556's granted-tree test, which needed the ownership claim to have
///    landed first.
/// 4. Otherwise the declared isolation decides, as before.
///
/// Test: see the module doc; `an_agent_that_leaves_its_worktree_blocks_the_shared_tree_again`
/// pins that an agent back in the dispatcher's checkout still counts.
pub(super) fn writes_in(d: &Delegation, cwd: &Path) -> bool {
    let here = worktree_root(cwd);
    let standing = d.last_agent_cwd.as_deref().and_then(worktree_root);
    // #8161: a live agent standing in this linked worktree writes in it.
    if here.is_some() && standing == here {
        return agent_write_risk(&d.agent) != AgentWriteRisk::ReadsOnly;
    }
    if d.cwd.as_deref() != Some(cwd) {
        return false;
    }
    // #8535: stamped here, standing in another harness tree — positively elsewhere.
    if standing.is_some() {
        return false;
    }
    shares_the_callers_tree(&d.agent, d.isolation.as_deref())
}
