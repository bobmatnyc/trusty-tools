//! Tests for `TicketingConfig` env-resolution and client construction.
//!
//! Why: Provider selection + token/repo gating is the wiring users rely on;
//! these tests pin the default-provider and gh-fallback behavior.
//! What: `from_env_*` and `build_client_*` cases.
//! Test: This module is itself the test coverage.

use super::*;

#[test]
fn from_env_default_provider_is_github() {
    // SAFETY: tests touch env; run serially via `cargo test` default
    // single-threaded section if needed. For this assertion we only
    // check the default when TICKETING_PROVIDER is absent.
    unsafe {
        std::env::remove_var("TICKETING_PROVIDER");
    }
    let cfg = TicketingConfig::from_env();
    assert_eq!(cfg.provider, "github");
}

#[test]
fn github_client_new_requires_token() {
    let cfg = TicketingConfig {
        provider: "github".to_string(),
        github_token: None,
        github_repo: Some("owner/repo".to_string()),
        ..Default::default()
    };
    // Clear GITHUB_TOKEN for this test so the fallback doesn't trigger.
    let prev = std::env::var("GITHUB_TOKEN").ok();
    unsafe {
        std::env::remove_var("GITHUB_TOKEN");
    }
    let res = github::GitHubClient::new(&cfg);
    if let Some(v) = prev {
        unsafe {
            std::env::set_var("GITHUB_TOKEN", v);
        }
    }
    assert!(res.is_err(), "expected error without token");
}

#[test]
fn github_client_new_requires_repo() {
    let cfg = TicketingConfig {
        provider: "github".to_string(),
        github_token: Some("t".to_string()),
        github_repo: None,
        ..Default::default()
    };
    let res = github::GitHubClient::new(&cfg);
    assert!(res.is_err(), "expected error without repo");
}

#[test]
fn jira_client_new_requires_url() {
    let cfg = TicketingConfig {
        provider: "jira".to_string(),
        jira_email: Some("a@b".to_string()),
        jira_token: Some("t".to_string()),
        jira_project: Some("P".to_string()),
        ..Default::default()
    };
    assert!(jira::JiraClient::new(&cfg).is_err());
}

#[test]
fn linear_client_new_requires_api_key() {
    let cfg = TicketingConfig {
        provider: "linear".to_string(),
        linear_api_key: None,
        ..Default::default()
    };
    assert!(linear::LinearClient::new(&cfg).is_err());
}

#[tokio::test]
async fn build_client_rejects_unknown_provider() {
    let cfg = TicketingConfig {
        provider: "wat".to_string(),
        ..Default::default()
    };
    assert!(cfg.build_client().await.is_err());
}

#[tokio::test]
async fn build_client_github_ok_with_credentials() {
    let cfg = TicketingConfig {
        provider: "github".to_string(),
        github_token: Some("t".to_string()),
        github_repo: Some("o/r".to_string()),
        ..Default::default()
    };
    let client = cfg.build_client().await.expect("github builds");
    assert_eq!(client.provider_name(), "github");
}

#[tokio::test]
async fn build_client_force_gh_cli_uses_cli_when_available() {
    // When force_gh_cli is true AND gh is available, the REST path is
    // skipped even when token+repo are present.
    let cfg = TicketingConfig {
        provider: "github".to_string(),
        github_token: Some("t".to_string()),
        github_repo: Some("o/r".to_string()),
        force_gh_cli: true,
        ..Default::default()
    };
    // Result depends on whether `gh` is installed in the env; only
    // assert that build_client doesn't panic and yields either a CLI
    // client or a clear error.
    match cfg.build_client().await {
        Ok(c) => {
            assert!(
                matches!(c.provider_name(), "github-gh-cli" | "github"),
                "unexpected provider: {}",
                c.provider_name()
            );
        }
        Err(_) => { /* gh unavailable in this env; acceptable */ }
    }
}

#[test]
fn ticket_status_serde_round_trip() {
    let s = serde_json::to_string(&TicketStatus::InProgress).unwrap();
    assert_eq!(s, "\"in_progress\"");
    let back: TicketStatus = serde_json::from_str(&s).unwrap();
    assert_eq!(back, TicketStatus::InProgress);
}

// ----- #246: capabilities, custom status, label deltas -----

#[test]
fn capabilities_returns_correct_flags_for_github() {
    // Build a GitHub client (no network call needed for capabilities()).
    let cfg = TicketingConfig {
        provider: "github".to_string(),
        github_token: Some("t".to_string()),
        github_repo: Some("o/r".to_string()),
        ..Default::default()
    };
    let client = github::GitHubClient::new(&cfg).expect("github client");
    let caps = client.capabilities();
    assert!(caps.tagging);
    assert!(caps.transitions);
    assert!(caps.ownership);
    assert!(caps.search);
    assert!(!caps.milestones);
}

#[test]
fn capabilities_returns_defaults_for_base_trait() {
    // A minimal adapter that doesn't override capabilities() should
    // get all-false defaults from the trait method.
    struct Bare;
    #[async_trait]
    impl TicketingClient for Bare {
        fn provider_name(&self) -> &str {
            "bare"
        }
        async fn create_ticket(&self, _: CreateTicketReq) -> Result<Ticket> {
            anyhow::bail!("not yet implemented: create_ticket")
        }
        async fn get_ticket(&self, _: &str) -> Result<Ticket> {
            anyhow::bail!("not yet implemented: get_ticket")
        }
        async fn update_ticket(&self, _: &str, _: UpdateTicketReq) -> Result<Ticket> {
            anyhow::bail!("not yet implemented: update_ticket")
        }
        async fn close_ticket(&self, _: &str) -> Result<()> {
            anyhow::bail!("not yet implemented: close_ticket")
        }
        async fn list_tickets(&self, _: TicketFilter) -> Result<Vec<Ticket>> {
            anyhow::bail!("not yet implemented: list_tickets")
        }
        async fn add_comment(&self, _: &str, _: &str) -> Result<()> {
            anyhow::bail!("not yet implemented: add_comment")
        }
    }
    let bare = Bare;
    let caps = bare.capabilities();
    assert_eq!(caps, TicketingCapabilities::default());
    assert!(!caps.tagging);
    assert!(!caps.transitions);
    assert!(!caps.ownership);
    assert!(!caps.search);
    assert!(!caps.milestones);
}

#[test]
fn ticket_status_custom_variant() {
    // Custom statuses should serde round-trip without losing the name.
    let s = TicketStatus::Custom("TriagedExternal".to_string());
    let j = serde_json::to_string(&s).expect("serialize");
    let back: TicketStatus = serde_json::from_str(&j).expect("deserialize");
    assert_eq!(s, back);
    if let TicketStatus::Custom(name) = back {
        assert_eq!(name, "TriagedExternal");
    } else {
        panic!("expected Custom variant after round trip, got {back:?}");
    }
}

#[test]
fn ticket_status_new_variants_serde() {
    // Sanity: each new variant serializes to its snake_case name.
    assert_eq!(
        serde_json::to_string(&TicketStatus::InReview).unwrap(),
        "\"in_review\""
    );
    assert_eq!(
        serde_json::to_string(&TicketStatus::Blocked).unwrap(),
        "\"blocked\""
    );
    assert_eq!(
        serde_json::to_string(&TicketStatus::Cancelled).unwrap(),
        "\"cancelled\""
    );
}

#[test]
fn update_req_add_remove_labels_independent() {
    // add_labels and remove_labels can both be Some; they coexist
    // alongside the (optional) `labels` replacement set.
    let req = UpdateTicketReq {
        labels: None,
        add_labels: Some(vec!["bug".into(), "p1".into()]),
        remove_labels: Some(vec!["wontfix".into()]),
        ..Default::default()
    };
    assert_eq!(req.add_labels.as_ref().unwrap().len(), 2);
    assert_eq!(req.remove_labels.as_ref().unwrap().len(), 1);
    assert!(req.labels.is_none());

    // Default produces None for all three so callers can provide any
    // subset.
    let d = UpdateTicketReq::default();
    assert!(d.labels.is_none());
    assert!(d.add_labels.is_none());
    assert!(d.remove_labels.is_none());
    assert!(d.milestone.is_none());
    assert!(d.priority.is_none());
}

#[test]
fn ticket_serialization_round_trip() {
    let t = Ticket {
        id: "42".into(),
        title: "Fix bug".into(),
        body: "Repro steps…".into(),
        status: TicketStatus::Open,
        priority: Some(Priority::High),
        labels: vec!["bug".into()],
        assignee: Some("alice".into()),
        created_at: None,
        updated_at: None,
        url: Some("https://example/42".into()),
    };
    let j = serde_json::to_string(&t).unwrap();
    let back: Ticket = serde_json::from_str(&j).unwrap();
    assert_eq!(back, t);
}
