//! End-to-end Stage 1 collection pipeline.
//!
//! Orchestrates git extraction, identity resolution, and optional GitHub
//! and JIRA fetches against a configured [`crate::core::config::Config`].

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use tracing::{info, warn};

use crate::collect::azdo::AzureDevOpsClient;
use crate::collect::bitbucket::BitbucketClient;
use crate::collect::errors::Result;
use crate::collect::fault::CollectionFault;
use crate::collect::git::walk_state;
use crate::collect::git::GitCollector;
use crate::collect::github::budget::RunBudget;
use crate::collect::github::GitHubClient;
use crate::collect::identity::IdentityResolver;
use crate::collect::linear_pipeline;
use crate::collect::notify;
use crate::collect::pr_provider::PrProvider;
use crate::collect::weeks::{clamp_week_to_range, weeks_in_range};
use crate::collect::work_item_pipeline;
use crate::core::config::Config;
use crate::core::db::{self, Database};
use crate::core::models::PullRequest;
use crate::core::progress::{ProgressBus, ProgressEvent, Stage};

/// Outcome of a `git fetch origin` attempt for a single repository.
///
/// Why: fetch errors are invisible unless the user reads tracing logs;
/// surfacing them in `CollectionStats` lets the CLI print an actionable
/// end-of-run summary.
/// What: three variants — Success (remote updated), Failed (network/auth error
/// recorded as a string), Skipped (no-fetch flag or no remote configured).
/// Test: covered by `commands::collect` integration tests.
#[derive(Debug, Clone)]
pub enum FetchOutcome {
    /// Remote was fetched successfully.
    Success {
        /// Name of the remote (usually `"origin"`).
        remote: String,
    },
    /// Fetch was attempted but failed.
    Failed {
        /// Name of the remote that was tried.
        remote: String,
        /// Human-readable error description.
        error: String,
    },
    /// Fetch was not attempted.
    Skipped {
        /// Reason the fetch was skipped (e.g. `"--no-fetch"` or `"no remote"`).
        reason: String,
    },
}

/// Per-repository fetch result, collected into [`CollectionStats::fetch_outcomes`].
///
/// Why: groups the display name of the repo with its fetch outcome so the
/// end-of-run summary can be printed without re-querying the git repo.
/// What: plain data carrier.
/// Test: covered by collection pipeline integration tests.
#[derive(Debug, Clone)]
pub struct PerRepoFetch {
    /// Display name of the repository (from config `name` or dir basename).
    pub repo: String,
    /// Outcome of the fetch attempt for this repo.
    pub outcome: FetchOutcome,
}

/// Aggregate statistics for a single pipeline run.
///
/// Why: callers (CLI, integration tests) need a single typed object
/// describing what the run did, both for stdout output and for asserting
/// expectations in tests.
/// What: counter struct populated by [`CollectionPipeline::run`]; the
/// `errors` vec accumulates non-fatal faults, each tagged with whether a whole
/// stage failed or one record was skipped (#5655).
/// Test: covered by `tests::collect_integration_repo` (integration test
/// that runs the pipeline against a fixture repo).
#[derive(Debug, Clone, Default)]
pub struct CollectionStats {
    /// Number of new commit rows written across all repositories.
    pub commits_collected: usize,
    /// Number of distinct authors observed and upserted.
    pub authors_resolved: usize,
    /// Number of PR rows written (zero if GitHub fetch disabled).
    pub prs_fetched: usize,
    /// Number of GitHub reviewer rows written via the reviews pass (issue #742).
    pub reviewers_fetched: usize,
    /// Number of Linear issues fetched (0 if Linear not configured).
    pub linear_issues_fetched: usize,
    /// Number of `(repo, week)` pairs that were collected this run.
    pub weeks_collected: usize,
    /// Number of `(repo, week)` pairs skipped because already present in
    /// `collection_runs` (and `force` was false).
    pub weeks_skipped: usize,
    /// Number of repositories whose full-history walk was skipped outright
    /// because the extract database was already current at their tips (#6073).
    ///
    /// Why: this is the only observable that separates a skipped walk from a
    /// full re-walk that happens to insert nothing — both leave
    /// `commits_collected` at zero, so a caller (or a test) asserting on that
    /// alone cannot tell whether the walk was avoided.
    /// Test: `crate::collect::git::extractor::tests::unchanged_head_skips_the_walk`.
    pub repos_skipped: usize,
    /// Non-fatal faults encountered, each tagged with its blast radius (#5655).
    ///
    /// Nothing in the pipeline aborts on a fault. A `StageFailed` entry means
    /// that stage's data is absent from the database, which is what
    /// [`Self::stage_failures`] hands the caller so the process exit code can
    /// say so.
    pub errors: Vec<CollectionFault>,
    /// Total `fact_commit_reachability` rows upserted across all repos.
    pub reachability_rows: usize,
    /// Per-repo fetch outcomes (one entry per repository attempted).
    ///
    /// Populated in the per-repo loop; used by the CLI to print the
    /// end-of-collect fetch summary.
    pub fetch_outcomes: Vec<PerRepoFetch>,
}

/// Top-level Stage 1 orchestrator.
///
/// Why: callers should not need to know the order of git extraction,
/// identity resolution, GitHub fetch, and JIRA fetch — the pipeline owns
/// the orchestration.
/// What: holds a validated [`Config`] plus boolean toggles for forced
/// re-collection, offline runs (`no_fetch`), and PR re-fetch
/// (`force_refresh_prs`). Constructed via [`Self::new`] + builder methods.
/// Test: covered by `tests::pipeline_constructs_with_default_config` and
/// the integration test `tests/integration_test.rs`.
pub struct CollectionPipeline {
    config: Config,
    force: bool,
    no_fetch: bool,
    force_refresh_prs: bool,
    /// When `true`, skip the tag and release-branch reachability scan
    /// (i.e. do not populate `fact_commit_reachability` with tag/branch data).
    skip_tag_reachability: bool,
    /// When `true`, seed every repository's revwalk from HEAD only (legacy
    /// 1.x behaviour).  When `false` (default since 2.0.0), all local branch
    /// heads and `refs/remotes/origin/*` refs are pushed so commits on
    /// non-default branches are not silently excluded.
    ///
    /// A per-repo `head_only: true` in `RepositoryConfig` provides the same
    /// opt-out for a single repository while keeping all-branch coverage for
    /// the rest.  The global flag here is OR-ed with the per-repo flag — if
    /// either is `true`, that repo walks HEAD only.
    head_only: bool,
    /// Explicit branch list for the `--branch` CLI filter.
    ///
    /// When non-empty, the revwalk is seeded from only these branch names
    /// (both `refs/heads/<name>` and `refs/remotes/origin/<name>` for each).
    /// Mutually exclusive with `head_only` — the CLI enforces this via
    /// `conflicts_with`.  An empty Vec means "no restriction" (the default).
    branches: Vec<String>,
    /// When `true`, exit non-zero after the collect summary if any repo had a
    /// fetch failure. Default `false` — failures are visible but non-fatal.
    strict_fetch: bool,
    /// When `true`, print a success line for every fetched repo in the summary
    /// (not just failures). Default `false` — only failures are printed.
    verbose_fetch: bool,
    /// #5197: optional live-progress sink. Defaults to
    /// [`ProgressBus::disabled`], on which every emit is a no-op — so the CLI
    /// path, including its `indicatif` bars, behaves exactly as before.
    progress: ProgressBus,
    /// #6565: the ONE rate-limit sleep budget this run shares across every
    /// GitHub client it builds — org discovery, the PR sweep, and the reviewer
    /// pass. Each used to own its own, so the ceiling was charged once per
    /// client and a breaker latched in one pass did not stop the next.
    github_budget: RunBudget,
}

impl CollectionPipeline {
    /// Construct a new pipeline from a validated [`Config`].
    ///
    /// Why: pipelines start with toggles disabled by default; callers opt
    /// in to forced re-collection or PR refresh via builder methods.
    /// What: stores the config; sets `force = no_fetch = force_refresh_prs
    /// = false`.
    /// Test: covered by `tests::pipeline_constructs_with_default_config`.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            force: false,
            no_fetch: false,
            force_refresh_prs: false,
            skip_tag_reachability: false,
            head_only: false,
            branches: Vec::new(),
            strict_fetch: false,
            verbose_fetch: false,
            progress: ProgressBus::disabled(),
            // #6565: constructed once here so every client this run builds
            // charges the same allowance.
            github_budget: RunBudget::new(),
        }
    }

    /// Attach a live-progress sink for the per-repository collect loop.
    ///
    /// Why: `tga tui` (#5197) needs to show which repository is being walked
    /// and how each one ended, while the walk is still running. Nothing else
    /// does, so the bus is opt-in and defaults to
    /// [`ProgressBus::disabled`] — on which every emit returns immediately,
    /// leaving the CLI path byte-identical to before this existed.
    /// What: builder setter for the pipeline's progress bus.
    /// Test: `tests::progress_is_disabled_by_default` and
    /// `tests::run_emits_a_terminal_event_per_repo`.
    #[must_use]
    pub fn with_progress(mut self, progress: ProgressBus) -> Self {
        self.progress = progress;
        self
    }

    /// Enable forced re-collection: every `(repo, ISO-week)` pair is
    /// collected regardless of whether `collection_runs` already has a row
    /// for it.
    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    /// If `true`, skip the pre-walk `git fetch origin` on each repository.
    ///
    /// Default is `false` (i.e. always fetch). Useful for offline runs or
    /// when the caller has already fetched.
    pub fn with_no_fetch(mut self, no_fetch: bool) -> Self {
        self.no_fetch = no_fetch;
        self
    }

    /// If `true`, skip the post-collection tag and release-branch reachability
    /// scan.
    ///
    /// When disabled, `fact_commit_reachability` rows for `on_any_tag`,
    /// `reachable_from_tags`, `on_release_branch`, and `release_branches` are
    /// not populated. Useful for trunk-based repos where no tags or release
    /// branches are used, or to reduce collection time on large repos with
    /// thousands of tags.
    pub fn with_skip_tag_reachability(mut self, skip: bool) -> Self {
        self.skip_tag_reachability = skip;
        self
    }

    /// Enable or disable the global HEAD-only revwalk escape hatch.
    ///
    /// Why: tga 2.0.0 changed the default to walk all local branches and remote
    /// tracking refs. This method lets the CLI `--head-only` flag propagate to
    /// every per-repo collector in the pipeline.  Per-repo `head_only: true` in
    /// `RepositoryConfig` provides the same opt-out for individual repos.
    /// What: when `true`, overrides all repos to seed from HEAD only; when
    /// `false` (the default), repos use their per-config `head_only` setting.
    /// Test: see `tests::head_only_legacy_behavior` in extractor.rs.
    pub fn with_head_only(mut self, head_only: bool) -> Self {
        self.head_only = head_only;
        self
    }

    /// Restrict the revwalk to an explicit list of branch names.
    ///
    /// Why: the `--branch <NAME[,NAME…]>` CLI flag lets callers scope collection
    /// to specific branches without touching the YAML config.  This is the
    /// pipeline-level counterpart that threads the list down to each
    /// `GitCollector`.
    /// What: when `branches` is non-empty, each `GitCollector` seeds the
    /// revwalk from `refs/heads/<name>` + `refs/remotes/origin/<name>` for
    /// every listed name, emitting a warning for names not found in a given
    /// repo.  An empty `branches` (the default) means "no restriction".
    /// Mutually exclusive with `head_only` — enforced at the CLI layer via
    /// `conflicts_with`.
    /// Test: see `tests::branch_filter_walks_only_named_branch` in
    /// `collect::git::extractor::tests`.
    pub fn with_branches(mut self, branches: Vec<String>) -> Self {
        self.branches = branches;
        self
    }

    /// When `true`, the pipeline returns a non-zero exit signal to the CLI if
    /// any repo had a fetch failure.
    ///
    /// Why: fetch failures are non-fatal by default (collection continues on
    /// local refs); `--strict-fetch` lets CI pipelines treat stale data as an
    /// error.
    /// What: sets the flag; the CLI checks
    /// [`CollectionStats::fetch_outcomes`] after `run()` and exits non-zero
    /// if any `Failed` variant is present and this flag is set.
    /// Test: the `commands::collect` handler reads this flag from `args`.
    pub fn with_strict_fetch(mut self, strict: bool) -> Self {
        self.strict_fetch = strict;
        self
    }

    /// When `true`, print a success line per fetched repo in the end-of-run
    /// summary (default: only failures are shown).
    ///
    /// Why: the default summary hides successful fetches to keep output brief;
    /// `--verbose-fetch` is useful when debugging network topology.
    /// What: sets the flag; the CLI uses it when printing the fetch summary.
    /// Test: the `commands::collect` handler reads this flag from `args`.
    pub fn with_verbose_fetch(mut self, verbose: bool) -> Self {
        self.verbose_fetch = verbose;
        self
    }

    /// Returns whether `--strict-fetch` was set.
    pub fn strict_fetch(&self) -> bool {
        self.strict_fetch
    }

    /// Returns whether `--verbose-fetch` was set.
    pub fn verbose_fetch(&self) -> bool {
        self.verbose_fetch
    }

    /// If `true`, re-fetch Azure DevOps pull requests even when their IDs are
    /// already present in `pull_requests`.
    ///
    /// This bypasses the [`crate::collect::azdo::get_existing_pr_numbers`]
    /// deduplication cache for the ADO provider, so stale rows persisted
    /// before v1.0.9 (with `commit_shas = '[]'`) are re-fetched and
    /// re-upserted with the correct merge SHA. Default is `false`.
    pub fn with_force_refresh_prs(mut self, force_refresh_prs: bool) -> Self {
        self.force_refresh_prs = force_refresh_prs;
        self
    }

    /// Borrow the underlying configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Run the full collection sequence against `db`.
    ///
    /// Each repository is processed sequentially; per-repo failures are
    /// recorded in [`CollectionStats::errors`] but do not abort the run.
    ///
    /// # Errors
    ///
    /// Returns a non-recoverable [`crate::collect::CollectError`] only for
    /// failures outside the per-repo loop.
    pub async fn run(&self, db: &mut Database) -> Result<CollectionStats> {
        let mut stats = CollectionStats::default();

        let resolver = IdentityResolver::from_config(&self.config);

        // #5197: one progress row per configured repository, in walk order.
        for repo_cfg in self.config.repositories.iter() {
            let repo_label = repo_cfg
                .name
                .clone()
                .unwrap_or_else(|| repo_cfg.path.display().to_string());
            // #5197: `started` with no total, not `advanced(repo_index,
            // repo_total)` — position among repositories shares this repo's
            // target, so the aggregate folded it into this row and a mid-walk
            // repo displayed "4/5" as if it were 80% through ITSELF. Nothing
            // emits intra-repo progress, so the honest statement is "in
            // flight, size unknown"; the how-many-repos roll-up is the stage
            // header, which counts done / failed / skipped / running rows.
            self.progress.emit(ProgressEvent::started(
                Stage::Collect,
                repo_label.clone(),
                None,
            ));
            // Per-repo head_only is OR-ed with the global pipeline flag: if
            // either is true, that repo walks HEAD only.  This lets operators
            // set `--head-only` globally (the CLI flag) or `head_only: true`
            // per repo in YAML without requiring both to be set.
            let effective_head_only = self.head_only || repo_cfg.head_only;
            // Build a pre-fetch collector (with no_fetch = self.no_fetch) solely
            // to run perform_fetch once and capture the outcome. Then build the
            // walk collector with no_fetch=true so the per-week collect_window
            // calls don't re-fetch.
            let pre_fetch_collector = match GitCollector::new(repo_cfg) {
                Ok(c) => c
                    .no_fetch(self.no_fetch)
                    .with_head_only(effective_head_only)
                    .with_explicit_branches(self.branches.clone()),
                Err(e) => {
                    let msg = format!("failed to open repo {}: {e}", repo_cfg.path.display());
                    warn!("{msg}");
                    // #5197: a repo that never opens must still reach a
                    // terminal state, or its row spins forever in the TUI.
                    self.progress.emit(ProgressEvent::failed(
                        Stage::Collect,
                        repo_label.clone(),
                        msg.clone(),
                    ));
                    stats.fail_stage(msg);
                    continue;
                }
            };
            // Perform the one-shot fetch and record the outcome (#334).
            let fetch_result = pre_fetch_collector.perform_fetch();
            stats.fetch_outcomes.push(fetch_result);

            // Walk collector always has no_fetch=true: the fetch was either just
            // performed above, or was intentionally skipped (--no-fetch).
            let collector = match GitCollector::new(repo_cfg) {
                Ok(c) => c
                    .no_fetch(true)
                    .with_head_only(effective_head_only)
                    .with_explicit_branches(self.branches.clone())
                    // #5197: an attached bus means a TUI owns the terminal, so
                    // the walk's indicatif spinner must not draw over it.
                    .with_progress(self.progress.clone()),
                Err(e) => {
                    let msg = format!("failed to open repo {}: {e}", repo_cfg.path.display());
                    warn!("{msg}");
                    // #5197: a repo that never opens must still reach a
                    // terminal state, or its row spins forever in the TUI.
                    self.progress.emit(ProgressEvent::failed(
                        Stage::Collect,
                        repo_label.clone(),
                        msg.clone(),
                    ));
                    stats.fail_stage(msg);
                    continue;
                }
            };
            let before = stats.commits_collected as u64;
            let errors_before = stats.errors.len();
            let failures = self.collect_repo_by_week(db, &collector, &mut stats);
            let collected = stats.commits_collected as u64 - before;
            // #5197: a repo whose weeks failed reported `Completed` — "ok, 1
            // commit" — because the walk's errors only reached stats.errors.
            self.progress.emit(if failures == 0 {
                ProgressEvent::completed(Stage::Collect, repo_label, collected)
            } else {
                let first = stats
                    .errors
                    .get(errors_before)
                    .map_or("", |f| f.message.as_str());
                ProgressEvent::failed(
                    Stage::Collect,
                    repo_label,
                    format!("{failures} error(s), {collected} commit(s) collected; {first}"),
                )
            });
        }

        // Tag and release-branch reachability scan (issue #279).
        // Run once after all per-repo git walks, before PR fetches, because the
        // reachability data is derived purely from the local git graph.
        if !self.skip_tag_reachability {
            self.run_reachability_scan(db, &mut stats);
        } else {
            info!("skipping tag/release-branch reachability scan (--skip-tag-reachability)");
        }

        // Backfill authors from observed commits.
        stats.authors_resolved = self.upsert_observed_authors(db, &resolver)?;

        // Issue #68: any commit with NULL author_id after identity resolution
        // is "phantom" — it would be counted as a distinct developer in
        // reports. Surface the count so the operator can extend the alias map.
        if let Ok(unresolved) = count_unresolved_commits(db) {
            if unresolved > 0 {
                let msg = format!(
                    "WARNING: {unresolved} commits have unresolved author identities and may \
                     inflate developer counts. Run `tga aliases list` to review, or extend \
                     `developer_aliases` in the config to map missing identities."
                );
                warn!("{msg}");
                notify::warning(&self.progress, "collect", &msg);
            }
        }

        // PR providers (GitHub, Bitbucket, …) run concurrently. Each
        // provider fetches on its own task, then we persist the results
        // sequentially on the main task because `Database` is not `Sync`.
        self.fetch_and_store_prs(db, &mut stats).await;

        // Optional: Azure DevOps connection probe + work-item enrichment.
        if let Some(azdo_cfg) = self.config.azure_devops_config() {
            let client = AzureDevOpsClient::new(azdo_cfg.clone());
            match client.test_connection().await {
                Ok(info) => info!(
                    user = info.user_name.as_deref().unwrap_or("?"),
                    org = %info.organization_url,
                    "Azure DevOps connection verified",
                ),
                Err(e) => {
                    warn!("Azure DevOps connection failed (non-fatal): {e}");
                }
            }
            // #5219: the ADO work-item pull moved to `work_item_pipeline`,
            // which runs it through `PmAdapter` alongside JIRA and GitHub.
            if azdo_cfg.fetch_prs {
                match self.fetch_and_persist_azdo_prs(db, azdo_cfg).await {
                    Ok(n) => {
                        info!(prs = n, "stored ADO pull requests");
                        stats.prs_fetched += n;
                    }
                    Err(e) => {
                        stats.fail_stage(format!("ADO PR fetch failed: {e}"));
                    }
                }
            }
        }

        linear_pipeline::fetch_and_store_linear_issues(db, &self.config, &mut stats).await;

        // #5219: JIRA, GitHub Issues and Azure DevOps all reach `work_items`
        // through one `PmAdapter`-driven pass. Linear stays above because it
        // writes the provider-specific `linear_issues` in the same pass; see
        // `work_item_pipeline`'s module doc.
        work_item_pipeline::fetch_and_persist_work_items(db, &self.config, &mut stats).await;

        Ok(stats)
    }

    /// Run the tag and release-branch reachability scan for every configured
    /// repository and accumulate the results into `stats`.
    ///
    /// Why: after commits are stored, we can walk the git graph once per repo to
    /// build the tag/branch ancestry maps and write `fact_commit_reachability`
    /// rows.  Non-fatal — errors are pushed into `stats.errors` so one broken
    /// repo (e.g. a bare clone without tags) does not abort the full run.
    /// What: iterates `self.config.repositories`, resolves each path, calls
    /// [`crate::collect::git::reachability::scan_and_persist`], and accumulates
    /// `rows_upserted` into `stats.reachability_rows`.
    /// Test: covered by the integration test in `reachability::tests`.
    fn run_reachability_scan(&self, db: &mut Database, stats: &mut CollectionStats) {
        use crate::collect::git::reachability::scan_and_persist;
        use crate::core::config::expand_path;

        let cfg = &self.config.reachability;

        if !cfg.track_tags && !cfg.track_release_branches {
            info!("reachability tracking disabled by config (track_tags=false, track_release_branches=false)");
            return;
        }

        let conn = db.connection();
        for repo_cfg in &self.config.repositories {
            let path = expand_path(&repo_cfg.path);
            let name = repo_cfg
                .name
                .clone()
                .or_else(|| {
                    path.file_name()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| path.display().to_string());

            info!(repo = %name, "running reachability scan");
            match scan_and_persist(&path, conn, cfg, Some(&name)) {
                Ok(r) => {
                    info!(
                        repo = %name,
                        rows = r.rows_upserted,
                        default_branch = r.default_branch_commits,
                        tagged = r.tagged_commits,
                        release_branch = r.release_branch_commits,
                        "reachability scan complete"
                    );
                    stats.reachability_rows += r.rows_upserted;
                }
                Err(e) => {
                    let msg = format!("reachability scan failed for {name}: {e}");
                    warn!("{msg}");
                    stats.fail_stage(msg);
                }
            }
        }
    }

    /// Build the set of [`PrProvider`] instances enabled by the current
    /// configuration. Each provider's construction is independent — a failure
    /// is logged on `stats.errors` but does not abort the run.
    ///
    /// `org_discovered` contains `(owner, repo)` pairs already fetched from
    /// the GitHub org-discovery pass (issue #742); they are unioned with the
    /// per-repo resolver output before the GitHub client is constructed.
    /// `workspace_discovered` is the Bitbucket equivalent from the
    /// `bitbucket.workspaces` pass (#5220), unioned with the singular
    /// `workspace`/`repo_slug` pair the same way.
    ///
    /// Why: separating construction (sync) from org-discovery (async) keeps
    /// the async surface minimal — callers do discovery then hand the results
    /// in here so the final JoinSet spawning stays straightforward.
    /// What: builds a [`GitHubClient`] (if `github.fetch_prs=true`) and a
    /// [`BitbucketClient`] (if `bitbucket.fetch_prs=true`).
    /// Test: covered by the collector integration tests and
    /// `build_pr_providers_github_included` below.
    fn build_pr_providers(
        &self,
        stats: &mut CollectionStats,
        org_discovered: &[(String, String)],
        workspace_discovered: &crate::collect::bitbucket::WorkspaceDiscovery,
    ) -> Vec<Box<dyn PrProvider + Send + Sync>> {
        let mut providers: Vec<Box<dyn PrProvider + Send + Sync>> = Vec::new();

        if let Some(gh_cfg) = &self.config.github {
            if gh_cfg.fetch_prs {
                // Multi-repo resolution (#87, #742): union per-repo resolution
                // with org-discovery results before constructing the client.
                let repos =
                    crate::collect::github::org_discovery::resolve_github_repos_with_discovered(
                        gh_cfg,
                        &self.config.repositories,
                        org_discovered,
                    );
                if repos.is_empty() {
                    info!(
                        "GitHub PR fetch skipped: no github.repo, no per-repo org, \
                         no github.org/orgs resolvable from repositories[] or org discovery"
                    );
                } else if gh_cfg.token.is_none() && std::env::var("GITHUB_TOKEN").ok().is_none() {
                    // Issue #211: surface the token misconfiguration loudly.
                    // Without a PAT, GitHub limits anonymous traffic to 60
                    // requests/hour, which silently truncates org-wide PR
                    // pulls and is the #1 reason `pull_requests` ends up
                    // empty after a `tga collect` run.
                    let msg = "GitHub PR fetch is enabled (github.fetch_prs=true) but \
                               no token is configured. Set `github.token` or the \
                               GITHUB_TOKEN env var to a PAT with `repo` scope (public \
                               repos only need `public_repo`); without it, GitHub \
                               rate-limits to 60 requests/hour and most PRs will be \
                               missed.";
                    warn!("{msg}");
                    notify::warning(&self.progress, "collect", &format!("warning: {msg}"));
                    info!(
                        repo_count = repos.len(),
                        "GitHub PR fetcher will scan {} repo(s) anonymously",
                        repos.len()
                    );
                    match GitHubClient::new_for_prs(gh_cfg, repos)
                        .map(|c| c.with_run_budget(&self.github_budget))
                    {
                        Ok(gh) => providers.push(Box::new(gh)),
                        Err(e) => stats.fail_stage(format!("GitHub client init failed: {e}")),
                    }
                } else {
                    info!(
                        repo_count = repos.len(),
                        "GitHub PR fetcher will scan {} repo(s)",
                        repos.len()
                    );
                    match GitHubClient::new_for_prs(gh_cfg, repos)
                        .map(|c| c.with_run_budget(&self.github_budget))
                    {
                        Ok(gh) => providers.push(Box::new(gh)),
                        Err(e) => stats.fail_stage(format!("GitHub client init failed: {e}")),
                    }
                }
            } else {
                // Issue #211: when the github config block exists but
                // fetch_prs is false (the default), the pull_requests table
                // ends up empty even though the user has clearly opted into
                // GitHub integration. Emit a one-shot diagnostic so the
                // operator can find the toggle without grepping the source.
                info!(
                    "GitHub PR fetch disabled (github.fetch_prs=false). Set \
                     `github.fetch_prs: true` in your config to populate the \
                     pull_requests table."
                );
            }
        } else if has_github_like_repos(&self.config.repositories) {
            // Issue #211: zero `pull_requests` rows is the single most
            // common "tga seems broken" question. Detect the most likely
            // misconfiguration (repos look like GitHub clones but no
            // `github:` block in the config) and tell the operator how to
            // fix it before they go hunting through the code.
            let msg = "Repositories look like GitHub clones, but no `github:` config \
                       block is present. To populate the `pull_requests` table, add:\n\
                       \n\
                       github:\n  \
                         token: \"${GITHUB_TOKEN}\"   # PAT with `repo` scope\n  \
                         fetch_prs: true\n  \
                         repo: \"owner/name\"         # OR `org: \"owner\"` for org-wide\n";
            tracing::info!("{msg}");
        }
        if let Some(bb_cfg) = &self.config.bitbucket {
            if bb_cfg.fetch_prs {
                // #5220: the repository set is the configured pair unioned with
                // whatever workspace discovery could read.
                let repos = crate::collect::bitbucket::resolve_bitbucket_repos(
                    bb_cfg,
                    &workspace_discovered.repos,
                );
                if repos.is_empty() {
                    info!(
                        "Bitbucket PR fetch skipped: no bitbucket.workspace/repo_slug pair \
                         and bitbucket.workspaces discovery returned no repositories"
                    );
                } else {
                    info!(
                        repo_count = repos.len(),
                        "Bitbucket PR fetcher will scan {} repo(s)",
                        repos.len()
                    );
                    // #6084: a truncated discovery walk must reach the run's
                    // faults, and `pr_pipeline` drains them off the provider.
                    match BitbucketClient::new_for_repos(bb_cfg, repos)
                        .map(|c| c.with_notices(workspace_discovered.notices.clone()))
                    {
                        Ok(bb) => providers.push(Box::new(bb)),
                        Err(e) => stats.fail_stage(format!("Bitbucket client init failed: {e}")),
                    }
                }
            }
        }
        providers
    }

    /// Run every configured PR provider concurrently, persist their results,
    /// then run the GitHub reviewer ingestion pass (issue #742).
    ///
    /// Why: org-discovery (async) must complete before the GitHub client is
    /// built; reviewer ingestion runs serially after PRs are stored so we
    /// have valid `pr_id` FK values.
    /// What: (1) org-discovery via [`super::github_pipeline::run_github_org_discovery`],
    /// (2) build providers + concurrent PR fetch + store, (3) serial reviewer
    /// pass via [`super::github_pipeline::fetch_and_store_github_reviewers`].
    /// Test: the PR-fetch path is covered by existing collector integration
    /// tests; the reviewer pass is covered by `reviewer_store` unit tests.
    async fn fetch_and_store_prs(&self, db: &mut Database, stats: &mut CollectionStats) {
        // Phase 1: async org-discovery so build_pr_providers has the full
        // repo set before constructing the GitHub client.
        let org_discovered = if let Some(gh_cfg) = &self.config.github {
            if gh_cfg.fetch_prs && (!gh_cfg.orgs.is_empty() || gh_cfg.org.is_some()) {
                super::github_pipeline::run_github_org_discovery(gh_cfg, &self.github_budget).await
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // #5220: Bitbucket workspace discovery, same phase and same shape as
        // the GitHub org pass above.
        let workspace_discovered = match &self.config.bitbucket {
            Some(bb_cfg) if bb_cfg.fetch_prs => {
                crate::collect::bitbucket::run_workspace_discovery(bb_cfg).await
            }
            _ => crate::collect::bitbucket::WorkspaceDiscovery::default(),
        };

        let providers = self.build_pr_providers(stats, &org_discovered, &workspace_discovered);
        if providers.is_empty() {
            return;
        }

        let mut set: tokio::task::JoinSet<(String, Result<Vec<PullRequest>>)> =
            tokio::task::JoinSet::new();
        // Keep providers alive in an Arc so the spawned task can return its
        // name and the orchestrator can still call `store_pull_requests`.
        let providers: Vec<std::sync::Arc<dyn PrProvider + Send + Sync>> =
            providers.into_iter().map(std::sync::Arc::from).collect();

        for p in &providers {
            let p = std::sync::Arc::clone(p);
            let name = p.name().to_string();
            set.spawn(async move {
                let result = p.fetch_pull_requests().await;
                (name, result)
            });
        }

        // Drain results as they complete. Persistence runs on the main task
        // (where `&mut Database` is safe to use) and uses the matching
        // provider's `store_pull_requests`. The drain lives in
        // `pr_pipeline` so every fault-severity decision sits in one place.
        super::pr_pipeline::drain_and_store_pull_requests(set, &providers, db, stats).await;

        // Phase 3 (issue #742): GitHub reviewer ingestion pass (serial, after
        // PRs are stored so FK lookups succeed).
        if let Some(gh_cfg) = &self.config.github {
            if gh_cfg.fetch_prs && gh_cfg.fetch_pr_reviews {
                super::github_pipeline::fetch_and_store_github_reviewers(
                    db,
                    gh_cfg,
                    self.force_refresh_prs,
                    stats,
                    &self.github_budget,
                )
                .await;
            }
        }
    }

    /// Collect a single repository week-by-week, skipping `(repo, ISO-week)`
    /// pairs that already have a row in `collection_runs` unless `force` is
    /// set. All non-fatal errors are pushed into `stats.errors` so that one
    /// bad week (or bad repo) does not abort the entire run.
    ///
    /// Returns how many errors this call appended to `stats.errors` — a
    /// non-zero count is what makes the caller emit
    /// [`ProgressEvent::failed`] instead of a success this repo did not earn
    /// (#5197).
    /// Test: `crate::collect::tests::run_reports_failed_when_a_week_fails`.
    fn collect_repo_by_week(
        &self,
        db: &mut Database,
        collector: &GitCollector,
        stats: &mut CollectionStats,
    ) -> usize {
        let errors_before = stats.errors.len();
        let repo_name = collector.name().to_string();

        // Derive the [from, to] NaiveDate window from the collector's
        // configured since/until. The week-level skip mechanism (the
        // `collection_runs` table) is the only reason re-running on a
        // 58K-commit repo is tolerable, so we want to take the bounded
        // path whenever AT LEAST a `since` bound is available, defaulting
        // `to` to "today" when `until` is absent (the common case for
        // --weeks / --from).
        //
        // The fully-unbounded path (no `since` at all) is dangerous on
        // large monorepos: full-history traversal + no week bookkeeping
        // means a re-run repeats the entire walk. We keep it for
        // backwards compatibility but warn loudly.
        let (from, to) = match (collector.since(), collector.until()) {
            (Some(s), Some(u)) => (s.date_naive(), u.date_naive()),
            (Some(s), None) => (s.date_naive(), Utc::now().date_naive()),
            (None, Some(u)) => {
                // Unusual: `until` without `since`. Treat the window as
                // open-ended on the lower side and walk full history up
                // to `until` — emit the same warning as the fully
                // unbounded case so the user knows.
                warn!(
                    repo = %repo_name,
                    "until_date set without since_date — collecting full git history. \
                     Use --weeks N or set analysis.since_date in config to limit scope."
                );
                notify::warning(
                    &self.progress,
                    &repo_name,
                    &format!(
                    "warning: [{repo_name}] no since_date / --weeks — collecting FULL git history. \
                     Set analysis.since_date or pass --weeks N to limit scope."
                ),
                );
                match collector.collect_window(db, None, Some(u)) {
                    Ok(n) => {
                        info!(repo = %repo_name, commits = n, "extracted (until-only)");
                        stats.commits_collected += n;
                    }
                    Err(e) => {
                        let msg = format!("collection failed for {repo_name}: {e}");
                        warn!("{msg}");
                        stats.fail_stage(msg);
                    }
                }
                return stats.errors.len() - errors_before;
            }
            (None, None) => {
                // Fully unbounded — full history traversal with no week
                // bookkeeping. Warn explicitly per Bug #65. #6073 makes the
                // re-run cheap by recording the tip each completed walk
                // reached; the warning still stands for a first run.
                warn!(
                    repo = %repo_name,
                    "no since_date or --weeks flag set — collecting full git history. \
                     Use --weeks N or set analysis.since_date in config to limit scope."
                );
                notify::warning(
                    &self.progress,
                    &repo_name,
                    &format!(
                    "warning: [{repo_name}] no since_date / --weeks — collecting FULL git history. \
                     Set analysis.since_date or pass --weeks N to limit scope."
                ),
                );
                self.collect_unbounded(db, collector, stats);
                return stats.errors.len() - errors_before;
            }
        };

        for week in weeks_in_range(from, to) {
            let (year, week_no, _, _) = week;
            // Skip-if-collected check.
            if !self.force {
                match db::is_week_collected(db, &repo_name, year, week_no) {
                    Ok(true) => {
                        info!("Skipping {repo_name} W{week_no} {year} — already collected");
                        notify::progress(
                            &self.progress,
                            &repo_name,
                            &format!(
                                "Skipped   W{week_no:02} {year}: already collected \
                             (use --force to re-collect) [{repo_name}]"
                            ),
                        );
                        stats.weeks_skipped += 1;
                        continue;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        let msg = format!(
                            "collection_runs lookup failed for {repo_name} W{week_no} {year}: {e}"
                        );
                        warn!("{msg}");
                        stats.skip_item(msg);
                        continue;
                    }
                }
            }

            // Clamp the week to the user-requested range so we don't pull
            // commits outside [from, to] on partial-week boundaries.
            let (win_start, win_end) = clamp_week_to_range(week, from, to);
            let since_ts = naive_date_start_utc(win_start);
            let until_ts = naive_date_end_utc(win_end);

            match collector.collect_window(db, Some(since_ts), Some(until_ts)) {
                Ok(n) => {
                    info!(
                        repo = %repo_name,
                        year,
                        week = week_no,
                        commits = n,
                        "extracted week"
                    );
                    let line = format!("Collected W{week_no:02} {year}: {n} commits [{repo_name}]");
                    notify::progress(&self.progress, &repo_name, &line);
                    stats.commits_collected += n;
                    stats.weeks_collected += 1;
                    let repo_count = self.config.repositories.len();
                    if let Err(e) =
                        db::record_collection_run(db, &repo_name, year, week_no, n, repo_count)
                    {
                        let msg = format!(
                            "failed to record collection_run for {repo_name} W{week_no} {year}: {e}"
                        );
                        warn!("{msg}");
                        stats.skip_item(msg);
                    }
                }
                Err(e) => {
                    let msg = format!("collection failed for {repo_name} W{week_no} {year}: {e}");
                    warn!("{msg}");
                    stats.skip_item(msg);
                }
            }
        }
        stats.errors.len() - errors_before
    }

    /// Walk a repository's full history, skipping or narrowing the walk when
    /// the extract database is already current for its tips (#6073).
    ///
    /// Why: this path has no `collection_runs` bookkeeping, so before #6073
    /// every re-run re-walked the entire history — the issue measured that
    /// against a 594 MB extract database, reached through trusty-audit's
    /// per-repo retry of a render-stage failure. `--force` restores the
    /// unconditional full walk.
    /// What: reads the recorded [`walk_state`], compares it against the
    /// repository's current tips AND this run's [`walk_state::WalkScope`], and
    /// dispatches on the resulting [`walk_state::WalkPlan`]. The state is
    /// marked in flight before the walk and recorded complete only after the
    /// walk returns `Ok`, so both an interrupted run and one whose revwalk
    /// aborted re-walk in full rather than skipping on partial data. Any
    /// bookkeeping failure degrades to a full walk rather than aborting the
    /// repository.
    /// Test: `crate::collect::git::extractor::tests::{unchanged_head_skips_the_walk,
    /// advanced_head_walks_only_the_new_commits,
    /// unreachable_base_forces_a_full_rewalk,
    /// a_scoped_walk_does_not_license_skipping_a_full_one,
    /// an_aborted_revwalk_is_not_recorded_as_a_completed_walk}`.
    pub(crate) fn collect_unbounded(
        &self,
        db: &mut Database,
        collector: &GitCollector,
        stats: &mut CollectionStats,
    ) {
        let repo_name = collector.name().to_string();
        // The scope comes from the collector, not from `self`, so it reflects
        // what the revwalk will actually seed — including a per-repo
        // `head_only: true` the pipeline ORed in (#6073 review).
        let scope = collector.walk_scope();
        let tips = match collector.walk_tips() {
            Ok(t) => Some(t),
            Err(e) => {
                // Without tips there is nothing to compare or record; fall
                // back to the pre-#6073 unconditional walk.
                warn!(repo = %repo_name, error = %e, "could not read repository tips; walking in full");
                None
            }
        };

        let plan = match (&tips, self.force) {
            // `--force` is the existing "re-collect everything" lever and
            // keeps meaning exactly that here.
            (_, true) => walk_state::WalkPlan::Full {
                reason: walk_state::FullWalkReason::Forced,
            },
            (None, _) => walk_state::WalkPlan::Full {
                reason: walk_state::FullWalkReason::NeverWalked,
            },
            (Some(t), false) => {
                let recorded = walk_state::load(db.connection(), &repo_name).unwrap_or_else(|e| {
                    warn!(repo = %repo_name, error = %e, "could not read walk state; walking in full");
                    None
                });
                let reachable = recorded
                    .as_ref()
                    .is_some_and(|s| collector.base_is_reachable(&s.head_sha));
                walk_state::plan(recorded.as_ref(), t, &scope, reachable)
            }
        };

        let hide = match &plan {
            walk_state::WalkPlan::Skip => {
                let line = format!(
                    "Skipped   full history: extract db already current at {head} \
                     (use --force to re-walk) [{repo_name}]",
                    head = tips.as_ref().map(|t| t.head_sha.as_str()).unwrap_or("")
                );
                info!(repo = %repo_name, "{line}");
                notify::progress(&self.progress, &repo_name, &line);
                stats.repos_skipped += 1;
                return;
            }
            walk_state::WalkPlan::Incremental { base_sha } => match git2::Oid::from_str(base_sha) {
                Ok(oid) => Some(oid),
                Err(e) => {
                    warn!(repo = %repo_name, error = %e, "recorded walk base is malformed; walking in full");
                    None
                }
            },
            walk_state::WalkPlan::Full { reason } => {
                info!(
                    repo = %repo_name,
                    "collecting FULL git history: {}",
                    reason.as_str()
                );
                None
            }
        };

        // Mark the walk in flight so a run interrupted here re-walks in full.
        // This preserves any recorded head/digest/scope, so an interrupt does
        // not overwrite the last known-good base with tips nothing walked.
        if let Err(e) = walk_state::mark_in_flight(db.connection(), &repo_name) {
            warn!(repo = %repo_name, error = %e, "could not mark walk in flight");
        }

        match collector.collect_window_hiding(db, None, None, hide) {
            Ok(n) => {
                info!(repo = %repo_name, commits = n, ?hide, "extracted (unbounded)");
                stats.commits_collected += n;
                if let Some(t) = &tips {
                    if let Err(e) =
                        walk_state::record_complete(db.connection(), &repo_name, t, &scope)
                    {
                        warn!(repo = %repo_name, error = %e, "could not record completed walk");
                    }
                }
            }
            Err(e) => {
                let msg = format!("collection failed for {repo_name}: {e}");
                warn!("{msg}");
                stats.fail_stage(msg);
            }
        }
    }

    /// Read distinct `(author_name, author_email)` pairs from `commits`
    /// and upsert them via the resolver, then link `commits.author_id`.
    fn upsert_observed_authors(
        &self,
        db: &mut Database,
        resolver: &IdentityResolver,
    ) -> Result<usize> {
        // Collect distinct pairs first to avoid holding a Statement across
        // mutating calls.
        let pairs: Vec<(String, String)> = {
            let conn = db.connection();
            let mut stmt = conn.prepare(
                "SELECT DISTINCT author_name, author_email FROM commits WHERE author_id IS NULL",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            out
        };

        let mut count = 0usize;
        for (name, email) in pairs {
            let author_id = resolver.upsert_author(db, &name, &email)?;
            db.connection().execute(
                "UPDATE commits SET author_id = ?1 \
                 WHERE author_id IS NULL AND author_name = ?2 AND author_email = ?3",
                rusqlite::params![author_id, name, email],
            )?;
            count += 1;
        }
        Ok(count)
    }

    /// Fetch ADO pull requests referenced by commit-message `Merged PR NNNN:`
    /// patterns and persist them (with reviewers) under provider `'azdo'`.
    ///
    /// Why: ADO PRs are the source of review-pattern signals (vote
    /// distribution, reviewer load) that are absent from the bare git history;
    /// they live in the same `pull_requests` table as GitHub PRs but
    /// scoped by the `provider` column.
    /// What: pulls commit messages, extracts PR IDs, fetches each PR serially
    /// from `GET {org}/{project}/_apis/git/pullrequests/{id}`, and upserts
    /// rows into `pull_requests` + `pr_reviewers`. When `force_refresh_prs`
    /// is set, the PR-ID deduplication cache is bypassed so stale rows are
    /// re-fetched.
    /// Test: PR-ID extraction, DB CRUD, and config wiring are covered in
    /// `azdo::pr_fetcher::tests`. The full path is exercised by integration
    /// tests gated on a live ADO instance.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::collect::CollectError`] for SQL failures. HTTP
    /// failures on individual PRs are logged and do not abort the run.
    async fn fetch_and_persist_azdo_prs(
        &self,
        db: &mut Database,
        azdo_cfg: &crate::core::config::AzureDevOpsConfig,
    ) -> Result<usize> {
        use crate::collect::azdo::AdoPrFetcher;

        let messages: Vec<String> = {
            let conn = db.connection();
            let mut stmt = conn.prepare("SELECT message FROM commits")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            out
        };

        let fetcher = match AdoPrFetcher::new(azdo_cfg.clone()) {
            Ok(f) => f,
            Err(e) => {
                warn!("ADO PR fetcher init failed: {e}");
                return Ok(0);
            }
        };
        let conn = db.connection();
        let stored = fetcher
            .run_with_options(
                conn,
                messages.iter().map(String::as_str),
                self.force_refresh_prs,
            )
            .await?;
        Ok(stored)
    }
}

/// Best-effort detector for "this repo looks like a GitHub clone".
///
/// Why: when zero `pull_requests` end up in the DB after `tga collect`,
/// nine times out of ten the cause is "no `github:` block in the YAML".
/// Detecting this cheaply lets us emit one concrete remediation line
/// instead of leaving the operator to grep through the source. See issue
/// #211.
/// What: for each [`RepositoryConfig`], opens the repo via `git2`, reads
/// `origin`'s URL, and returns `true` as soon as one URL matches the
/// known GitHub forms (HTTPS, SSH, `ssh://git@github.com/...`). Any error
/// (no `origin`, unreadable repo, non-GitHub URL) is silently skipped so
/// this never fires a false positive in CI or test fixtures.
/// Test: covered indirectly — exercised by `tga collect` runs against
/// real clones; pure-string parsing is covered by
/// `crate::collect::github::client::extract_owner_repo_from_url`.
fn has_github_like_repos(repositories: &[crate::core::config::RepositoryConfig]) -> bool {
    for repo_cfg in repositories {
        let Ok(repo) = git2::Repository::open(&repo_cfg.path) else {
            continue;
        };
        let Ok(remote) = repo.find_remote("origin") else {
            continue;
        };
        let Some(url) = remote.url() else {
            continue;
        };
        if url.contains("github.com") {
            return true;
        }
    }
    false
}

/// Convert a calendar date to the UTC instant at 00:00:00 on that day.
fn naive_date_start_utc(d: NaiveDate) -> DateTime<Utc> {
    let ndt = d
        .and_hms_opt(0, 0, 0)
        .expect("00:00:00 is always a valid time");
    Utc.from_utc_datetime(&ndt)
}

/// Count commits where `author_id IS NULL` — these are commits whose author
/// identity could not be linked to a row in the `authors` table.
///
/// Why: see issue #68. Phantom identities silently inflate developer counts
/// in downstream reports, so we want to surface their existence loudly.
/// What: returns the COUNT(*) of NULL-author-id commits, or `Err` on a SQL
/// failure (callers should treat the error as best-effort and not abort).
/// Test: seed an in-memory DB with one commit whose author_id is NULL and
/// one with author_id set; assert the count is 1.
fn count_unresolved_commits(db: &Database) -> Result<usize> {
    let n: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM commits WHERE author_id IS NULL",
            [],
            |r| r.get(0),
        )
        .map_err(crate::core::TgaError::from)?;
    Ok(n as usize)
}

/// Convert a calendar date to the UTC instant at 23:59:59 on that day.
fn naive_date_end_utc(d: NaiveDate) -> DateTime<Utc> {
    let ndt = d
        .and_hms_opt(23, 59, 59)
        .expect("23:59:59 is always a valid time");
    Utc.from_utc_datetime(&ndt)
}
