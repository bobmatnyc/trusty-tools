//! The agent roster `tm hook --pm-guard` classifies dispatches against (#9011).
//!
//! Why: the shared-tree race check (#4480) and the main-checkout worktree rule
//! (ADR-0048) both ask what a dispatched agent IS, and since #9011 the answer
//! lives in instructional content, not in the binary. With no content, every
//! bundled engineer becomes an unknown name and the race check would admit two
//! unisolated writers into one git HEAD. Owner ruling 09(a) on #9011: the guard
//! REFUSES instead (fails closed).
//! What: [`dispatch_roster`] resolves the roster once for a PM's own
//! subagent dispatch; any other call needs none and gets `Ok(None)`. A content
//! error becomes the deny reason, which names `tm content install`.
//! Test: `a_dispatch_is_refused_when_content_is_missing`,
//! `a_non_dispatch_call_resolves_nothing`.

use trusty_mpm::core::agent::is_subagent_dispatch_tool;
use trusty_mpm::core::content_source::{AgentContentError, AgentRoster};

/// The roster for this call: `Ok(Some)` for a PM dispatch whose roster
/// resolved, `Ok(None)` when the call needs no classification, `Err(reason)`
/// (DENY) when a PM dispatch cannot be classified.
pub(crate) fn dispatch_roster(
    caller_is_subagent: bool,
    tool_name: &str,
) -> Result<Option<AgentRoster>, String> {
    dispatch_roster_with(
        caller_is_subagent,
        tool_name,
        trusty_mpm::core::content_source::agent_roster,
    )
}

/// [`dispatch_roster`] with the roster resolver given.
pub(crate) fn dispatch_roster_with(
    caller_is_subagent: bool,
    tool_name: &str,
    resolve: impl FnOnce() -> Result<AgentRoster, AgentContentError>,
) -> Result<Option<AgentRoster>, String> {
    if caller_is_subagent || !is_subagent_dispatch_tool(tool_name) {
        return Ok(None);
    }
    resolve().map(Some).map_err(|err| deny_reason(&err))
}

/// The refusal text for a dispatch made with no agent roster.
fn deny_reason(err: &AgentContentError) -> String {
    format!(
        "Dispatch denied (#9011): this guard cannot tell whether the agent writes files, \
         because the agent roster is unavailable — {err}. Without it, two file-mutating \
         agents could share one git HEAD with no error at any step, so the guard refuses \
         rather than guesses. Install the content and re-issue the dispatch."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::content_source::{DevOverride, agent_roster_in};

    /// Owner ruling 09(a): no content means the PM's dispatch is refused, and
    /// the reason names the fix. Made to allow (`Ok(None)`), this fails.
    #[test]
    fn a_dispatch_is_refused_when_content_is_missing() {
        let cache = tempfile::tempdir().expect("tempdir");
        for tool in trusty_mpm::core::agent::SUBAGENT_DISPATCH_TOOLS {
            let verdict = dispatch_roster_with(false, tool, || {
                agent_roster_in(cache.path(), DevOverride::Off)
            });
            let reason = verdict.expect_err("a dispatch with no roster must be refused");
            assert!(reason.contains("tm content install"), "{reason}");
        }
    }

    /// Ordinary tool calls and a subagent's own calls never resolve content.
    #[test]
    fn a_non_dispatch_call_resolves_nothing() {
        let never = || -> Result<AgentRoster, AgentContentError> {
            panic!("a call that needs no classification must not resolve content")
        };
        assert!(matches!(
            dispatch_roster_with(false, "Bash", never),
            Ok(None)
        ));
        assert!(matches!(
            dispatch_roster_with(true, "Agent", never),
            Ok(None)
        ));
    }
}
