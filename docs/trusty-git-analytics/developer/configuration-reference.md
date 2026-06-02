# Configuration Reference

`tga` reads a YAML configuration file (`config.yaml` by default) that controls which
repositories to analyze, which external integrations to connect, and how classification
and reporting behave.

---

## Table of Contents

1. [File Location and Loading](#1-file-location-and-loading)
2. [Environment Variable Fallbacks](#2-environment-variable-fallbacks)
3. [Annotated Example Config](#3-annotated-example-config)
4. [Section Reference](#4-section-reference)
   - [database](#database)
   - [repositories](#repositories)
   - [output](#output)
   - [classification](#classification)
   - [github](#github)
   - [bitbucket](#bitbucket)
   - [jira](#jira)
   - [linear](#linear)
   - [pm.azure_devops](#pmazure_devops)
   - [team](#team)
   - [developer_aliases](#developer_aliases)
   - [cache](#cache)
5. [Special Notes](#5-special-notes)
6. [Validation Rules](#6-validation-rules)
7. [Multiple Repositories Example](#7-multiple-repositories-example)
8. [Full LLM Config Example](#8-full-llm-config-example)

---

## 1. File Location and Loading

By default `tga` looks for `config.yaml` in the current working directory. Use `--config`
to specify a different path:

```bash
tga analyze --config /etc/tga/production.yaml
```

Unknown YAML keys are silently ignored. This means a config file written for a newer
version of `tga` (or for the Python `gitflow-analytics` predecessor) loads without error
in older binaries.

### Database path resolution

`tga` uses a SQLite database (default: `tga.db`). Database path precedence:

1. `--database` CLI flag (highest priority).
2. `database:` key in the YAML config file.
3. Default `tga.db` (lowest priority).

**Important — relative path anchoring:** relative paths in options 2 and 3 are always
resolved relative to the **config file's directory**, not to the process working
directory. This is intentional: cron jobs and launchd services often run with cwd
set to `/` or some unrelated directory, so cwd-relative resolution would silently
open or create a ghost database at the wrong path.

Absolute paths (starting with `/`) and tilde-prefixed paths (`~/…`) are passed
through unchanged.

For production cron/launchd deployments, using an absolute path is the safest option:

```yaml
database: /var/data/tga.db
```

The `--database` CLI flag is resolved as-is (the shell expands `~` and relative paths
before passing them to `tga`).

---

## 2. Environment Variable Fallbacks

The following config fields can be omitted when the corresponding environment variable
is set. The environment variable takes precedence over the config file value.

| Config field | Environment variable |
|---|---|
| `github.token` | `GITHUB_TOKEN` |
| `bitbucket.app_password` | `BITBUCKET_APP_PASSWORD` |
| `bitbucket.token` | `BITBUCKET_TOKEN` |
| `classification.openrouter_api_key` | `OPENROUTER_API_KEY` |
| Logging verbosity | `RUST_LOG` (overrides `--log` and `-v`) |

Note: `tga` does not perform `${VAR}` interpolation inside YAML values. Either set the
field to the literal value, or rely on the environment variable fallback (where supported).

---

## 3. Annotated Example Config

```yaml
# ── Repositories (required) ─────────────────────────────────────────────────
repositories:
  - path: ~/code/backend-api      # Local git clone path; ~ is expanded
    name: backend-api             # Display name in reports (default: directory basename)
    branch: main                  # Branch to walk (default: auto-detect default branch)
    since_date: "2025-01-01"      # Ignore commits before this date (ISO 8601)
    until_date: "2025-12-31"      # Ignore commits after this date (ISO 8601)
    org: my-github-org            # GitHub org/owner for PR correlation

# ── Output ──────────────────────────────────────────────────────────────────
output:
  directory: ./reports            # Where report files are written; ~ is expanded
  formats:                        # Formats to produce
    - csv
    - json
    - markdown
  include_unclassified: false     # Include unclassified commits in output
  include_merges: false           # Include merge commits in output
  include_files: false            # Include file-level rows in output

# ── Classification ───────────────────────────────────────────────────────────
classification:
  rules_file: ./my-rules.yaml     # Custom rules YAML; extend or replace built-ins
  use_llm: false                  # Enable LLM fallback tier
  llm_model: "anthropic/claude-3.5-haiku"  # Model to use for LLM classification
  llm_provider: auto              # openrouter | openai | bedrock | auto
  openrouter_api_key: ""          # Or set OPENROUTER_API_KEY env var
  confidence_threshold: 0.7       # Minimum confidence to accept a result [0, 1]
  min_coverage_pct: 20.0          # Warn if fewer than this % of commits classified [0, 100]
  llm_fallback_threshold: 0.0     # Commits with confidence above this skip LLM entirely
  llm_fallback_concurrency: 8     # Max concurrent LLM requests during fallback pass

# ── GitHub ──────────────────────────────────────────────────────────────────
github:
  token: ""                       # Or set GITHUB_TOKEN env var
  org: my-github-org              # GitHub organization/owner
  repo: my-repo                   # Repository name (if single-repo focus)
  fetch_prs: false                # Fetch PR metadata from GitHub API
  ticket_regex: ""                # Custom regex for ticket detection; capture group 1 = ID

# ── Bitbucket ───────────────────────────────────────────────────────────────
bitbucket:
  username: ""                    # Bitbucket account username (Basic auth)
  app_password: ""                # Or set BITBUCKET_APP_PASSWORD env var
  token: ""                       # Or set BITBUCKET_TOKEN env var (Bearer; takes precedence)
  workspace: ""                   # Required when fetch_prs: true
  repo_slug: ""                   # Required when fetch_prs: true
  fetch_prs: false
  api_base_url: "https://api.bitbucket.org/2.0"

# ── JIRA ────────────────────────────────────────────────────────────────────
jira:
  url: https://company.atlassian.net
  username: user@company.com
  token: ""                       # JIRA API token
  project_key: ENG                # Filter JIRA issues by project key
  ticket_regex: ""                # Custom regex; capture group 1 = ticket ID

# ── Linear ──────────────────────────────────────────────────────────────────
linear:
  api_key: ""
  team_keys:                      # Linear team identifiers to fetch
    - ENG
    - FE
  fetch_on_reference: true        # Fetch issue details when ref appears in a commit
  ticket_regex: ""                # Custom regex; capture group 1 = ticket ID

# ── Azure DevOps ─────────────────────────────────────────────────────────────
pm:
  azure_devops:
    organization_url: https://dev.azure.com/myorg   # Required
    pat: ""                       # Azure DevOps Personal Access Token; required
    project: MyProject            # Default ADO project (or use `projects` list)
    projects:                     # List of ADO projects (alternative to single `project`)
      - MyProject
      - InfraTeam
    ticket_regex: '(?i)\bAB#(\d+)\b'   # Default; change capture group 1 = work item ID
    team_keys: []                 # ADO team keys to scope fetching
    fetch_on_reference: true      # Fetch work items when AB#N appears in commits
    fetch_prs: false              # Fetch ADO pull requests and reviewer data

# ── Team members ─────────────────────────────────────────────────────────────
team:
  members:
    - name: Alice Park
      email: alice.park@company.com
      aliases:
        - aparks@gmail.com
        - alice-park-github
  aliases:                        # Free-form alias → canonical name map
    "aparks": "Alice Park"

# ── Developer aliases (alternative format) ───────────────────────────────────
# Use developer_aliases OR team.members, not both.
developer_aliases:
  "John Doe":
    - "john.doe@company.com"
    - "jdoe@gmail.com"

# ── External aliases file ────────────────────────────────────────────────────
# Mutually exclusive with inline developer_aliases above.
# aliases_file: ~/config/tga-aliases.yaml

# ── Cache ────────────────────────────────────────────────────────────────────
cache:
  directory: ~/.tga-cache         # ~ is expanded
```

---

## 4. Section Reference

### database

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `database` | PathBuf | no | `tga.db` (config dir) | Path to the SQLite database. Supports `~`. Relative paths anchor to the config file's directory. |

See [Database path resolution](#database-path-resolution) above for the full
precedence rules and anchoring behaviour.

### repositories

Required. A list of one or more repository entries.

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `path` | PathBuf | yes | — | Filesystem path to the git repo. Supports `~` expansion. |
| `name` | String | no | Directory basename | Display name used in reports and DB records. |
| `branch` | String | no | Auto-detect | Branch to walk. If omitted, `tga` uses the repo's current HEAD branch. |
| `since_date` | String | no | — | Skip commits before this date (ISO 8601: `YYYY-MM-DD`). |
| `until_date` | String | no | — | Skip commits after this date (ISO 8601: `YYYY-MM-DD`). |
| `org` | String | no | — | GitHub org or owner for correlating commits to PRs. |

`since_date` / `until_date` in the repository config set a permanent floor/ceiling for
that repo. For per-run date windowing, use the CLI flags `--weeks`, `--from`, `--to`.

### output

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `directory` | PathBuf | no | `./reports` | Output directory. Alias: `output_path`. Supports `~`. |
| `formats` | Vec\<String\> | no | `[csv, json, markdown]` | Formats to produce. Valid values: `csv`, `json`, `markdown`. |
| `include_unclassified` | bool | no | `false` | When `true`, commits that were not classified appear in output. |
| `include_merges` | bool | no | `false` | When `true`, merge commits are included in output and metrics. |
| `include_files` | bool | no | `false` | When `true`, file-level change rows are included in output. |

### classification

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `rules_file` | PathBuf | no | — | Path to a custom rules YAML file. Supports `~`. |
| `use_llm` | bool | no | `false` | Enable the LLM fallback tier (Tier 3). |
| `llm_model` | String | no | Provider-specific default | LLM model identifier (e.g. `anthropic/claude-3.5-haiku` for OpenRouter). |
| `llm_provider` | String | no | `auto` | LLM provider: `openrouter`, `openai`, `bedrock`, or `auto`. `auto` prefers OpenRouter when `OPENROUTER_API_KEY` is set. |
| `openrouter_api_key` | String | no | `$OPENROUTER_API_KEY` | OpenRouter API key. Env var takes precedence. |
| `confidence_threshold` | f64 | no | `0.7` | Minimum confidence score [0, 1] to accept a classification result. |
| `custom_categories` | Vec | no | — | Additional subcategory definitions. |
| `min_coverage_pct` | f64 | no | `20.0` | Emit a warning in the report when fewer than this percentage of commits are classified [0, 100]. |
| `llm_fallback_threshold` | f64 | no | `0.0` | Commits with a rule-based confidence above this value skip the LLM tier entirely. Set to e.g. `0.5` to avoid sending already-confident results to the LLM. |
| `llm_fallback_concurrency` | usize | no | `8` | Maximum concurrent LLM requests during the fallback pass. |

### github

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `token` | String | no | `$GITHUB_TOKEN` | GitHub Personal Access Token. Required for PR fetching. |
| `org` | String | no | — | GitHub organization or owner. |
| `repo` | String | no | — | Repository name for single-repo configurations. |
| `fetch_prs` | bool | no | `false` | When `true`, fetch pull request metadata from the GitHub API during `collect`. |
| `ticket_regex` | String | no | — | Custom regex for GitHub ticket detection in commit messages. Capture group 1 must contain the ticket ID. |

### bitbucket

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `username` | String | no | — | Bitbucket account username (required for Basic auth). |
| `app_password` | String | no | `$BITBUCKET_APP_PASSWORD` | Bitbucket App Password (Basic auth). |
| `token` | String | no | `$BITBUCKET_TOKEN` | Workspace or repository access token (Bearer auth). Takes precedence over `app_password`. |
| `workspace` | String | conditional | — | Workspace slug. Required when `fetch_prs: true`. |
| `repo_slug` | String | conditional | — | Repository slug. Required when `fetch_prs: true`. |
| `fetch_prs` | bool | no | `false` | Fetch PR metadata from Bitbucket API. |
| `api_base_url` | String | no | `https://api.bitbucket.org/2.0` | API base URL override. |

### jira

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `url` | String | conditional | — | JIRA instance base URL (e.g. `https://company.atlassian.net`). Required when using JIRA integration. |
| `username` | String | no | — | JIRA account email. |
| `token` | String | no | — | JIRA API token. |
| `project_key` | String | no | — | Filter JIRA issues to this project key. |
| `jira_project_mappings` | HashMap | no | — | Map of project key → work type for classification hints. |
| `ticket_regex` | String | no | — | Custom regex. Capture group 1 = ticket ID. |

### linear

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `api_key` | String | no | — | Linear API key. |
| `team_keys` | Vec\<String\> | no | — | Linear team identifiers to scope issue fetching. |
| `fetch_on_reference` | bool | no | `true` | When `true`, fetch issue details from Linear whenever a ticket reference is detected in a commit message. |
| `ticket_regex` | String | no | — | Custom regex. Capture group 1 = ticket ID. |

### pm.azure_devops

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `organization_url` | String | yes | — | ADO organization URL (e.g. `https://dev.azure.com/myorg`). |
| `pat` | String | yes | — | Azure DevOps Personal Access Token. |
| `project` | String | conditional | — | Default ADO project. Either `project` or `projects` must be provided. |
| `projects` | Vec\<String\> | conditional | — | List of ADO projects. Alternative to `project`. |
| `ticket_regex` | String | no | `(?i)\bAB#(\d+)\b` | Regex for detecting ADO work item references. Capture group 1 = work item ID. |
| `team_keys` | Vec\<String\> | no | — | ADO team keys for scoping. |
| `fetch_on_reference` | bool | no | `true` | Fetch work item details when an `AB#N` reference is detected. |
| `fetch_prs` | bool | no | `false` | Fetch ADO pull requests and reviewer votes. |

### team

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `members` | Vec | no | — | List of team members with explicit identity mappings. |
| `members[].name` | String | yes | — | Canonical display name. |
| `members[].email` | String | yes | — | Primary email address. |
| `members[].aliases` | Vec\<String\> | no | — | Additional emails, GitHub handles, or other identifiers. |
| `aliases` | HashMap | no | — | Free-form alias-to-canonical-name lookup. |

### developer_aliases

Alternative to `team.members`. Maps canonical name to a list of email aliases:

```yaml
developer_aliases:
  "John Doe":
    - "john.doe@company.com"
    - "jdoe@gmail.com"
```

The first email-shaped entry in each list is used as the canonical email.

Use either `developer_aliases` or `team.members`, not both. For teams with more than
20 developers, prefer the external file approach:

```yaml
aliases_file: ~/config/tga-aliases.yaml
```

### cache

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `directory` | PathBuf | no | `~/.tga-cache` | Cache directory. Supports `~` expansion. |

---

## 5. Special Notes

### Path expansion

`~` is expanded to the current user's home directory in the following fields:
`repositories[].path`, `aliases_file`, `cache.directory`, `output.directory`.

### ticket_regex capture group

When setting a custom `ticket_regex` for any integration (GitHub, JIRA, Linear, ADO),
the regex must contain a capture group 1 that matches the ticket identifier. For example:

```yaml
# Correct: capture group 1 captures "ENG-123"
ticket_regex: '([A-Z]+-\d+)'

# Correct: ADO default pattern
ticket_regex: '(?i)\bAB#(\d+)\b'

# Incorrect: no capture group
ticket_regex: '[A-Z]+-\d+'
```

### LLM provider auto-detection

When `llm_provider: auto` (the default), `tga` selects a provider at runtime:
- If `OPENROUTER_API_KEY` is set (or `openrouter_api_key` is in config): use OpenRouter
- Otherwise: use OpenAI with `OPENAI_API_KEY`

Set `llm_provider` explicitly to `openrouter`, `openai`, or `bedrock` to override this.

---

## 6. Validation Rules

`tga` validates the following at startup and will refuse to run if these are violated:

| Rule | Condition |
|---|---|
| `confidence_threshold` | Must be in the range [0, 1] |
| `min_coverage_pct` | Must be in the range [0, 100] |
| `pm.azure_devops` (if present) | Either `project` or `projects` must be non-empty |
| `bitbucket` (if `fetch_prs: true`) | Both `workspace` and `repo_slug` must be set |
| `repositories` | Must contain at least one entry |

---

## 7. Multiple Repositories Example

```yaml
repositories:
  - path: ~/code/backend-api
    name: backend-api
    branch: main
    org: acme-corp

  - path: ~/code/frontend-app
    name: frontend-app
    branch: main
    org: acme-corp

  - path: ~/code/infra
    name: infra
    branch: master
    org: acme-corp

github:
  token: ""          # Set GITHUB_TOKEN
  org: acme-corp
  fetch_prs: true

output:
  directory: ./reports
  formats: [csv, json, markdown]
```

Each repository produces its own per-repo metrics while also contributing to the
aggregated cross-repo summaries.

---

## 8. Full LLM Config Example with OpenRouter

```yaml
repositories:
  - path: ~/code/my-service
    name: my-service

classification:
  rules_file: ./rules.yaml
  use_llm: true
  llm_provider: openrouter
  llm_model: "anthropic/claude-3.5-haiku"
  openrouter_api_key: ""   # Or set OPENROUTER_API_KEY env var
  confidence_threshold: 0.7
  min_coverage_pct: 20.0
  # Commits already classified with confidence > 0.5 skip the LLM entirely.
  # This avoids unnecessary API calls for well-matched rule results.
  llm_fallback_threshold: 0.5
  # Allow up to 8 concurrent LLM requests to speed up the fallback pass.
  llm_fallback_concurrency: 8

output:
  directory: ./reports
  formats: [csv, json, markdown]
```

When `llm_provider: auto` and `OPENROUTER_API_KEY` is set in the environment, `tga`
will also use OpenRouter without requiring explicit `llm_provider` in the config.
