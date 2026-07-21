---
name: cargo-publish
description: Publish one or more Rust crates to crates.io from the trusty-tools workspace
user-invocable: true
version: "1.0.0"
category: local-ops
tags: [cargo, publish, release, workspace, local-ops]
effort: high
when_to_use: When publishing one or more crates to crates.io from this workspace; when bumping versions; when investigating dry-run failures; when sequencing multi-crate releases with shared dependencies
---

# Cargo Publish Workflow for trusty-tools

Complete reference for publishing Rust crates to crates.io in the trusty-tools workspace.
Codifies lessons from 18+ publishes across two recent sessions.

## Canonical Workflow

Every publish follows this exact sequence:

```
0. scripts/check-publish-ready.sh <crate>   — MANDATORY, MUST PASS
1. Pre-flight checks (fmt, clippy, tests)
2. cargo publish --dry-run
3. git tag <crate-name>-v<version>
4. git push origin <crate-name>-v<version>
5. scripts/preflight-publish.sh <crate>     — MANDATORY, MUST PASS (run again, immediately before step 6)
6. cargo publish
7. Wait 60-120s for propagation
8. Verify with curl to crates.io API
9. cargo install --path crates/<dir> --locked (binaries only)
10. Verify <binary> --version
```

**Critical**: Never skip dry-run. Never publish from the main checkout.

## Step 0: Publish-Only-From-Merged-Main Guard (MANDATORY, issue #2227)

🔴 **Publish only from merged main / the pushed release tag — never an
unmerged branch.** Before running `cargo publish` for ANY crate, run:

```bash
scripts/check-publish-ready.sh <crate-name-or-dir>
# or
make publish-check CRATE=<crate-name-or-dir>
```

**Do NOT run `cargo publish` if this fails.** It asserts two things against
the true `origin/main` tip (fetched fresh, never trusted from a possibly-stale
local ref):

1. **GUARD 1 (merged-main)**: current HEAD is `origin/main` itself or an
   ancestor of it.
2. **GUARD 2 (version tag)**: the release tag `<crate>-v<version>` (accepts
   `tga-v<version>` too, for `trusty-git-analytics`) has been pushed to origin
   and its commit is on merged main.

**Why this exists**: issue #2209 — a publish ran from an unmerged feature
branch that was missing a P0 fix. That branch's build became the crates.io
"latest" version, and every concurrent worktree session that ran
`cargo install <crate>` picked up the regressed build until it was caught and
corrected. With concurrent sessions routinely running in this repo,
"whatever branch happens to be checked out" is a correctness hazard, not just
a process nicety — this script turns "publish only from merged main" into a
mechanical gate instead of a convention someone can forget under pressure.

**Escape hatch (rare, deliberate use only)**: `ALLOW_UNMERGED_PUBLISH=1`
downgrades both guard failures to a loud warning and exits 0. Only use this
when you have a specific, understood reason to publish from an unmerged
commit — the default path is always "merge to main first, then publish."

## Step 5: Identity + Clean-Tree + Version-Not-Live Guard (MANDATORY, closes the 2026-07-08 collision)

🔴 **Run `scripts/preflight-publish.sh` immediately before every `cargo publish`
— treat any nonzero exit as an absolute stop.** On 2026-07-08 a crate was
published to crates.io out-of-band — from an UNMERGED branch, under the
WRONG gh account — burning crates.io version 0.22.0 with fix-less content
(a burned version number can never be reused). `check-publish-ready.sh`
above already covers "merged main" + "tag pushed"; this script closes the two
gaps that incident fell through — WHO is publishing, and whether the target
version is already live:

```bash
scripts/preflight-publish.sh trusty-mpm
```

Reads the crate's name/version straight from `crates/trusty-mpm/Cargo.toml`
(pass an explicit version as a second argument to check a hypothetical
version instead). Runs four checks and fails loud on any of them:

1. **merged-main**: current HEAD's commit SHA is EXACTLY `origin/main`'s HEAD
   SHA (stricter than `check-publish-ready.sh`'s ancestor check).
2. **identity**: the active `gh auth status` account is `bobmatnyc`. Any
   other active account fails with the remedy `gh auth switch --user bobmatnyc`.
3. **clean-tree**: `git status --porcelain` is empty.
4. **version-not-live**: the target version is not already published on
   crates.io (queries `https://crates.io/api/v1/crates/<name>/<version>`) —
   this is the exact guard that would have caught the 0.22.0 collision.

`scripts/preflight-publish.sh --check-only <crate>` runs all four checks
unconditionally and prints a `[PASS]`/`[FAIL]` line per check without
assuming you're mid-publish — use it to preview status. `--help` documents
the rare, logged `PREFLIGHT_ALLOW_DETACHED=1` override for check 1 (validated
release worktrees only — misuse of it is exactly how the incident happened).
No override exists for the identity check.

## Step 6: Version-Parity Guard (MANDATORY, issue #3366)

Before bumping a crate's version to publish, confirm the crate ISN'T already
drifted — i.e. that its CURRENT (pre-bump) Cargo.toml version, if already
live on crates.io, still matches the local `src/` tree. This is the
`trusty-common`/`trusty-agents-common` incident: several commits of source
changes landed on `main` without a version bump while the old version number
was already published, so `cargo publish --dry-run` for a downstream crate
failed with "symbol not found" for symbols that only existed in the drifted,
unpublished source.

```bash
make version-parity-check
# or directly:
scripts/check-version-parity.sh
```

This also runs automatically on every push to `main`
(`.github/workflows/version-parity.yml`) so drift is caught right after it
merges rather than discovered as a release blocker. See
`crates/trusty-publish-guard` for the underlying check (fails closed: a
crate whose live/local comparison can't be verified is treated as a failure,
never a silent pass).

For the RELATED but distinct cross-crate ordering hazard (publishing a crate
before a sibling it depends on is live — the 2026-07-20 incident), see
`scripts/publish-dry-run-order.sh` in "Cross-Crate Publish Ordering" below —
it now computes the dependency order mechanically instead of by hand.

## Worktree Discipline (MANDATORY)

Always operate from a dedicated git worktree, never the main checkout.

```bash
# Provision a fresh worktree off origin/main
git fetch origin main
git worktree add -b feature/publish-<crate> \
    .claude/worktrees/publish-<crate> origin/main
cd .claude/worktrees/publish-<crate>

# Work, test, tag, and push from inside this worktree
# When complete: git worktree remove --force .claude/worktrees/publish-<crate>
```

**Why**: Concurrent sessions may hold uncommitted work in the main checkout.
Worktrees are isolated. After a squash-merge, clean up the local branch:
```bash
git branch -D feature/publish-<crate>
git push origin --delete feature/publish-<crate>
```

## macOS cdhash Trap (RED — High Impact)

**NEVER do this**:
```bash
cp target/release/<binary> ~/.cargo/bin/<binary>
```

The kernel caches code-signing identity by `cdhash` (executable hash).
A plain `cp` over an existing on-PATH binary leaves a **stale cache**.
The next exec is **SIGKILL'd** as:
```
EXC_CRASH / CODESIGNING — Taskgated Invalid Signature
zsh: killed (no output — looks exactly like OOM kill)
```

**ALWAYS do this instead**:
```bash
cargo install --path crates/<dir> --locked
```

`cargo install` writes to a temp file and renames atomically, keeping the
kernel cache consistent. If a manual copy is ever unavoidable:
```bash
cp target/release/<binary> ~/.cargo/bin/<binary>
codesign --force --sign - ~/.cargo/bin/<binary>  # Regenerate signature
```

## Pre-flight Quality Gates

All of these must pass. No `--allow-dirty`, `--no-verify`, or `--force` flags:

```bash
scripts/check-publish-ready.sh <crate>   # Step 0 — merged-main guard (issue #2227)
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p <crate>
cargo check --workspace
```

Abort if anything fails. Fix it, commit, and restart.

## pnpm-artifact Cleanup (NEW — Learned Today)

Crates with embedded Svelte UIs (e.g., `trusty-search/ui`) may generate
untracked `node_modules/` and `pnpm-workspace.yaml` that block dry-run.

If `cargo publish --dry-run` fails with "working directory is dirty":

```bash
git status                       # Eyeball what's flagged
git clean -fdX                   # Remove ONLY gitignored content (-X flag)
# If specific untracked yaml remains after review, rm it explicitly
git status                       # Verify clean
```

**Critical**: Never clean tracked files. `git clean -fdX` removes only
`.gitignore`-listed files, not source code.

## License Field Gotcha

crates.io rejects `license = "Elastic-2.0"` (not in SPDX registry).

**For Elastic-2.0 licensed crates**:
```toml
# ✗ WRONG
license = "Elastic-2.0"

# ✓ CORRECT
license-file = "LICENSE"
```

**For MIT licensed crates**:
```toml
license = "MIT"
```

## [patch.crates-io] Semantics

**Local workspace builds** resolve internal deps via path, ignoring
`[patch.crates-io]` overrides.

**But `cargo publish`** resolves ALL dependencies from the live crates.io
registry — the same view downstream consumers will see.

**Implication**: When crate A's public API changes and crate B depends on A
(via `workspace = true`), you MUST:

1. Bump A's version in `crates/A/Cargo.toml`
2. **Publish A to crates.io FIRST**
3. **Wait 60-120s for propagation**
4. **Then publish B**

**If you skip this**: `cargo publish --dry-run` for B fails with
"dependency not found" because crates.io doesn't yet have A at the new version.

## Cross-Crate Publish Ordering (RED — Common Pitfall)

**Automated (issue #3366)**: `scripts/publish-dry-run-order.sh` computes the
dependency order mechanically from `cargo metadata` and runs
`cargo publish --dry-run -p <crate>` for every publishable crate in that
order (dependencies before dependents), stopping at the first failure — this
replaces re-deriving the manual recipe below by hand for a full-workspace
release. Pass specific crate names to restrict it to a partial release (it
still includes any publishable crate they depend on, in order):

```bash
scripts/publish-dry-run-order.sh --list-only          # print the order, run nothing
scripts/publish-dry-run-order.sh                      # dry-run every publishable crate, in order
scripts/publish-dry-run-order.sh trusty-search trusty-common
```

The manual recipe below remains useful for reasoning about WHY a given order
is correct, and the crate list in "Dependency Publish Order" further down is
historical/illustrative rather than mechanically maintained — trust the
script's computed order, not that hand-written list.

### The Recipe

1. **Identify all changed crates** (from git diff, PR title, or description)
2. **Read each changed crate's `[dependencies]`** for `workspace = true` entries
3. **Resolve those deps to versions** (check `Cargo.lock`)
4. **Check crates.io** for each dependency version:
   ```bash
   curl -s https://crates.io/api/v1/crates/<crate>/<version> | head -c 100
   ```
   JSON metadata = already live; 404 = not yet published
5. **Build a publish order**: publish all missing versions first, wait for
   propagation, then downstream crates

### Dependency Publish Order (trusty-tools)

Publish library crates before the crates that depend on them. The ordering for this workspace:

```
trusty-common → trusty-mcp-core → trusty-embedder → trusty-symgraph
  → trusty-search, trusty-memory-core, trusty-analyze
  → trusty-mpm-core → trusty-mpm-client → trusty-mpm-daemon, trusty-mpm-mcp
  → trusty-mpm-cli, trusty-mpm-tui
```

If only a subset of these crates changed, publish only the changed ones and their direct downstream dependents, in order.

### Worked Example From Today

**Session publishes**: trusty-common 0.8.0, trusty-search 0.13.1, tga 1.4.2

**Analysis**:
- `trusty-search` depends on `trusty-common` (workspace = true, resolves to 0.8.0)
- `tga` depends on `trusty-common` (workspace = true, resolves to 0.8.0)
- crates.io has trusty-common 0.7.0 but NOT 0.8.0 yet

**Correct order**:
1. Publish trusty-common 0.8.0
2. Sleep 100s, verify propagation
3. Then publish trusty-search 0.13.1
4. Then publish tga 1.4.2

**What happened if we skipped**:
```bash
cargo publish --dry-run -p trusty-search
# ERROR: dependency trusty-common v0.8.0 not found on crates.io
# (because we only just published it, crates.io needs 60-120s)
```

## Propagation Wait (60-120 seconds)

After `cargo publish` succeeds with status 200 OK:

```bash
# Immediately after: ✓ crates.io ingestion complete
# Next 60-120s: ✓ metadata replicating to CDN, search index updating
```

**Before publishing a crate that depends on this one**, verify:

```bash
# Wait ~100s, then check
curl -s https://crates.io/api/v1/crates/<crate>/<version> | head -c 200

# Success: JSON metadata appears (version is now live)
# {"crate":{"name":"...","versions":[...]},...}

# Still waiting: 404 Not Found
# {"errors":[{"detail":"Crate not found"}]}
```

If 404 after 120s, something went wrong. Check:
```bash
cargo search <crate> --limit 1
```

## Tag Pattern: <crate-package-name>-v<version>

Use the **crate package name** from `Cargo.toml`, NOT the directory name.

**Reference: Abbreviations table from CLAUDE.md**:
- `trusty-git-analytics` → `-p tga` → tag: **`tga-v1.4.2`** ✓
- `trusty-search` → `-p trusty-search` → tag: **`trusty-search-v0.13.1`** ✓
- `trusty-common` → `-p trusty-common` → tag: **`trusty-common-v0.8.0`** ✓
- `open-mpm` → `-p open-mpm` → tag: **`open-mpm-v0.2.3`** ✓

> **tga tag aliases (issue #1128):** the binary-release workflow accepts **both**
> `tga-v<version>` and `trusty-git-analytics-v<version>` — they resolve to the
> same build config and Homebrew formula (the parse step canonicalizes the `tga`
> prefix to `trusty-git-analytics`). The documented form above (`tga-v<version>`,
> matching the published package name) is preferred; you no longer need a second
> `trusty-git-analytics-v<version>` tag to trigger a successful binary release.

The crate name **always** comes from the `name` field in `Cargo.toml`:

```bash
# Inside the worktree, when in doubt:
grep "^name = " crates/<dir>/Cargo.toml
```

## Crate Name vs Directory Name

Most match (`crates/trusty-search/` → `-p trusty-search`).

**Exceptions** (always verify `Cargo.toml`):
- `crates/trusty-git-analytics/` → `name = "tga"` → `-p tga`
- `crates/open-mpm/` → `name = "open-mpm"` → `-p open-mpm`

If `cargo -p <name>` returns "package not found":
```bash
grep "^name = " crates/<dir>/Cargo.toml
```

## Single-Install Convention

A main crate's binary release must include **every binary** required to run
that crate. Sidecar daemons are bundled via `[[bin]]` shims pointing at the
sidecar's `run()` entry point.

**Example: trusty-search**
```toml
# crates/trusty-search/Cargo.toml
[[bin]]
name = "trusty-search"
path = "src/bin/main.rs"

[[bin]]
name = "trusty-embedder"  # Sidecar daemon (optional utility)
path = "src/bin/embedder.rs"
```

Users invoke:
```bash
cargo install trusty-search
# Both trusty-search AND trusty-embedder land in ~/.cargo/bin/
```

## publish=false Guard

Before running `cargo publish` for any crate, verify it is not marked non-publishable:

```bash
grep "publish" crates/<dir>/Cargo.toml
```

If the output contains `publish = false`, **do not publish** that crate. Common non-published crates include binary/CLI crates and internal tooling crates. When in doubt, read the manifest.

## Sidecar Publish Rule (RED)

**Sidecar lib crates whose lib is a dependency of a published main crate
MUST be published to crates.io.**

Do NOT set `publish = false` on such crates. Example:

```toml
# crates/trusty-embedder/Cargo.toml
[package]
name = "trusty-embedder"
publish = true  # ← REQUIRED even if users never cargo install it directly
```

**Why**: When you `cargo publish -p trusty-search`, Cargo's dependency
resolver requires every transitive lib dependency to exist on crates.io at
the declared version, even if the binary isn't published separately.
Downstream consumers don't manually install the sidecar, but Cargo's
resolution during their build REQUIRES it to be available.

**If you set `publish = false`**: `cargo publish -p trusty-search --dry-run`
fails with "dependency not found" because the sidecar lib can't be resolved.

## Versioning Conventions

### Semver Bump Rules (by Conventional Commit Type)

Always read the git log since the last tag to determine the correct bump before editing any version:

```bash
git log <crate-name>-v<last-version>..HEAD --oneline -- crates/<dir>/
```

Map commit types to semver components:

| Commit type | Version component |
|---|---|
| `feat:` | MINOR (x.Y.0) |
| `fix:`, `chore:`, `perf:`, `refactor:` | PATCH (x.y.Z) |
| `BREAKING CHANGE` in footer, or `!` suffix on any type | MAJOR (X.0.0) |

Examples by change type:

| Change Type | Example | Bump Rule |
|---|---|---|
| New public function | `feat: add auth handler` | Minor (x.y → x.y+1.0) |
| Bug fix | `fix: resolve race in async` | Patch (x.y.z → x.y.z+1) |
| Chore / perf / refactor | `chore: update deps` | Patch |
| **BREAKING** public API | `feat!: remove deprecated fn` | Major post-1.0; Minor pre-1.0 |

**Workspace-pinned versions**:
- Crates using `[workspace.package]` (trusty-mpm-* family) bump together
- Edit version once in root `Cargo.toml`, all members inherit it
- Tag each crate individually (`trusty-mpm-core-v<ver>`, `trusty-mpm-cli-v<ver>`, etc.)
- Publish in dependency order: core first, then consumers

### Checking the Last Released Version

```bash
# From git tags
git tag --list '<crate-name>-v*' | sort -V | tail -1

# From crates.io (if published)
cargo search <crate-name> | head -3
```

## Pre-Publish Sequence (Detailed)

```bash
# 1. Inside worktree, change to repo root
cd /Volumes/Kemono/Users/masa/Projects/trusty-tools/.claude/worktrees/publish-<crate>

# 2. Verify on origin/main (git status should be clean from worktree creation)
git status

# 3. Edit version(s) in Cargo.toml
vim crates/<crate>/Cargo.toml
# or for workspace-pinned:
vim Cargo.toml

# 4. Run pre-flight checks
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p <crate>
cargo check --workspace

# 5. If UI changes (trusty-search, trusty-memory), check for stale artifacts
git status  # Look for node_modules/ or pnpm-workspace.yaml
git clean -fdX  # If present

# 6. Commit version bump
git add -A
git commit -m "chore: bump <crate> to v<version>"

# 7. Dry run (essential — catches dependency issues early)
cargo publish --dry-run -p <crate>

# 8. If dry-run fails:
#    - Read the error (usually "dependency X version Y not found on crates.io")
#    - Publish that dependency first
#    - Wait 100s
#    - Retry this crate's dry-run

# 9. Tag
git tag <crate>-v<version>

# 10. Push tag to origin
git push -u origin <crate>-v<version>

# 11. Publish to crates.io
cargo publish -p <crate>

# 12. Verification step
sleep 100
curl -s https://crates.io/api/v1/crates/<crate>/<version> | head -c 200

# 13. For binaries: install locally
cargo install --path crates/<crate> --locked

# 14. Verify binary version
<binary> --version
```

## Worked Example: Two-Step Publish (trusty-common + trusty-search)

**Scenario**: trusty-common public API changed (breaking); trusty-search
depends on it. Both need to publish.

```bash
# === STEP 1: PUBLISH trusty-common 0.8.0 ===
cd .claude/worktrees/publish-trusty-common

# Edit, test, commit, tag
vim crates/trusty-common/Cargo.toml  # 0.7.0 → 0.8.0
cargo test -p trusty-common
git commit -m "chore: bump trusty-common to v0.8.0"
git tag trusty-common-v0.8.0
git push origin trusty-common-v0.8.0

# Dry run
cargo publish --dry-run -p trusty-common  # ✓ PASS

# Publish
cargo publish -p trusty-common

# === PROPAGATION WAIT ===
sleep 100

# Verify on crates.io
curl -s https://crates.io/api/v1/crates/trusty-common/0.8.0 | head -c 200
# {"crate":{"name":"trusty-common",...},"versions":[...],...}  ← LIVE

# === STEP 2: PUBLISH trusty-search 0.13.1 ===
cd .claude/worktrees/publish-trusty-search

# Edit, test, commit, tag
vim crates/trusty-search/Cargo.toml  # 0.13.0 → 0.13.1
cargo test -p trusty-search
git commit -m "chore: bump trusty-search to v0.13.1"
git tag trusty-search-v0.13.1
git push origin trusty-search-v0.13.1

# Dry run (now trusty-common 0.8.0 IS on crates.io)
cargo publish --dry-run -p trusty-search  # ✓ PASS

# Publish
cargo publish -p trusty-search

# Verify
sleep 100
curl -s https://crates.io/api/v1/crates/trusty-search/0.13.1 | head -c 200

# Install
cargo install --path crates/trusty-search --locked
trusty-search --version
```

## Common Dry-Run Failures & Remedies

### "dependency X not found"
- That dependency hasn't been published yet or caches.io hasn't synced it
- Publish the dependency first, wait 100s, retry

### "working directory is dirty / changes will not be published"
- Untracked files (especially `node_modules/`, `pnpm-workspace.yaml`)
- Run `git clean -fdX` (gitignored files only)
- Never use `-f` alone (deletes all untracked, including source)

### "version already exists"
- This version was already published
- Bump to a new version or verify you meant a different version

### "license field is invalid"
- Using `license = "Elastic-2.0"` (not in SPDX registry)
- Use `license-file = "LICENSE"` instead

### Cannot find package in workspace
- Wrong package name (e.g., `-p trusty-git-analytics` instead of `-p tga`)
- Check `name` field in `Cargo.toml`

## Git Tag / Release Convention (from CLAUDE.md)

Each crate is tagged independently: `<crate-name>-v<version>`

Release flow:
1. Bump version in crate's `Cargo.toml`
2. Run `cargo test -p <crate>` and lint checks
3. Commit the version bump
4. Create tag: `git tag <crate-name>-v<version>`
5. Push tag: `git push origin <crate-name>-v<version>`
6. Publish: `cargo publish -p <crate>`
7. Install binary (if applicable): `cargo install --path crates/<dir> --locked`

## Cleanup After Publishing

Once the PR merges and the main branch absorbs your commits:

```bash
# From main checkout or any other worktree:
git worktree remove --force .claude/worktrees/publish-<crate>

# Clean up local branch
git branch -D feature/publish-<crate>

# Clean up remote branch (if pushed)
git push origin --delete feature/publish-<crate>
```

## Quality Checklist

Before declaring a publish complete:

- [ ] `scripts/check-publish-ready.sh <crate>` (or `make publish-check CRATE=<crate>`) passed
- [ ] Pre-flight checks passed (fmt, clippy, tests, check)
- [ ] Dry-run succeeded
- [ ] Tag created with correct name pattern
- [ ] Tag pushed to origin
- [ ] `cargo publish` succeeded (status 200 OK)
- [ ] Waited 100s and verified on crates.io API
- [ ] Binary installed with `cargo install --path … --locked` (if applicable)
- [ ] `<binary> --version` shows correct version
- [ ] Worktree cleaned up (`git worktree remove`)
- [ ] Local and remote branches cleaned up

## Connection-Safe Daemon Restart (issue #534)

When upgrading a launchd-managed trusty-* daemon (trusty-memory, trusty-search,
trusty-analyze), use SIGTERM via `launchctl bootout` — **never**
`launchctl kickstart -k` which sends SIGKILL and drops live connections.

### Why SIGTERM instead of SIGKILL

As of issue #534, all three daemons implement graceful shutdown via
`axum::serve(...).with_graceful_shutdown(trusty_common::shutdown_signal())`.
When SIGTERM arrives:

1. The daemon stops accepting new connections.
2. All in-flight requests are drained (allowed to complete normally).
3. Cleanup code runs (addr files removed, BM25 supervisor reaped, etc.).
4. The process exits cleanly.

SIGKILL bypasses all of this: active requests die mid-stream, cleanup is
skipped, and the `mcp_bridge` in the Claude Code session receives an abrupt
socket close.

### Safe upgrade sequence (macOS launchd)

```bash
# 1. Stop the daemon gracefully (SIGTERM → drain → exit)
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/<label>.plist

# 2. Rebuild and install the new binary
cargo install --path crates/<crate-dir> --locked

# 3. Restart the daemon
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/<label>.plist
```

**Do NOT use `launchctl kickstart -k <label>`** — the `-k` flag sends SIGKILL
to the running instance before starting a new one, which kills in-flight
requests without draining.

### When to restart

Prefer restarting **between Claude Code sessions** (i.e., when no `.mcp.json`
MCP bridge process is actively connected). Even with graceful shutdown, the
`mcp_bridge` will need to reconnect after a restart — it does so automatically
with exponential backoff (200ms → 30s cap), so brief mid-session restarts are
now transparent to Claude Code for requests that were between calls. Restarts
during an active in-flight request will still lose that one request.

## References

- **CLAUDE.md**: "Build and Test Commands", "Git Tag / Release Convention", "Parallel Worktree Discipline"
- **GitHub**: Release tag format at `https://github.com/bobmatnyc/trusty-tools/releases`
- **crates.io API**: `https://crates.io/api/v1/crates/<name>/<version>`
