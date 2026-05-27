# trusty-git-analytics

Analyze git repositories to measure developer productivity — classify commit work types, track weekly velocity, and export CSV/JSON/Markdown reports.

## What It Does

`tga` walks one or more local git repositories, collects every commit into a SQLite database, classifies each commit into a work category (feature, bugfix, refactor, etc.) using a multi-tier classification cascade (including the new weighted-sum Tier 2.5 added in 1.3.0), then aggregates the results into per-author, per-week, DORA, velocity, and quality reports. It is a feature-complete Rust port of [gitflow-analytics](https://github.com/bobmatnyc/gitflow-analytics) with the same YAML config schema and the same SQLite schema — existing config files work without modification.

1.4.0 adds four new external classification sources (Linear, Shortcut, Confluence, Datadog) and fixes a SQLite WAL data-loss bug (issue #298) that could cause up to 22 minutes of classification work to be silently discarded on exit.

## Installation

### From crates.io (recommended)

```bash
cargo install tga
```

This installs the `tga` binary to `~/.cargo/bin/`. Ensure `~/.cargo/bin` is in your `PATH`.

### From source

```bash
git clone https://github.com/bobmatnyc/trusty-tools
cd trusty-tools/crates/trusty-git-analytics
cargo install --path .
```

### Verify installation

```bash
tga --version
tga --help
```

## Quick Start

### Run Your First Analysis

**Step 1** — Create a `config.yaml`:

```yaml
repositories:
  - path: ~/code/my-project
    name: my-project

output:
  directory: ./reports
  formats: [csv, json, markdown]
```

**Step 2** — Run the full pipeline:

```bash
tga analyze --config config.yaml
```

**Step 3** — Find reports in `./reports/`:

A full run writes 14 files: 9 CSV, 4 JSON, and 1 Markdown report. The most
commonly used ones are:

```
reports/
├── authors.csv         # Per-author commit summary
├── weekly_activity.csv # Week-by-week breakdown
├── ... (7 more CSV files: DORA, velocity, quality, etc.)
├── report.json         # Full structured payload
├── ... (3 more JSON files)
└── report.md           # Narrative Markdown report
```

## Configuration

### Minimal config.yaml

```yaml
repositories:
  - path: ~/code/my-repo
    name: my-repo
```

All other sections are optional. When `output.formats` is omitted, all three formats (CSV, JSON, Markdown) are written.

### Full config reference

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `repositories` | list | required | Repos to analyze |
| `developer_aliases` | map | `{}` | Canonical name → list of emails/aliases |
| `team` | object | — | Alternative to `developer_aliases`; roster with email |
| `output.directory` | path | `./reports` | Where reports are written |
| `output.formats` | list | `[csv, json, markdown]` | `csv`, `json`, and/or `markdown` |
| `output.include_unclassified` | bool | `false` | Include commits with no category |
| `output.include_merges` | bool | `false` | Include merge commits |
| `output.include_files` | bool | `false` | Include file-level change detail |
| `classification.rules_file` | path | — | Path to custom rules YAML/JSON |
| `classification.use_llm` | bool | `false` | Enable LLM fallback tier |
| `classification.llm_model` | string | `gpt-4o-mini` | LLM model identifier |
| `classification.confidence_threshold` | float | `0.7` | Minimum acceptance confidence |
| `classification.llm_fallback_threshold` | float | `0.65` | Commits with confidence above this value skip the LLM tier. Raised from `0.0` in 1.3.0; see [LLM fallback threshold](#llm-fallback-threshold-migration). |
| `classification.llm_fallback_concurrency` | uint | `8` | Max concurrent LLM requests during fallback |
| `github.token` | string | `$GITHUB_TOKEN` | GitHub PAT for PR fetch. Required scopes: `public_repo` for public repos, `repo` for private repos. Without a token, GitHub rate-limits anonymous traffic to 60 requests/hour and most PRs will be missed. |
| `github.org` | string | — | Org slug for org-wide PR queries |
| `github.repo` | string | — | Single repo slug (`owner/name`) |
| `github.fetch_prs` | bool | `false` | Fetch pull request metadata. **Must be `true` for `tga pr-metrics` to return data** — when left at the default, `tga collect` writes zero rows to `pull_requests` (issue #211). |
| `github.ticket_regex` | string | — | Override regex for detecting GitHub ticket refs in commit messages |
| `jira.url` | string | — | JIRA base URL |
| `jira.username` | string | — | JIRA API username (email for Cloud) |
| `jira.token` | string | — | JIRA API token |
| `jira.project_key` | string | — | Project key filter (e.g. `API`) |
| `jira.ticket_regex` | string | — | Override regex for detecting JIRA ticket refs in commit messages |
| `linear.ticket_regex` | string | — | Override regex for detecting Linear ticket refs in commit messages |
| `pm.azure_devops.organization_url` | string | — | ADO org URL (e.g. `https://dev.azure.com/myorg`) |
| `pm.azure_devops.pat` | string | — | Azure DevOps Personal Access Token |
| `pm.azure_devops.project` | string | — | Default ADO project name |
| `pm.azure_devops.fetch_on_reference` | bool | `false` | Fetch work items when `AB#N` refs appear in commits |
| `pm.azure_devops.fetch_prs` | bool | `false` | Fetch ADO pull requests and reviewer data |
| `pm.azure_devops.ticket_regex` | string | `AB#(\d+)` | Override regex for detecting ADO work item refs in commit messages |
| `pm.bitbucket.workspace` | string | — | Bitbucket Cloud workspace slug |
| `pm.bitbucket.repo_slug` | string | — | Repository slug within the workspace |
| `pm.bitbucket.fetch_prs` | bool | `false` | Fetch Bitbucket Cloud pull request metadata |
| `pm.bitbucket.token` | string | `$BITBUCKET_TOKEN` | Bearer token (App password or OAuth) |
| `pm.bitbucket.username` | string | — | Atlassian account username for Basic auth |
| `pm.bitbucket.app_password` | string | — | Atlassian App password for Basic auth (alternative to `token`) |
| `cache.directory` | path | — | Cache directory (supports `~`) |
| `version` | string | — | Schema version; stored for compatibility |
| `profile` | string | — | Named profile; stored for compatibility |
| `dora.deployment_source` | string | `git_tags` | Source for `tga deployments collect`. One of `git_tags`, `github_releases`, `github_actions`, `manual`. |
| `dora.deployment_tag_pattern` | regex | `^v?[0-9]+\.[0-9]+\.[0-9]+(-...)?$` | Tags matching this regex are ingested as deployments. |
| `dora.production_branch` | string | `main` | Default branch that production deployments come from. |
| `dora.failure_signals` | list | `[]` | One signal per entry; `work_type` (classification category) and/or `commit_message_pattern` (regex) + `within_hours` window. |
| `dora.datadog_dir` | path | — | Directory of Datadog incident exports (.json). Currently a stub — JIRA SRE path is the default MTTR source. |
| `jira.jira_project_mappings` | map | `{}` | Project key → work type (issue #206). Fires as Tier 1.6 — outranks the generic ticket regex. |
| `jira.jira_project_mapping_confidence` | float | `0.88` | Per-verdict confidence for the JIRA mapping tier. |

Paths support `~` expansion. Config files from the Python `gitflow-analytics` tool load without changes — unknown keys are silently ignored.

### developer_aliases vs team.members

**`developer_aliases`** (Python-compatible flat map):

```yaml
developer_aliases:
  "Alice Smith":
    - "alice@company.com"
    - "asmith@company.com"
    - "alice@personal.dev"
  "Bob Jones":
    - "bob@company.com"
    - "129991831+bobgithub@users.noreply.github.com"
```

**`team.members`** (structured roster with canonical email):

```yaml
team:
  members:
    - name: Alice Smith
      email: alice@company.com
      aliases:
        - asmith@company.com
        - alice@personal.dev
```

When `developer_aliases` is non-empty it takes precedence over `team.members`. Use `developer_aliases` when migrating an existing Python config file; use `team.members` for new setups where canonical email matters for downstream tooling.

### Example: multi-repo config with GitHub

See [`configs/example-config.yaml`](configs/example-config.yaml) for a working example that covers multiple repositories, developer aliases, and CSV+Markdown output.

## CLI Reference

All subcommands accept these global flags:

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `config.yaml` | Path to config YAML |
| `--database <PATH>` | `tga.db` | Path to SQLite database |
| `-v / -vv / -vvv` | warnings only | Increase log verbosity |

### tga analyze

Run the full pipeline: collect → classify → report.

```bash
tga analyze [--config <PATH>] [--database <PATH>] [--output <DIR>]
            [--skip-collect] [--skip-classify] [--weeks <N>]
            [--from <DATE>] [--to <DATE>] [--dry-run]
            [--validate-only] [--no-validate]
```

| Flag | Description |
|------|-------------|
| `--skip-collect` | Skip Stage 1; use commits already in the database |
| `--skip-classify` | Skip Stage 2; use existing classifications |
| `--output <DIR>` | Override `output.directory` from config |
| `--weeks <N>` | Limit collection to the last N weeks (overrides config `start_date`) |
| `--from <DATE>` | Start date for collection (ISO 8601 `YYYY-MM-DD`); mutually exclusive with `--weeks` |
| `--to <DATE>` | End date for collection (ISO 8601 `YYYY-MM-DD`); defaults to today |
| `--dry-run` | Perform all steps against an in-memory database; the on-disk database is left untouched |
| `--validate-only` | Run configuration validation and exit (0 on success, 1 on errors) |
| `--no-validate` | Skip pre-flight configuration validation |

```bash
# Full pipeline
tga analyze --config config.yaml

# Re-run reports only (commits already collected and classified)
tga analyze --skip-collect --skip-classify --output ./reports-v2
```

### tga collect

Stage 1: extract commits from git repositories into the database.

```bash
tga collect [--config <PATH>] [--database <PATH>]
            [--repos <NAME,...>] [--from <DATE>] [--to <DATE>] [--weeks <N>]
            [--since <DATE>] [--until <DATE>] [--dry-run]
            [--force-refresh-prs] [--validate-only] [--no-validate]
```

| Flag | Description |
|------|-------------|
| `--repos <NAME,...>` | Comma-separated list of repository names to collect; others are skipped |
| `--from <DATE>` | Collect commits on or after this ISO 8601 date; mutually exclusive with `--weeks` |
| `--to <DATE>` | Collect commits on or before this ISO 8601 date; defaults to today |
| `--since <DATE>` | Legacy alias for `--from` (Python-predecessor compatibility); `--from` takes precedence |
| `--until <DATE>` | Legacy alias for `--to` (Python-predecessor compatibility); `--to` takes precedence |
| `--weeks <N>` | Limit collection to the last N weeks; `--weeks` takes precedence over `--from`/`--to` |
| `--dry-run` | Run collection against an in-memory database; the on-disk database is left untouched |
| `--force-refresh-prs` | Re-fetch ADO pull requests even when already cached (backfills pre-v1.0.9 rows) |
| `--validate-only` | Run configuration validation and exit (0 on success, 1 on errors) |
| `--no-validate` | Skip pre-flight configuration validation |

```bash
tga collect --repos my-project --from 2024-01-01 --to 2024-03-31
tga collect --weeks 4   # collect last 4 weeks across all repos
```

### tga classify

Stage 2: run the classification cascade over collected commits.

```bash
tga classify [--config <PATH>] [--database <PATH>]
             [--rules <PATH>] [--use-llm] [--backfill-complexity]
             [--no-external]
```

| Flag | Description |
|------|-------------|
| `--rules <PATH>` | Override `classification.rules_file` from config. Custom rules default to priority 110 (above built-in 100) and are standalone by default (`extend_defaults: false`). |
| `--use-llm` | Enable LLM fallback regardless of config setting |
| `--backfill-complexity` | Fill missing complexity scores (1–5) for already-classified commits via the LLM, without re-running the full cascade; category, confidence, and method are left untouched |
| `--no-external` | Disable all external classification sources (JIRA, GitHub Issues) for this run, even if configured in the rules file or config |

```bash
tga classify --rules ./custom-rules.yaml --use-llm
tga classify --no-external   # offline / CI mode
```

### tga report

Stage 3: generate reports from classified commits.

```bash
tga report [--config <PATH>] [--database <PATH>]
           [--output <DIR>] [--formats <FMT,...>]
```

| Flag | Description |
|------|-------------|
| `--output <DIR>` | Override `output.directory` from config |
| `--formats <FMT,...>` | Comma-separated: `csv`, `json`, `markdown` |

```bash
tga report --output ./q1-reports --formats csv,json
```

### tga pr-metrics

Aggregate pull-request metrics per engineer from the `pull_requests` cache.

```bash
tga pr-metrics [--config <PATH>] [--database <PATH>]
               [--weeks <N>] [--csv] [--output <PATH>]
```

| Flag | Description |
|------|-------------|
| `--weeks <N>` | Limit metrics to PRs created within the last N weeks |
| `--csv` | Emit CSV instead of an aligned text table |
| `--output <PATH>` | Write output to a file (CSV with `--csv`, otherwise the text table) |

```bash
tga pr-metrics --weeks 12 --csv --output pr-metrics.csv
```

The `pr_comments_given` and `avg_revisions` columns are reserved for future
use and currently always output `0.0`; the underlying review-comment and
revision-count data is not yet tracked.

### tga backfill

Retroactive maintenance operations that update existing commit rows in place
(outside the normal `collect → classify → report` pipeline).

```bash
tga backfill <SUBCOMMAND> [--dry-run]
```

| Subcommand | Description |
|------------|-------------|
| `ai-detection` | Re-run LLM classification on low-confidence prior LLM verdicts |
| `revert-flags` | Scan commit messages for revert patterns and set `is_revert` |
| `ticket-ids` | Scan commit messages for ticket refs and update `ticket_id`/`ticketed` |
| `reachability` | Re-run the full reachability scan (default branch + tags + release branches) and upsert `fact_commit_reachability` without re-collecting commits — use this to fix `on_default_branch=0` rows in existing databases (issue #290) |

| Flag | Description |
|------|-------------|
| `--dry-run` | Report how many rows would change without writing |
| `--repo NAME` | (reachability only) Only backfill the named repository (repeatable) |

```bash
tga backfill revert-flags --dry-run
tga backfill ticket-ids
tga backfill reachability           # fix on_default_branch for all repos
tga backfill reachability --repo my-service  # single repo only
```

### tga override

Manage manual classification overrides (Tier 0). Rows here pin a commit's
verdict regardless of what the rule-based or LLM tiers would produce.

```bash
tga override <SUBCOMMAND>
```

| Subcommand | Description |
|------------|-------------|
| `add <SHA> <WORK_TYPE> <CHANGE_TYPE> [--notes <TEXT>] [--repo <PATH>]` | Insert (or replace) an override row for a commit SHA |
| `list [--repo <PATH>]` | List every override row, optionally filtered by repository |
| `remove <SHA> [--yes]` | Delete the override row(s) for a SHA (`--yes` skips confirmation) |

```bash
tga override add abc1234 feature feature --notes "manual review"
tga override list
tga override remove abc1234 --yes
```

### tga rules

Introspect the classification rule set (issue #209). Useful for tuning
rules and answering "why was this commit classified as X?".

```bash
tga rules list                       # every loaded rule, sorted by priority
tga rules show <commit-sha>          # verdict + method recorded for a commit
tga rules test "feat: add login"     # dry-run the cascade against a message
```

### tga deployments / tga incidents / tga dora

DORA metrics infrastructure (issues #207, #208, #212, #213).

```bash
# Step 1: ingest deployment events (default: git tags matching dora.deployment_tag_pattern)
tga deployments collect

# Step 2 (optional): ingest production incidents from JIRA SRE issues
tga incidents collect

# Step 3: compute Deployment Frequency, Lead Time, Change Failure Rate, MTTR.
# Also rebuilds the `deployment_failures` derived join from the current
# `dora.failure_signals` config — safe to re-run after a config edit.
tga dora
```

The four DORA metrics land in pre-computed SQL views
(`v_deployment_frequency`, `v_lead_time`, `v_change_failure_rate`,
`v_mttr`) so dashboards can read them directly without re-aggregating.

### Tag & Release-Branch Reachability (issues #279, #290)

`tga collect` automatically populates `fact_commit_reachability` with five columns that distinguish "deployed via cherry-pick to a release branch and tagged" from "abandoned WIP":

| Column | Type | Meaning |
|---|---|---|
| `on_default_branch` | boolean | `true` if the commit is reachable from the repo's default branch (`main`/`master`) |
| `on_any_tag` | boolean | `true` if any git tag reaches this commit |
| `reachable_from_tags` | JSON array | Tag names that contain this commit |
| `on_release_branch` | boolean | `true` if commit is on any configured release branch |
| `release_branches` | JSON array | Matching release-branch names |

**`on_default_branch` fix (1.4.1 — issue #290):** Prior to 1.4.1, `on_default_branch`
was declared in the schema but never populated — every row had `on_default_branch = 0`.
`tga collect` now auto-detects each repo's default branch via `refs/remotes/origin/HEAD`
(the symref set by `git clone`), falling back through `refs/heads/main`,
`refs/heads/master`, `refs/remotes/origin/main`, `refs/remotes/origin/master`.
If none of these refs exist for a repo, a warning is logged and `on_default_branch`
stays 0 for that repo's commits. To fix existing databases without re-running the
full collection pipeline, see `tga backfill reachability` below.

This resolves a systematic blind spot: bug fixes and security patches cherry-picked to `release/*` branches, tagged for production, and never merged to `main` previously showed `on_default_branch = false` — making them indistinguishable from abandoned WIP. With this feature, the 32% "merged rate" for bug fixes turns out to be much higher when deployed-via-tag commits are counted.

**Config:**

```yaml
reachability:
  track_tags: true               # default: true
  track_release_branches: true   # default: true
  release_branch_patterns:       # default: ["release/*", "hotfix/*", "chore/release-*", "v*"]
    - "release/*"
    - "hotfix/*"
    - "chore/release-*"
    - "v*"
```

Set `track_tags: false` to skip the tag scan (useful for trunk-based repos with thousands of tags). The default is `true` because the scan is O(repo + refs), not O(repo × refs × commits).

Optionally disable the scan for a single run:

```bash
tga collect --skip-tag-reachability
```

**Useful derived queries:**

```sql
-- What % of bug fixes actually shipped (via any path)?
SELECT
  COUNT(*) FILTER (WHERE on_default_branch OR on_any_tag OR on_release_branch)
  * 100.0 / COUNT(*) AS shipped_pct
FROM classifications cl
JOIN commits c ON c.classification_id = cl.id
JOIN fact_commit_reachability fcr ON fcr.commit_sha = c.sha
WHERE cl.category = 'bug_fix';

-- Cherry-pick rate: commits reachable from tags but not on main
SELECT repo, COUNT(*)
FROM fact_commit_reachability
WHERE on_any_tag AND NOT on_default_branch
GROUP BY repo;
```

## Pipeline Architecture

```
git repos ──┐
             │   collect      SQLite (tga.db)   classify       SQLite    report
GitHub API ──┼──────────────► [commits]        ──────────────► [classif]─────────► CSV (×9)
JIRA API ────┤   (libgit2,    [authors]         (7-tier                 ► JSON
Linear API ──┤   reqwest)     [pull_requests]   cascade,                ► Markdown
ADO API  ────┘                [work_items]      Rayon-parallel)
```

**Stage 1 — collect** (`tga::collect`): opens each repository with libgit2, walks the configured branch, extracts commit metadata and diff stats, resolves author identities, fetches GitHub PR / JIRA issue / Linear / Azure DevOps work item metadata via REST/GraphQL, and writes everything to SQLite.

**Stage 2 — classify** (`tga::classify`): reads unclassified commits from the database, runs each message through the classification cascade (see below), and writes a classification verdict back. Rule-based tiers execute in parallel via Rayon.

**Stage 3 — report** (`tga::report`): reads the classified database, aggregates per-author, per-week, DORA, velocity, and quality statistics, and writes the configured output formats to the output directory.

## Classification

### Classification Cascade

Each commit message is tested against tiers in order. The first tier to produce a confident result wins.

**Tier 0 — Manual Override** (confidence 1.0): looks up the `(commit_hash, repo_path)` pair in the `classification_overrides` table. Managed via `tga override add|list|remove`.

**Tier 1.5 — Issue Type** (confidence 0.90): when the commit has ticket references resolving to rows in `issue_cache`, maps the upstream issue type (`bug`, `story`, `task`, `spike`, etc.) directly to a `change_type`.

**Tier 1.6 — JIRA Project Mapping** (default confidence 0.88, override via `jira.jira_project_mapping_confidence`): when `jira.jira_project_mappings` is configured, maps the JIRA project key prefix of any `[A-Z]+-\d+` reference to a `change_type`. Fires *before* the regex tier so project mappings outrank the generic `jira-ticket` regex rule (issue #206). Example config:

```yaml
jira:
  jira_project_mappings:
    TQL: bug_fix
    APEX: integration
    INFRA: platform_infrastructure
    SEC: security
  jira_project_mapping_confidence: 0.88   # optional, default 0.88
```

Tier-0 manual overrides and exact-keyword conventional-commit prefixes (e.g. `fix:`) still beat this tier.

**Tier 1 — Exact (Aho-Corasick)**: builds a single finite-state machine from every keyword list across every rule and scans the message in O(n) time. Matches `feat:`, `fix:`, `chore:`, etc. Confidence 0.85–0.95.

**Tier 2 — Regex**: applies pre-compiled regex patterns from the rule set. Handles anchored conventional-commit patterns (`^feat(\([^)]*\))?!?:`) and JIRA ticket IDs (`\b[A-Z][A-Z0-9]+-\d+\b`).

**Tier 2.5 — Weighted Sum** (new in 1.3.0): composes five independent signals into per-category scores and emits a verdict when the argmax score meets the minimum confidence threshold (default 0.55). Active for all commits including when `extend_defaults: false`. See [Weighted-Sum Tier](#weighted-sum-tier-250-new-in-130).

**Tier 3 — Fuzzy heuristics**: detects merge commits (via `is_merge` flag or `Merge pull request` prefix) and reverts (via `Revert` prefix). No external dependencies. Suppressed when `extend_defaults: false`.

**Tier 7 — LLM fallback** (optional, async): calls an OpenAI-compatible API (**OpenRouter** by default, **AWS Bedrock** behind the `bedrock` cargo feature) when tiers 0–3 leave a commit below the fallback threshold. Disabled by default; enable with `analysis.llm_classification.enabled: true` or `--use-llm`. Results are only accepted when `confidence >= confidence_threshold` (default 0.7). See [LLM fallback threshold](#llm-fallback-threshold-migration).

### Default Rules

| ID | Category | Keywords / Patterns |
|----|----------|---------------------|
| `cc-feat` | `feature` | `feat:`, `feature:`, `^feat(...)?!?:` |
| `cc-fix` | `bugfix` | `fix:`, `bugfix:`, `hotfix`, `^fix(...)?!?:` |
| `cc-chore` | `chore` | `chore:`, `^chore(...)?!?:` |
| `cc-docs` | `documentation` | `docs:`, `doc:`, `^docs?(...)?!?:` |
| `cc-refactor` | `refactor` | `refactor:`, `^refactor(...)?!?:` |
| `cc-test` | `test` | `test:`, `tests:`, `^tests?(...)?!?:` |
| `cc-ci` | `ci` | `ci:`, `^ci(...)?!?:` |
| `cc-perf` | `performance` | `perf:`, `^perf(...)?!?:` |
| `cc-style` | `style` | `style:`, `^style(...)?!?:` |
| `cc-build` | `build` | `build:`, `^build(...)?!?:` |
| `cc-revert` | `revert` | `revert:`, `^revert(...)?!?:` |
| `breaking-change` | `breaking` | `breaking change`, `breaking-change` |
| `jira-ticket` | `feature` (ticketed) | `\b[A-Z][A-Z0-9]+-\d+\b` |
| `kw-bug` | `bugfix` | `bug`, `defect` |
| `kw-security` | `bugfix` (security) | `security`, `cve-`, `vulnerability` |

Commits that match no rule are assigned category `uncategorized` with confidence 0.0.

### Custom Rules File

Supply your own rules as a YAML (or JSON) file:

```yaml
# my-rules.yaml
version: "1.0"
extend_defaults: false   # true = merge with built-ins; false = standalone (default)
rules:
  - id: my-deploy
    category: deployment
    keywords:
      - "deploy:"
      - "release:"
    patterns:
      - "(?i)^deploy(ment)?:"
    priority: 80
    confidence: 0.9
```

```bash
tga classify --rules ./my-rules.yaml
# or in config.yaml:
# classification:
#   rules_file: ./my-rules.yaml
```

### Rules YAML schema

Each entry under `rules:` is a **Rule** with these fields:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `id` | string | **required** | Unique rule identifier (used in logs and `tga rules test`) |
| `category` | string | **required** | The classification label written to the database |
| `subcategory` | string | `null` | Optional leaf label (e.g. `"sql-injection"` under `category: "security"`) |
| `keywords` | list of strings | `[]` | Exact substrings, matched case-insensitively via Aho-Corasick |
| `pattern` / `patterns` | string or list | `[]` | Regex patterns (see note below) |
| `priority` | integer | `110` | Higher = checked first. Custom-rule default (110) beats built-ins (peak 115 for `cc-revert`, 100 for `cc-feat`/`cc-fix`) |
| `confidence` | float 0–1 | `0.85` | Score attached to the verdict; only accepted if ≥ `confidence_threshold` |

**`pattern:` vs `patterns:`** — both key names are accepted:

```yaml
# Singular string (most natural for a single regex)
pattern: "(?i)^feat[:(]"

# Plural list (multiple alternates for one rule)
patterns:
  - "(?i)^feat[:(]"
  - "(?i)^feature[:(]"
```

Using `pattern:` with a single string is exactly equivalent to `patterns:` with a single-element list. Both forms were silently mishandled in versions ≤ 1.2.0 (issue #259); if you had rules that never fired, this was the cause.

The top-level `RuleSet` also has two optional fields:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `version` | string | `null` | Schema version tag (informational) |
| `extend_defaults` | bool | `false` | When `true`, custom rules are merged with the built-in set; same-`id` entries override the built-in |

### Weighted-Sum Tier 2.5 (new in 1.3.0)

The weighted-sum tier is a lightweight ensemble that sits between the regex tier and the fuzzy tier. It was introduced in 1.3.0 (issue #270) after evaluating fuzzy-logic crates — none of them provided sufficient recall improvement over the stepped keyword approach at acceptable binary size cost.

#### How it works

Five signals are evaluated independently and their scores are summed per category. The category with the highest total score wins, provided the score meets the `min_confidence` threshold (default 0.55). Confidence is clamped to `[min_confidence, 0.95]`.

| Signal | How scored |
|--------|------------|
| Keyword density | 0 matches → 0.0; 1 match → 0.40; 2 matches → 0.60; 3+ matches → 0.75 |
| Ticket prefix | Uniform +0.05 when a `PROJ-123` style prefix is detected |
| Message length | Short (<12 chars): boosts KTLO/Merge (+0.10), Maintenance (+0.05); suppresses Feature (−0.05). Long (>80 chars): boosts Feature/PlatformWork (+0.10), Maintenance/Bugfix (+0.05). |
| Merge indicator | Dedicated weight vector fires when `is_merge=true` or message starts with "merge…" |
| File paths | Test files → Maintenance/Bugfix signal; docs/markdown → Content signal; manifests → KTLO/Maintenance signal |

If two or more categories share the exact same top score, the tier returns no verdict (tie-breaking falls through to fuzzy or LLM).

The tier is active even when `extend_defaults: false`. Unlike the fuzzy tier, which emits fixed built-in category strings (`merge`, `feature`, `chore`), the weighted-sum tier picks its verdict dynamically by signal composition and does not embed built-in category knowledge directly.

#### Configuration

```yaml
classification:
  weighted_sum:
    enabled: true         # default: true. Set false to disable this tier entirely.
    min_confidence: 0.55  # default: 0.55. Minimum score to emit a verdict.
```

To disable just this tier without touching anything else:

```yaml
classification:
  weighted_sum:
    enabled: false
```

### LLM Fallback Threshold Migration

**Breaking change in 1.3.0**: `classification.llm_fallback_threshold` default raised from `0.0` to `0.65`.

At `0.0` (the old default), the LLM fallback would only fire when every deterministic tier returned confidence exactly `0.0` — effectively never. This made `use_llm: true` a no-op for most real-world commit messages. The new default of `0.65` routes any deterministic verdict below 0.65 to the LLM when `use_llm: true`.

**Affected setups**: configs that set `use_llm: true` (or `--use-llm`) and relied on the LLM *never* re-classifying commits that the deterministic tiers had already classified with confidence below 0.65. In practice this affects configs where the fuzzy tier's outputs (0.40 for short chore messages, 0.60 for bare ticket refs) were intentionally accepted as final.

**Migration options**:

Option 1 — restore the old behavior explicitly:

```yaml
classification:
  llm_fallback_threshold: 0.0   # pin to old default
```

Option 2 — accept the new default and tune upward if the LLM fires too often:

```yaml
classification:
  llm_fallback_threshold: 0.70  # only route very low-confidence verdicts to LLM
```

**Users with `use_llm: false`** (the default) are not affected by this change.

### Rule Priority and extend_defaults (issue #259)

Custom rules default to **priority 110** — one step above the highest built-in rule priority (100). This means user rules win over the default ruleset without needing an explicit `priority:` entry in every rule.

Custom rule files also default to **standalone** mode (`extend_defaults: false`): only the rules in the file are applied. Opt in to merging with the built-in defaults by adding `extend_defaults: true`.

**Fuzzy-tier gating (1.2.2)**: the fuzzy heuristic tier (which emits the built-in category strings `merge`, `feature`, `chore`) is suppressed when `extend_defaults: false`. This aligns with the principle that `extend_defaults: false` means "no built-in hardcoded classification." If you see `method=fuzzy_match` rows for a config with `extend_defaults: false`, upgrade to 1.2.2.

**Weighted-sum tier and extend_defaults (1.3.0)**: unlike the fuzzy tier, the weighted-sum tier (Tier 2.5) remains active even with `extend_defaults: false`. It picks its verdict via signal composition rather than emitting fixed built-in strings, so disabling it requires `classification.weighted_sum.enabled: false`.

```yaml
# my-rules.yaml — standalone by default (no built-in rules loaded)
extend_defaults: false   # optional: explicitly document the default
rules:
  - id: my-deploy
    category: deployment
    keywords: ["deploy:", "release:"]
    # priority defaults to 110 — beats the built-in cc-feat (100)
```

```yaml
# my-addons.yaml — augments the defaults
extend_defaults: true
rules:
  - id: my-payments
    category: payments
    keywords: ["payment:", "billing:"]
    confidence: 0.92
```

**Diagnosing rules that never fire**: if your custom categories never appear in reports, run:

```bash
tga rules list --rules ./my-rules.yaml   # shows all loaded rules with their matchers
tga rules test "feat: add login" --rules ./my-rules.yaml   # dry-run a message
tga classify -vv --rules ./my-rules.yaml   # verbose mode shows per-rule decisions
```

A `WARN rule has no keywords or patterns` in the log output means a rule's YAML key was silently dropped — the most common cause is writing `pattern:` (singular) in a version before 1.2.1. Upgrade to 1.2.1+ and both `pattern:` and `patterns:` are accepted.

See also: [Multi-source classification](#multi-source-classification-issue-260) for external ticket system integration.

### Multi-source classification (issue #260)

`tga classify` can consult external ticket systems — JIRA Cloud/Server and
GitHub Issues — as high-confidence classification signals **before** the
commit-message rule tiers run.

External sources fire as **Tier 0.5** — after manual overrides, before
commit-message rules — with a fixed confidence of `0.92`. They are enabled
by default when configured; use `--no-external` to opt out for a run.

**Priority model** (highest to lowest):

1. Manual overrides (`tga override add`, confidence 1.0)
2. External sources — JIRA issue type / GitHub Issues labels (confidence 0.92)
3. Custom commit-message rules (default priority 110)
4. Built-in TGA commit-message rules (priority 100)
5. LLM fallback (when `use_llm: true`)

**Configuration** — add a `sources:` list to `config.yaml` under
`classification:`:

```yaml
classification:
  no_external: false    # opt-out flag; sources fire by default once configured
  sources:

    # --- JIRA source ---
    - type: jira
      base_url: "https://yourco.atlassian.net"
      token_env: JIRA_API_TOKEN      # name of the env var holding your API token
      email_env: JIRA_USER_EMAIL     # optional — only needed for JIRA Cloud Basic auth
      project_keys: ["PROJ", "ENG"]  # empty list = match all projects
      field_mappings:
        # Maps JIRA issue type → tga category. Case-sensitive on the JIRA side.
        issue_type:
          Bug: bug_fix
          Story: new_feature
          Task: tech_debt_refactoring
          Sub-task: tech_debt_refactoring
          Epic: new_feature
        # Maps JIRA labels → tga category. Labels are case-sensitive.
        labels:
          ktlo: tech_debt_refactoring
          security: security
        # Maps JIRA components → tga category.
        components:
          "CI/CD": devops
          Platform: platform_infrastructure

    # --- GitHub Issues source ---
    - type: github_issues
      repo: "acme/widgets"           # "owner/repo" slug
      token_env: GITHUB_TOKEN        # name of the env var holding your PAT
      label_mappings:
        bug: bug_fix
        enhancement: new_feature
        dependencies: tech_debt_refactoring
        documentation: documentation
        security: security
```

**Field-mapping precedence within a JIRA source** (highest to lowest):
`issue_type` → `labels` → `components`. The first mapping that matches wins.

**Credential handling**: tokens are read from the **named environment
variables** at runtime — never store them directly in config files:

```bash
export JIRA_API_TOKEN="your-jira-api-token"
export JIRA_USER_EMAIL="you@yourco.com"   # JIRA Cloud only
export GITHUB_TOKEN="your-github-pat"
tga classify --config config.yaml
```

**Failure mode**: a missing token, HTTP 401/404, or network error causes the
source to be skipped with a `WARN` log and the pipeline falls through to
commit-message rules for affected commits. Classification never panics on
source failures.

**Caching**: each unique ticket is fetched at most once per `tga classify`
run (in-memory dedup, never persisted to disk). A 15 000-commit run against
a project with 500 unique JIRA tickets makes 500 API calls, not 15 000.

**Disabling external sources**: use `--no-external` to skip all sources for
a run (useful in CI or offline environments):

```bash
tga classify --no-external
```

See [`examples/multi-source-config.yaml`](examples/multi-source-config.yaml)
for a fully annotated example.

    # --- Linear source (new in 1.4.0) ---
    - type: linear
      api_key_env: LINEAR_API_KEY         # Personal API Key (not OAuth)
      team_keys: ["ENG", "INFRA"]         # Empty = match all Linear teams.
                                          # Use this to disambiguate from JIRA keys
                                          # when both follow the PROJ-NNN pattern.
      field_mappings:
        issue_type:
          Bug: bug_fix
          Feature: new_feature
          Improvement: tech_debt_refactoring
        labels:
          security: security
          ktlo: tech_debt_refactoring
        cycle:
          "Sprint 42": new_feature        # active cycle/sprint → category

    # --- Shortcut source (new in 1.4.0) ---
    - type: shortcut
      api_token_env: SHORTCUT_API_TOKEN
      field_mappings:
        story_type:
          bug: bug_fix
          feature: new_feature
          chore: tech_debt_refactoring
        labels:
          security: security
        workflow_state:
          "In Progress": new_feature      # state name → category

    # --- Confluence source (new in 1.4.0) ---
    # Note: Confluence confidence is fixed at 0.80 (lower than the default 0.92).
    # References are extracted from /wiki/spaces/.../pages/<id> URLs and
    # Smart Commit tags ([CONF-<id>] in commit messages).
    - type: confluence
      base_url: "https://yourco.atlassian.net"
      token_env: CONFLUENCE_API_TOKEN
      email_env: CONFLUENCE_EMAIL         # Confluence Cloud Basic auth email
      label_mappings:
        runbook: devops
        architecture: tech_debt_refactoring
        security: security

    # --- Datadog source (new in 1.4.0) ---
    # Matches commit SHAs (7–40 hex chars) in commit messages against the
    # Datadog Events API deployment events. Confidence is fixed at 0.95.
    - type: datadog
      api_key_env: DD_API_KEY
      app_key_env: DD_APP_KEY
      site: datadoghq.com                 # optional, default "datadoghq.com"
      default_category: devops            # category written when a deployment
                                          # event is found (default: "devops")
      lookback_days: 30                   # how far back to search events (default: 30)
```

**Linear key disambiguation**: Linear keys and JIRA keys share the same
`PROJ-NNN` shape. Set `team_keys` on the Linear source to the exact team
prefixes you use — keys that do not match any configured prefix are silently
skipped by the Linear source (and may still be picked up by JIRA if that
source is also configured).

**Shortcut reference formats**: two forms are recognized automatically:
- `[ch1234]` — bracket form (in commit message body or subject)
- `sc-1234` — sc-prefix form (common in branch names carried into messages)

**Confluence reference formats**: two forms are recognized:
- `/wiki/spaces/MYSPACE/pages/12345` — full URL (page ID extracted)
- `[CONF-12345]` — Smart Commit tag (page ID after the hyphen)

**Credential handling for new sources**:

```bash
export LINEAR_API_KEY="lin_api_..."  # pragma: allowlist secret
export SHORTCUT_API_TOKEN="..."      # pragma: allowlist secret
export CONFLUENCE_API_TOKEN="..."    # pragma: allowlist secret
export CONFLUENCE_EMAIL="you@yourco.com"
export DD_API_KEY="..."              # pragma: allowlist secret
export DD_APP_KEY="..."              # pragma: allowlist secret
tga classify --config config.yaml
```

See [`examples/multi-source-config.yaml`](examples/multi-source-config.yaml)
for a fully annotated example covering all six sources.

See also: [Rules YAML schema](#rules-yaml-schema) for commit-message rule authoring.

## Operational Notes

### Fixing `on_default_branch = 0` for existing databases (1.4.1 — issue #290)

If your `tga.db` was built before 1.4.1, every row in `fact_commit_reachability`
has `on_default_branch = 0` because the column was never populated. You can fix
this **without re-running the full collection pipeline** (which can take 20+
minutes on large corpora):

```bash
# Verify the problem (sum should be 0 on a pre-1.4.1 DB)
sqlite3 tga.db "SELECT SUM(on_default_branch), COUNT(*) FROM fact_commit_reachability;"

# Fix in-place — upserts all five reachability columns, no row deletion
tga backfill reachability

# Verify the fix (sum should now be > 0 for repos with main/master)
sqlite3 tga.db "SELECT SUM(on_default_branch), SUM(on_any_tag), COUNT(*) FROM fact_commit_reachability;"
sqlite3 tga.db "SELECT repo, SUM(on_default_branch) FROM fact_commit_reachability GROUP BY repo LIMIT 10;"
```

The backfill is **idempotent** — running it multiple times produces identical
results and never deletes rows. Only the five reachability columns are touched.

**Auto-detection logic:** for each repository, tga tries (in order):
1. `refs/remotes/origin/HEAD` (symref — set by `git clone`, points at whatever
   the remote considers its default branch)
2. `refs/heads/main`
3. `refs/heads/master`
4. `refs/remotes/origin/main`
5. `refs/remotes/origin/master`

If none of the above resolve to a commit, a `WARN` is logged and
`on_default_branch` remains 0 for that repo.

### SQLite WAL durability (fixed in 1.4.0 — issue #298)

`tga` opens its database in WAL (Write-Ahead Log) mode for concurrent-read
performance. Prior to 1.4.0, the WAL was never explicitly checkpointed on
exit, so the main `.db` file could lag behind the `-wal` sidecar. On a large
corpus this meant up to 22 minutes of classification work was held only in the
WAL and could be lost if the file was copied or the process was killed.

**1.4.0 fix**: `tga classify` now calls `PRAGMA wal_checkpoint(TRUNCATE)` on
clean exit, flushing all WAL frames into the main database file and zeroing
the WAL. In addition, a periodic `PRAGMA wal_checkpoint(PASSIVE)` fires every
`classification.checkpoint_every` commits (default 0 = disabled) to cap the
data-loss window during long runs.

```yaml
classification:
  checkpoint_every: 1000  # PASSIVE checkpoint every 1000 classified commits
                          # (0 = off, which is the default)
```

The checkpoint is non-fatal: if it fails (e.g., `SQLITE_CORRUPT`), `tga`
logs a `WARN` and continues. You can manually flush at any time:

```bash
sqlite3 tga.db 'PRAGMA wal_checkpoint(TRUNCATE)'
```

## Output Formats

### CSV

Nine CSV files are written when `csv` is in the format list (per-author,
weekly activity, DORA metrics, velocity, quality, and related breakdowns).
The two most commonly used are documented below.

**`authors.csv`** — one row per author:

| Column | Description |
|--------|-------------|
| `name` | Canonical author name |
| `email` | Canonical author email |
| `commit_count` | Total commits |
| `insertions` | Total lines added |
| `deletions` | Total lines deleted |
| `files_changed` | Total files changed |
| `first_commit` | ISO 8601 timestamp of earliest commit |
| `last_commit` | ISO 8601 timestamp of most recent commit |

**`weekly_activity.csv`** — one row per week/author/repository bucket:

| Column | Description |
|--------|-------------|
| `week` | ISO week label, e.g. `2024-W03` |
| `author` | Author name |
| `repository` | Repository name |
| `commit_count` | Commits in this bucket |
| `insertions` | Lines added in this bucket |
| `deletions` | Lines deleted in this bucket |

### JSON

Four JSON files are written when `json` is in the format list:
`report.json`, `velocity_summary.json`, `quality_summary.json`, and
`dora_summary.json`. The primary one is documented below.

**`report.json`** — full structured payload:

```json
{
  "generated_at": "2024-03-15T10:00:00Z",
  "period_start": "2024-01-01T00:00:00Z",
  "period_end":   "2024-03-14T23:59:59Z",
  "total_commits": 347,
  "total_authors": 8,
  "category_breakdown": { "feature": 120, "bugfix": 45, ... },
  "authors": [
    {
      "name": "Alice Smith",
      "email": "alice@company.com",
      "commit_count": 87,
      "insertions": 4200,
      "deletions": 1100,
      "files_changed": 310,
      "categories": { "feature": 50, "bugfix": 20, ... },
      "first_commit": "...",
      "last_commit": "..."
    }
  ],
  "repositories": [
    {
      "name": "my-project",
      "commit_count": 347,
      "author_count": 8,
      "insertions": 18000,
      "deletions": 6000,
      "top_categories": [["feature", 120], ["bugfix", 45]]
    }
  ],
  "weekly_activity": [
    {
      "week": "2024-W03",
      "author": "Alice Smith",
      "repository": "my-project",
      "commit_count": 12,
      "insertions": 500,
      "deletions": 120,
      "categories": { "feature": 8, "bugfix": 4 }
    }
  ]
}
```

### Markdown

**`report.md`** — a narrative report containing a summary header, per-author commit table, category breakdown, and weekly activity section. Suitable for pasting into Confluence or a PR description.

## Development

### Build and Test

```bash
# Build everything
cargo build

# Build release binary
cargo build --release

# Run all tests
cargo test

# Lint (zero warnings required)
cargo clippy -- -D warnings

# Format check (CI gate)
cargo fmt --check

# Auto-format
cargo fmt

# Generate rustdoc
cargo doc --open
```

### Running Against Real Repos

`configs/example-config.yaml` is a working example that analyzes repositories using `developer_aliases`. Copy it, adjust paths and names to match your setup, then run:

```bash
tga analyze --config configs/example-config.yaml --database tga.db
```

### CI Gates

The GitHub Actions workflow (`ci.yml`) requires:
- `cargo fmt -- --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cargo doc --no-deps` with `RUSTDOCFLAGS="-D warnings"`

## Crate Structure

Single `tga` crate (consolidated from the original 5-crate workspace):

| Module | Path | Purpose |
|--------|------|---------|
| `tga::core` | `src/core/` | Shared types, config, DB schema, migrations, error types |
| `tga::collect` | `src/collect/` | Stage 1: git extraction (libgit2), GitHub/JIRA/Linear/ADO clients, `PmAdapter` trait |
| `tga::classify` | `src/classify/` | Stage 2: seven-tier classification cascade |
| `tga::report` | `src/report/` | Stage 3: CSV/JSON/Markdown output |
| `commands` (binary-private) | `src/commands/` | Subcommand handlers wired into `src/main.rs` |

## Performance

Benchmarked against the Duetto production corpus (72,608 commits, 20 JIRA projects) on
Apple M4 Max (128 GB RAM) with tga 1.3.0:

| Metric | Value |
|--------|-------|
| CPU classify throughput (rule cascade only) | ~113,000 commits/sec |
| Coverage — rule tiers only (`--no-external`) | 64.3% of 72,608 commits |
| Coverage — with JIRA external source | 67.7% of 72,608 commits |
| Peak RSS (full classify pass) | 235 MB |
| weighted_sum tier contribution (new in 1.3.0) | 9.6–11.0% of classified commits |

JIRA external-source classification adds 3.4 percentage points of coverage at the cost of
~23 minutes of network-bound JIRA API time (the rule-cascade itself completes in 0.64s on
this corpus). Use `--no-external` for fast iteration on rule files.

Full benchmark: [`docs/trusty-git-analytics/regression-testing/v1.3.0-2026-05-27.md`](
../../docs/trusty-git-analytics/regression-testing/v1.3.0-2026-05-27.md)

## License

**Non-commercial use only.** See [LICENSE](LICENSE) for terms.

Elastic License 2.0
