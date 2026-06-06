// TODO(#246): unify with src/ticketing/ TicketingClient — this module shells
// out to the `gh` CLI via tokio::process while src/ticketing/ exposes a
// trait-based REST + gh-CLI client. Consolidate so workflow runs reuse the
// same identity registry and transport selection as the ticketing agent.

//! Automatic GitHub issue lifecycle management for workflow runs (#84).
//!
//! # Intent
//! Why: Every workflow run optionally creates and manages a GitHub issue that
//! tracks the run from start to completion — phases, costs, and final status —
//! giving full traceability from git history to GitHub Issues without manual
//! effort. Opt-in per-workflow via `ticket_management.enabled = true`.
//! What: Wraps the `gh` CLI via `tokio::process::Command`. A `TicketManager`
//! owns a single `issue_number` across the lifecycle; hooks fire at workflow
//! start, after each phase, and at success/failure. All `gh` failures are
//! non-fatal — they are logged and the workflow continues.
//! Test: `ticket_manager_disabled_is_noop`, `ticket_management_config_deserializes`.

use std::path::Path;

use tokio::process::Command;

use crate::workflow::config::TicketManagementConfig;

/// Manages a GitHub issue across the lifetime of a single workflow run.
///
/// Why: Centralizes the `gh` CLI invocations so the engine only deals with
/// domain-level events (`on_phase_complete`, `on_workflow_success`, ...).
/// What: Stateful — creates a tracking issue in `on_workflow_start` and stores
/// the resulting number so subsequent comment/close calls reference it.
/// Test: `ticket_manager_disabled_is_noop` exercises the `enabled=false` path.
pub struct TicketManager {
    config: TicketManagementConfig,
    /// GitHub issue number created at workflow start, if any.
    pub issue_number: Option<u64>,
}

impl TicketManager {
    /// Construct a new `TicketManager` with the given config. No side effects.
    ///
    /// Why: Separates construction (cheap, always-safe) from the `gh` call in
    /// `on_workflow_start` so the engine can build the manager before deciding
    /// whether the run should even start.
    /// What: Stores the config; `issue_number` starts as `None`.
    /// Test: `ticket_manager_disabled_is_noop`.
    pub fn new(config: TicketManagementConfig) -> Self {
        Self {
            config,
            issue_number: None,
        }
    }

    /// Returns true if ticket management is enabled in the config.
    ///
    /// Why: Callers can early-exit before building expensive payload data
    /// (summaries, file counts) when ticketing is off.
    /// What: Returns `self.config.enabled`.
    /// Test: `ticket_manager_disabled_is_noop` asserts this is false by default.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Create the tracking issue at workflow start.
    ///
    /// Why: Establishes the single GitHub issue that every later hook will
    /// comment on or close. Called once, before the phase loop begins.
    /// What: Invokes `gh issue create` with labels/assignee/milestone from the
    /// config, parses the issue URL in stdout, stores the number in
    /// `self.issue_number`. On `gh` failure, logs a warning and leaves the
    /// number unset — subsequent hooks become no-ops.
    /// Test: Integration only (requires a real repo); config deserialization
    /// is covered by `ticket_management_config_deserializes`.
    pub async fn on_workflow_start(
        &mut self,
        workflow_name: &str,
        build_num: u64,
        task_preview: &str,
    ) -> anyhow::Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let preview_trunc: String = task_preview.chars().take(60).collect();
        let title = format!("[Workflow Run] {workflow_name} build #{build_num} — {preview_trunc}");

        let body = format!(
            "## Workflow Run\n\n\
             **Workflow:** `{}`\n\
             **Build:** #{}\n\
             **Started:** {}\n\n\
             ### Task\n{}\n\n\
             ---\n*Managed automatically by trusty-agents ticket lifecycle*",
            workflow_name,
            build_num,
            chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
            task_preview
        );

        // Build gh command args. Using owned Strings keeps lifetimes simple
        // given the conditional pushes below.
        let mut args: Vec<String> = vec![
            "issue".into(),
            "create".into(),
            "--repo".into(),
            self.config.repo.clone(),
            "--title".into(),
            title,
            "--body".into(),
            body,
        ];

        let labels = self.config.labels.join(",");
        if !labels.is_empty() {
            args.push("--label".into());
            args.push(labels);
        }

        if !self.config.assignee.is_empty() {
            args.push("--assignee".into());
            args.push(self.config.assignee.clone());
        }

        if !self.config.milestone.is_empty() {
            args.push("--milestone".into());
            args.push(self.config.milestone.clone());
        }

        let output = Command::new("gh").args(&args).output().await?;

        if output.status.success() {
            // `gh issue create` prints the issue URL on stdout; the issue
            // number is the last path segment.
            let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if let Some(num_str) = url.rsplit('/').next()
                && let Ok(num) = num_str.parse::<u64>()
            {
                self.issue_number = Some(num);
                tracing::info!(
                    issue = num,
                    workflow = workflow_name,
                    build = build_num,
                    "ticket manager: created issue"
                );
            }
        } else {
            let err = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("ticket manager: issue create failed: {}", err);
        }

        Ok(())
    }

    /// Add a phase-completion comment to the tracked issue.
    ///
    /// Why: Gives reviewers a per-phase timeline of model, duration, tokens,
    /// and cost without having to dig through perf JSON.
    /// What: No-op when disabled, when phase_comments is off, or when no issue
    /// was created. Otherwise posts a markdown table comment via `gh issue
    /// comment`.
    /// Test: Integration only.
    #[allow(clippy::too_many_arguments)]
    pub async fn on_phase_complete(
        &self,
        phase_name: &str,
        model: &str,
        duration_ms: u64,
        prompt_tokens: u32,
        completion_tokens: u32,
        cost_usd: f64,
        status: &str,
    ) -> anyhow::Result<()> {
        if !self.config.enabled || !self.config.phase_comments {
            return Ok(());
        }
        let Some(issue) = self.issue_number else {
            return Ok(());
        };

        let comment = format!(
            "### Phase: `{}` {}\n\n\
             | Field | Value |\n\
             |-------|-------|\n\
             | Model | `{}` |\n\
             | Duration | {:.1}s |\n\
             | Prompt tokens | {} |\n\
             | Completion tokens | {} |\n\
             | Cost | ${:.4} |",
            phase_name,
            status,
            model,
            duration_ms as f64 / 1000.0,
            prompt_tokens,
            completion_tokens,
            cost_usd
        );

        self.add_comment(issue, &comment).await
    }

    /// Add a final success comment and optionally close the issue.
    ///
    /// Why: Closes the loop on the tracking issue with a summary of the full
    /// run (cost, duration, QA outcome, files produced).
    /// What: Posts a summary comment; if `close_on_success` is true, invokes
    /// `gh issue close`. No-op when disabled or no issue was created.
    /// Test: Integration only.
    pub async fn on_workflow_success(
        &self,
        total_cost: f64,
        total_duration_ms: u64,
        qa_summary: &str,
        files_count: usize,
    ) -> anyhow::Result<()> {
        if !self.config.enabled {
            return Ok(());
        }
        let Some(issue) = self.issue_number else {
            return Ok(());
        };

        let comment = format!(
            "## ✅ Workflow Completed Successfully\n\n\
             | Metric | Value |\n\
             |--------|-------|\n\
             | Total cost | ${:.4} |\n\
             | Total duration | {:.1}s |\n\
             | QA results | {} |\n\
             | Files generated | {} |",
            total_cost,
            total_duration_ms as f64 / 1000.0,
            qa_summary,
            files_count
        );

        self.add_comment(issue, &comment).await?;

        if self.config.close_on_success {
            let close_status = Command::new("gh")
                .args([
                    "issue",
                    "close",
                    &issue.to_string(),
                    "--repo",
                    &self.config.repo,
                    "--comment",
                    "Closing — workflow run succeeded.",
                ])
                .status()
                .await?;
            if close_status.success() {
                tracing::info!(issue = issue, "ticket manager: closed issue");
            } else {
                tracing::warn!(issue = issue, "ticket manager: close failed");
            }
        }

        Ok(())
    }

    /// Add a failure comment; leave the issue open for investigation.
    ///
    /// Why: Failed runs stay visible in the open issues list so someone can
    /// triage them. The perf artifact path is included to aid postmortems.
    /// What: Posts a markdown-formatted failure comment. No-op when disabled
    /// or no issue was created.
    /// Test: Integration only.
    pub async fn on_workflow_failure(
        &self,
        failed_phase: &str,
        error_msg: &str,
        perf_json_path: Option<&Path>,
    ) -> anyhow::Result<()> {
        if !self.config.enabled {
            return Ok(());
        }
        let Some(issue) = self.issue_number else {
            return Ok(());
        };

        let perf_ref = perf_json_path
            .map(|p| format!("\n**Perf artifact:** `{}`", p.display()))
            .unwrap_or_default();

        let comment = format!(
            "## ❌ Workflow Failed\n\n\
             **Failed phase:** `{}`\n\
             **Error:** {}{}\n\n\
             Issue left open for investigation.",
            failed_phase, error_msg, perf_ref
        );

        self.add_comment(issue, &comment).await
    }

    /// Search open issues for similar work and add a "Related Issues" comment.
    ///
    /// Why: Cross-links prior related runs so PRs/issues form a loose graph
    /// without manual linking.
    /// What: Runs `gh issue list --search <keywords>` with a JSON+jq formatter,
    /// filters out the current issue, and posts the result. No-op when
    /// disabled, `auto_relate` is off, or no issue was created.
    /// Test: Integration only.
    pub async fn auto_relate(&self, task_keywords: &str) -> anyhow::Result<()> {
        if !self.config.enabled || !self.config.auto_relate {
            return Ok(());
        }
        let Some(issue) = self.issue_number else {
            return Ok(());
        };

        let output = Command::new("gh")
            .args([
                "issue",
                "list",
                "--repo",
                &self.config.repo,
                "--state",
                "all",
                "--search",
                task_keywords,
                "--limit",
                "5",
                "--json",
                "number,title",
                "--jq",
                ".[] | \"#\\(.number): \\(.title)\"",
            ])
            .output()
            .await?;

        let related = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if related.is_empty() {
            return Ok(());
        }

        let self_prefix = format!("#{issue}");
        let filtered: Vec<&str> = related
            .lines()
            .filter(|l| !l.starts_with(&self_prefix))
            .collect();
        if filtered.is_empty() {
            return Ok(());
        }

        let comment = format!(
            "### Related Issues (auto-detected)\n\n{}",
            filtered.join("\n")
        );
        self.add_comment(issue, &comment).await
    }

    /// Internal: post a markdown comment on an issue. Non-fatal on failure.
    ///
    /// Why: Centralizes the `gh issue comment` invocation so every hook
    /// renders identically and error-handling is uniform.
    /// What: Invokes `gh issue comment <n> --repo <r> --body <body>`, logs a
    /// warning on non-zero exit. Always returns `Ok(())`.
    /// Test: Covered implicitly by the hook-level integration tests.
    async fn add_comment(&self, issue: u64, body: &str) -> anyhow::Result<()> {
        let output = Command::new("gh")
            .args([
                "issue",
                "comment",
                &issue.to_string(),
                "--repo",
                &self.config.repo,
                "--body",
                body,
            ])
            .output()
            .await?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(issue = issue, "ticket manager: comment failed: {}", err);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: A freshly-constructed `TicketManager` with the default (disabled)
    /// config must never issue `gh` calls and must report `enabled() == false`.
    /// What: Construct with `TicketManagementConfig::default()` and assert
    /// `enabled()` is false and `issue_number` starts as `None`. Also invoke
    /// every hook to ensure each short-circuits cleanly.
    /// Test: This test.
    #[tokio::test]
    async fn ticket_manager_disabled_is_noop() {
        let mut tm = TicketManager::new(TicketManagementConfig::default());
        assert!(!tm.enabled());
        assert!(tm.issue_number.is_none());

        // Every hook must succeed as a no-op when disabled.
        tm.on_workflow_start("wf", 1, "preview").await.unwrap();
        assert!(
            tm.issue_number.is_none(),
            "disabled manager must not create issue"
        );

        tm.on_phase_complete("code", "m", 100, 1, 2, 0.01, "✅ success")
            .await
            .unwrap();
        tm.on_workflow_success(0.1, 1000, "14/15 passed", 3)
            .await
            .unwrap();
        tm.on_workflow_failure("qa", "boom", None).await.unwrap();
        tm.auto_relate("some keywords").await.unwrap();
    }

    /// Why: `TicketManagementConfig` must round-trip through JSON with
    /// sensible defaults so existing workflows keep parsing after #84 lands.
    /// What: Deserialize a minimal `{"enabled": true, "repo": "x/y"}` doc and
    /// assert defaults apply for the other fields (auto_relate, phase_comments,
    /// close_on_success all default to true).
    /// Test: This test.
    #[test]
    fn ticket_management_config_deserializes() {
        let raw = r#"{"enabled": true, "repo": "bobmatnyc/trusty-agents"}"#;
        let cfg: TicketManagementConfig = serde_json::from_str(raw).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.repo, "bobmatnyc/trusty-agents");
        assert!(cfg.auto_relate);
        assert!(cfg.phase_comments);
        assert!(cfg.close_on_success);
        assert!(cfg.labels.is_empty());
        assert!(cfg.assignee.is_empty());
        assert!(cfg.milestone.is_empty());
    }

    /// Why: Default-constructed config must be disabled so adding the field to
    /// `WorkflowDef` cannot accidentally start creating tickets for existing
    /// workflows that never opted in.
    /// What: Assert `TicketManagementConfig::default().enabled == false`.
    /// Test: This test.
    #[test]
    fn ticket_management_config_default_is_disabled() {
        let cfg = TicketManagementConfig::default();
        assert!(!cfg.enabled);
    }
}
