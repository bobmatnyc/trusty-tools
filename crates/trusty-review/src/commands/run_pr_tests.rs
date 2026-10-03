//! `run`'s PR-context flags (#8654) and per-repo index resolution (#8651).
//!
//! Why: `run` built `CallerContext::default()`, so PR context never reached
//! the reviewer, and derived its index once from the CWD with a `"main"`
//! fallback, so a PR in another repo was reviewed against the wrong index.
//! What: the flags are parsed through clap and read into a `CallerContext`;
//! a capturing LLM proves a canary reaches the reviewer prompt through the
//! same `run_input` `cmd_run` builds; a registry fake drives
//! `pr_index_for_run`. No network, no credentials.
//! Test: this module IS the tests.

use std::sync::{Arc, Mutex};

use clap::Parser as _;
use trusty_review::{
    config::ReviewConfig,
    integrations::{
        EmbedderState, HealthResponse, IndexInfo, NullSearchClient, SearchClient,
        SearchClientError, SearchResult,
        search_client::{IndexIdentity, IndexStatusResponse},
    },
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    pipeline::{DiffSource, ReviewDeps, pr_index::PrIndex, run_review},
};

use super::{RunArgs, caller_context_from_args, pr_index_for_run, run_input};

// ── fakes ───────────────────────────────────────────────────────────────────

/// APPROVE stub that records every prompt (system + messages) it is sent.
struct PromptCapture(Mutex<Vec<String>>);

#[async_trait::async_trait]
impl LlmProvider for PromptCapture {
    fn name(&self) -> &str {
        "prompt-capture-8654"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let mut text = req.system.clone();
        for m in &req.messages {
            text.push('\n');
            text.push_str(&m.content);
        }
        if let Ok(mut seen) = self.0.lock() {
            seen.push(text);
        }
        Ok(LlmResponse {
            text: "```json\n{\"verdict\":\"APPROVE\",\"summary\":\"ok\",\"findings\":[]}\n```"
                .into(),
            model: req.model.clone(),
            input_tokens: 1,
            output_tokens: 1,
            latency_ms: 0,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// A trusty-search registry reporting `repo_identity` per index; `None`
/// models a registry that cannot be listed.
struct Registry(Option<Vec<IndexIdentity>>);

#[async_trait::async_trait]
impl SearchClient for Registry {
    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        Ok(HealthResponse {
            status: "ok".into(),
            embedder: EmbedderState::Bool(true),
            warmboot_summary: None,
        })
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(vec![])
    }

    async fn list_index_identities(
        &self,
        repo_identity: Option<&str>,
    ) -> Result<Vec<IndexIdentity>, SearchClientError> {
        let all = self
            .0
            .clone()
            .ok_or_else(|| SearchClientError::Transport("connection refused".into()))?;
        Ok(all
            .into_iter()
            .filter(|i| repo_identity.is_none() || i.repo_identity.as_deref() == repo_identity)
            .collect())
    }

    async fn index_status(&self, id: &str) -> Result<IndexStatusResponse, SearchClientError> {
        Ok(IndexStatusResponse::ready(id))
    }

    async fn search(
        &self,
        _: &str,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Ok(vec![])
    }
}

fn two_repos() -> Registry {
    let entry = |id: &str, identity: &str| IndexIdentity {
        id: id.into(),
        root_path: Some(format!("/src/{id}")),
        repo_identity: Some(identity.into()),
        last_used_unix: None,
    };
    Registry(Some(vec![
        entry("main", "someone/scratch"),
        entry(
            "code-intelligence-9f1c2e3a",
            "duettoresearch/code-intelligence",
        ),
        entry("trusty-tools-4e2cf878", "bobmatnyc/trusty-tools"),
    ]))
}

/// A config with the `"main"` a CWD miss leaves behind, nothing pinned, and
/// the Hosted default `require_search`.
fn unpinned_config() -> ReviewConfig {
    let mut config = ReviewConfig::load(None);
    config.search_index = "main".into();
    config.search_index_explicit = false;
    config.context.require_search = None;
    config
}

fn pr(owner: &str, repo: &str) -> DiffSource {
    DiffSource::Github {
        owner: owner.into(),
        repo: repo.into(),
        pr: 7,
        token: String::new(),
    }
}

// ── #8654: PR-context flags ─────────────────────────────────────────────────

#[test]
fn pr_context_flags_read_text_and_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let discussion = dir.path().join("discussion.md");
    std::fs::write(&discussion, "author: this is intentional\n").expect("write");
    let at_file = format!("@{}", discussion.display());
    let args = RunArgs::try_parse_from([
        "run",
        "--pr-description",
        "literal description",
        "--pr-discussion",
        at_file.as_str(),
        "--referenced-code",
        "   ",
    ])
    .expect("parse");
    let caller = caller_context_from_args(&args).expect("readable flags");
    assert_eq!(
        caller.pr_description.as_deref(),
        Some("literal description")
    );
    assert_eq!(
        caller.pr_discussion.as_deref(),
        Some("author: this is intentional\n")
    );
    assert_eq!(caller.referenced_code, None, "a blank value is no context");
}

/// Fail-open check: an `@file` that cannot be read fails the run, naming the
/// flag and the path — never a review that silently lost its context.
#[test]
fn unreadable_pr_context_file_fails_the_run() {
    let args = RunArgs::try_parse_from(["run", "--pr-description", "@/nonexistent/8654.md"])
        .expect("parse");
    let err = caller_context_from_args(&args).expect_err("an unreadable file is an error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("--pr-description") && msg.contains("/nonexistent/8654.md"),
        "{msg}"
    );
}

const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
                    @@ -1 +1 @@\n-fn a() {}\n+fn a() { println!(\"a\"); }\n";

/// The canary in a `--pr-description @file` reaches the reviewer prompt
/// through the `ReviewInput` `cmd_run` builds — before #8654 `run` always
/// passed `CallerContext::default()`.
#[tokio::test]
async fn pr_description_flag_reaches_the_reviewer_prompt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let desc = dir.path().join("desc.md");
    std::fs::write(&desc, "PR description CANARY-8654-run").expect("write");
    let at_desc = format!("@{}", desc.display());
    let reviewer = reviewer_prompt(&dir, DIFF, &["--pr-description", at_desc.as_str()]).await;
    assert!(
        reviewer.contains("CANARY-8654-run"),
        "the --pr-description text must reach the reviewer prompt: {reviewer}"
    );
}

/// A local diff carrying a `# Context:` preamble — the shape
/// code-intelligence's subprocess adapter sends — reaches the reviewer as the
/// PR description. Before #8654 the parser discarded it as unattributable.
#[tokio::test]
async fn context_preamble_on_a_local_diff_reaches_the_reviewer_prompt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let diff = format!("# Context: PR description CANARY-8654-preamble\n{DIFF}");
    let reviewer = reviewer_prompt(&dir, &diff, &[]).await;
    assert!(
        reviewer.contains("CANARY-8654-preamble"),
        "the preamble must reach the reviewer prompt: {reviewer}"
    );
}

/// Run a `--local-diff` review of `diff_text` with `extra` flags under a
/// capturing LLM and return the reviewer prompt.
async fn reviewer_prompt(dir: &tempfile::TempDir, diff_text: &str, extra: &[&str]) -> String {
    let diff = dir.path().join("change.diff");
    std::fs::write(&diff, diff_text).expect("write");
    let diff_arg = diff.display().to_string();
    let mut argv = vec![
        "run",
        "--local-diff",
        diff_arg.as_str(),
        "--no-log",
        "--json",
    ];
    argv.extend_from_slice(extra);
    let args = RunArgs::try_parse_from(argv).expect("parse");

    let caller = caller_context_from_args(&args).expect("readable flag");
    let input = run_input(
        &args,
        DiffSource::LocalFile { path: diff },
        "m".into(),
        caller,
    );
    let llm = Arc::new(PromptCapture(Mutex::new(Vec::new())));
    let deps = ReviewDeps {
        llm: llm.clone(),
        verifier: None,
        search: Arc::new(NullSearchClient::new("no search in #8654 tests")),
        analyze: None,
        dedup: None,
    };
    let mut config = ReviewConfig::load(None);
    config.context.require_search = Some(false);
    config.context.require_analyze = false;
    config.log_dir = dir.path().to_path_buf();
    run_review(&config, input, deps).await;

    let prompts = llm.0.lock().map(|p| p.clone()).unwrap_or_default();
    prompts
        .first()
        .cloned()
        .expect("the reviewer must be called")
}

// ── #8651: per-repo index ───────────────────────────────────────────────────

/// Two GitHub-PR runs in different repos, neither indexed as `"main"`, each
/// resolve their own index; before #8651 both used the CWD/`"main"` index.
#[tokio::test]
async fn github_run_resolves_its_own_repo_index() {
    let config = unpinned_config();
    for (owner, repo, want) in [
        (
            "duettoresearch",
            "code-intelligence",
            "code-intelligence-9f1c2e3a",
        ),
        ("bobmatnyc", "trusty-tools", "trusty-tools-4e2cf878"),
    ] {
        let index = pr_index_for_run(&config, &two_repos(), &pr(owner, repo), false)
            .await
            .unwrap_or_else(|e| panic!("{owner}/{repo}: {e:#}"));
        assert_eq!(
            index,
            Some(PrIndex::Resolved(want.into())),
            "{owner}/{repo}"
        );
    }
}

/// Fail-open check: a GitHub-PR run whose repo has no index, or whose registry
/// cannot be read (search required by default on this surface), fails —
/// never a review against `"main"`.
#[tokio::test]
async fn github_run_with_no_repo_index_fails() {
    let config = unpinned_config();
    let err = pr_index_for_run(&config, &two_repos(), &pr("acme", "unindexed"), false)
        .await
        .expect_err("an unindexed repo must fail");
    assert!(format!("{err:#}").contains("acme/unindexed"), "{err:#}");

    let err = pr_index_for_run(&config, &Registry(None), &pr("acme", "widget"), false)
        .await
        .expect_err("an unreadable registry must fail while search is required");
    assert!(format!("{err:#}").contains("acme/widget"), "{err:#}");
}

/// An operator-pinned index (`TRUSTY_SEARCH_INDEX` or `--source-root`) and a
/// local diff keep the existing CWD/`--source-root` resolution.
#[tokio::test]
async fn pinned_or_local_run_keeps_cwd_resolution() {
    let mut pinned = unpinned_config();
    pinned.search_index_explicit = true;
    let source = pr("bobmatnyc", "trusty-tools");
    let none = pr_index_for_run(&pinned, &two_repos(), &source, false).await;
    assert_eq!(none.ok(), Some(None), "TRUSTY_SEARCH_INDEX wins");

    let config = unpinned_config();
    let none = pr_index_for_run(&config, &two_repos(), &source, true).await;
    assert_eq!(none.ok(), Some(None), "--source-root wins");

    let none = pr_index_for_run(&config, &two_repos(), &DiffSource::Stdin, false).await;
    assert_eq!(none.ok(), Some(None), "a local diff names no repo");
}
