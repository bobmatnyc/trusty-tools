//! The outbound-channel MCP families `tm hook --egress-guard` denies (#8454).
//!
//! Why: G3 of #8454 makes the daemon's owner channel the only outbound message
//! path from a tm-launched session, so every MCP tool that can reach Slack,
//! Gmail, Notion or Google Chat is denied — read, search and list tools too
//! (Bob ruling 4). The table is built into the code (Architect Q6): a config
//! file the session can write must never be able to drop a family.
//! What: [`FamilyTable`] holds the server-segment patterns;
//! [`FamilyTable::classify`] maps a tool name to a [`Classification`]. Claude
//! Code names an MCP tool `mcp__<server>__<tool>`. The server segment is
//! normalized — lowercased, every character but ASCII letters and digits
//! dropped — and denied when it contains a family name, so `Slack-MCP`,
//! `claude_ai_Slack` and `S.l.a.c.k` all match `slack`. A `gworkspace` or
//! `google_workspace` server is denied only for its mail, chat and message
//! tools. [`FamilyTable::classify_resource_server`] judges the server an MCP
//! resource tool names.
//!
//! Limits, by design of a name match: Unicode confusables (a Cyrillic `а` in
//! `slаck`) are dropped by the normalization, not mapped, so they evade it; a
//! generic proxy server (a Zapier-style `run_action`) is not a family until
//! configuration adds it; Bash egress is slice S3.
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
/// `google_workspace` normalizes to `googleworkspace`, which lacks `gworkspace`.
const GWORKSPACE_SERVERS: &[&str] = &[GWORKSPACE, "googleworkspace"];

/// Normalized tool-segment fragments that make a `gworkspace` tool a messaging
/// tool: `mail` covers `gmail` and `email`, `draft` the send-a-draft path.
const GWORKSPACE_MESSAGING: &[&str] = &["mail", "chat", "message", "draft"];

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
    /// tool, both judged by [`FamilyTable::server_family`].
    /// Test: `egress_family_match_normalizes_case_and_separators`,
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
        let (tool, server) = match segments[1..].split_last() {
            Some((tool, server)) => (normalize(tool), normalize(&server.join("_"))),
            None => return Classification::Malformed("no server segment"),
        };
        self.server_family(&server, Some(&tool))
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
    /// Test: `egress_denies_family_mcp_resource_tools`,
    /// `egress_allows_resource_tools_naming_unrelated_servers`.
    pub(crate) fn classify_resource_server(&self, server: &str) -> Classification {
        let server = normalize(server);
        if server.is_empty() {
            return Classification::Malformed("a server name with no letter or digit");
        }
        self.server_family(&server, None)
    }

    /// The family of a normalized `server`, given its normalized `tool`.
    ///
    /// What: a server containing a pattern is that [`Classification::Family`].
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
        let messaging = tool.is_none_or(|t| GWORKSPACE_MESSAGING.iter().any(|m| t.contains(m)));
        if messaging && GWORKSPACE_SERVERS.iter().any(|g| server.contains(g)) {
            return Classification::Family(GWORKSPACE.to_owned());
        }
        Classification::Unrelated
    }
}
