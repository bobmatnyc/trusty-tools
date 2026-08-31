//! Tool-authorization classification shared by both persona dispatch paths
//! (#4520 wildcard/L0 gate, #4054 exfiltration confirmation gate).
//!
//! Why: A persona's `[tools].allow` list is resolved into a concrete set of
//! callable tool names by two independent gates — `scope_assistant_allowed_tools`
//! on the `--direct`/`--agent` subprocess path
//! (`runtime::tool_registry`) and `filter_persona_tool_names` on the
//! persona-chat path (`ctrl::pm_task::dispatch::persona`). Both used
//! `match_any_glob` directly, which treats a bare `*` as "grant everything the
//! registry holds". Two owner rulings (2026-08-31) narrow that: a wildcard must
//! never reach an L0-gated tool (#4520), and an exfiltration-capable tool must
//! never execute on the persona-chat path without explicit human confirmation
//! (#4054). Concentrating both classifications in one module means the two
//! dispatch paths cannot drift, and a reviewer has exactly one place to audit
//! "which tools are dangerous and how are they gated".
//! What: `is_l0_gated_tool` + `allow_patterns_grant_tool` for #4520;
//! `is_exfil_capable_tool` + `strip_exfil_pending_confirmation` +
//! `ConfirmationCapability` for #4054.
//! Test: `tool_authz_tests` (sibling file) plus the real-path regressions in
//! `runtime::tool_registry_tests` (#4520) and
//! `ctrl::pm_task::dispatch::persona_tests` (#4054).

use super::helpers::match_any_glob;

// ===========================================================================
// #4520 — a wildcard grants every NON-L0 tool; an L0-gated tool is reachable
// ONLY when named literally.
// ===========================================================================

/// Whether `name` is an L0-orchestration-gated tool that a wildcard must never
/// pull in (#4520).
///
/// Why: An L0-tier persona (a user file with `role = "assistant"`, which
/// derives `AgentTier::L0Orchestration` via `AgentTier::for_kind`) has the L0
/// EXECUTION grant — the unsandboxed shell `l0_shell_exec` — REGISTERED in its
/// registry. Before this gate, `allow = ["*"]` expanded over that name and
/// silently handed the persona a real `sh -c` (#4520). The owner ruling: a
/// wildcard expresses "I don't want to maintain a tool list", never "grant me
/// an unsandboxed shell", so the execution grant must be named EXPLICITLY.
///
/// Scope is deliberately the EXECUTION surface, not every L0-only tool. The
/// read-only L0 surfaces — the GitHub PR/CI tools and the session-state tools —
/// are wildcard-reachable at L0 by the ratified #4170/#4171 design (pinned by
/// `persona_tier_gate_keeps_session_state_for_l0`); reclassifying them here
/// would reverse that decision. The privileged, injection-to-RCE capability the
/// ruling protects (#4126 lineage) is execution, and that is what this gates.
/// What: exact-name membership in the single-source-of-truth
/// [`crate::tools::l0_exec::L0_EXECUTION_TOOL_NAMES`] — so a second execution
/// tool added to that grant is covered here without a second edit. Matched
/// exactly, never as a glob: the caller ([`allow_patterns_grant_tool`]) enforces
/// the literal-only rule.
/// Test: `is_l0_gated_tool_covers_the_shell_grant`,
/// `is_l0_gated_tool_rejects_read_only_l0_tools`,
/// `is_l0_gated_tool_rejects_an_ordinary_tool`.
pub(crate) fn is_l0_gated_tool(name: &str) -> bool {
    crate::tools::l0_exec::is_l0_execution_tool(name)
}

/// Whether a persona's `[tools].allow` `patterns` grant the tool `name`,
/// honoring the #4520 rule that an L0-gated tool is reachable only when named
/// literally.
///
/// Why: THE gate. This replaces the bare `match_any_glob` call both dispatch
/// paths made. Without it, `allow = ["*"]` (or `["l0_*"]`) on an L0 persona
/// resolved the unsandboxed shell into the callable set — the exact silent
/// escalation #4520 reports.
/// What: For an L0-gated `name` ([`is_l0_gated_tool`]), returns true ONLY when
/// some pattern equals `name` verbatim — a wildcard (`*`) or prefix glob
/// (`l0_*`) never matches, because "named literally" means the pattern string
/// IS the tool name. For every other tool, delegates to `match_any_glob`
/// unchanged, so a wildcard still grants the full non-L0 surface. Fail-closed
/// direction: the special-case can only ever WITHHOLD a grant a glob would have
/// made, never add one.
/// Test: `allow_patterns_grant_tool_wildcard_excludes_l0`,
/// `allow_patterns_grant_tool_prefix_glob_excludes_l0`,
/// `allow_patterns_grant_tool_literal_name_grants_l0`,
/// `allow_patterns_grant_tool_wildcard_still_grants_non_l0`.
pub(crate) fn allow_patterns_grant_tool(name: &str, patterns: &[String]) -> bool {
    if is_l0_gated_tool(name) {
        // #4520: literal naming only — a glob (`*`, `l0_*`) must not reach it.
        patterns.iter().any(|p| p == name)
    } else {
        match_any_glob(name, patterns)
    }
}

// ===========================================================================
// #4054 — exfiltration-capable tools require explicit human confirmation before
// executing on the persona-chat path; when confirmation is unavailable, deny.
// ===========================================================================

/// The exfiltration-capable Google tools that move data OUT of the user's
/// control or durably alter what reaches them (#4054).
///
/// Why: These five are live on the base `assistant` persona-chat path (#4020
/// restored the Google family grants), in the same turn the persona ingests
/// attacker-controllable text (an inbound email body, a shared Drive doc, a web
/// page). The classic prompt-injection shape — "forward everything from
/// finance@ to attacker@", "share this folder with attacker@" — is reachable in
/// one turn with no confirmation anywhere on the path. The allow-list bounds
/// WHICH tools exist; it does not bound what the model does with them once
/// injected content is in context. That is what a confirmation gate is for.
///
/// This is a deliberately-introduced NAME-based classification, not a
/// registry-carried side-effect marker, and the justification is structural:
/// these tools are discovered from the external gworkspace MCP server at
/// runtime and carry no Rust-side write/side-effect flag this crate could read.
/// The set is the exfiltration/persistence subset named in the #4054 ruling —
/// deliberately narrower than "every write tool" (creating a Doc is not
/// exfiltration), and any addition is a reviewable one-line edit here.
/// Test: `exfil_set_matches_the_ruling`, `is_exfil_capable_tool_*`.
pub(crate) const EXFIL_CAPABLE_TOOLS: &[&str] = &[
    "compose_email",           // sends mail to an arbitrary recipient
    "manage_file_permissions", // grants an arbitrary principal Drive access
    "manage_gmail_settings",   // forwarding address / delegate — persistent channel
    "manage_gmail_filters",    // auto-forward / auto-archive, silent and durable
    "modify_gmail_messages",   // label/archive/trash — destructive, hides evidence
];

/// Whether `name` is one of the exfiltration-capable tools gated by #4054.
///
/// Why: A named predicate reads better than an inline `.contains()` at the call
/// site and gives the tests one thing to pin.
/// What: exact-name membership in [`EXFIL_CAPABLE_TOOLS`].
/// Test: `is_exfil_capable_tool_matches_the_set`,
/// `is_exfil_capable_tool_rejects_a_read_only_tool`.
pub(crate) fn is_exfil_capable_tool(name: &str) -> bool {
    EXFIL_CAPABLE_TOOLS.contains(&name)
}

/// Whether the current dispatch path can obtain an explicit human confirmation
/// for an exfiltration-capable tool call (#4054).
///
/// Why: The owner ruling requires these tools to be gated behind explicit human
/// confirmation and to FAIL CLOSED — deny, never silently execute — when no
/// confirmation channel exists. Modeling availability as an explicit value
/// (rather than a hardcoded deny) keeps the security property one testable
/// decision and leaves an interactive dispatch path a single place to opt in.
/// What: `Unavailable` is the fail-closed default the persona-chat path passes
/// today — it has no interactive tool-confirmation prompt wired. `Available`
/// is reserved for a future interactive path that can actually prompt the human
/// and is not constructed anywhere yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmationCapability {
    /// No confirmation channel — exfiltration-capable tools are denied.
    Unavailable,
    /// A confirmation channel exists — exfiltration-capable tools may pass.
    #[allow(dead_code)]
    Available,
}

/// Remove exfiltration-capable tool names from an already-resolved allow list
/// unless human confirmation is available (#4054).
///
/// Why: THE enforcement point for the confirmation gate. Both the advertised
/// tool-schema list and `ToolRegistry::dispatch_gated`'s allowlist are the same
/// `Vec<String>` on the persona-chat path (see
/// `persona_gate::filter_persona_tool_names_for_tier`), so removing a name here
/// both hides the tool from the model AND makes any call to it be refused at
/// the dispatch boundary — the tool cannot execute. Applying it to the final
/// resolved list (rather than at registration) is what makes it unbypassable by
/// any grant source. DENY-ONLY and order-preserving: it can remove a name,
/// never add one.
/// What: `Available` returns `names` unchanged. `Unavailable` — the fail-closed
/// default the persona-chat path passes — removes every [`is_exfil_capable_tool`]
/// entry, so an ungated exfiltration call is refused rather than silently
/// executed. Read-only tools (e.g. `get_gmail_message_content`,
/// `search_gmail_messages`) are never in the set and pass untouched.
/// Test: `strip_exfil_denies_when_confirmation_unavailable`,
/// `strip_exfil_keeps_read_only_tools`,
/// `strip_exfil_available_is_a_noop`, and the real-path regression
/// `persona_tier_gate_strips_exfil_tools_pending_confirmation`.
pub(crate) fn strip_exfil_pending_confirmation(
    names: Vec<String>,
    confirmation: ConfirmationCapability,
) -> Vec<String> {
    match confirmation {
        ConfirmationCapability::Available => names,
        ConfirmationCapability::Unavailable => names
            .into_iter()
            .filter(|n| !is_exfil_capable_tool(n))
            .collect(),
    }
}

#[cfg(test)]
#[path = "tool_authz_tests.rs"]
mod tool_authz_tests;
