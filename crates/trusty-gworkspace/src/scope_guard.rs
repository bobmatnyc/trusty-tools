//! Refuse a tool call that a read-only profile cannot make, naming the scope.
//!
//! Why: A profile minted with `setup --scopes readonly` (#10746 ok-T20) holds
//! no write scope. Calling a write tool on it reaches Google, which answers
//! `403 ACCESS_TOKEN_SCOPE_INSUFFICIENT` with "Request had insufficient
//! authentication scopes" and does not say which scope is missing. The guard
//! answers before the request with the scope's name, so the caller knows to
//! switch profile instead of retrying.
//! What: [`readonly_refusal`] returns an error message when the acting
//! profile's recorded scopes are all read-only-preset scopes and the tool
//! needs a scope outside them. Any other profile — including a full profile
//! and a legacy token with no recorded scopes — gets `None`, so its calls run
//! exactly as before.
//! Test: `write_tool_on_readonly_profile_names_the_missing_scope`,
//! `read_tools_pass_on_readonly_profile`,
//! `non_readonly_profiles_are_never_refused`, and the dispatch-level
//! `server::scope_tests`.

use crate::api::constants::READONLY_SCOPES;
use crate::openrpc::scopes_for_tool;

const GMAIL_READONLY: &str = "https://www.googleapis.com/auth/gmail.readonly";
const DRIVE_READONLY: &str = "https://www.googleapis.com/auth/drive.readonly";
const DOCUMENTS_READONLY: &str = "https://www.googleapis.com/auth/documents.readonly";

// #10746: ok-T20 read-only scope preset
/// The read-only scope that lets a read tool run, or `None` for a tool that
/// writes (or may write) and so needs its full scope.
///
/// Why: `scopes_for_tool` lists the broad scope each tool is minted with
/// (`gmail.modify`, `drive`, …); a read tool also runs under the narrower
/// read-only scope, and the guard must not refuse it.
/// What: Gmail, Drive and Docs tools that only read. A `manage_*` tool is a
/// write tool here even when one of its actions only reads.
fn readonly_scope_for_tool(name: &str) -> Option<&'static str> {
    match name {
        "search_gmail_messages"
        | "get_gmail_message_content"
        | "download_gmail_attachment"
        | "list_message_attachments" => Some(GMAIL_READONLY),
        "list_drive_contents"
        | "search_drive_files"
        | "get_drive_file_content"
        | "list_shared_drives" => Some(DRIVE_READONLY),
        "get_document"
        | "get_document_structure"
        | "find_tables_in_document"
        | "get_document_named_styles" => Some(DOCUMENTS_READONLY),
        _ => None,
    }
}

/// Tools that touch only local state and never call a Google API.
fn is_local_tool(name: &str) -> bool {
    matches!(
        name,
        "list_accounts"
            | "set_default_account"
            | "remove_account"
            | "add_account"
            | "format_email_content"
    )
}

// #10746: ok-T20 read-only scope preset
/// The refusal message for `tool` on `profile`, or `None` to let it run.
///
/// Why: see the module doc. The check keys on the recorded scopes, not on a
/// stored preset name, because the scopes are what Google enforces.
/// What: `None` unless `granted` is non-empty and every entry is a
/// [`READONLY_SCOPES`] member. On such a profile a local tool runs, a read
/// tool runs when its read-only scope is granted, and every other tool is
/// refused with the first scope it needs that the profile lacks.
/// Test: `write_tool_on_readonly_profile_names_the_missing_scope`,
/// `read_tools_pass_on_readonly_profile`,
/// `non_readonly_profiles_are_never_refused`.
pub fn readonly_refusal(tool: &str, profile: &str, granted: &[String]) -> Option<String> {
    let is_readonly_profile = !granted.is_empty()
        && granted
            .iter()
            .all(|s| READONLY_SCOPES.contains(&s.as_str()));
    if !is_readonly_profile || is_local_tool(tool) {
        return None;
    }
    let has = |scope: &str| granted.iter().any(|g| g == scope);
    let missing = match readonly_scope_for_tool(tool) {
        Some(read_scope) if has(read_scope) => return None,
        Some(read_scope) => read_scope,
        None => scopes_for_tool(tool).iter().copied().find(|s| !has(s))?,
    };
    Some(format!(
        "tool '{tool}' needs OAuth scope {missing}, which read-only profile '{profile}' \
         was not granted (it was set up with `--scopes readonly`). Use a profile with \
         the full scope set, or re-run `trusty-gworkspace-mcp setup --profile {profile}` \
         without `--scopes readonly`."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readonly() -> Vec<String> {
        READONLY_SCOPES.iter().map(|s| s.to_string()).collect()
    }

    /// Why: every write tool must be refused on a read-only profile, and the
    /// message must name a scope the profile lacks.
    /// What: walks the whole tool registry; each tool with no read-only
    /// mapping and a Google scope must be refused with one of its own scopes.
    /// Test: this test.
    #[test]
    fn write_tool_on_readonly_profile_names_the_missing_scope() {
        let granted = readonly();
        let tools = crate::tools::tool_list_response();
        let mut refused = 0;
        for tool in tools["tools"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            if is_local_tool(name) || readonly_scope_for_tool(name).is_some() {
                continue;
            }
            let msg = readonly_refusal(name, "ro", &granted)
                .unwrap_or_else(|| panic!("write tool {name} must be refused"));
            let named = scopes_for_tool(name)
                .iter()
                .any(|s| msg.contains(&format!("scope {s},")) && !granted.contains(&s.to_string()));
            assert!(named, "{name}: {msg}");
            assert!(msg.contains("'ro'"), "{msg}");
            refused += 1;
        }
        assert!(refused > 40, "only {refused} write tools checked");
        let msg = readonly_refusal("compose_email", "ro", &granted).unwrap();
        assert!(
            msg.contains("https://www.googleapis.com/auth/gmail.send"),
            "{msg}"
        );
    }

    /// Why: the preset exists so read tools work; refusing them would make it
    /// useless.
    /// What: read and local tools pass; a read tool whose read-only scope the
    /// grant lacks is refused with that read-only scope.
    /// Test: this test.
    #[test]
    fn read_tools_pass_on_readonly_profile() {
        let granted = readonly();
        for name in [
            "search_gmail_messages",
            "get_drive_file_content",
            "get_document",
            "list_accounts",
            "format_email_content",
        ] {
            assert_eq!(readonly_refusal(name, "ro", &granted), None, "{name}");
        }
        let no_drive: Vec<String> = granted
            .into_iter()
            .filter(|s| s != DRIVE_READONLY)
            .collect();
        let msg = readonly_refusal("search_drive_files", "ro", &no_drive).unwrap();
        assert!(msg.contains(DRIVE_READONLY), "{msg}");
    }

    /// Why: the default must stay exactly as today.
    /// What: a full grant, a legacy empty grant and a grant with one write
    /// scope are never refused, even for a write tool.
    /// Test: this test.
    #[test]
    fn non_readonly_profiles_are_never_refused() {
        let full: Vec<String> = crate::api::constants::OAUTH_SCOPES
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut mixed = readonly();
        mixed.push("https://www.googleapis.com/auth/gmail.modify".into());
        for granted in [full, Vec::new(), mixed] {
            for name in ["compose_email", "manage_drive_file", "manage_events"] {
                assert_eq!(readonly_refusal(name, "p", &granted), None, "{name}");
            }
        }
    }
}
