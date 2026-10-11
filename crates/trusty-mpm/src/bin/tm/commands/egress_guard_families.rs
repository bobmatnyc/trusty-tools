//! Seam (#8454 S1): the outbound-channel family table. Implementation follows.

/// What a tool name is to the egress guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Classification {
    /// Not an outbound-channel tool.
    Unrelated,
    /// An MCP tool of the named family.
    Family(String),
    /// An `mcp__` name the guard cannot split into server and tool.
    Malformed(&'static str),
}

/// The server-segment patterns the guard denies.
#[derive(Debug, Clone)]
pub(crate) struct FamilyTable {
    server_patterns: Vec<String>,
}

impl FamilyTable {
    /// Seam: the built-in table.
    pub(crate) fn builtin() -> Self {
        Self {
            server_patterns: Vec::new(),
        }
    }

    /// Seam: add patterns.
    pub(crate) fn with_added<I, S>(self, _patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self
    }

    /// Seam: classify a tool name.
    pub(crate) fn classify(&self, _tool_name: &str) -> Classification {
        let _ = &self.server_patterns;
        Classification::Unrelated
    }
}
