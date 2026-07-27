//! JIRA REST client for issue metadata, plus (issue #3966) changelog and
//! comment extraction backing `fact_ticket_transitions` and
//! `fact_jira_comment_detail`.

pub mod client;
pub mod sync;

pub use client::{ChangelogIssue, JiraClient, JiraComment, JiraIssue, JiraTransition};
