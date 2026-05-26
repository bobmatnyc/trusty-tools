//! Backend dispatcher.
//!
//! Why: The MCP layer wants `client.resolve(...)` and a single dyn Backend.
//! What: Constructs concrete backends from config and stores them by name.
//! Test: `tests::resolve_picks_default`.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};

use crate::tickets::api::backends::{
    Backend, github::GitHubBackend, jira::JiraBackend, linear::LinearBackend,
};
use crate::tickets::api::config::{BackendConfig, Config};

/// Multi-backend dispatcher.
///
/// Why: One process, many backends.
/// What: Holds `Arc<dyn Backend>` per backend name + a default.
/// Test: `tests::list_backends_lists_configured`.
pub struct BackendClient {
    backends: HashMap<String, Arc<dyn Backend>>,
    default_backend: Option<String>,
}

impl BackendClient {
    /// Build from config.
    ///
    /// Why: Single entry point used by the binary.
    /// What: Instantiates each configured backend; non-fatal errors are
    /// logged via `tracing::warn` so partial configs still work.
    /// Test: `tests::from_config_constructs_empty`.
    pub async fn from_config(config: Config) -> Result<Self> {
        let mut backends: HashMap<String, Arc<dyn Backend>> = HashMap::new();
        for (name, bc) in config.backends.into_iter() {
            let result: Result<Arc<dyn Backend>> = match bc {
                BackendConfig::Github(c) => GitHubBackend::new(c).map(|b| Arc::new(b) as _),
                BackendConfig::Jira(c) => JiraBackend::new(c).map(|b| Arc::new(b) as _),
                BackendConfig::Linear(c) => LinearBackend::new(c).map(|b| Arc::new(b) as _),
            };
            match result {
                Ok(b) => {
                    backends.insert(name, b);
                }
                Err(e) => {
                    tracing::warn!("backend '{name}' could not be initialised: {e}");
                }
            }
        }
        Ok(Self {
            backends,
            default_backend: config.default_backend,
        })
    }

    /// Resolve which backend to use.
    ///
    /// Why: Tools accept an optional `backend` arg; absent that we use the
    /// configured default; absent that we use the only configured one.
    /// What: Returns an error if no backend matches.
    /// Test: `tests::resolve_picks_default`.
    pub fn resolve(&self, requested: Option<&str>) -> Result<Arc<dyn Backend>> {
        let key = requested
            .map(|s| s.to_string())
            .or_else(|| self.default_backend.clone())
            .or_else(|| {
                if self.backends.len() == 1 {
                    self.backends.keys().next().cloned()
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                anyhow!(
                    "no backend specified and no default configured (configured: {:?})",
                    self.list_backends()
                )
            })?;
        self.backends
            .get(&key)
            .cloned()
            .ok_or_else(|| anyhow!("backend '{key}' not configured"))
    }

    /// List configured backend names.
    ///
    /// Why: Surfaced via the `list_backends` MCP tool.
    /// What: Sorted for deterministic output.
    /// Test: `tests::list_backends_lists_configured`.
    pub fn list_backends(&self) -> Vec<String> {
        let mut v: Vec<String> = self.backends.keys().cloned().collect();
        v.sort();
        v
    }

    /// Return the configured default backend name, if any.
    pub fn default_backend(&self) -> Option<&str> {
        self.default_backend.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn from_config_constructs_empty() {
        let cfg = Config::default();
        let client = BackendClient::from_config(cfg).await.unwrap();
        assert!(client.list_backends().is_empty());
        assert!(client.resolve(None).is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_backends_lists_configured() {
        // We can't easily construct a real backend without creds, so
        // build a fake BackendClient by hand.
        let mut backends: HashMap<String, Arc<dyn Backend>> = HashMap::new();
        backends.insert("github".into(), Arc::new(FakeBackend("github")));
        backends.insert("linear".into(), Arc::new(FakeBackend("linear")));
        let client = BackendClient {
            backends,
            default_backend: Some("github".into()),
        };
        let names = client.list_backends();
        assert_eq!(names, vec!["github".to_string(), "linear".to_string()]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_picks_default() {
        let mut backends: HashMap<String, Arc<dyn Backend>> = HashMap::new();
        backends.insert("github".into(), Arc::new(FakeBackend("github")));
        backends.insert("linear".into(), Arc::new(FakeBackend("linear")));
        let client = BackendClient {
            backends,
            default_backend: Some("linear".into()),
        };
        let b = client.resolve(None).unwrap();
        assert_eq!(b.name(), "linear");
        let b = client.resolve(Some("github")).unwrap();
        assert_eq!(b.name(), "github");
    }

    // --- helpers ---

    struct FakeBackend(&'static str);

    #[async_trait::async_trait]
    impl Backend for FakeBackend {
        fn name(&self) -> &'static str {
            self.0
        }
        async fn create_issue(
            &self,
            _p: crate::tickets::api::backends::CreateIssueParams,
        ) -> Result<crate::tickets::api::models::Issue> {
            unimplemented!()
        }
        async fn get_issue(&self, _id: &str) -> Result<crate::tickets::api::models::Issue> {
            unimplemented!()
        }
        async fn update_issue(
            &self,
            _id: &str,
            _p: crate::tickets::api::backends::UpdateIssueParams,
        ) -> Result<crate::tickets::api::models::Issue> {
            unimplemented!()
        }
        async fn close_issue(
            &self,
            _id: &str,
            _c: Option<&str>,
        ) -> Result<crate::tickets::api::models::Issue> {
            unimplemented!()
        }
        async fn reopen_issue(&self, _id: &str) -> Result<crate::tickets::api::models::Issue> {
            unimplemented!()
        }
        async fn list_issues(
            &self,
            _p: crate::tickets::api::backends::ListIssuesParams,
        ) -> Result<Vec<crate::tickets::api::models::Issue>> {
            unimplemented!()
        }
        async fn search_issues(
            &self,
            _p: crate::tickets::api::backends::SearchIssuesParams,
        ) -> Result<Vec<crate::tickets::api::models::Issue>> {
            unimplemented!()
        }
        async fn add_comment(
            &self,
            _i: &str,
            _b: &str,
        ) -> Result<crate::tickets::api::models::Comment> {
            unimplemented!()
        }
        async fn list_comments(
            &self,
            _i: &str,
        ) -> Result<Vec<crate::tickets::api::models::Comment>> {
            unimplemented!()
        }
        async fn update_comment(
            &self,
            _i: &str,
            _c: &str,
            _b: &str,
        ) -> Result<crate::tickets::api::models::Comment> {
            unimplemented!()
        }
        async fn delete_comment(&self, _i: &str, _c: &str) -> Result<()> {
            unimplemented!()
        }
        async fn list_labels(&self) -> Result<Vec<crate::tickets::api::models::Label>> {
            unimplemented!()
        }
        async fn create_label(
            &self,
            _n: &str,
            _c: Option<&str>,
            _d: Option<&str>,
        ) -> Result<crate::tickets::api::models::Label> {
            unimplemented!()
        }
        async fn add_labels(&self, _i: &str, _l: &[String]) -> Result<()> {
            unimplemented!()
        }
        async fn remove_labels(&self, _i: &str, _l: &[String]) -> Result<()> {
            unimplemented!()
        }
        async fn list_milestones(&self) -> Result<Vec<crate::tickets::api::models::Milestone>> {
            unimplemented!()
        }
        async fn create_milestone(
            &self,
            _p: crate::tickets::api::backends::CreateMilestoneParams,
        ) -> Result<crate::tickets::api::models::Milestone> {
            unimplemented!()
        }
        async fn close_milestone(
            &self,
            _id: &str,
        ) -> Result<crate::tickets::api::models::Milestone> {
            unimplemented!()
        }
        async fn get_milestone_issues(
            &self,
            _id: &str,
        ) -> Result<Vec<crate::tickets::api::models::Issue>> {
            unimplemented!()
        }
        async fn list_projects(&self) -> Result<Vec<crate::tickets::api::models::Project>> {
            unimplemented!()
        }
        async fn get_project(&self, _id: &str) -> Result<crate::tickets::api::models::Project> {
            unimplemented!()
        }
        async fn list_epics(&self) -> Result<Vec<crate::tickets::api::models::Issue>> {
            unimplemented!()
        }
        async fn get_epic_issues(
            &self,
            _id: &str,
        ) -> Result<Vec<crate::tickets::api::models::Issue>> {
            unimplemented!()
        }
        async fn create_project_update(
            &self,
            _p: &str,
            _b: &str,
            _h: Option<&str>,
        ) -> Result<crate::tickets::api::models::ProjectUpdate> {
            unimplemented!()
        }
        async fn list_project_updates(
            &self,
            _p: &str,
        ) -> Result<Vec<crate::tickets::api::models::ProjectUpdate>> {
            unimplemented!()
        }
        async fn list_states(&self) -> Result<Vec<String>> {
            unimplemented!()
        }
        async fn transition_issue(
            &self,
            _id: &str,
            _s: &str,
        ) -> Result<crate::tickets::api::models::Issue> {
            unimplemented!()
        }
        async fn assign_issue(
            &self,
            _id: &str,
            _a: &str,
        ) -> Result<crate::tickets::api::models::Issue> {
            unimplemented!()
        }
    }
}
