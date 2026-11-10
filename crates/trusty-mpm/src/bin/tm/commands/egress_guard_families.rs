//! The outbound-channel MCP families `tm hook --egress-guard` denies (#8454).
//!
//! Why: G3 of #8454 makes the daemon's owner channel the only outbound message
//! path from a tm-launched session, so every MCP tool that can reach Slack,
//! Gmail, Notion or Google Chat is denied — read, search and list tools too
//! (Bob ruling 4). The table is built into the code (Architect Q6): a config
//! file the session can write must never be able to drop a family. The one
//! narrowing is per call, not per family: `egress_guard_slack_read` (#10387)
//! lifts two exact Slack read tools for a project the host `config.yaml`
//! names, and that file is a trust anchor no session may write.
//! What: [`FamilyTable`] holds the server-segment patterns;
//! [`FamilyTable::classify`] maps a tool name to a [`Classification`]. Claude
//! Code names an MCP tool `mcp__<server>__<tool>`. The server segment is
//! normalized — lowercased, every character but ASCII letters and digits
//! dropped — and denied when it contains a family name, so `Slack-MCP`,
//! `claude_ai_Slack` and `S.l.a.c.k` all match `slack`. Any tool whose
//! normalized tool segment contains `gmail` is denied, whatever its server. A
//! `gworkspace`, `google_workspace` or `gsuite` server is denied only for its
//! mail, chat and message tools. [`FamilyTable::classify_resource_server`]
//! judges the server an MCP resource tool names. #10735 (owner ruling D6d):
//! a claude.ai connector server ([`is_claude_ai_connector`]) is exempt from
//! the four built-in families; every other server keeps its deny.
//!
//! Limits, by design of a name match: Unicode confusables (a Cyrillic `а` in
//! `slаck`) are dropped by the normalization, not mapped, so they evade it; a
//! generic proxy server (a Zapier-style `run_action`) is not a family until
//! configuration adds it; Bash egress is `egress_guard_bash` (S3).
//! Test: `egress_guard_tests.rs`; end to end in
//! `tests/tm_hook_egress_guard_8454.rs`.

/// Server-segment families whose every tool is denied. #8454 fix round:
/// `googlemail` because `google-mail` normalizes to it, which lacks `gmail`.
const SERVER_FAMILIES: &[&str] = &[
    "slack",
    "gmail",
    "googlemail",
    "notion",
    "googlechat",
    "gchat",
];

/// The Google Workspace server family, denied for its messaging tools only.
const GWORKSPACE: &str = "gworkspace";

/// Normalized server spellings of [`GWORKSPACE`]. #8454 fix round:
/// `google_workspace` normalizes to `googleworkspace`, which lacks `gworkspace`;
/// final round: `gsuite`, as in `mcp-gsuite`.
const GWORKSPACE_SERVERS: &[&str] = &[GWORKSPACE, "googleworkspace", "gsuite"];

/// The normalized tool-segment fragment denied on every server (#8454 final
/// round): an unlisted server name such as `workspace-mcp` still carries
/// `send_gmail_message`.
const GMAIL_TOOL: &str = "gmail";

/// Normalized tool-segment fragments that make a `gworkspace` tool a messaging
/// tool: `mail` covers `gmail` and `email`, `draft` the send-a-draft path.
const GWORKSPACE_MESSAGING: &[&str] = &["mail", "chat", "message", "draft"];

/// The server prefix Claude Code gives a claude.ai connector: `claude.ai
/// Notion` becomes `claude_ai_Notion` (#10735).
const CONNECTOR_PREFIX: &str = "claude_ai_";

/// True when `server` names a claude.ai connector (#10735, owner ruling D6d).
///
/// What: lowercases `server`, maps every character but an ASCII letter or
/// digit to `_`, and checks for [`CONNECTOR_PREFIX`], so the tool-name form
/// `claude_ai_Slack` and the display form `claude.ai Slack` both match.
/// Test: `claude_ai_connectors_are_exempt_from_the_family_deny_10735`.
pub(crate) fn is_claude_ai_connector(server: &str) -> bool {
    let mapped: String = server
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    mapped.starts_with(CONNECTOR_PREFIX)
}

/// `family` unless `server` is a claude.ai connector and `family` one of the
/// built-in Slack, Gmail, Notion or Google Chat families (#10735).
fn unless_connector(server: &str, family: Classification) -> Classification {
    match &family {
        Classification::Family(name)
            if is_claude_ai_connector(server)
                && (SERVER_FAMILIES.contains(&name.as_str()) || name == GMAIL_TOOL) =>
        {
            Classification::Unrelated
        }
        _ => family,
    }
}

/// What a tool name is to the egress guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Classification {
    /// Not an outbound-channel tool.
    Unrelated,
    /// An MCP tool of the named family.
    Family(String),
    /// An `mcp__` name the guard cannot split into a non-empty server and tool.
    Malformed(&'static str),
}

/// The server-segment patterns the guard denies, built-in ones first.
///
/// Invariant: every [`SERVER_FAMILIES`] entry is present; the only way to
/// change the table is [`FamilyTable::with_added`], which appends.
#[derive(Debug, Clone)]
pub(crate) struct FamilyTable {
    server_patterns: Vec<String>,
}

/// `segment` lowercased, keeping ASCII letters and digits only.
fn normalize(segment: &str) -> String {
    segment
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

impl FamilyTable {
    /// The built-in families, with nothing added.
    pub(crate) fn builtin() -> Self {
        Self {
            server_patterns: SERVER_FAMILIES.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    /// This table plus `patterns`, each a server-segment family.
    ///
    /// Why (Architect Q6): configuration may widen the deny, never narrow it,
    /// so the hook point appends and offers no removal. No caller in S1: the
    /// config read lands with the policy slice (S4).
    /// What: normalizes each pattern like a server segment; drops one that
    /// normalizes to empty (it would match every server) or is already present.
    /// Test: `egress_config_patterns_add_and_never_remove`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_added<I, S>(mut self, patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for pattern in patterns {
            let pattern = normalize(pattern.as_ref());
            if !pattern.is_empty() && !self.server_patterns.contains(&pattern) {
                self.server_patterns.push(pattern);
            }
        }
        self
    }

    /// Classify one `tool_name`.
    ///
    /// Why: the deny must hold against casing and separator lookalikes
    /// (fail-open audit 5, 6), and an `mcp__` name too short to carry a server
    /// and a tool is a delivery fault the guard denies (§4).
    /// What: a name that is not `mcp__…` after ASCII lowercasing is
    /// [`Classification::Unrelated`]. Otherwise it splits on `__`; fewer than
    /// three segments, or an empty one, is [`Classification::Malformed`]. The
    /// segments between `mcp` and the last are the server; the last is the
    /// tool, both judged by [`FamilyTable::server_family`]; a claude.ai
    /// connector server then drops a built-in family (#10735).
    /// Test: `egress_family_match_normalizes_case_and_separators`,
    /// `claude_ai_connectors_are_exempt_from_the_family_deny_10735`,
    /// `egress_gworkspace_denies_only_mail_chat_message_tools`,
    /// `egress_short_or_hollow_mcp_names_are_malformed`.
    pub(crate) fn classify(&self, tool_name: &str) -> Classification {
        let lowered = tool_name.to_ascii_lowercase();
        if !lowered.starts_with("mcp__") {
            return Classification::Unrelated;
        }
        let segments: Vec<&str> = lowered.split("__").collect();
        if segments.len() < 3 {
            return Classification::Malformed("fewer than three `__`-separated segments");
        }
        if segments.iter().any(|s| s.is_empty()) {
            return Classification::Malformed("an empty `__`-separated segment");
        }
        let (tool, raw) = match segments[1..].split_last() {
            Some((tool, server)) => (normalize(tool), server.join("_")),
            None => return Classification::Malformed("no server segment"),
        };
        // #10735: a claude.ai connector is exempt from the four families.
        unless_connector(&raw, self.server_family(&normalize(&raw), Some(&tool)))
    }

    /// Classify the `server` an MCP resource tool names in its `tool_input`.
    ///
    /// Why (#8454 fix round, Bob ruling 4): `ReadMcpResourceTool` and
    /// `ListMcpResourcesTool` carry the server outside `tool_name`, so a family
    /// read through them would otherwise be unrelated.
    /// What: [`FamilyTable::server_family`] over the normalized `server`, with
    /// no tool — a resource is not a tool, so a Google Workspace server is
    /// denied whole. A name with no ASCII letter or digit is
    /// [`Classification::Malformed`].
    /// A claude.ai connector server drops a built-in family (#10735).
    /// Test: `egress_denies_family_mcp_resource_tools`,
    /// `egress_allows_resource_tools_naming_unrelated_servers`,
    /// `claude_ai_connectors_are_exempt_from_the_family_deny_10735`.
    pub(crate) fn classify_resource_server(&self, server: &str) -> Classification {
        let normalized = normalize(server);
        if normalized.is_empty() {
            return Classification::Malformed("a server name with no letter or digit");
        }
        unless_connector(server, self.server_family(&normalized, None))
    }

    /// The family of a normalized `server`, given its normalized `tool`.
    ///
    /// What: a server containing a pattern is that [`Classification::Family`].
    /// A `tool` containing [`GMAIL_TOOL`] is the `gmail` family on any server.
    /// A server containing a [`GWORKSPACE_SERVERS`] spelling is the
    /// `gworkspace` family when `tool` is `None` or contains a
    /// [`GWORKSPACE_MESSAGING`] fragment. Anything else is unrelated.
    /// Test: `egress_gworkspace_denies_only_mail_chat_message_tools`.
    fn server_family(&self, server: &str, tool: Option<&str>) -> Classification {
        if let Some(family) = self
            .server_patterns
            .iter()
            .find(|p| server.contains(p.as_str()))
        {
            return Classification::Family(family.clone());
        }
        // #8454 final round: a gmail tool is mail whatever its server is named.
        if tool.is_some_and(|t| t.contains(GMAIL_TOOL)) {
            return Classification::Family(GMAIL_TOOL.to_owned());
        }
        let messaging = tool.is_none_or(|t| GWORKSPACE_MESSAGING.iter().any(|m| t.contains(m)));
        if messaging && GWORKSPACE_SERVERS.iter().any(|g| server.contains(g)) {
            return Classification::Family(GWORKSPACE.to_owned());
        }
        Classification::Unrelated
    }
}

#[cfg(test)]
mod lockdown_tests {
    use super::{GWORKSPACE, GWORKSPACE_MESSAGING, SERVER_FAMILIES};
    use trusty_mpm::core::architect_lockdown::ARCHITECT_DENY_GLOBS;

    /// #10127: the Architect's launch deny names every gworkspace messaging
    /// word this hook denies. #10735: it names no Slack, Gmail, Notion or
    /// Google Chat family, whose globs would also deny the claude.ai
    /// connectors. The hook's separator-blind spellings (`google_workspace`,
    /// `gsuite`) are hook-only.
    #[test]
    fn the_architect_deny_covers_gworkspace_and_no_connector_family() {
        for family in SERVER_FAMILIES {
            let glob = format!("mcp__*{family}*");
            assert!(!ARCHITECT_DENY_GLOBS.contains(&glob.as_str()), "{glob}");
        }
        for word in GWORKSPACE_MESSAGING {
            let glob = format!("mcp__*{GWORKSPACE}*__*{word}*");
            assert!(ARCHITECT_DENY_GLOBS.contains(&glob.as_str()), "{glob}");
        }
    }
}
