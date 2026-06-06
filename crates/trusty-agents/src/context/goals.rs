//! Goal block injected at the top of every agent prompt (#68).
//!
//! Why: Multi-phase workflows drift when later phases rediscover the task from
//! scratch. The planner emits a compact `GoalBlock` (1 primary + up to 2
//! secondary) that is injected at the START of every downstream agent's
//! system prompt so each step stays anchored to the original deliverable.
//! What: Serializable struct with a `to_prompt_header` formatter.
//! Test: See unit tests at the bottom of this file.

use serde::{Deserialize, Serialize};

/// Structured task goals produced by the planner.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GoalBlock {
    pub primary: String,
    #[serde(default)]
    pub secondary: Vec<String>,
    #[serde(default)]
    pub task_split_required: bool,
}

impl GoalBlock {
    /// Format as a compact prompt header injected before all other context.
    ///
    /// Why: All downstream agents must see the same goals in the same place
    /// so the LLM's attention reliably lands on them.
    /// What: Produces "## TASK GOALS\n**Primary:** ...\n**Secondary N:** ...".
    /// Test: `goal_block_header_formats`.
    pub fn to_prompt_header(&self) -> String {
        let mut lines = vec![
            "## TASK GOALS".to_string(),
            format!("**Primary:** {}", self.primary),
        ];
        for (i, s) in self.secondary.iter().enumerate() {
            lines.push(format!("**Secondary {}:** {}", i + 1, s));
        }
        lines.join("\n")
    }

    /// True when the primary goal is empty (treat as absent).
    pub fn is_empty(&self) -> bool {
        self.primary.is_empty()
    }
}

/// Parse a `GoalBlock` from the first ```goal ... ``` fenced code block in
/// `text`. Returns `None` if no fenced block is present or JSON parsing fails.
///
/// Why: The planner emits the goal block as a fenced JSON payload so it's
/// trivially extractable without a full Markdown parser.
/// What: Scans for ```goal on a line, reads until the next ```, parses JSON.
/// Test: `parse_goal_block_from_text_extracts_json`.
pub fn parse_goal_block_from_text(text: &str) -> Option<GoalBlock> {
    let mut in_block = false;
    let mut body = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !in_block {
            if trimmed.starts_with("```goal") {
                in_block = true;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    if body.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<GoalBlock>(body.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_block_header_formats() {
        let g = GoalBlock {
            primary: "Ship foo".into(),
            secondary: vec!["bar".into(), "baz".into()],
            task_split_required: false,
        };
        let h = g.to_prompt_header();
        assert!(h.starts_with("## TASK GOALS"));
        assert!(h.contains("**Primary:** Ship foo"));
        assert!(h.contains("**Secondary 1:** bar"));
        assert!(h.contains("**Secondary 2:** baz"));
    }

    #[test]
    fn goal_block_is_empty_when_primary_empty() {
        let g = GoalBlock::default();
        assert!(g.is_empty());
    }

    #[test]
    fn parse_goal_block_from_text_extracts_json() {
        let text = "Preamble\n\n```goal\n{\"primary\":\"X\",\"secondary\":[\"y\"],\"task_split_required\":false}\n```\n\nRest";
        let g = parse_goal_block_from_text(text).expect("parses");
        assert_eq!(g.primary, "X");
        assert_eq!(g.secondary, vec!["y"]);
        assert!(!g.task_split_required);
    }

    #[test]
    fn parse_goal_block_returns_none_when_absent() {
        assert!(parse_goal_block_from_text("no block here").is_none());
    }

    #[test]
    fn parse_goal_block_returns_none_on_bad_json() {
        let text = "```goal\n{not json}\n```";
        assert!(parse_goal_block_from_text(text).is_none());
    }
}
