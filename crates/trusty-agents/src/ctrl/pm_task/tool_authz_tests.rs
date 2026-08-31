//! Unit tests for `tool_authz` — the L0 wildcard gate (#4520) and the
//! exfiltration confirmation gate (#4054). Split out to keep `tool_authz.rs`
//! focused; behavior is also pinned end-to-end through the real dispatch paths
//! in `runtime::tool_registry_tests` and `persona_tests`.

use super::*;

// --- #4520: is_l0_gated_tool classification ------------------------------

#[test]
fn is_l0_gated_tool_covers_the_shell_grant() {
    assert!(is_l0_gated_tool(crate::tools::l0_exec::L0_SHELL_EXEC));
}

#[test]
fn is_l0_gated_tool_rejects_read_only_l0_tools() {
    // The read-only L0 surfaces keep their #4170/#4171 wildcard-reachability;
    // only the execution grant is literal-only.
    assert!(!is_l0_gated_tool("gh_pr_view"));
    assert!(!is_l0_gated_tool("session_state_list"));
}

#[test]
fn is_l0_gated_tool_rejects_an_ordinary_tool() {
    assert!(!is_l0_gated_tool("web_search"));
    assert!(!is_l0_gated_tool("delegate_to_agent"));
    assert!(!is_l0_gated_tool("compose_email"));
}

// --- #4520: allow_patterns_grant_tool literal-only rule ------------------

#[test]
fn allow_patterns_grant_tool_wildcard_excludes_l0() {
    let patterns = vec!["*".to_string()];
    assert!(
        !allow_patterns_grant_tool(crate::tools::l0_exec::L0_SHELL_EXEC, &patterns),
        "a bare wildcard must not pull in the L0 shell grant"
    );
}

#[test]
fn allow_patterns_grant_tool_prefix_glob_excludes_l0() {
    let patterns = vec!["l0_*".to_string()];
    assert!(
        !allow_patterns_grant_tool(crate::tools::l0_exec::L0_SHELL_EXEC, &patterns),
        "a prefix glob is not a literal name and must not grant the L0 shell"
    );
}

#[test]
fn allow_patterns_grant_tool_literal_name_grants_l0() {
    let patterns = vec![crate::tools::l0_exec::L0_SHELL_EXEC.to_string()];
    assert!(
        allow_patterns_grant_tool(crate::tools::l0_exec::L0_SHELL_EXEC, &patterns),
        "an L0 tool named literally must still be granted"
    );
}

#[test]
fn allow_patterns_grant_tool_wildcard_still_grants_non_l0() {
    let patterns = vec!["*".to_string()];
    assert!(
        allow_patterns_grant_tool("web_search", &patterns),
        "a wildcard must still grant every non-L0 tool"
    );
    // Prefix globs keep working for ordinary tools too.
    let git = vec!["git_*".to_string()];
    assert!(allow_patterns_grant_tool("git_log", &git));
    assert!(!allow_patterns_grant_tool("web_search", &git));
}

// --- #4054: exfiltration classification ----------------------------------

#[test]
fn exfil_set_matches_the_ruling() {
    // Pinned so a well-meaning edit that drops one of the seven is caught.
    // Extended from five to seven by the owner ruling 2026-08-31 (manage_events,
    // manage_drive_file).
    let mut set = EXFIL_CAPABLE_TOOLS.to_vec();
    set.sort_unstable();
    assert_eq!(
        set,
        vec![
            "compose_email",
            "manage_drive_file",
            "manage_events",
            "manage_file_permissions",
            "manage_gmail_filters",
            "manage_gmail_settings",
            "modify_gmail_messages",
        ]
    );
}

#[test]
fn is_exfil_capable_tool_matches_the_set() {
    assert!(is_exfil_capable_tool("compose_email"));
    assert!(is_exfil_capable_tool("manage_file_permissions"));
    assert!(is_exfil_capable_tool("manage_gmail_settings"));
    assert!(is_exfil_capable_tool("manage_gmail_filters"));
    assert!(is_exfil_capable_tool("modify_gmail_messages"));
    // Owner ruling 2026-08-31 extension.
    assert!(is_exfil_capable_tool("manage_events"));
    assert!(is_exfil_capable_tool("manage_drive_file"));
}

#[test]
fn is_exfil_capable_tool_rejects_a_read_only_tool() {
    // Read-only Gmail/Drive tools ingest untrusted content but move nothing
    // out — they must stay ungated.
    assert!(!is_exfil_capable_tool("get_gmail_message_content"));
    assert!(!is_exfil_capable_tool("search_gmail_messages"));
    assert!(!is_exfil_capable_tool("get_drive_file_content"));
    // Creating a Doc is a write, but not exfiltration — deliberately excluded.
    assert!(!is_exfil_capable_tool("create_document"));
}

// --- #4054: strip_exfil_pending_confirmation fail-closed -----------------

#[test]
fn strip_exfil_denies_when_confirmation_unavailable() {
    let names = vec![
        "compose_email".to_string(),
        "get_gmail_message_content".to_string(),
        "manage_file_permissions".to_string(),
    ];
    let kept = strip_exfil_pending_confirmation(names, ConfirmationCapability::Unavailable);
    assert_eq!(
        kept,
        vec!["get_gmail_message_content".to_string()],
        "exfil tools must be denied when no confirmation channel exists; \
         read-only tools survive"
    );
}

#[test]
fn strip_exfil_keeps_read_only_tools() {
    let names = vec![
        "get_gmail_message_content".to_string(),
        "search_gmail_messages".to_string(),
        "web_search".to_string(),
    ];
    let kept = strip_exfil_pending_confirmation(names.clone(), ConfirmationCapability::Unavailable);
    assert_eq!(kept, names, "no exfil tool present, nothing stripped");
}

#[test]
fn strip_exfil_available_is_a_noop() {
    let names = vec![
        "compose_email".to_string(),
        "get_gmail_message_content".to_string(),
    ];
    let kept = strip_exfil_pending_confirmation(names.clone(), ConfirmationCapability::Available);
    assert_eq!(
        kept, names,
        "with confirmation available the gate must pass exfil tools through"
    );
}

// --- display-drift predicate (#4520 + #4054 composed) --------------------

#[test]
fn persona_surface_grants_tool_excludes_l0_and_exfil_under_wildcard() {
    let wildcard = vec!["*".to_string()];
    // L0 execution grant: literal-only, so a wildcard does not display it.
    assert!(!persona_surface_grants_tool(
        crate::tools::l0_exec::L0_SHELL_EXEC,
        &wildcard
    ));
    // Exfil-capable tools: fail closed with no confirmation channel.
    assert!(!persona_surface_grants_tool("compose_email", &wildcard));
    assert!(!persona_surface_grants_tool("manage_events", &wildcard));
    assert!(!persona_surface_grants_tool("manage_drive_file", &wildcard));
    // Ordinary and read-only tools are still displayed as granted.
    assert!(persona_surface_grants_tool("web_search", &wildcard));
    assert!(persona_surface_grants_tool(
        "get_gmail_message_content",
        &wildcard
    ));
    // A literal name still grants the L0 shell.
    let literal = vec![crate::tools::l0_exec::L0_SHELL_EXEC.to_string()];
    assert!(persona_surface_grants_tool(
        crate::tools::l0_exec::L0_SHELL_EXEC,
        &literal
    ));
}
