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

use super::{
    PR_CONTEXT_FILE_CAP, RunArgs, RunPin, caller_context_from_args, pr_index_for_run, run_input,
};

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

/// Text flags are literal — a leading `@` is plain text, so "@alice" stays
/// verbatim — and each `-file` twin reads its file. A text flag and its
/// `-file` twin conflict.
#[test]
fn pr_context_flags_read_text_and_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let discussion = dir.path().join("discussion.md");
    std::fs::write(&discussion, "author: this is intentional\n").expect("write");
    let file = discussion.display().to_string();
    let args = RunArgs::try_parse_from([
        "run",
        "--pr-description",
        "@alice please review the retry loop",
        "--pr-discussion-file",
        file.as_str(),
        "--referenced-code",
        "   ",
    ])
    .expect("parse");
    let caller = caller_context_from_args(&args).expect("readable flags");
    assert_eq!(
        caller.pr_description.as_deref(),
        Some("@alice please review the retry loop")
    );
    assert_eq!(
        caller.pr_discussion.as_deref(),
        Some("author: this is intentional\n")
    );
    assert_eq!(caller.referenced_code, None, "a blank value is no context");

    for (text, file_flag) in [
        ("--pr-description", "--pr-description-file"),
        ("--pr-discussion", "--pr-discussion-file"),
        ("--referenced-code", "--referenced-code-file"),
    ] {
        let both = RunArgs::try_parse_from(["run", text, "x", file_flag, file.as_str()]);
        assert!(both.is_err(), "{text} and {file_flag} must conflict");
    }
}

/// Fail-open check: a `-file` that cannot be read fails the run, naming the
/// flag and the path — never a review that silently lost its context.
#[test]
fn unreadable_pr_context_file_fails_the_run() {
    let args = RunArgs::try_parse_from(["run", "--pr-description-file", "/nonexistent/8654.md"])
        .expect("parse");
    let err = caller_context_from_args(&args).expect_err("an unreadable file is an error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("--pr-description-file") && msg.contains("/nonexistent/8654.md"),
        "{msg}"
    );
}

/// A file over the 256 KiB cap, and a non-regular file such as `/dev/zero`
/// (which an unbounded read never finishes), are refused; a file at exactly
/// the cap is read whole.
#[test]
fn pr_context_file_over_the_cap_or_not_regular_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cap = usize::try_from(PR_CONTEXT_FILE_CAP).expect("cap fits usize");
    let at_cap = dir.path().join("at-cap.md");
    std::fs::write(&at_cap, "a".repeat(cap)).expect("write");
    let over = dir.path().join("over.md");
    std::fs::write(&over, "a".repeat(cap + 1)).expect("write");

    let read = |path: &str| {
        let args = RunArgs::try_parse_from(["run", "--referenced-code-file", path]).expect("parse");
        caller_context_from_args(&args).map(|c| c.referenced_code.map_or(0, |t| t.len()))
    };
    let len = read(&at_cap.display().to_string()).expect("a file at the cap is read");
    assert_eq!(len, cap);
    let err = read(&over.display().to_string()).expect_err("a file over the cap is refused");
    assert!(format!("{err:#}").contains("cap"), "{err:#}");
    for path in ["/dev/zero", dir.path().to_str().expect("utf-8 tempdir")] {
        let err = read(path).expect_err("a non-regular file is refused");
        assert!(
            format!("{err:#}").contains("not a regular file"),
            "{path}: {err:#}"
        );
    }
}

const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
                    @@ -1 +1 @@\n-fn a() {}\n+fn a() { println!(\"a\"); }\n";

/// The canary in a `--pr-description-file` reaches the reviewer prompt
/// through the `ReviewInput` `cmd_run` builds — before #8654 `run` always
/// passed `CallerContext::default()`.
#[tokio::test]
async fn pr_description_flag_reaches_the_reviewer_prompt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let desc = dir.path().join("desc.md");
    std::fs::write(&desc, "PR description CANARY-8654-run").expect("write");
    let desc = desc.display().to_string();
    let reviewer = reviewer_prompt(&dir, DIFF, &["--pr-description-file", desc.as_str()]).await;
    assert!(
        reviewer.contains("CANARY-8654-run"),
        "the --pr-description-file text must reach the reviewer prompt: {reviewer}"
    );
}

/// A PR description past the per-field cap reaches the prompt cut, with a
/// visible truncation marker; text past the cap never does (#8654).
#[tokio::test]
async fn oversized_pr_description_is_truncated_visibly_in_the_prompt() {
    use trusty_review::config::constants::MAX_CALLER_CONTEXT_CHARS;
    let dir = tempfile::tempdir().expect("tempdir");
    let long = format!(
        "CANARY-8654-head {}CANARY-8654-tail",
        "x".repeat(MAX_CALLER_CONTEXT_CHARS)
    );
    let reviewer = reviewer_prompt(&dir, DIFF, &["--pr-description", long.as_str()]).await;
    assert!(reviewer.contains("CANARY-8654-head"), "the head survives");
    assert!(
        !reviewer.contains("CANARY-8654-tail"),
        "text past the cap is cut"
    );
    assert!(reviewer.contains("[... truncated: "), "the cut is marked");
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
        let index = pr_index_for_run(&config, &two_repos(), &pr(owner, repo), RunPin::None)
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
    let err = pr_index_for_run(
        &config,
        &two_repos(),
        &pr("acme", "unindexed"),
        RunPin::None,
    )
    .await
    .expect_err("an unindexed repo must fail");
    assert!(format!("{err:#}").contains("acme/unindexed"), "{err:#}");

    let err = pr_index_for_run(
        &config,
        &Registry(None),
        &pr("acme", "widget"),
        RunPin::None,
    )
    .await
    .expect_err("an unreadable registry must fail while search is required");
    assert!(format!("{err:#}").contains("acme/widget"), "{err:#}");
}

/// A pinned config: `index` in `search_index`, with the flag either pin sets.
fn pinned_config(index: &str) -> ReviewConfig {
    let mut config = unpinned_config();
    config.search_index = index.into();
    config.search_index_explicit = true;
    config
}

/// `TRUSTY_SEARCH_INDEX` is a pin, not a bypass: a foreign pin — such as an
/// ambient value set for every session — is refused, naming both repos; a pin
/// recorded for the PR's repo is used. Before this round the pin skipped
/// per-repo resolution and the run reviewed against the foreign index.
#[tokio::test]
async fn foreign_pin_is_refused_and_matching_pin_is_used() {
    let source = pr("bobmatnyc", "trusty-tools");
    let err = pr_index_for_run(&pinned_config("main"), &two_repos(), &source, RunPin::Env)
        .await
        .expect_err("a foreign pin must be refused");
    let msg = format!("{err:#}");
    for needle in [
        "TRUSTY_SEARCH_INDEX",
        "someone/scratch",
        "bobmatnyc/trusty-tools",
    ] {
        assert!(msg.contains(needle), "{needle}: {msg}");
    }

    let config = pinned_config("trusty-tools-4e2cf878");
    let index = pr_index_for_run(&config, &two_repos(), &source, RunPin::Env).await;
    assert_eq!(
        index.ok(),
        Some(Some(PrIndex::Resolved("trusty-tools-4e2cf878".into())))
    );

    let local = pr_index_for_run(&config, &two_repos(), &DiffSource::Stdin, RunPin::Env).await;
    assert_eq!(local.ok(), Some(None), "a local diff keeps CWD resolution");
}

/// The index `--source-root` mapped to is checked against the PR repo: a
/// mismatch fails, a legacy index with no recorded identity is used (with a
/// warning), and a `--source-root` that already forced diff-only stays so.
#[tokio::test]
async fn source_root_index_is_checked_against_the_pr_repo() {
    let Registry(Some(mut indexes)) = two_repos() else {
        unreachable!("two_repos lists indexes")
    };
    indexes.push(IndexIdentity {
        id: "legacy-tt".into(),
        root_path: Some("/src/legacy-tt".into()),
        repo_identity: None,
        last_used_unix: None,
    });
    let registry = Registry(Some(indexes));
    let source = pr("bobmatnyc", "trusty-tools");

    let err = pr_index_for_run(
        &pinned_config("main"),
        &registry,
        &source,
        RunPin::SourceRoot,
    )
    .await
    .expect_err("a --source-root index of another repo must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("--source-root") && msg.contains("someone/scratch"),
        "{msg}"
    );

    let config = pinned_config("legacy-tt");
    let legacy = pr_index_for_run(&config, &registry, &source, RunPin::SourceRoot).await;
    assert_eq!(
        legacy.ok(),
        Some(Some(PrIndex::Resolved("legacy-tt".into())))
    );

    let diff_only = pr_index_for_run(&config, &registry, &source, RunPin::SourceRootDiffOnly).await;
    assert_eq!(diff_only.ok(), Some(None), "the diff-only fallback is kept");
}

/// `cmd_run`'s pin classification: `--source-root` wins over
/// `TRUSTY_SEARCH_INDEX`, as `resolve_source_root_arg` decides (#8651).
#[test]
fn run_pin_classifies_the_flags() {
    assert_eq!(RunPin::from_flags(true, true, false), RunPin::SourceRoot);
    assert_eq!(
        RunPin::from_flags(true, true, true),
        RunPin::SourceRootDiffOnly
    );
    assert_eq!(RunPin::from_flags(false, true, false), RunPin::SourceRoot);
    assert_eq!(
        RunPin::from_flags(false, true, true),
        RunPin::SourceRootDiffOnly
    );
    assert_eq!(RunPin::from_flags(false, false, false), RunPin::None);
}

/// With no `--source-root`, the env pin is still classified as a pin, so
/// `pr_index_for_run` verifies it against the PR repo (#8651).
#[test]
fn run_pin_env_pin_applies_without_source_root() {
    assert_eq!(RunPin::from_flags(true, false, false), RunPin::Env);
    assert_eq!(RunPin::from_flags(true, false, true), RunPin::Env);
}
