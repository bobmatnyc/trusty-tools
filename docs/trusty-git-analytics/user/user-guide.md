# User Guide

`trusty-git-analytics` (`tga`) is a git productivity analytics tool. It extracts commit
history from one or more local git repositories, classifies each commit by work type, and
produces CSV, JSON, and Markdown reports you can use to understand how engineering time
is being spent week over week.

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [Installation](#2-installation)
3. [Quick Start](#3-quick-start)
4. [Common Workflows](#4-common-workflows)
5. [Understanding Output](#5-understanding-output)
6. [Managing Developer Identities](#6-managing-developer-identities)
7. [Manual Classification Overrides](#7-manual-classification-overrides)
8. [Maintenance](#8-maintenance)
9. [Troubleshooting](#9-troubleshooting)

---

## 1. Introduction

`tga` runs a three-stage pipeline:

1. **Collect** — walks git commit history, optionally fetches pull request metadata from
   GitHub, Bitbucket, Azure DevOps, or ticket data from JIRA/Linear, and stores everything
   in a local SQLite database (`tga.db`).
2. **Classify** — assigns each commit a work type using a four-tier cascade: exact keyword
   rules, regex rules, fuzzy/structural rules, and an optional LLM fallback.
3. **Report** — aggregates the classified data into a set of CSV, JSON, and Markdown files
   that describe commit volumes, work-type breakdowns, PR cycle times, and more.

**What it produces per report run**: 9 CSV files, 4 JSON files, and 1 Markdown summary.

---

## 2. Installation

### Option A: cargo install (recommended)

```bash
cargo install tga
```

Requires a Rust stable toolchain. Install Rust from [rustup.rs](https://rustup.rs) if
you don't have one. The binary is placed in `~/.cargo/bin/tga` — ensure that directory
is on your `PATH`.

### Option B: Build from source

```bash
git clone https://github.com/bobmatnyc/trusty-git-analytics
cd trusty-git-analytics
cargo build --release
# Binary is at: target/release/tga
cp target/release/tga /usr/local/bin/tga
```

### Option C: Pre-built binaries

Pre-built binaries for macOS (x86_64 and aarch64), Linux (x86_64), and Windows (x86_64)
are published on the
[GitHub Releases page](https://github.com/bobmatnyc/trusty-git-analytics/releases).
Download the binary for your platform, make it executable, and place it on your `PATH`:

```bash
# Example for macOS arm64
chmod +x tga-aarch64-apple-darwin
mv tga-aarch64-apple-darwin /usr/local/bin/tga
```

### Verify installation

```bash
tga --version
tga --help
```

No runtime dependencies are required. SQLite is bundled; no system Python, libgit2, or
OpenSSL is needed.

---

## 3. Quick Start

Five steps from zero to your first report.

### Step 1: Install tga

See [Installation](#2-installation) above.

### Step 2: Run the setup wizard

```bash
cd /path/to/your/reports/directory
tga install
```

The wizard prompts for:
- Path(s) to your local git repositories
- GitHub personal access token (optional — for PR metadata)
- JIRA credentials (optional)
- Output directory for reports
- LLM provider for classification (optional)

The wizard writes `config.yaml` in the current directory.

### Step 3: Review config.yaml

Open `config.yaml` and confirm the repository paths and any credentials look correct.
See the [Configuration Reference](../developer/configuration-reference.md) for all available options.

### Step 4: Run the full pipeline

```bash
tga analyze --weeks 12
```

This collects the last 12 weeks of commits, classifies them, and writes reports to the
output directory specified in your config (default: `./reports`).

### Step 5: Read your reports

```
reports/
├── commit_summary.csv
├── developer_summary.csv
├── weekly_trends.csv
├── ... (9 CSV files total)
├── summary.json
├── developer_metrics.json
├── ... (4 JSON files total)
└── report.md
```

Open `report.md` for a human-readable summary, or import the CSV files into your
analytics tool of choice.

---

## 4. Common Workflows

### Full pipeline run

Run the full collect → classify → report pipeline for the last 12 weeks:

```bash
tga analyze --weeks 12
```

### Incremental weekly run (cron)

Add new data for the past week without re-collecting already-processed history:

```bash
tga analyze --weeks 1
```

`tga` tracks collection state per (repository, ISO year, ISO week). Weeks already
collected are skipped automatically, so this is safe to run on a schedule.

Example cron (runs every Monday at 8 AM):

```
0 8 * * 1 cd /path/to/workdir && tga analyze --weeks 1 --config config.yaml
```

### Dry run to test config

Verify your config is valid and see what would be collected, without writing to the
database:

```bash
tga analyze --dry-run
```

### Run stages individually

You can run each stage separately. This is useful when you want to collect once but
experiment with different classification settings:

```bash
# Stage 1: collect git data
tga collect --weeks 12

# Stage 2: classify commits
tga classify

# Stage 3: generate reports
tga report --output ./reports
```

### Skip collection when data is fresh

If you've already run `tga collect` recently and only want to re-run classification
and reporting (for example, after tuning your rules file):

```bash
tga analyze --skip-collect
```

### Date range analysis

Analyze a specific calendar period:

```bash
tga analyze --from 2025-01-01 --to 2025-03-31
```

### Re-run classification only

Re-classify all commits for the last 8 weeks (useful after updating your rules file):

```bash
tga classify --weeks 8
```

### Enable LLM classification and backfill complexity scores

To classify commits that rules couldn't handle, enable the LLM tier. After initial
LLM classification, you can backfill the 1–5 complexity score for all commits:

```bash
tga classify --use-llm
tga classify --backfill-complexity
```

`--backfill-complexity` populates complexity scores for commits that already have a
classification but no complexity score — it does not re-run the full classification
cascade.

### View PR metrics

Show pull request metrics for the last 8 weeks:

```bash
tga pr-metrics --weeks 8
```

### Export PR metrics to CSV

```bash
tga pr-metrics --weeks 8 --csv --output pr-report.csv
```

The PR metrics table contains one row per author with columns:
`author`, `prs_opened`, `prs_merged`, `pr_comments_given`, `merge_rate`,
`avg_cycle_time_hours`, `avg_revisions`.

Note: `pr_comments_given` and `avg_revisions` are not yet implemented and will show 0.

---

## 5. Understanding Output

Each `tga report` run (or `tga analyze`) writes the following files to the output
directory.

### CSV files (9 total)

| File | Contents |
|------|----------|
| `commit_summary.csv` | One row per commit: SHA, author, date, repository, classification, confidence, work type |
| `developer_summary.csv` | Per-developer totals: commit count, lines added/deleted, classification breakdown |
| `weekly_trends.csv` | Per-developer per-ISO-week commit and line counts |
| `work_type_breakdown.csv` | Commit counts grouped by top-level work type (Feature, Bugfix, KTLO, etc.) |
| `classification_detail.csv` | Detailed classification results including subcategory, confidence, and method used |
| `ticketed_commits.csv` | Commits where a ticket reference was detected (JIRA, Linear, GitHub, ADO) |
| `pr_summary.csv` | One row per pull request: number, title, author, state, merged date, cycle time |
| `weekly_dora_metrics.csv` | Per-ISO-week DORA metrics (lead time, deployment frequency) |
| `unclassified_commits.csv` | Commits that fell through all tiers without a classification (present only if `include_unclassified: true` in config) |

### JSON files (4 total)

| File | Contents |
|------|----------|
| `summary.json` | Overall run metadata: date range, repository list, total commits, classification coverage percentage |
| `developer_metrics.json` | Per-developer structured metrics with nested work type breakdowns |
| `dora_summary.json` | DORA metric aggregates across the full report period |
| `classification_stats.json` | Classification method distribution (what fraction used exact rules vs. regex vs. LLM) |

### Markdown file (1)

`report.md` — A narrative summary of the period: total commits, work type distribution
table, top contributors, classification coverage, and any coverage warnings.

---

## 6. Managing Developer Identities

The same engineer often commits under multiple names and email addresses (work email,
personal email, GitHub handle, etc.). `tga` resolves these to canonical identities using
a combination of exact alias matching and Jaro-Winkler fuzzy matching.

### List canonical identities

```bash
tga aliases list
```

### Merge two identities

If `tga` created two separate canonical entries for the same person, merge them:

```bash
# Merge "jdoe-github" into "John Doe" (keeps "John Doe")
tga aliases merge "jdoe-github" "John Doe"

# Skip the confirmation prompt
tga aliases merge "jdoe-github" "John Doe" --yes
```

### Define aliases in config.yaml

For deterministic identity resolution, declare aliases explicitly in `config.yaml`:

```yaml
developer_aliases:
  "John Doe":
    - "john.doe@company.com"
    - "jdoe@gmail.com"
    - "john-doe-github"

  "Jane Smith":
    - "jane.smith@company.com"
    - "jsmith@personal.com"
```

The first email-like entry in each list is used as the canonical email address.

### Use an external aliases file

For teams with many developers, keep aliases in a separate YAML file:

```yaml
# config.yaml
aliases_file: "~/config/tga-aliases.yaml"
```

```yaml
# tga-aliases.yaml
developers:
  - name: "John Doe"
    primary_email: "john.doe@company.com"
    aliases:
      - "jdoe@gmail.com"
      - "john-doe-github"
```

The external file supports `~` path expansion. It can be kept under version control
separately from your config.

---

## 7. Manual Classification Overrides

If the automatic classification for a specific commit is wrong and you want to fix it
permanently, use the override system (Tier 0). Override entries take priority over all
rule-based and LLM classifications.

### Add an override

```bash
tga override add <SHA> <WORK_TYPE> <CHANGE_TYPE>
```

Example:

```bash
tga override add abc1234 feature new-feature --notes "Correctly a feature, not a refactor"
```

To scope the override to a specific repository (useful when the same SHA appears in
multiple repos):

```bash
tga override add abc1234 bugfix hotfix --repo my-service
```

### List overrides

```bash
tga override list

# Scope to a specific repository
tga override list --repo my-service
```

### Remove an override

```bash
tga override remove abc1234

# Skip the confirmation prompt
tga override remove abc1234 --yes
```

---

## 8. Maintenance

### Backfill AI detection confidence

After tuning your `confidence_threshold`, clear all low-confidence LLM classifications
so they will be re-processed on the next `tga classify` run:

```bash
tga backfill ai-detection
```

This removes classification entries where the LLM confidence was below 0.7, leaving
the commits unclassified so the cascade will retry them.

### Backfill revert flags

If you updated your revert-detection patterns, rescan all commit messages to update
the `is_revert` flag:

```bash
tga backfill revert-flags
```

### Backfill ticket IDs

Rescan all commit messages and update `ticket_id` and the `ticketed` boolean for any
commits where ticket detection logic has changed:

```bash
tga backfill ticket-ids
```

All `tga backfill` subcommands support `--dry-run` to preview changes without writing
to the database.

---

## 9. Troubleshooting

### No commits found

**Symptom**: `tga collect` reports 0 commits.

**Checks**:
1. Verify the `path` in your config points to a valid git repository:
   ```bash
   git -C /your/repo/path log --oneline -5
   ```
2. Confirm the date range includes commits. Try `--weeks 52` for a wider window.
3. Check the branch setting. If `branch` is set in config, ensure that branch exists:
   ```bash
   git -C /your/repo/path branch -a
   ```
4. Run with `-v` to see the revwalk range:
   ```bash
   tga collect --weeks 4 -v
   ```

### Classification coverage is low

**Symptom**: `report.md` shows less than 20% of commits classified.

**Fixes**:
1. Add a custom rules file targeting your team's commit message conventions:
   ```yaml
   # config.yaml
   classification:
     rules_file: "./my-rules.yaml"
   ```
2. Enable LLM classification for commits the rules miss:
   ```yaml
   classification:
     use_llm: true
   ```
3. Run `tga backfill ai-detection` if you recently added rules, then re-classify.

### LLM classification is not firing

**Symptom**: `use_llm: true` is set but the `classification_stats.json` shows 0 LLM
classifications.

**Checks**:
1. Confirm your API key is set. For OpenRouter:
   ```bash
   echo $OPENROUTER_API_KEY
   ```
   Or set it in config: `classification.openrouter_api_key: "sk-or-..."` <!-- pragma: allowlist secret -->
   (the `sk-or-...` shown here is a placeholder, not a real key)
2. Check the `llm_provider` setting. Default is `auto`, which prefers OpenRouter when
   `OPENROUTER_API_KEY` is present, otherwise falls back to OpenAI.
3. Run with `-vv` to see LLM request/response logging:
   ```bash
   tga classify --use-llm -vv
   ```

### Date range returns unexpected data

**Symptom**: Results include commits outside the expected date range.

**Notes**:
- `--weeks N` counts ISO weeks backward from the current date. For example, `--weeks 1`
  covers the current ISO week (Monday through Sunday), which may span the previous
  calendar month.
- `--from` and `--to` are inclusive date boundaries in `YYYY-MM-DD` format.
- `--weeks` takes priority over `--from`/`--to`. If both are supplied, `--weeks` wins.

### Git fetch fails on collect

**Symptom**: `tga collect` logs a fetch warning but continues.

`tga` runs `git fetch origin` before each repository revwalk. Authentication is
non-interactive (SSH agent, then default key files). If the fetch fails, collection
continues using local refs — you won't miss commits that are already present locally.

To skip fetching entirely (offline mode or when CI has already fetched):

```bash
tga collect --no-fetch
```

### "no repositories matched --repos filter"

Repository names come from `repositories[].name` in config, defaulting to the directory
basename of `path`. Check configured names and adjust your `--repos` filter to match:

```bash
grep -A3 'repositories:' config.yaml
```

### Getting more diagnostic output

```bash
# Info-level (collection progress, file counts)
tga analyze -v

# Debug-level (per-commit classification decisions)
tga analyze -vv

# Trace-level (raw HTTP requests/responses)
tga analyze -vvv

# Per-module level control
RUST_LOG=tga::classify=debug,warn tga classify
```
