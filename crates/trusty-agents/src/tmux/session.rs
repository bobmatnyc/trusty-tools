//! Tmux session and pane data structures.
//!
//! Why: Strongly-typed wrappers around tmux's `list-sessions` / `list-panes`
//! output so the rest of the codebase doesn't have to parse colon-separated
//! lines repeatedly.
//! What: `TmuxSession` and `TmuxPane` structs with `parse(line)` constructors
//! that decode the `-F` format strings the orchestrator uses.
//! Test: See unit tests at the bottom of this file.

use super::error::{Result, TmuxError};

/// Represents a tmux session.
#[derive(Debug, Clone)]
pub struct TmuxSession {
    /// Session name.
    pub name: String,
    /// Unix timestamp when the session was created.
    pub created_at: i64,
    /// Session group (`#{session_group}`). Sessions that share a group mirror
    /// identical content (created via `tmux new-session -t <existing>`).
    /// `None` when the session is not part of any group — tmux reports this
    /// as an empty string which we normalize to `None`.
    pub group: Option<String>,
    /// Panes in this session. Populated lazily by the orchestrator.
    pub panes: Vec<TmuxPane>,
}

impl TmuxSession {
    /// Create a new TmuxSession.
    pub fn new(name: impl Into<String>, created_at: i64) -> Self {
        Self {
            name: name.into(),
            created_at,
            group: None,
            panes: Vec::new(),
        }
    }

    /// Parse session from tmux list-sessions output line.
    ///
    /// Why: Centralizes parsing of the tmux list-sessions format so callers
    /// don't have to know the field layout. Supports both the legacy
    /// two-field format and the three-field format that includes
    /// `#{session_group}` for group-based deduplication.
    /// What: Accepts `name:timestamp` or `name:timestamp:group`. An empty
    /// group string is normalized to `None`.
    /// Test: Parse `"mysession:1706000000"` -> group is None;
    /// parse `"mysession:1706000000:grp1"` -> group is `Some("grp1")`;
    /// parse `"mysession:1706000000:"` -> group is None.
    pub fn parse(line: &str) -> Result<Self> {
        // splitn(3) so a trailing group field is preserved even if empty.
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() < 2 {
            return Err(TmuxError::ParseError(format!(
                "invalid session format: {}",
                line
            )));
        }

        let name = parts[0].to_string();
        let created_at: i64 = parts[1]
            .trim()
            .parse()
            .map_err(|_| TmuxError::ParseError(format!("invalid timestamp: {}", parts[1])))?;

        let group = if parts.len() == 3 {
            let g = parts[2].trim();
            if g.is_empty() {
                None
            } else {
                Some(g.to_string())
            }
        } else {
            None
        };

        Ok(Self {
            name,
            created_at,
            group,
            panes: Vec::new(),
        })
    }
}

/// Represents a pane within a tmux session.
#[derive(Debug, Clone)]
pub struct TmuxPane {
    /// Pane ID (e.g., "%0", "%1").
    pub id: String,
    /// Pane index within window.
    pub index: u32,
    /// Whether this pane is active.
    pub active: bool,
    /// Pane width in characters.
    pub width: u32,
    /// Pane height in characters.
    pub height: u32,
}

impl TmuxPane {
    /// Create a new TmuxPane.
    pub fn new(id: impl Into<String>, index: u32, active: bool, width: u32, height: u32) -> Self {
        Self {
            id: id.into(),
            index,
            active,
            width,
            height,
        }
    }

    /// Parse pane from tmux list-panes output line.
    ///
    /// Why: Decodes the colon-separated `-F` format produced by `list-panes`
    /// into a typed struct so callers don't repeat the parse logic.
    /// What: Expects `pane_id:pane_index:pane_active:pane_width:pane_height`.
    /// Test: Parse `"%0:0:1:120:40"` -> active true; parse `"%1:1:0:80:24"`
    /// -> active false; missing fields return ParseError.
    pub fn parse(line: &str) -> Result<Self> {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() != 5 {
            return Err(TmuxError::ParseError(format!(
                "invalid pane format: {}",
                line
            )));
        }

        let id = parts[0].to_string();
        let index: u32 = parts[1]
            .parse()
            .map_err(|_| TmuxError::ParseError(format!("invalid pane index: {}", parts[1])))?;
        let active = parts[2] == "1";
        let width: u32 = parts[3]
            .parse()
            .map_err(|_| TmuxError::ParseError(format!("invalid pane width: {}", parts[3])))?;
        let height: u32 = parts[4]
            .parse()
            .map_err(|_| TmuxError::ParseError(format!("invalid pane height: {}", parts[4])))?;

        Ok(Self {
            id,
            index,
            active,
            width,
            height,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::orchestrator::TmuxOrchestrator;
    use super::*;

    #[test]
    fn test_parse_session_no_group() {
        let line = "mysession:1706000000";
        let session = TmuxSession::parse(line).unwrap();
        assert_eq!(session.name, "mysession");
        assert_eq!(session.created_at, 1706000000);
        assert!(session.group.is_none());
        assert!(session.panes.is_empty());
    }

    #[test]
    fn test_parse_session_with_group() {
        let line = "mysession:1706000000:grp1";
        let session = TmuxSession::parse(line).unwrap();
        assert_eq!(session.name, "mysession");
        assert_eq!(session.created_at, 1706000000);
        assert_eq!(session.group.as_deref(), Some("grp1"));
    }

    #[test]
    fn test_parse_session_with_empty_group() {
        let line = "mysession:1706000000:";
        let session = TmuxSession::parse(line).unwrap();
        assert!(session.group.is_none());
    }

    #[test]
    fn test_parse_session_invalid_format() {
        assert!(TmuxSession::parse("noseparator").is_err());
        assert!(TmuxSession::parse("mysession:notanumber").is_err());
    }

    #[test]
    fn test_parse_pane_active() {
        let pane = TmuxPane::parse("%0:0:1:120:40").unwrap();
        assert_eq!(pane.id, "%0");
        assert_eq!(pane.index, 0);
        assert!(pane.active);
        assert_eq!(pane.width, 120);
        assert_eq!(pane.height, 40);
    }

    #[test]
    fn test_parse_pane_inactive() {
        let pane = TmuxPane::parse("%1:1:0:80:24").unwrap();
        assert_eq!(pane.id, "%1");
        assert_eq!(pane.index, 1);
        assert!(!pane.active);
        assert_eq!(pane.width, 80);
        assert_eq!(pane.height, 24);
    }

    #[test]
    fn test_parse_pane_invalid_format() {
        assert!(TmuxPane::parse("%0:0:1:120").is_err()); // missing height
        assert!(TmuxPane::parse("%0:abc:1:120:40").is_err()); // bad index
    }

    #[test]
    fn test_is_available_returns_bool() {
        // Just confirm the function returns without panicking; result depends on host.
        let _ = TmuxOrchestrator::is_available();
    }
}
