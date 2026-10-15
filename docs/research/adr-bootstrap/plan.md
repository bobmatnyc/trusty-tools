# /tm-adr init bootstraps a portable ADR gate in any project

Plan document for the ADR bootstrap epic. The `tm-epic` skill governs the
tracker and phase shape. `tm issue epic create --from` reads the `## Epic plan`
section; the H1 above becomes the tracker's outcome. Filing metadata the
parser does not read sits under `## Filing` at the end.

## Context

A project that wants ADRs today copies trusty-tools' `docs/adr/` and
`scripts/check_adr.sh` by hand, and the copy drifts. The research ran in two
rounds, then went through a devil's-advocate review (verdict
NEEDS_IMPROVEMENT, every finding applied), and was revised with the owner's
answers of 2026-10-15. Those answers settled the entry-file path
(`content/skills/tm-adr.md` stays, because the catalog reads only top-level
`skills/<stem>.md`; deployed tiers hold `<tier>/tm-adr/SKILL.md`), the type
labels from `TICKETING.md`, the milestone `trusty-mpm 1.9.0`, the component
`dx` with a `no-component-label` comment, the bash 3.2 floor with its CI
check, the bootstrapped script path `<ADR_DIR>/check_adr.sh`, the content
bundle version (chosen at implementation), and the workstream label
`ws/tm-trusty-tools-adr-init`. Decisions D1–D9 below carry the result.

Related issues:

- https://github.com/bobmatnyc/trusty-tools/issues/9132
- https://github.com/bobmatnyc/trusty-tools/issues/8619
- https://github.com/bobmatnyc/trusty-tools/issues/9055
- https://github.com/bobmatnyc/trusty-tools/issues/8389
- https://github.com/bobmatnyc/trusty-tools/issues/8758
- https://github.com/bobmatnyc/trusty-tools/issues/3171
- https://github.com/bobmatnyc/trusty-tools/issues/3173

## Epic plan

A project that wants ADRs today copies trusty-tools' `docs/adr/` and `scripts/check_adr.sh` by hand, and the copy drifts. After this epic, `/tm-adr init` scaffolds an ADR directory plus a portable, self-verifying check script in any project as one PR, and every later `/tm-adr` run reports when the project's script copy is behind the deployed skill and offers an update PR.

### Outcomes

- **O1** `/tm-adr init` on a project with no ADR setup yields one PR that adds `<ADR_DIR>/{README,template,INDEX,0001-record-architecture-decisions}.md`, `<ADR_DIR>/check_adr.sh` (mode 100755, byte-identical to the deployed skill copy), `.adr-dir`, a `.gitattributes` `eol=lf` line for the script, and `.github/workflows/adr-check.yml` when `.github/workflows/` exists. On a project with any existing ADR setup it writes nothing and names the marker it found.
- **O2** The shipped `check_adr.sh` runs with no `tm` present under bash 3.2 or newer. `--self-test` passes from heredoc fixtures; `--verify-self` exits 1 after any one-byte body edit; configuration is flags and `ADR_CHECK_*` env only.
- **O3** trusty-tools runs the shipped script byte-for-byte from `scripts/check_adr.sh` through `doc-numbers.yml`, and `/tm-adr` in this checkout reports the copy `up to date`.

### Ratified decisions

| # | Decision |
|---|---|
| D1 | Delivery is an `init` mode of the existing user-invocable `tm-adr` skill, `metadata.version` 2.0.0 → 2.1.0, shipped by a content release (`content-v*`, version set at implementation). No new roster entry, no `tm adr` subcommand, `content/skills/tm-init.md` untouched. |
| D2 | `tm-adr` becomes a directory skill. The entry file stays `content/skills/tm-adr.md` (the catalog reads only top-level `skills/<stem>.md`); new siblings `content/skills/tm-adr/references/{README,template,INDEX,0001-record-architecture-decisions}.md`, `references/adr-check.yml`, `scripts/check_adr.sh`. The bootstrapped script lives at `<ADR_DIR>/check_adr.sh`. |
| D3 | Detection of an existing setup stops init and reports the marker: `.adr-dir`, `.log4brains.yml`, `adr/`, `decisions/`, `architecture-decisions/`, any `NNNN-*.md` containing `## Decision`, an ADR mention in CLAUDE.md. |
| D4 | Every target-project write is a PR. `documentation` executes a fixed command list in a worktree (`cp` from the skill base directory Claude Code injects at load, `cmp`, `chmod +x`, `--verify-self`, `--self-test`, `git add` by name, `git commit`); `version-control` pushes and opens the PR. The CLAUDE.md pointer needs a second confirmation. |
| D5 | Script configuration is flags with `ADR_CHECK_*` env mirrors; `.adr-dir` supplies the default directory. No settings file. `[adr]` in `.trusty-mpm.toml` rejected (`ProjectLevelConfig` is `deny_unknown_fields`; bash parses no TOML). |
| D6 | Auto-update is detect-then-PR. Header: `# trusty-adr-check`, `# tm-adr-version: <skill metadata.version>`, `# body-sha256: <hex of the body with \r stripped>`. Five outcomes from header comparison: `up to date`, `tm behind`, `update available` (older and pristine → offer PR), `locally edited` (never overwritten), `unmanaged`. Hash tool order: `shasum -a 256`, `sha256sum`, `openssl dgst -sha256`. |
| D7 | Wiring: `.github/workflows/adr-check.yml` when `.github/workflows/` exists (push on the default branch only, `concurrency` group, `timeout-minutes`, `paths` filter on `<ADR_DIR>/**`); otherwise a run-it note. |
| D8 | `tm-adr.md` is decoupled from trusty-tools: one absolute DOC-46 URL, no DOC-38/DOC-43, no "0013", generic examples, one template copy. |
| D9 | The script's supported floor is bash 3.2. CI runs `--self-test` on the runner's bash (3.2 or newer); a static check rejects bash-4-only constructs. |

### Ordering

Phase 2 makes trusty-tools consume the script Phase 1 ships, byte-for-byte
from the deployed skill tier. It cannot start until Phase 1 is in a published
content release and deployed on the implementing host, and until Phase 1's
scratch-deploy check (AC9) is clean — a deploy-chain defect it exposes is a
separate crate issue that blocks Phase 2, not Phase 1 work.

<!-- phases:start -->
| # | Phase | Issue | State | Gate |
|---|-------|-------|-------|------|
| 1 | tm-adr 2.1.0 ships init mode, portable check_adr.sh and CI self-test | TBD | open | None |
| 2 | trusty-tools adopts the shipped check_adr.sh | TBD | open | Phase 1 in a published `content-v*` release and deployed on the implementing host; Phase 1 AC9 clean or its crate issue closed |
<!-- phases:end -->

### Phase: tm-adr 2.1.0 ships init mode, portable check_adr.sh and CI self-test

Gate: None.

Ships the skill, the script and the CI self-test. Nothing in this repository
consumes the script yet; that is Phase 2, so this phase can be reviewed and
reverted on its own. Decisions D1–D9 are in the tracker.

#### Scope

- `content/skills/tm-adr.md` (entry file keeps its path), `metadata.version` 2.1.0: init mode, detection, five-outcome update check, fixed `documentation` brief, decoupled wording.
- `content/skills/tm-adr/references/`: `README.md`, `template.md`, `INDEX.md` (one row), `0001-record-architecture-decisions.md` (Accepted, full metadata, Related Decisions "No prior decisions to vet against."), `adr-check.yml`.
- `content/skills/tm-adr/scripts/check_adr.sh`: bash 3.2-clean port of `scripts/check_adr.sh` with flags, env mirrors, three-line header, `--verify-self`, `--self-test` from heredoc fixtures.
- `content/manifest.toml`: member via `check_content.py sync`; `[bundle].version` minor bump, value chosen at implementation against the latest `content-v*` tag. `content/changelog.d/<pr>-tm-adr-init.md`.
- `.github/workflows/doc-numbers.yml`: `content/skills/tm-adr/**` in `paths`; steps running the content copy's `--self-test`, `--verify-self`, the bash-4-construct grep, and a header-version == `metadata.version` check.

#### Acceptance criteria

- **AC1** `git diff --name-only origin/main` lists only `content/**` and `.github/workflows/doc-numbers.yml`; `content/skills/tm-adr.md` is modified, not renamed. `crates/trusty-mpm/src/assets/framework-manifest.toml` and `content/skills/tm-init.md` are unchanged. `python3 scripts/check_content.py tree`, `python3 scripts/check_content.py bump` and `bash scripts/check_changelog_fragment.sh --staged` exit 0.
- **AC2** `bash content/skills/tm-adr/scripts/check_adr.sh --self-test` exits 0 under bash 3.2 or newer, and CI runs it on the runner's bash in `doc-numbers.yml`. `grep -cE 'mapfile|readarray|declare -A|\|&|;;&|\$\{[a-zA-Z_]+(,,|\^\^)' content/skills/tm-adr/scripts/check_adr.sh` prints 0 and runs as a CI step. Fixtures come from heredocs into `mktemp -d`; `grep -c 'docs/adr'` inside the self-test function prints 0. ADR filenames are enumerated with `find … -print0` and `read -r -d ''`. A missing directory exits 2; an empty one exits 0 with one notice line.
- **AC3** Precedence is flag > `ADR_CHECK_*` env > `.adr-dir` (dir only) > default. Self-test cases: fixture `0002` without metadata passes with `--metadata-from 0003` and fails without it; `ADR_CHECK_METADATA_FROM=0003` with `--metadata-from 0001` fails `0002` (flag wins); `--no-index` skips the INDEX checks; without `--crate-decisions` the scan summary reports one directory, with `--crate-decisions 'docs/*/decisions'` it reports every match. `grep -rc 'adr-check.conf' content/skills/tm-adr/ content/skills/tm-adr.md` prints 0 everywhere.
- **AC4** The header is exactly three lines: `# trusty-adr-check`, `# tm-adr-version: 2.1.0`, `# body-sha256: <hex>`; `tm-adr-version` equals `metadata.version` in `content/skills/tm-adr.md`. `--verify-self` exits 0 on the pristine file, 1 after one body byte changes, 0 on a copy with `\r\n` line endings (CR stripped before hashing). With `shasum` absent from `PATH` it uses `sha256sum`; with both absent, `openssl dgst -sha256`; with none, exit 3 naming the missing tool.
- **AC5** `content/skills/tm-adr.md` carries each statement below, grep-checkable: (a) init runs only on an explicit user request and is never invoked by `tm-init`; (b) the D3 marker list, each followed by "stop and report"; (c) the only source path is the base directory injected at skill load — `grep -c 'claude-config/skills'` and `grep -c '\.claude/skills'` print 0; (d) init refuses with a named reason when `<base>/scripts/check_adr.sh` is absent; (e) the five outcome names from D6; (f) header-only comparison, diff only on request; (g) the project copy is located by `git grep -l '^# trusty-adr-check'`; (h) `.trusty-mpm.toml` `agent_worktree = false` → dispatch with `isolation: "worktree"`, else stop with the reason that `.sh` is in `pm_guard.rs::SOURCE_CODE_EXTENSIONS`; (i) the CLAUDE.md pointer waits for a second confirmation; (j) 0 hits for `DOC-38`, `DOC-43`, `0013`, `MSRV`, `this repo`; exactly 1 hit for `DOC-46`, as `https://github.com/bobmatnyc/trusty-tools/blob/main/docs/specs/DOC-46-adr-standard.md`; `grep -c '^## Consequences'` prints 0 (template body only in `references/template.md`).
- **AC6** The `documentation` brief in `tm-adr.md` is a literal command list — `cp` for every file, `cmp` on the script, `chmod +x`, `bash <copy> --verify-self`, `bash <copy> --self-test`, `bash <copy>`, `git add <path>` per file, `git commit -m` — and `grep -c 'cat <<'` prints 0. The brief tells `version-control`, not `documentation`, to push and open the PR.
- **AC7** `doc-numbers.yml` has no new workflow beside it; its Doc numbers run on the PR is green with the new steps visible in the run log (link in the PR body).
- **AC8** Dry runs on scratch fixture repos, run by the implementing session, evidence in the PR body:
  - DR-1 fresh repo with `.github/workflows/`: one PR adds the O1 file set; `cmp <ADR_DIR>/check_adr.sh <base>/scripts/check_adr.sh` exits 0; `git ls-files -s <ADR_DIR>/check_adr.sh` shows `100755`; `adr-check.yml` has `push.branches: [<default>]`, a `concurrency` group whose `cancel-in-progress` is false on the default branch, `timeout-minutes`, and `paths` covering `<ADR_DIR>/**`. Evidence: the PR's `git log --stat` and `git ls-files -s`.
  - DR-2 fresh repo without `.github/`: the PR adds no workflow; the run-it note is in README.md and the PR body. Evidence: PR file list.
  - DR-3 repo containing only `.log4brains.yml`: nothing written, transcript names the marker. Evidence: transcript and empty `git status --porcelain`.
  - DR-4 repo with a differing `docs/adr/README.md`: that file is byte-identical after (`cmp` before/after) and named in the transcript; absent files are written.
  - DR-5 on the DR-1 result: pristine → `up to date`; `tm-adr-version` lowered → `update available`; one body byte changed → `locally edited`, nothing written; header removed → `unmanaged`; `tm-adr-version` raised → `tm behind`, nothing written. Evidence: transcript lines.
  - DR-6 `.trusty-mpm.toml` with `agent_worktree = false`: the dispatch carries `isolation: "worktree"` or the transcript stops with the named reason; `git status --porcelain` in the main checkout is empty.
  - DR-7 the bootstrapped `<ADR_DIR>` passes `bash <ADR_DIR>/check_adr.sh` with exit 0.
- **AC9** Scratch deploy: build the bundle from the PR branch (`bash scripts/package_content.sh --version <bundle version>`), install it to a scratch home from a cwd outside any trusty-tools checkout (`tm content install --from <bundle>` then `tm install`), then `cmp <scratch tier>/tm-adr/scripts/check_adr.sh content/skills/tm-adr/scripts/check_adr.sh` exits 0 for every tier that holds `tm-adr/SKILL.md`. Commands and output in the PR body. A failure is a separate crate issue (first suspects `core/skill_source.rs::materialize_skill_artifacts`, `skills/deployer.rs`), filed as blocked-by on Phase 2; it is not fixed in this PR.

#### Non-goals

`tm-init.md`; the roster and `framework-manifest.toml`; a `tm adr` subcommand; this repository's `scripts/check_adr.sh` and `docs/adr/` (Phase 2); a settings file; pre-commit/Makefile/justfile/package.json wiring; the exec bit in the bundle; `tm doctor` rows; non-`.md` orphan pruning.

#### Risk

`documentation` runs on haiku and re-types files it Writes; `cp` + `cmp` is
the only path to a byte-identical script. CRLF checkouts would break the
digest; `.gitattributes` and CR stripping absorb it. The deploy chain may drop
the `.sh` extra (AC9); finding that here, before any project depends on it, is
the point of the check.

### Phase: trusty-tools adopts the shipped check_adr.sh

Gate: Phase 1 in a published `content-v*` release and deployed on the implementing host; Phase 1 AC9 clean or its crate issue closed.

Replaces this repository's `scripts/check_adr.sh` with the byte-identical
deployed skill copy, so trusty-tools is the first consumer of what Phase 1
ships and `/tm-adr` here stops reporting `unmanaged`. Decisions D5, D6 and D9
are in the tracker.

#### Scope

- `scripts/check_adr.sh`: the deployed `tm-adr/scripts/check_adr.sh`, mode 100755.
- `.github/workflows/doc-numbers.yml`: self-test step becomes `bash scripts/check_adr.sh --self-test`; new `bash scripts/check_adr.sh --verify-self` step; gate becomes `bash scripts/check_adr.sh --metadata-from 0014 --crate-decisions 'docs/*/decisions'`.
- `docs/specs/DOC-46-adr-standard.md`: `scripts/check_adr_selftest.sh` → `bash scripts/check_adr.sh --self-test`.

#### Acceptance criteria

- **AC1** `cmp scripts/check_adr.sh ~/.trusty-tools/trusty-mpm/claude-config/skills/tm-adr/scripts/check_adr.sh` exits 0 and `bash scripts/check_adr.sh --verify-self` exits 0; both outputs in the PR body.
- **AC2** `git ls-files -s scripts/check_adr.sh` shows mode `100755` (the bundle copy is 0644).
- **AC3** `bash scripts/check_adr.sh --metadata-from 0014 --crate-decisions 'docs/*/decisions'` exits 0 on the current corpus and its scan summary counts more than one directory; the same command without `--metadata-from` exits 1 naming ADR-0001 through ADR-0013; without `--crate-decisions` the summary counts one directory.
- **AC4** `doc-numbers.yml` runs `--self-test`, `--verify-self`, then the gate, in that order; no new workflow; the Doc numbers run on the PR is green (link in the PR body).
- **AC5** `grep -c check_adr_selftest docs/specs/DOC-46-adr-standard.md` prints 0 and `grep -c 'check_adr.sh --self-test' docs/specs/DOC-46-adr-standard.md` prints at least 1.
- **AC6** `/tm-adr` run in this checkout reports `up to date` (transcript line in the PR body).

#### Non-goals

Changing `docs/adr/` content or numbering; a `.adr-dir` or `adr-check.yml` in this repository (`doc-numbers.yml` stays the gate); any crate change.

#### Risk

The ported script may differ in behaviour from the current one on this corpus
— AC3's three runs are the check. A deploy-chain defect found by Phase 1 AC9
means the deployed copy is not what the content holds; the gate refuses to
start until that issue is closed.

#### Gate procedure

The gate line above is checked by these steps, in order.

1. The Phase 1 PR reports `MERGED` (`gh pr view <pr> --json state`).
2. A content release contains it: `git tag -l 'content-v*' --contains <squash sha>` is non-empty (the tag Phase 1 chose).
3. The skill is deployed on the implementing host. `tm content update` only re-pins the bundle; skill tiers deploy on `tm install` or a session launch. When `tm content status` prints `source: dev`: fast-forward the main checkout to `origin/main`, then `tm install`. When it prints `source: bundle`: `tm content update`, then `tm install`. Then `cmp content/skills/tm-adr/scripts/check_adr.sh ~/.trusty-tools/trusty-mpm/claude-config/skills/tm-adr/scripts/check_adr.sh` exits 0.
4. Phase 1 AC9 was clean, or the crate issue it filed is closed.

### Deferred

<!-- deferred:start -->
| Item | Why deferred | Where it went |
|------|--------------|---------------|
| `.adr-check.conf` settings file and parser | Only trusty-tools needs non-default settings; flags cover it | unscheduled |
| pre-commit / Makefile / justfile / package.json wiring | Untested grammars; GitHub Actions covers the common case | unscheduled |
| Executable bit on deployed skill extras (`scripts/package_content.sh` writes mode 0644) | Crate change, rung 4; `bash <copy>` and `chmod +x` in the brief cover it | unscheduled |
| `tm doctor` stale-copy row for managed scripts | Crate change, rung 3 | unscheduled |
| Orphan pruning of non-`.md` skill files (`core/skill_source.rs::prune_orphaned_skill_files`) | Crate change, rung 3 | unscheduled |
| `agent_worktree = false` handling beyond explicit `isolation: "worktree"` or stop-with-reason | Decide after DR-6 shows whether the override is reliable | unscheduled |
<!-- deferred:end -->

## Maintenance

The phases block is regenerated from child issue state — never patched by
hand. The deferred block is amended deliberately, only when scope leaves the
plan or a deferred item lands. The follow-ups block is appended as findings
surface; at most two of them per phase become standalone issues, and only at
severity HIGH or above. The sections above the markers are authored once.

Comments on the tracker are limited to changes of plan: a decision made, a
phase re-scoped, a risk realized, an item deferred, a gate lifted. Progress
belongs in the phases block; evidence belongs in the phase issue or its PR.

Issue numbers are not written back here. The `TBD` cells in the phases block
above are this document's own copy; the tracker's block supersedes them once
the epic is filed.

## Filing

Metadata `tm issue epic create` does not read from the plan. Applied by the
filing session.

### Tracker

- Title at creation: `[EPIC] /tm-adr init bootstraps a portable ADR gate in any project`; retitled `[EPIC <N>] …` after read-back.
- Labels: `epic`, `dx`, `area:agent-prompts`, `ws/tm-trusty-tools-adr-init`.
- Milestone: `trusty-mpm 1.9.0`. Project #45, Status Backlog.
- Comment in the same dispatch: `no-component-label: no crate owns content/skills/tm-adr/ or .github/workflows/`.
- Close rule: both phases closed; closing comment maps O1–O3 to evidence.

### Phase 1

- Title: `[EPIC_<N> PHASE_1] tm-adr 2.1.0 ships init mode, portable check_adr.sh and CI self-test`.
- Labels: `enhancement`, `dx`, `area:agent-prompts`, `ws/tm-trusty-tools-adr-init`. Milestone: parent's. Project #45, Backlog. Parent `<N>`.
- Rung 3: ships executable code into this repo's CI and every bootstrapped repo's CI, and edits `doc-numbers.yml`; no crate source changes.
- Close rule: Merged.
- PR title: `feat(tm-adr): [EPIC_<N> PHASE_1] ship /tm-adr init and a portable check_adr.sh`.
- Comment in the same dispatch: `no-component-label: no crate owns content/skills/tm-adr/ or .github/workflows/`.

### Phase 2

- Title: `[EPIC_<N> PHASE_2] trusty-tools adopts the shipped check_adr.sh`.
- Labels: `chore`, `ci`, `dx`, `area:ci`, `ws/tm-trusty-tools-adr-init`. Milestone: parent's. Project #45, Backlog. Parent `<N>`; blocked-by `<P1>`.
- Rung 3: behaviour change in a gate script, verified by its self-test and the Doc numbers workflow; no crate touched.
- Close rule: Merged.
- PR title: `chore(adr): [EPIC_<N> PHASE_2] run the shipped check_adr.sh in doc-numbers.yml`.
- Comment in the same dispatch: `no-component-label: no crate owns scripts/, .github/workflows/ or docs/specs/`.

### Sequence

1. Commit this document to `origin/main`; record the permalink SHA.
2. Create the label `ws/tm-trusty-tools-adr-init` if it does not exist; run `tm issue seed-labels`.
3. `tm issue epic create --from docs/research/adr-bootstrap/plan.md --milestone "trusty-mpm 1.9.0" --component dx --project 45 --session tm-trusty-tools-adr-init --phase-type enhancement`.
4. Post-create edits: Phase 2 type `chore`; `area:*` on all three; `ci` on Phase 2; blocked-by P1 → P2 if not set; confirm the `ws/` label landed.
5. `no-component-label` comment on each issue.
6. `tm issue audit` on all three; `tm issue epic sync <N>`.
