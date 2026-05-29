# Migrating from `gitflow-analytics` (Python) to `trusty-git-analytics` (Rust)

This guide is for users of the Python tool
[`gitflow-analytics`](https://github.com/bobmatnyc/gitflow-analytics)
(invoked as `gfa`) who want to switch to the Rust port `trusty-git-analytics`
(invoked as `tga`).

The Rust port aims for **drop-in compatibility** for the common case: same
YAML config schema, same SQLite schema, same report outputs. Most users
should be able to point `tga` at an existing config file and database and
get equivalent results — significantly faster.

---

## Prerequisites

- **Rust toolchain** (only if building from source): Rust 1.75+ via
  [rustup](https://rustup.rs). The Rust edition is 2021.
- **No system SQLite needed** — `tga` bundles SQLite via the `rusqlite`
  crate's `bundled` feature.
- **No system libgit2 needed** — `git2` is statically linked.
- **OpenSSL not required** — HTTP uses `rustls`.

The binary is fully self-contained and statically links its native
dependencies.

---

## Installation

### Option 1: `cargo install` (recommended)

```bash
cargo install trusty-git-analytics
```

This installs the `tga` binary into `~/.cargo/bin`. Make sure that
directory is on your `PATH`.

### Option 2: Build from source

```bash
git clone https://github.com/bobmatnyc/trusty-git-analytics
cd trusty-git-analytics
cargo build --release
# Binary at ./target/release/tga
```

### Option 3: Pre-built binaries

Pre-built binaries for macOS, Linux, and Windows are published on the
[GitHub Releases page](https://github.com/bobmatnyc/trusty-git-analytics/releases).
Download, `chmod +x`, and move into `PATH`.

### Verify

```bash
tga --version
tga --help
```

---

## Configuration Compatibility

`tga` accepts the same YAML schema as `gitflow-analytics`. In most cases
you can copy `config.yaml` over unchanged.

### Compatible fields (no changes needed)

- `repositories[]` (`path`, `name`, `branch`, `since_date`, `until_date`)
- `github` (`token`, `org`, `repos`, `fetch_prs`)
- `jira` (`url`, `email`, `token`, `project_keys`)
- `linear` (`api_key`, `team_keys`, `fetch_on_reference`)
- `classification` (`rules_file`, `use_llm`, `llm_model`,
  `confidence_threshold`, `custom_categories`)
- `output` (`directory`, `formats`, `include_unclassified`,
  `include_merges`, `include_files`)
- `aliases` (developer identity merging)

### New in `tga` (Rust-only additions)

- `classification.llm_provider` — `"openrouter"`, `"openai"`, or
  `"auto"` (default `"auto"`). The Python tool only supported OpenAI.
- `classification.openrouter_api_key` — optional explicit OpenRouter key.
  Falls back to `$OPENROUTER_API_KEY`.
- `azure_devops` block (Azure DevOps Work Items, including `AB#` extraction).
- `analysis.min_coverage_pct` — emit a warning when classification
  coverage falls below this threshold.

### Removed / not-yet-ported

- Per-developer cost tracking is not yet implemented.
- Slack / Teams notification hooks are not yet implemented.

If you depend on a feature that isn't listed as compatible, please open
an issue on the GitHub repository.

---

## Database Migration

The on-disk SQLite schema is **a superset** of the Python tool's schema.
The Rust migration runner applies versioned migrations on every `Database::open`
call, so:

1. **Back up your existing database** before first run:
   ```bash
   cp ~/.gitflow-analytics/data.db ~/.gitflow-analytics/data.db.backup
   ```
2. Point `tga` at the same `.db` file (via `--database` or config):
   ```bash
   tga --database ~/.gitflow-analytics/data.db analyze
   ```
3. The first run upgrades the schema in-place. WAL mode is enabled.

If you'd rather start clean (recommended for large schema gaps):

```bash
rm ~/.gitflow-analytics/data.db
tga --config config.yaml analyze --weeks 12
```

The week-level `collection_runs` table means re-collection is incremental
— `tga` will only walk weeks it hasn't seen before.

---

## CLI Flag Mapping

| Python `gfa`                  | Rust `tga`                | Notes                                                  |
|-------------------------------|---------------------------|--------------------------------------------------------|
| `gfa analyze`                 | `tga analyze`             | Full pipeline (collect → classify → report)            |
| `gfa collect`                 | `tga collect`             | Stage 1 only                                           |
| `gfa classify`                | `tga classify`            | Stage 2 only                                           |
| `gfa report`                  | `tga report`              | Stage 3 only                                           |
| `--config path.yaml`          | `--config path.yaml`      | Identical                                              |
| `--database data.db`          | `--database data.db`      | Identical                                              |
| `-v` / `-vv`                  | `-v` / `-vv` / `-vvv`     | Verbosity is a count: warn / info / debug / trace      |
| `--weeks N`                   | `--weeks N`               | Identical (last N weeks)                               |
| `--since YYYY-MM-DD`          | `--from YYYY-MM-DD`       | Renamed; `--since` kept as a legacy alias              |
| `--until YYYY-MM-DD`          | `--to YYYY-MM-DD`         | Renamed; `--until` kept as a legacy alias              |
| `--repos repo1,repo2`         | `--repos repo1,repo2`     | Identical                                              |
| `--force`                     | `--force` / `-f`          | Re-collect already-collected weeks                     |
| `--dry-run`                   | `--dry-run`               | Identical (writes go to an in-memory DB)               |
| `--no-cache`                  | _(not needed)_            | `tga` re-collection is week-level by default           |
| `--output dir/`               | `--output dir/`           | Identical                                              |
| `--format csv,json,md`        | `--formats csv,json,md`   | Pluralized                                             |
| _(not in Python)_             | `--no-fetch`              | Skip the pre-walk `git fetch` (offline mode)           |
| _(not in Python)_             | `tga install`             | Interactive config wizard                              |
| _(not in Python)_             | `tga aliases`             | Manage developer identity aliases from the CLI         |
| _(not in Python)_             | `tga backfill`            | Re-derive `is_revert` / `ticket_id` columns            |

`--weeks` is mutually exclusive with `--from` / `--to`. If both are passed,
`tga` will reject the invocation.

---

## Behavioral Differences

### Performance

- **git extraction** is 10–50× faster on large monorepos. The Rust port
  uses `libgit2` directly (no `git` subprocess fork-per-commit) and
  stops the revwalk as soon as it crosses below the `since` boundary.
- **Classification** uses `rayon` for batch parallelism. On a 58K-commit
  history the Tier-1/2/3 cascade runs in seconds.
- **Database** is opened in WAL journal mode on every run.

### Remote fetching

`tga` runs `git fetch origin` automatically before each per-repo revwalk
so the local clone is up to date. Authentication is **non-interactive**
(SSH agent → `~/.ssh/id_ed25519` → `~/.ssh/id_rsa` → default credential
helper). If none of those succeed the fetch is downgraded to a warning
and collection continues against local refs.

Pass `--no-fetch` to skip this step entirely (useful for offline runs
or when CI has already fetched).

### LLM classification

The Python tool routes LLM calls through OpenAI. `tga` adds **OpenRouter**
as a first-class provider:

```yaml
classification:
  use_llm: true
  llm_provider: "openrouter"        # or "openai" or "auto"
  llm_model: "anthropic/claude-3.5-sonnet"
```

Auto-detect mode (`llm_provider: "auto"`, the default) prefers OpenRouter
when `OPENROUTER_API_KEY` is set, otherwise falls back to OpenAI.

### Error handling

Per-repo failures are non-fatal in both tools. `tga` additionally
warns-and-continues on:

- Pre-walk fetch failures (auth, transport, certificate)
- Optional API client init failures (GitHub, Linear, ADO)
- Single-week `collection_runs` lookup failures

The aggregated error list is printed at the end of each run.

### Logging

`tga` uses the `tracing` crate. Verbosity can be controlled two ways:

- `--log <LEVEL>` — explicit level (`error` / `warn` / `info` / `debug` / `trace`)
- `-v` / `-vv` / `-vvv` — shortcut for `info` / `debug` / `trace`

The `RUST_LOG` environment variable, if set, takes precedence over both
flags (e.g. `RUST_LOG=tga::collect=debug,warn`). Default is `warn`.

---

## Troubleshooting

### "no repositories matched --repos filter"

Repository names come from `repositories[].name` in YAML, defaulting to
the basename of `path`. List configured names:

```bash
yq '.repositories[].name' config.yaml
```

### "pre-walk fetch failed (auth/transport)"

`tga` couldn't authenticate to the remote non-interactively. Options:

1. Start an SSH agent: `eval $(ssh-agent) && ssh-add ~/.ssh/id_ed25519`
2. Use a different remote URL (HTTPS with a credential helper)
3. Pass `--no-fetch` to skip the fetch and use local refs only

### "LLM provider auto-selected: openai" but I wanted OpenRouter

Either:
- Set `OPENROUTER_API_KEY` in the environment, or
- Set `classification.llm_provider: "openrouter"` explicitly, or
- Set `classification.openrouter_api_key: "sk-or-..."` in the config  # pragma: allowlist secret

### Database migration failed mid-run

Restore from backup, then either:

```bash
# Option A: fresh DB
rm path/to/data.db && tga analyze --weeks 12

# Option B: file an issue with the migration error output
```

Migrations are designed to be idempotent — re-running `tga` after a
failure should not corrupt state.

### Reports look different from `gfa`

Some formatting differences exist (CSV column order, rounding, ISO-week
boundaries on calendar edges). Open an issue if a substantive metric
disagrees — the goal is parity within rounding.

### Build error: "could not find SSL"

You should not see this — `tga` uses `rustls` and bundles SQLite. If
you do, you may have a stale `Cargo.lock`. Try:

```bash
cargo clean
cargo build --release
```

---

## Getting Help

- **Issue tracker**: https://github.com/bobmatnyc/trusty-git-analytics/issues
- **Requirements docs**: `docs/trusty-git-analytics/requirements/` (full schema specs)
- **Architecture notes**: `docs/trusty-git-analytics/developer/architecture.md`

When filing a bug, include:

1. `tga --version` output
2. Sanitized `config.yaml` (redact tokens)
3. Command line invocation
4. Full output with `-vv` (debug verbosity)
