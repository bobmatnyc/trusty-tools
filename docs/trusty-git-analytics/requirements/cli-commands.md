# CLI Commands

The binary is `tga` (built from the `tga-cli` crate). A `gitflow-analytics` symlink or alias
is recommended for compatibility with scripts targeting the Python predecessor.

```
tga <SUBCOMMAND> [FLAGS]
```

Global flags:

| Flag | Type | Description |
|------|------|-------------|
| `--config <PATH>` / `-c` | path | Path to config YAML (default: `./config.yaml`) |
| `--database <PATH>` / `-d` | path | Path to SQLite database (default: `./tga.db`) |
| `--log <LEVEL>` | enum | `error` / `warn` / `info` / `debug` / `trace` (default: `warn`). Overrides `-v`. |
| `-v` / `-vv` / `-vvv` | count | Verbosity shortcut: `info` / `debug` / `trace` |
| `--help` | | Print help |
| `--version` | | Print version |

The `RUST_LOG` environment variable, when set, takes precedence over both
`--log` and `-v` (supports the standard `tracing-subscriber` `EnvFilter`
syntax, e.g. `RUST_LOG=tga::collect=debug,warn`).

## ISO Week Targeting

Three mutually-exclusive selectors for time range:

| Flag | Repeatable | Description |
|------|------------|-------------|
| `--weeks <N>` | no | Look back N ISO weeks from today |
| `--week <YYYY-Www>` | yes | Target one or more specific ISO weeks (e.g. `--week 2025-W12 --week 2025-W14`) |
| `--from <DATE> --to <DATE>` | no | Date range, both required if either used |

Validation: `--week` is mutually exclusive with both `--weeks` and `--from`/`--to`.
`--from`/`--to` must be used together.

---

## Subcommands

### `tga analyze`

Run the full pipeline: collect → classify → report.

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `./config.yaml` | Config file |
| `--weeks <N>` | 4 | Lookback weeks |
| `--week <YYYY-Www>` | — | Specific ISO week (repeatable) |
| `--from <DATE>` | — | Range start (YYYY-MM-DD) |
| `--to <DATE>` | — | Range end (YYYY-MM-DD) |
| `--output <PATH>` | from config | Output directory |
| `--force` | false | Bypass week-level cache immutability |
| `--anonymize` | from config | Override anonymization |
| `--generate-csv` | true | Emit CSV reports |
| `--reclassify` | false | Re-run classification on cached commits |

### `tga collect`

Stage 1 only — extract git data and external APIs into SQLite cache.

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `./config.yaml` | |
| `--weeks <N>` | 4 | |
| `--week <YYYY-Www>` | — | Specific week (repeatable) |
| `--from <DATE>` | — | |
| `--to <DATE>` | — | |
| `--force` | false | Override `weekly_fetch_status` immutability |
| `--log <LEVEL>` | warn | (global) |

### `tga classify`

Stage 2 only — run classification cascade against cached commits.

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `./config.yaml` | |
| `--weeks <N>` | 4 | |
| `--week <YYYY-Www>` | — | |
| `--from <DATE>` | — | |
| `--to <DATE>` | — | |
| `--reclassify` | false | Re-classify previously-classified commits |
| `--log <LEVEL>` | warn | (global) |
| `--show-jira-signals` | false | Emit JIRA signal diagnostics per commit |
| `--validate-coverage` | false | Exit non-zero if coverage below threshold |
| `--coverage-threshold <PCT>` | 20.0 | Minimum classification coverage % |

### `tga report`

Stage 3 only — generate reports from cache.

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `./config.yaml` | |
| `--weeks <N>` | 4 | |
| `--week <YYYY-Www>` | — | |
| `--from <DATE>` | — | |
| `--to <DATE>` | — | |
| `--output <PATH>` | from config | Output directory |
| `--generate-csv` | true | |
| `--anonymize` | from config | |
| `--log <LEVEL>` | warn | (global) |

### `tga fetch`

Fetch external data only (GitHub PRs/issues, JIRA tickets) — no git extraction.

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `./config.yaml` | |
| `--source <SOURCE>` | all | `github` / `jira` / `confluence` / `all` |
| `--weeks <N>` | 4 | |

### `tga aliases`

LLM-based identity alias suggestion / generation.

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `./config.yaml` | |
| `--apply` | false | Apply suggestions to identities.db |
| `--dry-run` | true | Print suggestions only |

### `tga identities`

Identity management subcommands.

#### `tga identities list`

| Flag | Description |
|------|-------------|
| `--config <PATH>` | |
| `--include-aliases` | Show all aliases per canonical |

#### `tga identities merge`

| Flag | Description |
|------|-------------|
| `--config <PATH>` | |
| `--source <ID>` | Canonical ID to merge from |
| `--target <ID>` | Canonical ID to merge into (kept) |

### `tga pr-metrics`

Weekly PR metrics aggregation into `weekly_pr_metrics`.

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `./config.yaml` | |
| `--weeks <N>` | 4 | |
| `--rebuild` | false | Drop existing rows for range before inserting |

### `tga override`

Manage manual classification overrides (Tier 0).

| Flag | Description |
|------|-------------|
| `--config <PATH>` | |
| `--commit <HASH>` | Commit hash |
| `--repo <PATH>` | Repository path |
| `--change-type <TYPE>` | One of 19 change_type values |
| `--work-type <TYPE>` | Optional override |
| `--reason <STRING>` | Required justification |
| `--remove` | Remove existing override |

### `tga install`

Setup wizard. Creates `config.yaml`. On a terminal with no flags it prompts;
given `--host` / `--pm`, or with stdin not a terminal, it runs from flags and
environment variables and never prompts ([#5216](https://github.com/bobmatnyc/trusty-tools/issues/5216)).
With `--host github --org <ORG>` it pages the GitHub API for the org's
repositories and writes them into the generated config.

| Flag | Description |
|------|-------------|
| `--output <PATH>` | Output config path (default `./config.yaml`) |
| `--force` | Overwrite existing config |
| `--non-interactive` | Never prompt; implied when stdin is not a terminal |
| `--host <local\|github\|bitbucket>` | Where repositories come from (required non-interactively) |
| `--org <ORG>` | GitHub org to discover repositories from |
| `--workspace <WORKSPACE>` | Bitbucket Cloud workspace |
| `--repo <OWNER/NAME>` | Explicit remote repository; repeatable |
| `--repo-path <PATH>` | Already-cloned repository; repeatable |
| `--repo-cache <DIR>` | Where remote repositories are expected on disk (default `./repos`) |
| `--host-token <TOKEN>` | Host API token; falls back to `$GITHUB_TOKEN` / `$BITBUCKET_TOKEN` |
| `--pm <none\|github\|jira\|linear>` | PM system supplying work items (required non-interactively) |
| `--jira-url`, `--jira-user`, `--jira-token` | JIRA credentials; fall back to `$JIRA_URL`, `$JIRA_EMAIL`, `$JIRA_API_TOKEN` |
| `--linear-api-key <KEY>` | Linear API key; falls back to `$LINEAR_API_KEY` |
| `--linear-team <TEAM>` | Linear team key to scope issue fetches to; repeatable |
| `--output-dir <DIR>` | Report output directory (default `./tga-output`) |
| `--llm-provider <PROVIDER>` | `none`, `openai` or `openrouter` (default `none`) |
| `--llm-api-key <KEY>` | LLM API key; falls back to the provider's conventional env var |

**Precedence:** a flag value wins over its environment variable. A credential
taken from the environment is written to the config as a `${VAR}` reference,
not as the secret itself.

Run non-interactively with a required flag missing, install names every missing
flag at once rather than blocking on a prompt:

```bash
$ tga install --host github --pm none < /dev/null
Error: `tga install` has no terminal to prompt on and these required flags are missing:
  --org <ORG> (or --repo <OWNER/NAME>, repeatable)
  --host-token <TOKEN> (or set $GITHUB_TOKEN)
```

Bitbucket Cloud workspace-to-repo discovery is not implemented yet
([#5220](https://github.com/bobmatnyc/trusty-tools/issues/5220)), so
`--host bitbucket` requires an explicit `--repo <workspace/slug>` list and
records that limitation as a comment in the generated config.

### `tga help`

Extended help with topic deep-dives.

```
tga help config           # YAML schema cheatsheet
tga help classification   # Cascade explanation
tga help iso-weeks        # ISO week targeting examples
```
