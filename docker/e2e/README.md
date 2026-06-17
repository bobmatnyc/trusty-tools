# Docker E2E Install-from-crates.io Smoke Test

This directory contains the Docker-based end-to-end smoke test harness for
the trusty-* tools. Unlike the source-build CI (`.github/workflows/ci.yml`),
this harness installs the **published crates from crates.io** on a clean
Linux base image and runs a minimal end-to-end scenario for each tool.

## Quick start (local run)

```bash
# From the workspace root
bash scripts/e2e-docker.sh

# Or via make
make e2e-docker
```

Docker must be installed and the daemon must be running. Build time is
15–30 minutes on a typical developer machine (four Rust crates compile from
source inside the container).

### Pin specific versions

```bash
bash scripts/e2e-docker.sh \
  --search-version 0.24.10 \
  --memory-version 0.15.3 \
  --mpm-version 0.6.2 \
  --analyze-version 0.5.0
```

Or via make:

```bash
make e2e-docker \
  TRUSTY_SEARCH_VERSION=0.24.10 \
  TRUSTY_MEMORY_VERSION=0.15.3 \
  TRUSTY_MPM_VERSION=0.6.2 \
  TRUSTY_ANALYZE_VERSION=0.5.0
```

## What each scenario asserts

### Scenario 1: trusty-search

Commands used:
```
trusty-search start
trusty-search index /e2e/sample-code --name smoke-fixture --lexical-only
trusty-search query authenticate --index smoke-fixture
```

Assertions:
1. Daemon becomes healthy (HTTP 200 on `/health`).
2. Lexical-only index created over the `sample-code/` directory, confirmed by
   non-zero chunk count in the output (the walker skips dirs named `fixtures`).
3. Query for `authenticate` returns results (the fixture's `src/auth.rs`
   contains an `authenticate` function).

Note: the directory is named `sample-code` (not `fixtures`) because
`trusty-search`'s walker skips any path component named `fixtures` by default
(it is in `SKIP_DIRS` as a test-data exclusion — issue #130).

#### Indexing-hygiene sub-assertion (version-gated)

The fixture includes `data/large_dataset.json` (177 KiB, well above the 64 KiB
threshold). Starting with trusty-search 0.25.0, `data/` directories and `.json`
files larger than 64 KiB are excluded from indexing by default (issue #1372).

The harness detects the installed version and:
- **If < 0.25.0**: logs `[SKIP] hygiene assertion requires trusty-search >= 0.25.0, installed X.Y.Z — skipping`. This is **not a failure**.
- **If >= 0.25.0**: asserts that `data/large_dataset.json` does not appear in
  the index's chunk list. Failure means the hygiene defaults regressed.

Once trusty-search 0.25.0 is published to crates.io, the nightly run
automatically promotes the SKIP to an active assertion with no code change.

### Scenario 2: trusty-memory

Commands used:
```
trusty-memory serve --foreground --http 127.0.0.1:7070
curl -X POST http://127.0.0.1:7070/api/v1/palaces -d '{"name":"personal"}'
curl -X POST http://127.0.0.1:7070/api/v1/palaces/personal/drawers -d '{"content":"..."}'
curl http://127.0.0.1:7070/api/v1/palaces/personal/recall?q=sentinel+value
```

Assertions:
1. Daemon becomes healthy.
2. `personal` palace created (palace named `personal` is always valid regardless
   of cwd / project root — no project markers required in Docker).
3. Memory stored (drawer create returns `{"id": "..."}"`).
4. Recall returns the stored sentinel text `42xyzABC`.

### Scenario 3: trusty-mpm

Commands used:
```
tm --version
tm --help
tm start
tm status
tm stop
```

Assertions:
1. Both `tm` and `trusty-mpm` binaries are installed (single-install convention).
2. `--version` returns non-empty output.
3. `--help` output mentions expected keywords (`usage`, `daemon`, `session`, etc.).
4. Daemon starts, `tm status` indicates running.

### Scenario 4: trusty-analyze

Commands used:
```
trusty-search start                          # trusty-analyze hard dependency
trusty-search index <fixture> --lexical-only
TRUSTY_SEARCH_URL=http://127.0.0.1:<port> trusty-analyze serve --foreground
trusty-analyze analyze smoke-analyze --top-k 5
curl http://127.0.0.1:7879/health
```
Note: `--search-url` is a global flag on the `trusty-analyze` top-level CLI, not
a `serve` subcommand flag. Using the `TRUSTY_SEARCH_URL` env var avoids the
awkward `trusty-analyze --search-url <url> serve` ordering.

Assertions:
1. Both trusty-search (dependency) and trusty-analyze daemons start healthy.
2. `trusty-analyze analyze` returns output mentioning chunks, files, or
   complexity keywords.
3. `/health` endpoint returns JSON with a `"status"` field.

## Fixture structure

The fixture lives at `docker/e2e/fixtures/sample-repo/` in the repo (source)
and is copied to `/e2e/sample-code/` inside the container (the Dockerfile
renames it to avoid the `fixtures` SKIP_DIR rule — see below).

```
/e2e/sample-code/          (inside container — NOT named "fixtures")
  Cargo.toml              — makes it a valid Rust project
  src/
    lib.rs                — defines DbPool, parse_json_body
    auth.rs               — defines authenticate(), verify_token()
  data/
    large_dataset.json    — 177 KiB JSON file (> 64 KiB threshold)
                            used for the hygiene gate assertion
```

The `data/` directory and the large JSON file are there specifically to test
that trusty-search >= 0.25.0 excludes them by default.

**Why "sample-code" not "fixtures":** `trusty-search`'s file walker includes
`"fixtures"` in its `SKIP_DIRS` list (issue #130) and silently prunes any
directory component with that name. Naming the container path `sample-code`
avoids this exclusion so the `.rs` files are actually indexed.

## When does this run?

The workflow (`.github/workflows/e2e-docker.yml`) is triggered by three conditions:

### 1. On-demand (manual)
Run from the GitHub Actions UI (**Actions → e2e-docker → Run workflow**) or the CLI:
```bash
gh workflow run e2e-docker.yml
```
Use this anytime to validate a published release before declaring it complete. Optionally pin specific crate versions via the workflow inputs.

### 2. Release gate (scheduled on-demand)
When a release batch spans **≥3 crates OR ≥3 PRs**, manually run the E2E validation before marking the release as done. This ensures published artifacts work end-to-end on a clean Linux image. Smaller releases (1–2 crates, 1–2 PRs) may skip this and rely on standard CI.

### 3. Nightly safety-net (automatic schedule)
Runs automatically **every day at 02:00 UTC**. This catches regressions in published crates independently of releases — e.g., crates.io outages, yanked dependencies, or external ecosystem changes.

### Why separate from main CI?

Source-build CI (`.github/workflows/ci.yml`) builds from the local checkout. This workflow is the complementary **install-from-crates.io** gate, proving that published binaries install cleanly on a clean Linux base image. This path is what users actually run, so it must be tested independently.

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `TRUSTY_SKIP_RAM_CHECK` | `1` | Bypass the 16 GB RAM guard. Required in Docker (CI runners have less RAM; we index a tiny fixture). |
| `XDG_DATA_HOME` | `/tmp/trusty-data` | Daemon state root — keeps all data inside the container. |
| `HOME` | `/root` | Required for discovery-file path expansion. |
