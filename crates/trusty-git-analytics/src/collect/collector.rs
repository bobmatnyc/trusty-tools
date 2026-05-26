//! End-to-end Stage 1 collection pipeline.
//!
//! Orchestrates git extraction, identity resolution, and optional GitHub
//! and JIRA fetches against a configured [`crate::core::config::Config`].

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use tracing::{info, warn};

use crate::collect::azdo::AzureDevOpsClient;
use crate::collect::bitbucket::BitbucketClient;
use crate::collect::errors::Result;
use crate::collect::git::GitCollector;
use crate::collect::github::GitHubClient;
use crate::collect::identity::IdentityResolver;
use crate::collect::linear::LinearClient;
use crate::collect::pr_provider::PrProvider;
use crate::collect::weeks::{clamp_week_to_range, weeks_in_range};
use crate::core::config::Config;
use crate::core::db::{self, Database};
use crate::core::models::PullRequest;

/// Aggregate statistics for a single pipeline run.
#[derive(Debug, Clone, Default)]
pub struct CollectionStats {
    /// Number of new commit rows written across all repositories.
    pub commits_collected: usize,
    /// Number of distinct authors observed and upserted.
    pub authors_resolved: usize,
    /// Number of PR rows written (zero if GitHub fetch disabled).
    pub prs_fetched: usize,
    /// Number of Linear issues fetched (0 if Linear not configured).
    pub linear_issues_fetched: usize,
    /// Number of `(repo, week)` pairs that were collected this run.
    pub weeks_collected: usize,
    /// Number of `(repo, week)` pairs skipped because already present in
    /// `collection_runs` (and `force` was false).
    pub weeks_skipped: usize,
    /// Per-repo error messages encountered (non-fatal).
    pub errors: Vec<String>,
}

/// Top-level Stage 1 orchestrator.
pub struct CollectionPipeline {
    config: Config,
    force: bool,
    no_fetch: bool,
    force_refresh_prs: bool,
}

impl CollectionPipeline {
    /// Construct a new pipeline from a validated [`Config`].
    pub fn new(config: Config) -> Self {
        Self {
            config,
            force: false,
            no_fetch: false,
            force_refresh_prs: false,
        }
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

        for repo_cfg in &self.config.repositories {
            let collector = match GitCollector::new(repo_cfg) {
                Ok(c) => c.no_fetch(self.no_fetch),
                Err(e) => {
                    let msg = format!("failed to open repo {}: {e}", repo_cfg.path.display());
                    warn!("{msg}");
                    stats.errors.push(msg);
                    continue;
                }
            };
            self.collect_repo_by_week(db, &collector, &mut stats);
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
                eprintln!("{msg}");
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
            if azdo_cfg.fetch_on_reference {
                if let Err(e) = self
                    .fetch_and_persist_azdo_work_items(db, &client, azdo_cfg)
                    .await
                {
                    stats
                        .errors
                        .push(format!("ADO work item persistence failed: {e}"));
                }
            }
            if azdo_cfg.fetch_prs {
                match self.fetch_and_persist_azdo_prs(db, azdo_cfg).await {
                    Ok(n) => {
                        info!(prs = n, "stored ADO pull requests");
                        stats.prs_fetched += n;
                    }
                    Err(e) => {
                        stats.errors.push(format!("ADO PR fetch failed: {e}"));
                    }
                }
            }
        }

        // Optional: Linear issue enrichment.
        if let Some(linear_cfg) = &self.config.linear {
            if linear_cfg.fetch_on_reference {
                match LinearClient::new(linear_cfg) {
                    Ok(client) => {
                        // Collect commit messages from DB.
                        let messages: Vec<String> = {
                            let conn = db.connection();
                            let mut stmt = match conn.prepare("SELECT message FROM commits") {
                                Ok(s) => s,
                                Err(e) => {
                                    stats
                                        .errors
                                        .push(format!("Linear: query commits failed: {e}"));
                                    return Ok(stats);
                                }
                            };
                            let rows = match stmt.query_map([], |row| row.get::<_, String>(0)) {
                                Ok(r) => r,
                                Err(e) => {
                                    stats
                                        .errors
                                        .push(format!("Linear: read commits failed: {e}"));
                                    return Ok(stats);
                                }
                            };
                            let mut out = Vec::new();
                            for r in rows.flatten() {
                                out.push(r);
                            }
                            out
                        };

                        let msg_refs: Vec<&str> = messages.iter().map(String::as_str).collect();
                        let issues = client
                            .fetch_referenced_issues(&msg_refs, &linear_cfg.team_keys)
                            .await;
                        for issue in &issues {
                            info!(
                                id = %issue.identifier,
                                state = %issue.state,
                                team = %issue.team,
                                "Linear issue fetched"
                            );
                        }
                        match client.store_issues(db, &issues) {
                            Ok(n) => {
                                info!(stored = n, "persisted linear_issues rows");
                                stats.linear_issues_fetched += n;
                            }
                            Err(e) => {
                                stats
                                    .errors
                                    .push(format!("Linear: store issues failed: {e}"));
                            }
                        }
                    }
                    Err(e) => {
                        stats.errors.push(format!("Linear client init failed: {e}"));
                    }
                }
            }
        }

        Ok(stats)
    }

    /// Build the set of [`PrProvider`] instances enabled by the current
    /// configuration. Each provider's construction is independent — a failure
    /// is logged on `stats.errors` but does not abort the run.
    fn build_pr_providers(
        &self,
        stats: &mut CollectionStats,
    ) -> Vec<Box<dyn PrProvider + Send + Sync>> {
        let mut providers: Vec<Box<dyn PrProvider + Send + Sync>> = Vec::new();

        if let Some(gh_cfg) = &self.config.github {
            if gh_cfg.fetch_prs {
                // Multi-repo resolution (#87): drive from `repositories[]` or
                // `github.org` when `github.repo` is not set. If nothing
                // resolves, skip GitHub PR fetching gracefully.
                let repos = crate::collect::github::client::resolve_github_repos(
                    gh_cfg,
                    &self.config.repositories,
                );
                if repos.is_empty() {
                    info!(
                        "GitHub PR fetch skipped: no github.repo, no per-repo org, \
                         and no github.org resolvable from repositories[]"
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
                    eprintln!("warning: {msg}");
                    info!(
                        repo_count = repos.len(),
                        "GitHub PR fetcher will scan {} repo(s) anonymously",
                        repos.len()
                    );
                    match GitHubClient::new_for_prs(gh_cfg, repos) {
                        Ok(gh) => providers.push(Box::new(gh)),
                        Err(e) => stats.errors.push(format!("GitHub client init failed: {e}")),
                    }
                } else {
                    info!(
                        repo_count = repos.len(),
                        "GitHub PR fetcher will scan {} repo(s)",
                        repos.len()
                    );
                    match GitHubClient::new_for_prs(gh_cfg, repos) {
                        Ok(gh) => providers.push(Box::new(gh)),
                        Err(e) => stats.errors.push(format!("GitHub client init failed: {e}")),
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
                match BitbucketClient::new(bb_cfg) {
                    Ok(bb) => providers.push(Box::new(bb)),
                    Err(e) => stats
                        .errors
                        .push(format!("Bitbucket client init failed: {e}")),
                }
            }
        }
        providers
    }

    /// Run every configured PR provider concurrently, then persist their
    /// results on the main task.
    ///
    /// We spawn one task per provider so a slow remote (or the second
    /// provider being absent) doesn't gate the others. Each task returns the
    /// fetched `Vec<PullRequest>` so the `Database` — which is not `Sync` —
    /// is only ever touched by the orchestrator.
    async fn fetch_and_store_prs(&self, db: &mut Database, stats: &mut CollectionStats) {
        let providers = self.build_pr_providers(stats);
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
        // provider's `store_pull_requests`.
        while let Some(joined) = set.join_next().await {
            let (provider_name, fetch_result) = match joined {
                Ok(t) => t,
                Err(e) => {
                    stats.errors.push(format!("PR fetch task panicked: {e}"));
                    continue;
                }
            };
            match fetch_result {
                Ok(prs) => {
                    // Find the matching provider for storage.
                    let Some(provider) = providers.iter().find(|p| p.name() == provider_name)
                    else {
                        stats.errors.push(format!(
                            "internal: no provider registered for '{provider_name}' \
                             when storing PRs"
                        ));
                        continue;
                    };
                    match provider.store_pull_requests(db, &prs) {
                        Ok(n) => {
                            info!(provider = %provider_name, prs = n, "stored pull requests");
                            stats.prs_fetched += n;
                        }
                        Err(e) => {
                            stats
                                .errors
                                .push(format!("{provider_name} PR store failed: {e}"));
                        }
                    }
                }
                Err(e) => {
                    stats
                        .errors
                        .push(format!("{provider_name} PR fetch failed: {e}"));
                }
            }
        }
    }

    /// Collect a single repository week-by-week, skipping `(repo, ISO-week)`
    /// pairs that already have a row in `collection_runs` unless `force` is
    /// set. All non-fatal errors are pushed into `stats.errors` so that one
    /// bad week (or bad repo) does not abort the entire run.
    fn collect_repo_by_week(
        &self,
        db: &mut Database,
        collector: &GitCollector,
        stats: &mut CollectionStats,
    ) {
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
                eprintln!(
                    "warning: [{repo_name}] no since_date / --weeks — collecting FULL git history. \
                     Set analysis.since_date or pass --weeks N to limit scope."
                );
                match collector.collect_window(db, None, Some(u)) {
                    Ok(n) => {
                        info!(repo = %repo_name, commits = n, "extracted (until-only)");
                        stats.commits_collected += n;
                    }
                    Err(e) => {
                        let msg = format!("collection failed for {repo_name}: {e}");
                        warn!("{msg}");
                        stats.errors.push(msg);
                    }
                }
                return;
            }
            (None, None) => {
                // Fully unbounded — full history traversal with no week
                // bookkeeping. Warn explicitly per Bug #65.
                warn!(
                    repo = %repo_name,
                    "no since_date or --weeks flag set — collecting full git history. \
                     Use --weeks N or set analysis.since_date in config to limit scope."
                );
                eprintln!(
                    "warning: [{repo_name}] no since_date / --weeks — collecting FULL git history. \
                     Set analysis.since_date or pass --weeks N to limit scope."
                );
                match collector.collect(db) {
                    Ok(n) => {
                        info!(repo = %repo_name, commits = n, "extracted (unbounded)");
                        stats.commits_collected += n;
                    }
                    Err(e) => {
                        let msg = format!("collection failed for {repo_name}: {e}");
                        warn!("{msg}");
                        stats.errors.push(msg);
                    }
                }
                return;
            }
        };

        for week in weeks_in_range(from, to) {
            let (year, week_no, _, _) = week;
            // Skip-if-collected check.
            if !self.force {
                match db::is_week_collected(db, &repo_name, year, week_no) {
                    Ok(true) => {
                        info!("Skipping {repo_name} W{week_no} {year} — already collected");
                        println!(
                            "Skipped   W{week_no:02} {year}: already collected \
                             (use --force to re-collect) [{repo_name}]"
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
                        stats.errors.push(msg);
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
                    println!("Collected W{week_no:02} {year}: {n} commits [{repo_name}]");
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
                        stats.errors.push(msg);
                    }
                }
                Err(e) => {
                    let msg = format!("collection failed for {repo_name} W{week_no} {year}: {e}");
                    warn!("{msg}");
                    stats.errors.push(msg);
                }
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

    /// Scan stored commit messages for `AB#N` references, batch-fetch the
    /// referenced ADO work items, and persist them in `work_items` and
    /// `commit_work_items`.
    ///
    /// The pipeline pulls `(sha, message)` from `commits`, computes the unique
    /// set of referenced IDs, calls
    /// [`AzureDevOpsClient::get_work_items`] in batches of up to 200, then
    /// upserts the resulting rows and inserts join-table links.
    ///
    /// All work-item linking is done in a single transaction so that a partial
    /// failure doesn't leave dangling rows.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::collect::CollectError`] if reading commits, calling
    /// ADO, or writing to SQLite fails.
    async fn fetch_and_persist_azdo_work_items(
        &self,
        db: &mut Database,
        client: &AzureDevOpsClient,
        azdo_cfg: &crate::core::config::AzureDevOpsConfig,
    ) -> Result<()> {
        use crate::collect::azdo::extract_work_item_refs;
        use std::collections::{BTreeSet, HashMap};

        // The ticket_regex pattern is validated at config load
        // (`Config::validate_ticket_regexes`), so compilation here cannot fail
        // under normal flow. We still propagate the error rather than panic
        // to keep the no-`unwrap()` invariant in library code from CLAUDE.md.
        let ticket_re = regex::Regex::new(&azdo_cfg.ticket_regex).map_err(|e| {
            crate::collect::CollectError::Config(format!(
                "pm.azure_devops.ticket_regex {:?} failed to compile: {e}",
                azdo_cfg.ticket_regex
            ))
        })?;

        // 1. Pull (sha, message) pairs from the database.
        let rows: Vec<(String, String)> = {
            let conn = db.connection();
            let mut stmt = conn.prepare("SELECT sha, message FROM commits")?;
            let mapped = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut out = Vec::new();
            for r in mapped {
                out.push(r?);
            }
            out
        };

        // 2. Build commit -> [work_item_id] map and the unique ID set.
        let mut commit_refs: HashMap<String, Vec<u32>> = HashMap::new();
        let mut all_ids: BTreeSet<u32> = BTreeSet::new();
        for (sha, msg) in &rows {
            let ids = extract_work_item_refs(&ticket_re, msg);
            if !ids.is_empty() {
                for id in &ids {
                    all_ids.insert(*id);
                }
                commit_refs.insert(sha.clone(), ids);
            }
        }

        if all_ids.is_empty() {
            info!(
                pattern = %azdo_cfg.ticket_regex,
                "No work-item references found in commit messages; skipping ADO work item fetch",
            );
            return Ok(());
        }

        // 3. Batch-fetch the referenced work items.
        let ids: Vec<u32> = all_ids.iter().copied().collect();
        let items = match client.get_work_items(&ids).await {
            Ok(v) => v,
            Err(e) => {
                warn!("ADO get_work_items failed: {e}");
                return Ok(());
            }
        };
        info!(
            fetched = items.len(),
            commits = commit_refs.len(),
            "Fetched ADO work items for commits",
        );

        // 4. Persist work items and commit links in a single transaction.
        let tx = db.connection_mut().transaction()?;
        let fetched_ids: std::collections::HashSet<u32> = items.iter().map(|w| w.id).collect();
        for w in &items {
            let raw_json = serde_json::to_string(w).ok();
            let tags_csv = if w.tags.is_empty() {
                None
            } else {
                Some(w.tags.join(","))
            };
            let row = crate::core::db::WorkItemRow {
                id: w.id.to_string(),
                source: "azdo".to_string(),
                title: w.title.clone(),
                status: w.state.clone(),
                item_type: w.work_item_type.clone(),
                tags: tags_csv,
                project: Some(w.team_project.clone()),
                url: w.url.clone(),
                raw_json,
            };
            crate::core::db::work_items::upsert_work_item(&tx, &row)?;
        }
        for (sha, ref_ids) in &commit_refs {
            for id in ref_ids {
                // Skip refs that ADO didn't return (deleted, scope-restricted)
                // to avoid FK violations on the join table.
                if !fetched_ids.contains(id) {
                    continue;
                }
                crate::core::db::work_items::link_commit_work_item(
                    &tx,
                    sha,
                    &id.to_string(),
                    "azdo",
                )?;
            }
        }
        tx.commit()?;
        Ok(())
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
