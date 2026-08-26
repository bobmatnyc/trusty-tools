//! Azure DevOps integration configuration (Phase 1).
//!
//! Phase 1 establishes the configuration schema, validation, and the
//! `AB#(\d+)` ticket-reference regex. No HTTP calls are made — see
//! [`crate::collect::azdo`] for the stub client. Phase 2 will add the
//! HTTP session, an auth probe against `GET _apis/connectionData`, and
//! work-item fetching.
//!
//! # Design decisions (Phase 1)
//!
//! - **PAT-only authentication.** OAuth / Azure AD is deferred to Phase 2.
//! - **Cloud-only.** On-premises ADO Server (TFS) URLs are rejected at
//!   config load time with an explicit error. Only `dev.azure.com` and
//!   `*.visualstudio.com` are accepted.
//! - **`AB#N` work-item references only.** Bare `#N` is intentionally
//!   excluded — it collides with GitHub PR/issue numbers.
//!
//! Lives under `pm.azure_devops` in YAML (clean namespace; avoids the
//! `jira` / `jira_integration` dual-stack of the Python predecessor).

use serde::Deserialize;

use crate::core::errors::TgaError;

/// Configuration for Microsoft Azure DevOps integration (Phase 1 — PAT auth, cloud only).
///
/// On-premises ADO Server (TFS) is not supported in Phase 1. Config validation
/// rejects non-cloud URLs at load time. Phase 2 will add OAuth and work-item fetching.
// #5770: `Debug` is hand-written in `super::credential_debug`, not derived —
// the derived one printed `pat` in the clear.
// #5775: `Serialize` is hand-written in `super::credential_serialize` for the
// same field.
#[derive(Clone, Deserialize)]
pub struct AzureDevOpsConfig {
    /// Azure DevOps organisation URL. Must be `https://dev.azure.com/{org}` or
    /// `https://{org}.visualstudio.com`. On-prem TFS/ADO Server URLs are rejected.
    pub organization_url: String,

    /// Personal Access Token. Supports `${AZURE_DEVOPS_PAT}` placeholder notation
    /// (note: tga does not interpolate env-vars — substitute before writing the config).
    /// Empty or whitespace-only values are rejected at validation time.
    pub pat: String,

    /// Azure DevOps project name (e.g. `"MyProject"`).
    ///
    /// Optional: when omitted, `projects` must be non-empty. When both `project`
    /// and `projects` are set, they are combined (preserving order, deduped) by
    /// [`Self::projects`]. Kept as `Option<String>` for back-compat with the
    /// pre-#91 single-project schema.
    #[serde(default)]
    pub project: Option<String>,

    /// Additional Azure DevOps project names to query (issue #91 — multi-project
    /// ADO orgs).
    ///
    /// PR fetching iterates each project in order until a hit is found
    /// (404 from project A falls through to project B). Empty by default to
    /// preserve the single-project schema.
    #[serde(default)]
    pub projects: Vec<String>,

    /// Regex pattern used to detect ADO work-item references in commit messages.
    /// Default: `"(?i)\\bAB#(\\d+)\\b"` — case-insensitive with word boundaries,
    /// matching the regex previously hardcoded in `extract_work_item_refs`
    /// before #90 made it configurable. Bare `#N` is intentionally excluded —
    /// it collides with GitHub PR/issue numbers.
    #[serde(default = "default_ticket_regex")]
    pub ticket_regex: String,

    /// Team keys to filter (e.g. `["ENG", "PLATFORM"]`). Empty = all teams.
    #[serde(default)]
    pub team_keys: Vec<String>,

    /// Whether to fetch work items on commit reference (Phase 2+, currently ignored).
    #[serde(default = "default_true")]
    pub fetch_on_reference: bool,

    /// Whether to fetch pull request metadata + reviewers from ADO.
    ///
    /// When `true`, the collector scans commit messages for `Merged PR NNNN:`
    /// patterns, fetches PR details for each unique ID via the ADO REST API,
    /// and stores them in the `pull_requests` and `pr_reviewers` tables with
    /// `provider = 'azdo'`. Default `false` to preserve the previous behaviour
    /// (no PR HTTP traffic) for users that haven't opted in.
    #[serde(default)]
    pub fetch_prs: bool,
}

fn default_ticket_regex() -> String {
    // Preserves byte-for-byte parity with the regex previously hardcoded
    // inside `extract_work_item_refs` (case-insensitive, word-bounded).
    // Changing this changes default-config user behaviour — see #90.
    r"(?i)\bAB#(\d+)\b".to_string()
}

fn default_true() -> bool {
    true
}

impl AzureDevOpsConfig {
    /// Validate the configuration.
    ///
    /// # Errors
    ///
    /// Returns [`TgaError::ConfigError`] when:
    /// - `organization_url` is empty
    /// - `organization_url` is an on-premises / TFS URL (only
    ///   `dev.azure.com` and `*.visualstudio.com` are accepted in Phase 1)
    /// - `pat` is empty or whitespace-only
    /// - both `project` and `projects` are empty (at least one project must be
    ///   configured)
    pub fn validate(&self) -> Result<(), TgaError> {
        if self.organization_url.trim().is_empty() {
            return Err(TgaError::ConfigError(
                "pm.azure_devops.organization_url must not be empty".into(),
            ));
        }
        if !is_cloud_url(&self.organization_url) {
            return Err(TgaError::ConfigError(format!(
                "pm.azure_devops.organization_url {:?} is not an Azure DevOps cloud URL — \
                 on-premises ADO Server / TFS is not supported in Phase 1 \
                 (only dev.azure.com and *.visualstudio.com are accepted)",
                self.organization_url
            )));
        }
        if self.pat.trim().is_empty() {
            return Err(TgaError::ConfigError(
                "pm.azure_devops.pat must not be empty (Phase 1 uses PAT authentication)".into(),
            ));
        }
        if self.projects().is_empty() {
            return Err(TgaError::ConfigError(
                "pm.azure_devops.project (or .projects) must not be empty".into(),
            ));
        }
        Ok(())
    }

    /// Return the deduplicated, order-preserving union of `project` and
    /// `projects`.
    ///
    /// Order: the single `project` (if any, and non-blank) comes first,
    /// followed by entries from `projects` in declaration order. Blank /
    /// whitespace-only entries are dropped. Used by PR fetching to try each
    /// project in turn until a 200 hit (issue #91).
    pub fn projects(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        if let Some(p) = self.project.as_deref() {
            let t = p.trim();
            if !t.is_empty() {
                out.push(t);
            }
        }
        for p in &self.projects {
            let t = p.trim();
            if !t.is_empty() && !out.contains(&t) {
                out.push(t);
            }
        }
        out
    }
}

/// Returns true if the URL is a valid Azure DevOps cloud URL.
///
/// Accepts only `dev.azure.com` and `*.visualstudio.com` host suffixes —
/// matches the Phase 1 ADR's "cloud only" decision.
fn is_cloud_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.contains("dev.azure.com") || lower.contains(".visualstudio.com")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(url: &str, pat: &str, project: &str) -> AzureDevOpsConfig {
        AzureDevOpsConfig {
            organization_url: url.to_string(),
            pat: pat.to_string(),
            project: if project.is_empty() {
                None
            } else {
                Some(project.to_string())
            },
            projects: vec![],
            ticket_regex: default_ticket_regex(),
            team_keys: vec![],
            fetch_on_reference: true,
            fetch_prs: false,
        }
    }

    #[test]
    fn cloud_url_accepted() {
        let c = cfg("https://dev.azure.com/myorg", "secret-pat", "MyProject");
        c.validate().expect("dev.azure.com URL should validate");
    }

    #[test]
    fn visualstudio_url_accepted() {
        let c = cfg("https://myorg.visualstudio.com", "secret-pat", "MyProject");
        c.validate().expect("visualstudio.com URL should validate");
    }

    #[test]
    fn on_prem_url_rejected() {
        let c = cfg("https://tfs.mycompany.com/tfs", "secret-pat", "MyProject");
        let err = c.validate().expect_err("on-prem URL must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("on-premises") || msg.contains("not an Azure DevOps cloud URL"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn empty_pat_rejected() {
        let c = cfg("https://dev.azure.com/myorg", "   ", "MyProject");
        let err = c.validate().expect_err("whitespace PAT must be rejected");
        assert!(format!("{err}").contains("pat"));
    }

    #[test]
    fn empty_url_rejected() {
        let c = cfg("", "secret", "MyProject");
        c.validate().expect_err("empty url must be rejected");
    }

    #[test]
    fn empty_project_rejected() {
        let c = cfg("https://dev.azure.com/myorg", "secret", "");
        c.validate().expect_err("empty project must be rejected");
    }

    #[test]
    fn default_ticket_regex_is_ab_hash() {
        // Must match the regex that was hardcoded in
        // `extract_work_item_refs` before #90 — case-insensitive with
        // word boundaries — so default-config users see identical behaviour
        // after the regex became configurable.
        assert_eq!(default_ticket_regex(), r"(?i)\bAB#(\d+)\b");
    }

    #[test]
    fn default_ticket_regex_matches_lowercase_ab() {
        // Regression guard: previously the hardcoded `(?i)` flag let
        // `ab#42` / `Ab#42` resolve. Dropping the flag in the default would
        // silently break ADO orgs that rely on case-insensitive matching.
        let re = regex::Regex::new(&default_ticket_regex()).expect("default compiles");
        for sample in ["AB#42", "ab#42", "Ab#42", "aB#42"] {
            assert!(re.is_match(sample), "default regex should match {sample:?}");
        }
        assert!(!re.is_match("xAB#42y"), "word boundaries must still apply");
    }

    #[test]
    fn yaml_deserialization() {
        let yaml = r#"
organization_url: "https://dev.azure.com/myorg"
pat: "secret-pat"
project: "MyProject"
team_keys: ["ENG", "PLATFORM"]
"#;
        let parsed: AzureDevOpsConfig =
            serde_yaml::from_str(yaml).expect("should deserialize cleanly");
        assert_eq!(parsed.organization_url, "https://dev.azure.com/myorg");
        assert_eq!(parsed.pat, "secret-pat");
        assert_eq!(parsed.project.as_deref(), Some("MyProject"));
        assert_eq!(parsed.team_keys, vec!["ENG", "PLATFORM"]);
        // Defaults applied.
        assert_eq!(parsed.ticket_regex, r"(?i)\bAB#(\d+)\b");
        assert!(parsed.fetch_on_reference);
    }

    // ----- Issue #91: multi-project tests -----

    #[test]
    fn projects_back_compat_single_project_field() {
        // YAML using only the legacy `project:` field must produce a
        // single-entry `projects()` list (back-compat with pre-#91 schema).
        let yaml = r#"
organization_url: "https://dev.azure.com/myorg"
pat: "secret-pat"
project: "BottomLineSystems"
"#;
        let parsed: AzureDevOpsConfig =
            serde_yaml::from_str(yaml).expect("should deserialize cleanly");
        assert_eq!(parsed.projects(), vec!["BottomLineSystems"]);
        parsed.validate().expect("single project should validate");
    }

    #[test]
    fn projects_new_list_form() {
        // YAML using only the new `projects:` list returns entries in order.
        let yaml = r#"
organization_url: "https://dev.azure.com/myorg"
pat: "secret-pat"
projects: ["A", "B"]
"#;
        let parsed: AzureDevOpsConfig =
            serde_yaml::from_str(yaml).expect("should deserialize cleanly");
        assert_eq!(parsed.projects(), vec!["A", "B"]);
        parsed.validate().expect("projects list should validate");
    }

    #[test]
    fn projects_both_fields_set_single_first_then_list() {
        // When both `project` and `projects` are set, the single value comes
        // first and the list follows. Result is deduped while preserving order.
        let yaml = r#"
organization_url: "https://dev.azure.com/myorg"
pat: "secret-pat"
project: "A"
projects: ["B", "C"]
"#;
        let parsed: AzureDevOpsConfig =
            serde_yaml::from_str(yaml).expect("should deserialize cleanly");
        assert_eq!(parsed.projects(), vec!["A", "B", "C"]);
    }

    #[test]
    fn projects_dedup_across_single_and_list() {
        // A name appearing in both `project` and `projects` must only appear
        // once in the output, in its first position.
        let yaml = r#"
organization_url: "https://dev.azure.com/myorg"
pat: "secret-pat"
project: "A"
projects: ["A", "B"]
"#;
        let parsed: AzureDevOpsConfig =
            serde_yaml::from_str(yaml).expect("should deserialize cleanly");
        assert_eq!(parsed.projects(), vec!["A", "B"]);
    }

    #[test]
    fn projects_filters_blank_strings() {
        // Empty / whitespace-only entries are dropped from both fields.
        let yaml = r#"
organization_url: "https://dev.azure.com/myorg"
pat: "secret-pat"
project: ""
projects: ["", "B"]
"#;
        let parsed: AzureDevOpsConfig =
            serde_yaml::from_str(yaml).expect("should deserialize cleanly");
        assert_eq!(parsed.projects(), vec!["B"]);
    }

    #[test]
    fn projects_both_empty_fails_validate() {
        // No project and empty projects list → validate() errors.
        let c = AzureDevOpsConfig {
            organization_url: "https://dev.azure.com/myorg".to_string(),
            pat: "secret".to_string(),
            project: None,
            projects: vec![],
            ticket_regex: default_ticket_regex(),
            team_keys: vec![],
            fetch_on_reference: true,
            fetch_prs: false,
        };
        let err = c
            .validate()
            .expect_err("empty project + empty projects must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("project"),
            "error should mention project: {msg}"
        );
    }

    #[test]
    fn projects_order_preserved() {
        // Declaration order is preserved (no alphabetical sort).
        let yaml = r#"
organization_url: "https://dev.azure.com/myorg"
pat: "secret-pat"
projects: ["Z", "A", "M"]
"#;
        let parsed: AzureDevOpsConfig =
            serde_yaml::from_str(yaml).expect("should deserialize cleanly");
        assert_eq!(parsed.projects(), vec!["Z", "A", "M"]);
    }

    #[test]
    fn yaml_deserialization_in_pm_block() {
        // Verifies the canonical YAML layout: pm.azure_devops.*
        let yaml = r#"
pm:
  azure_devops:
    organization_url: "https://myorg.visualstudio.com"
    pat: "x"
    project: "Demo"
"#;
        #[derive(Deserialize)]
        struct Wrap {
            pm: super::super::PmConfig,
        }
        let w: Wrap = serde_yaml::from_str(yaml).expect("pm.azure_devops should parse");
        let adc = w.pm.azure_devops.expect("azure_devops present");
        assert_eq!(adc.organization_url, "https://myorg.visualstudio.com");
        adc.validate().expect("should validate");
    }
}
