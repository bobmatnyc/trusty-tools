# Configuration Reference

The configuration file is YAML, matching the schema used by the Python predecessor
`gitflow-analytics`. All keys are deserialized via `serde_yaml` into typed structs in
`tga::core::config`. Paths support `~` expansion via the `shellexpand` crate.

## Top-Level Structure

```yaml
repositories: []          # list[RepositoryConfig], required
database: ~               # path  — SQLite DB override (added v2.2.2, issue #406)
llm: {}                   # LlmConfig — top-level LLM section (added v2.2.2, issue #407)
github: {}                # GitHubConfig
bitbucket: {}             # BitbucketConfig (Cloud only)
analysis: {}              # AnalysisConfig
output: {}                # OutputConfig
cache: {}                 # CacheConfig
jira: {}                  # JIRAConfig
jira_integration: {}      # JIRAIntegrationConfig
jira_project_mappings: {} # dict[str,str]
taxonomy_mapping: {}      # dict[str,str]
teams: {}                 # TeamsConfig
velocity: {}              # VelocityConfig
activity_scoring: {}      # ActivityScoringConfig
boilerplate_filter: {}    # BoilerplateFilterConfig
quality_report: {}        # QualityReportConfig
ai_detection: {}          # AIDetectionConfig
github_issues: {}         # GitHubIssuesConfig
confluence: {}            # ConfluenceConfig
```

## Sections

### `database` — SQLite database path (added v2.2.2, issue #406)

The path to the SQLite database file. Supports `~` home-directory expansion.

**Precedence** (highest first):

1. `--database` CLI flag — always wins.
2. `database:` field in this config file.
3. Hardcoded default `tga.db` in the current working directory.

```yaml
# Example: use a team-shared path
database: ~/data/team-analytics.db
```

This field is at the top level of `config.yaml` and is **not** inside any
nested section. Adding it here eliminates the need to pass `--database` on
every `tga` invocation.

---

### `llm` — Top-level LLM configuration (added v2.2.2, issue #407)

The `llm:` section controls how the LLM fallback tier reaches an inference
provider. It is separate from `classification:` (which controls *when* to call
the LLM). When `llm:` is present it takes precedence over the legacy
`classification.llm_provider` / `classification.openrouter_api_key` fields;
using those legacy fields emits a `tracing::warn!` deprecation message.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `source` | enum | `openrouter` | LLM provider: `openrouter`, `bedrock`, or `anthropic-api` |
| `api_key_env` | string | `OPENROUTER_API_KEY` | **Name** of the env var holding the API key (never the key itself). Ignored for `bedrock`. |
| `region` | string | None | AWS region (Bedrock only). When absent, the AWS SDK resolves the region from the environment (`AWS_DEFAULT_REGION`, profile, etc.). |
| `model` | string | provider-appropriate default | Provider-specific model id (see below). |

#### Source variants

**`openrouter`** (default)

Uses the OpenRouter API (OpenAI-compatible schema). The API key is read from
the environment variable named by `api_key_env` (default:
`OPENROUTER_API_KEY`). The variable is never stored in the config. If the
variable is unset or empty when LLM is enabled, `tga classify` exits
non-zero with an actionable error before writing any DB rows.

```yaml
llm:
  source: openrouter
  api_key_env: OPENROUTER_API_KEY   # or any custom var name
  model: gpt-4o-mini
```

**`bedrock`**

Uses AWS Bedrock with IAM credential-chain auth — no secret is stored in
the config file. Requires:

- Binary compiled with `--features bedrock` (if not, `tga classify` exits
  non-zero with "reinstall with --features bedrock").
- Valid AWS credentials in the default chain: `AWS_ACCESS_KEY_ID` /
  `AWS_SECRET_ACCESS_KEY` env vars, `~/.aws/credentials` profile, EC2
  instance metadata, ECS task role, or AWS SSO.
- `api_key_env` is ignored for this source.

```yaml
llm:
  source: bedrock
  region: us-east-1                  # optional; falls back to AWS SDK defaults
  model: anthropic.claude-3-5-sonnet-20241022-v2:0
```

**`anthropic-api`**

Recognized enum value; returns a clear "not yet implemented" error. Reserved
for a future direct Anthropic Messages API integration.

#### Default models by source

| Source | Default model |
|--------|---------------|
| `openrouter` | `gpt-4o-mini` |
| `bedrock` | `anthropic.claude-3-haiku-20240307-v1:0` |

#### Security note

`api_key_env` stores the **variable name** (e.g. `OPENROUTER_API_KEY`), never
the key value. The actual secret is read from the environment at runtime. Never
commit API keys to the config file.

---

### `repositories[]` — RepositoryConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | required | Display name for the repository |
| `path` | path | required | Local filesystem path (supports `~`) |
| `github_repo` | string | None | `owner/name` for GitHub API correlation |
| `project_key` | string | None | JIRA project key prefix |
| `branch` | string | None | Override default branch detection |

### `github` — GitHubConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `token` | string | env `GITHUB_TOKEN` | GitHub Personal Access Token |
| `owner` | string | None | Repository owner / org |
| `organization` | string | None | If set, discover all repos from this org |
| `base_url` | url | `https://api.github.com` | API base URL (GHE support) |
| `max_retries` | u32 | 3 | Retry count on transient failures |
| `backoff_factor` | f64 | 2.0 | Exponential backoff multiplier |
| `fetch_prs` | bool | false | Fetch pull request metadata from GitHub |
| `fetch_pr_reviews` | bool | true | Fetch review summaries with PRs |
| `open_pr_refresh_ttl_hours` | u32 | 1 | TTL for refreshing open PR snapshots |
| `ticket_regex` | string | None | Override regex for detecting GitHub ticket refs (e.g. `#(\d+)`) in commit messages. Added in v1.0.6 (#75). |

### `bitbucket` — BitbucketConfig

Bitbucket Cloud only. Bitbucket Server / Data Center is not supported.

Authentication accepts either an access token (Bearer) or an App Password
(Basic auth). Token takes precedence when both are populated.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `username` | string | None | Bitbucket account / workspace member username (required for Basic auth) |
| `app_password` | string | env `BITBUCKET_APP_PASSWORD` | Bitbucket App Password (Basic auth secret) |
| `token` | string | env `BITBUCKET_TOKEN` | Workspace / repository access token (Bearer auth) |
| `workspace` | string | required when `fetch_prs: true` | Workspace slug (`myteam` in `bitbucket.org/myteam/myrepo`) |
| `repo_slug` | string | required when `fetch_prs: true` | Repository slug (`myrepo` in `bitbucket.org/myteam/myrepo`) |
| `fetch_prs` | bool | `false` | Fetch pull request metadata |
| `api_base_url` | url | `https://api.bitbucket.org/2.0` | API base URL override (test seam) |

State mapping into the shared `pull_requests` table:

| Bitbucket state | Stored as |
|-----------------|-----------|
| `OPEN` | `open` |
| `MERGED` | `merged` |
| `DECLINED` | `closed` |
| `SUPERSEDED` | `closed` |

`DECLINED` and `SUPERSEDED` collapse onto `closed` because the shared schema
has no richer variants. Reports that need to distinguish them must consult
the raw Bitbucket payload, which is currently not persisted.

### `analysis` — AnalysisConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `exclude_authors` | list[string] | [] | Email patterns to exclude |
| `exclude_paths` | list[glob] | [] | File path globs to exclude from diff stats |
| `exclude_merge_commits` | bool | false | Skip merge commits entirely |
| `similarity_threshold` | f64 | 0.85 | Identity fuzzy match threshold (0–1) |
| `branch_analysis` | BranchAnalysisConfig | smart | Branch selection strategy |
| `ticket_detection` | TicketDetectionConfig | {} | Ticket regex configuration |
| `llm_classification` | LlmClassificationConfig | {} | LLM provider settings |
| `identity` | IdentityConfig | {} | Identity resolution settings |

#### `analysis.branch_analysis`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `strategy` | enum | `smart` | `smart` / `all` / `main_only` |
| `branch_commit_limit` | u32 | 1000 | Max commits per branch |
| `max_branches` | u32 | 50 | Max branches per repo |
| `active_days` | u32 | 90 | Only branches with commits in last N days (smart) |
| `include_patterns` | list[regex] | release/*, hotfix/* | Always-include patterns |
| `exclude_patterns` | list[regex] | dependabot/*, renovate/* | Always-exclude patterns |

#### `analysis.ticket_detection`

| Field | Type | Default |
|-------|------|---------|
| `jira_pattern` | regex | `[A-Z]{2,10}-\d+` |
| `github_pattern` | regex | `(?:closes\|fixes\|resolves)\s+#(\d+)` |
| `exclude_patterns` | list[regex] | `CVE-\d+`, `CWE-\d+`, `\d{8,}` |
| `commit_filter` | enum | `all` | `all` / `squash_merges_only` / `merge_commits` |

#### `analysis.llm_classification`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | true | Enable the LLM fallback tier |
| `provider` | enum | `openrouter` | `openrouter` / `bedrock` / `auto` |
| `model` | string | `mistralai/mistral-7b-instruct` | LLM model identifier |
| `api_key` | string | env `OPENROUTER_API_KEY` | API key (not used for `bedrock`) |
| `confidence_threshold` | f64 | 0.7 | Minimum confidence to accept an LLM result |
| `llm_fallback_threshold` | f64 | 0.0 | Commits with rule-based confidence **above** this value skip the LLM tier entirely. Setting to e.g. `0.5` avoids sending already-confident results to the LLM. Added in v1.0.6 (#78). |
| `llm_fallback_concurrency` | usize | 4 | Maximum concurrent LLM requests during the fallback pass (`buffer_unordered` cap). Increase to reduce wall-clock time when API latency is the bottleneck. Added in v1.0.6 (#83). |
| `batch_size` | u32 | 50 | Commits per LLM batch |
| `max_tokens` | u32 | 50 | Maximum tokens per LLM response |
| `temperature` | f64 | 0.1 | Sampling temperature |
| `timeout_seconds` | u32 | 30 | Per-request timeout |
| `cache_ttl_days` | u32 | 90 | Cache TTL for LLM results |

#### `analysis.identity`

| Field | Type | Default |
|-------|------|---------|
| `strip_suffixes` | list[string] | [] | Email suffixes to strip before matching |
| `manual_mappings` | list[ManualMapping] | [] | Forced canonical mappings |
| `fuzzy_threshold` | f64 | 0.85 |

### `output` — OutputConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `directory` | path | `./reports` | Where reports are written |
| `formats` | list[enum] | `[csv, json, markdown]` | Output formats |
| `csv_delimiter` | string | `","` | CSV delimiter |
| `csv_encoding` | string | `utf-8` | CSV encoding |
| `anonymize_enabled` | bool | false | Replace identities with `dev_N` IDs |

### `cache` — CacheConfig

| Field | Type | Default |
|-------|------|---------|
| `directory` | path | `~/.tga-cache` |
| `ttl_hours` | u32 | 168 (7 days) |
| `max_size_mb` | u32 | 1024 |

### `jira` — JIRAConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `access_user` | string | env `JIRA_USER` | JIRA API username (email for Cloud) |
| `access_token` | string | env `JIRA_TOKEN` | JIRA API token |
| `base_url` | url | required if JIRA used | JIRA instance base URL |
| `ticket_regex` | string | None | Override regex for detecting JIRA ticket refs (e.g. `([A-Z]+-\d+)`) in commit messages. Added in v1.0.6 (#75). |

### `jira_integration` — JIRAIntegrationConfig

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | false |
| `fetch_story_points` | bool | true |
| `project_keys` | list[string] | [] |
| `story_point_fields` | list[string] | `["customfield_10016"]` |

### `linear` — LinearConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `api_key` | string | env `LINEAR_API_KEY` | Linear API key |
| `team_id` | string | None | Limit to a specific Linear team |
| `ticket_regex` | string | None | Override regex for detecting Linear ticket refs (e.g. `([A-Z]+-\d+)`) in commit messages. Added in v1.0.6 (#75). |

### `pm.azure_devops` — AzureDevOpsConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `organization_url` | string | required | ADO org URL (e.g. `https://dev.azure.com/myorg`) |
| `pat` | string | required | Azure DevOps Personal Access Token |
| `project` | string | None | Default ADO project name |
| `fetch_on_reference` | bool | false | Fetch work items when `AB#N` refs appear in commits |
| `fetch_prs` | bool | false | Fetch ADO pull requests and reviewer data into `pull_requests` + `pr_reviewers` tables. Added in v1.0.6 (#84). |
| `ticket_regex` | string | `AB#(\d+)` | Override regex for detecting ADO work item refs in commit messages. Must contain a capture group. Added in v1.0.6 (#75). |

### `jira_project_mappings`

`dict<string,string>` — JIRA project key (uppercase) → change_type. Used in classification
Tier 3. Example:

```yaml
jira_project_mappings:
  PLAT: platform
  SEC: security
  DOC: documentation
```

### `taxonomy_mapping`

`dict<string,string>` — change_type → work_type custom remap. Applied as a SQL UPDATE pass
after classification. Example:

```yaml
taxonomy_mapping:
  feature: product_work
  bugfix: maintenance_work
  platform: platform_work
```

### `teams` — TeamsConfig

| Field | Type | Description |
|-------|------|-------------|
| `definitions` | dict[string, list[string]] | Team name → list of canonical IDs / emails |

### `velocity` — VelocityConfig

| Field | Type | Default |
|-------|------|---------|
| `cycle_time_min_hours` | f64 | 0.5 |
| `cycle_time_max_hours` | f64 | 720.0 |

### `activity_scoring` — ActivityScoringConfig

Weights must sum to 1.0:

| Field | Type | Default |
|-------|------|---------|
| `commits_weight` | f64 | 0.22 |
| `prs_weight` | f64 | 0.26 |
| `code_impact_weight` | f64 | 0.26 |
| `complexity_weight` | f64 | 0.11 |
| `ticketing_weight` | f64 | 0.15 |

### `boilerplate_filter` — BoilerplateFilterConfig

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | false |
| `avg_lines_per_commit_threshold` | u32 | 500 |
| `total_lines_threshold` | u32 | 10000 |
| `action` | enum | `flag` | `flag` / `exclude_from_averages` / `exclude` |

### `quality_report` — QualityReportConfig

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | true |
| `revert_patterns` | list[regex] | `["^revert", "rollback", "hotfix"]` |
| `min_revision_warning` | u32 | 3 |

### `ai_detection` — AIDetectionConfig

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | false |
| `confidence_threshold` | f64 | 0.7 |
| `signals` | list[enum] | all |

### `github_issues` — GitHubIssuesConfig

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | true |
| `fetch_closed` | bool | true |
| `lookback_days` | u32 | 365 |

### `confluence` — ConfluenceConfig

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | false |
| `base_url` | url | None |
| `access_user` | string | env `CONFLUENCE_USER` |
| `access_token` | string | env `CONFLUENCE_TOKEN` |
| `space_keys` | list[string] | [] |

## Complete Example

```yaml
repositories:
  - name: backend-api
    path: ~/code/backend-api
    github_repo: acme/backend-api
    project_key: API
  - name: frontend-app
    path: ~/code/frontend-app
    github_repo: acme/frontend-app
    project_key: WEB

github:
  token: ${GITHUB_TOKEN}
  organization: acme
  fetch_pr_reviews: true

jira:
  base_url: https://acme.atlassian.net
  access_user: ${JIRA_USER}
  access_token: ${JIRA_TOKEN}

jira_integration:
  enabled: true
  project_keys: [API, WEB, PLAT]

jira_project_mappings:
  PLAT: platform
  SEC: security

taxonomy_mapping:
  feature: product_work
  platform: platform_work

analysis:
  exclude_authors:
    - "dependabot[bot]@users.noreply.github.com"
  exclude_paths:
    - "**/node_modules/**"
    - "**/__generated__/**"
  branch_analysis:
    strategy: smart
    active_days: 90
  llm_classification:
    enabled: true
    provider: openrouter
    model: mistralai/mistral-7b-instruct

output:
  directory: ./reports
  formats: [csv, json, markdown]

cache:
  directory: ~/.tga-cache
  ttl_hours: 168
```
