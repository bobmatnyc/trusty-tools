//! JIRA REST client for issue metadata, plus (issue #3966) changelog and
//! comment extraction backing `fact_ticket_transitions` and
//! `fact_jira_comment_detail`.

pub mod client;
pub mod http;
pub mod jql_time;
pub mod model;
pub mod paging;
pub mod retry;
pub mod sync;

pub use client::{JiraClient, JiraIssue};
pub use model::{ChangelogIssue, ChangelogWalk, JiraComment, JiraTransition};
pub use retry::RetryPolicy;
