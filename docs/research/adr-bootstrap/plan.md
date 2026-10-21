# tm-adr bootstraps and maintains a generic ADR setup in any project

Plan document for the ADR bootstrap epic. The `tm-epic` skill governs the
tracker and phase shape. `tm issue epic create --from` reads the `## Epic plan`
section; the H1 above becomes the tracker's outcome. Filing metadata the
parser does not read sits under `## Filing` at the end.

## Context

This plan supersedes epic
[#9924](https://github.com/bobmatnyc/trusty-tools/issues/9924) (phases
[#9925](https://github.com/bobmatnyc/trusty-tools/issues/9925),
[#9926](https://github.com/bobmatnyc/trusty-tools/issues/9926)), closed as
not planned on 2026-10-20 because the owner revised the design after review.
A project that wants ADRs today copies trusty-tools' `docs/adr/` and
`scripts/check_adr.sh` by hand, and the copy drifts. `content/skills/tm-adr.md`
(2.0.0) hardcodes trusty-tools paths: `docs/<crate>/decisions/`,
`scripts/check_doc_numbers.sh`, relative DOC-38/DOC-43/DOC-46 links, "0013",
MSRV and crate examples. The owner ratified decisions D1–D15 on 2026-10-20;
the tracker carries them.

Amended 2026-10-20 after an impact verification of trusty-tools. The
amendments are in the Phase 1 and Phase 2 scope and criteria and in D5, D8,
D10 and D14. A further proposal (one failing self-test per existing
`check_adr.sh` rule, and a ruling on whether `--require-metadata-from` also
exempts Related Decisions) was considered and not adopted; the criteria keep
their current form.

Related issues:

- https://github.com/bobmatnyc/trusty-tools/issues/9132
- https://github.com/bobmatnyc/trusty-tools/issues/8619
- https://github.com/bobmatnyc/trusty-tools/issues/9055
- https://github.com/bobmatnyc/trusty-tools/issues/8389
- https://github.com/bobmatnyc/trusty-tools/issues/8758
- https://github.com/bobmatnyc/trusty-tools/issues/3171
- https://github.com/bobmatnyc/trusty-tools/issues/3173

## Epic plan

A project that wants ADRs today copies trusty-tools' `docs/adr/` and `scripts/check_adr.sh` by hand, and the copy drifts. After this epic, the `tm-adr` skill is generic: it carries the ADR standard, bootstraps `docs/adr/` with a self-verifying check script in any project as one PR, and reports on every later run whether the project's script copy is behind the deployed skill. trusty-tools consumes the same script and keeps only its own rules in DOC-46.

Supersedes [#9924](https://github.com/bobmatnyc/trusty-tools/issues/9924).

### Outcomes

- **O1** `/tm-adr init` on a project with no ADR setup yields one PR that adds `docs/adr/{README.md,template.md,INDEX.md,0001-record-architecture-decisions.md,check_adr.sh}` (script mode 100755, byte-identical to the deployed skill copy), a `.gitattributes` `eol=lf` line for the script, and `.github/workflows/adr-check.yml` when `.github/workflows/` exists. On a project with any other ADR setup it writes nothing and names the marker it found. On an existing `docs/adr/` with no `NNNN-*.md` it writes only the missing files and names the files it kept.
- **O2** The shipped `check_adr.sh`, `adr_init.sh` and `adr_status.sh` run with no `tm` present under bash 4 or newer, refuse older bash with exit 2, pass their own `--self-test`, and take configuration from flags and `ADR_CHECK_*` env only. `check_adr.sh --verify-self` exits 1 after any one-byte body edit.
- **O3** `content/skills/tm-adr.md` and `content/skills/tm-adr/**` contain no trusty-tools path, spec number or example. The generic ADR standard lives in `content/skills/tm-adr/references/adr-standard.md` and nowhere else.
- **O4** trusty-tools runs the shipped script byte-for-byte from `scripts/check_adr.sh` through `doc-numbers.yml`, DOC-46 holds only trusty-tools rules plus a pointer to the skill standard, and `/tm-adr` in this checkout reports the copy `up to date`.

### Ratified decisions

| # | Decision |
|---|---|
| D1 | One skill. `tm-adr` keeps its name and entry file `content/skills/tm-adr.md` (the catalog reads only top-level `skills/<stem>.md`); `metadata.version` 2.0.0 → 2.1.0, shipped by a content release (`content-v*`, version chosen at implementation). No `/adr` skill, no rename, no `tm adr` subcommand; `content/skills/tm-init.md` and `framework-manifest.toml` untouched. Contents become generic: trusty-tools-only rules live in trusty-tools' `docs/adr/README.md` and `doc-numbers.yml` flags. The layout-ADR paragraph (tm-adr.md 2.0.0 lines 53–56) is removed. |
| D2 | Directory skill layout: siblings `content/skills/tm-adr/references/{adr-standard.md,template.md,README.md,INDEX.md,0001-record-architecture-decisions.md,adr-check.yml}` and `content/skills/tm-adr/scripts/{check_adr.sh,adr_init.sh,adr_status.sh}`. |
| D3 | Location: `docs/adr/` is fixed for every project. No override in bootstrap, no `.adr-dir` written, no directory flag or env in any script. All tm-adr steps (create, supersede, amend, numbering, init, status) use `docs/adr/`. `check_adr.sh` resolves the project root as the git toplevel, else the current directory; its self-test `cd`s into each fixture repo. |
| D4 | Seed: a bootstrapped project gets no trusty-tools ADRs. It gets one freshly written generic `0001-record-architecture-decisions.md` (Accepted, full metadata, Related Decisions "No prior decisions to vet against."), plus `README.md` (with a short summary of the standard for people without tm), `template.md`, `INDEX.md` (one row), `check_adr.sh` (mode 100755, byte-identical to the skill copy), a `.gitattributes` `eol=lf` line for the script, and `.github/workflows/adr-check.yml` when `.github/workflows/` exists (otherwise a run-it note in README and the PR body). |
| D5 | Standard: the generic ADR standard is `content/skills/tm-adr/references/adr-standard.md` — when to write an ADR, metadata fields, status lifecycle (Proposed, Accepted, Rejected, Superseded by, Amended by), claim-then-populate numbering, consistency-vetting protocol with verdict codes (Consistent, Extends, Supersedes, Conflict), governance (never delete or rewrite accepted ADRs). It is skill-only, never copied into projects, and is the single source. The skill text has zero mentions of DOC-46. trusty-tools' DOC-46 is cut to trusty-tools-only rules (its §6 CI gate, per-crate decisions dirs, the ADR 0001–0013 metadata exemption) plus a pointer to the skill file (Phase 2). The cut-down DOC-46 keeps `## 3.` and `## 4.` as numbered headings, each a short pointer to the skill file, because immutable ADRs cite them. |
| D6 | Detection: init stops and names the marker when it finds an ADR folder other than `docs/adr/` (`adr/`, `decisions/`, `architecture-decisions/`), any `NNNN-*.md` containing `## Decision`, `.adr-dir`, `.log4brains.yml`, or an ADR mention in CLAUDE.md. `docs/adr/` itself is never a marker: an existing `docs/adr/` with no `NNNN-*.md` is filled in — existing files kept byte-identical and named in the output, missing files written. Adopting a project that already has ADRs is deferred. |
| D7 | Delivery: every target-project write is one PR. The skill ships `scripts/adr_init.sh` and `scripts/adr_status.sh`; the dispatched agent runs one command in a worktree, `version-control` pushes and opens the PR. The CLAUDE.md pointer needs a second user confirmation. Init runs only on an explicit user request and is never invoked by tm-init. The source path is only the skill base directory injected at load; init refuses with a named reason if `<base>/scripts/check_adr.sh` is absent. If `.trusty-mpm.toml` has `agent_worktree = false`, dispatch with `isolation: "worktree"`, else stop with the reason: "tm treats `.sh` files as source code and blocks writing or committing them in the main checkout (ADR-0044); init must run in a worktree." No crate-internal names in skill text. |
| D8 | Update check: detect-then-PR. `adr_status.sh` locates copies by `git grep -l '^# trusty-adr-check' -- '*.sh' '*.yml'`, compares each copy with the skill file of the same basename, reports each copy on its own line, and reports `up to date` only when all copies are. Five outcomes: `up to date`, `tm behind`, `update available` (older and pristine → offer PR), `locally edited` (never overwritten), `unmanaged`. Header-only comparison; diff only on request. A copy under `content/skills/tm-adr/` is reported as `source` and never offered an update PR. |
| D9 | Versioning: only `check_adr.sh` and `adr-check.yml` carry a version header: `# trusty-adr-check`, `# adr-check-version: <script's own version>`, `# body-sha256: <hex of body with \r stripped>`. The version is the script's own, bumped only when its body changes — not the skill's `metadata.version`. `template.md` and `README.md` are project-owned and unversioned. `adr-check.yml` is rendered at init (default branch), so the hash covers the rendered body. Hash tool order: `shasum -a 256`, `sha256sum`, `openssl dgst -sha256`; none → exit 3 naming the missing tool. |
| D10 | Script configuration: flags with `ADR_CHECK_*` env mirrors; no settings file; no `[adr]` in `.trusty-mpm.toml`. Flags renamed: `--extra-dirs <glob>` (was `--crate-decisions`), `--require-metadata-from <NNNN>` (was `--metadata-from`), `--no-index`. Precedence flag > env > default. `--extra-dirs` applies only the README-listing check to each matched directory (every `NNNN-*.md` is named in that directory's `README.md`), not the full ADR rule set. |
| D11 | Bash: no minimum-version port; bash 4+ is assumed. Each shipped script begins by exiting 2 with `needs bash 4+ (macOS: brew install bash)` when `BASH_VERSINFO[0] < 4`. README states bash 4+. (The current `scripts/check_adr.sh` uses `mapfile` and `declare -A`; stock macOS `/bin/bash` is 3.2.) |
| D12 | CI wiring for bootstrapped projects: `adr-check.yml` runs on `pull_request` and on push to the default branch, with a `concurrency` group and `timeout-minutes`, and no `paths` filter (a paths-filtered required check never reports on unrelated PRs). |
| D13 | Self-tests: `check_adr.sh --self-test` (heredoc fixtures in `mktemp -d`); `adr_init.sh --self-test` covering fresh-repo-with-workflows, fresh-repo-without-.github, `.log4brains.yml`-only, existing-`docs/adr`-with-differing-README, and bootstrapped-dir-passes-check; `adr_status.sh --self-test` covering the five outcomes. The worktree-dispatch case (`agent_worktree = false`) stays a manual transcript check in the PR body. |
| D14 | trusty-tools CI (`doc-numbers.yml`, no new workflow) runs the content copy's `--self-test` for all three scripts and `--verify-self`, and fails when the `check_adr.sh` or `adr-check.yml` body hash differs from the merge base while `adr-check-version` is unchanged. The comparison base is `git merge-base origin/${BASE_REF} HEAD` after an explicit fetch of the base ref; on `push`, or when the base lacks the file, the step prints `drift check: skipped (<reason>)` and exits 0; any other git error fails the step. |
| D15 | trusty-tools adoption (Phase 2): keeps its copy at `scripts/check_adr.sh` (mode 100755); `doc-numbers.yml` `cmp`s it against `content/skills/tm-adr/scripts/check_adr.sh` on every PR; gate invocation `bash scripts/check_adr.sh --require-metadata-from 0014 --extra-dirs 'docs/*/decisions'`. After any later script change, `/tm-adr` here reports `update available` until the next content release — expected. |

### Ordering

Phase 2 makes trusty-tools consume the script Phase 1 ships and cuts DOC-46
to a pointer at the standard Phase 1 creates. It cannot start before Phase 1
is merged, because the content copy it `cmp`s against and the
`adr-standard.md` it points at do not exist until then. One Phase 2 criterion
(`/tm-adr` reports `up to date`) also needs Phase 1 in a published
`content-v*` release and deployed on the implementing host, because the
status check compares the project copy with the deployed skill tier. A
deploy-chain defect found by Phase 1's scratch-deploy check is a separate
crate issue that blocks Phase 2, not Phase 1 work.

<!-- phases:start -->
| # | Phase | Issue | State | Gate |
|---|-------|-------|-------|------|
| 1 | tm-adr 2.1.0 ships generic content, adr-standard.md, init/status scripts and CI self-tests | TBD | open | None |
| 2 | trusty-tools adopts the shipped check_adr.sh and cuts DOC-46 to a pointer | TBD | open | Phase 1 merged; for AC8 (`/tm-adr` reports `up to date`) only, Phase 1 in a published `content-v*` release and deployed on the implementing host |
<!-- phases:end -->

### Phase: tm-adr 2.1.0 ships generic content, adr-standard.md, init/status scripts and CI self-tests

Gate: None.

Ships the generic skill, the standard, the three scripts and the CI
self-tests. Nothing in this repository consumes the scripts yet; that is
Phase 2, so this phase is reviewed and reverted on its own. Rung 3: it ships
executable code into this repository's CI and into every bootstrapped
repository, with no crate source change. Decisions D1–D14 are in the tracker.

#### Scope

- `content/skills/tm-adr.md` (entry file keeps its path), `metadata.version` 2.1.0: generic create/supersede/amend/numbering steps on `docs/adr/`, the init and status procedures, detection (D6), the five-outcome update check (D8), the worktree rule (D7), the layout-ADR paragraph removed (D1); a step telling the agent to read `docs/adr/README.md` for project conventions before it creates, supersedes or amends an ADR.
- `content/skills/tm-adr/references/`: `adr-standard.md` (D5), `README.md`, `template.md`, `INDEX.md` (one row), `0001-record-architecture-decisions.md` (D4), `adr-check.yml` (D9, D12).
- `content/skills/tm-adr/scripts/`: `check_adr.sh` (D3, D9, D10, D11, D13), `adr_init.sh` (D4, D6, D7, D11, D13), `adr_status.sh` (D8, D11, D13).
- `content/manifest.toml`: members via `python3 scripts/check_content.py sync`; `[bundle].version` minor bump, value chosen at implementation against the latest `content-v*` tag. `content/changelog.d/<pr>-tm-adr-init.md`.
- `content/skills/tm-capabilities/references/skills.md`: regenerated in the same PR, so the capabilities drift check does not fail the next crate PR.
- `.github/workflows/doc-numbers.yml`: steps running the content copy's three `--self-test`s, `--verify-self`, and the body-hash-versus-version drift check (D14). The drift step fetches the base ref and compares against the merge base (AC11). Both `paths:` lists gain `content/skills/tm-adr/**` (amendment A1).
- `content/skills/tm-adr/scripts/adr_status.sh` (amendment A8): locates copies with `git grep -l '^# trusty-adr-check' -- '*.sh' '*.yml'`, compares each copy with the skill file of the same basename, reports a copy under `content/skills/tm-adr/` as `source` and never offers it an update PR; `--self-test` adds a case for this.
- `check_adr.sh --extra-dirs` (amendment A5): applies only the README-listing check to each matched directory (every `NNNN-*.md` is named in that directory's `README.md`), not the full ADR rule set.

#### Acceptance criteria

- **AC1** `git diff --name-only origin/main...HEAD` lists only paths under `content/` and `.github/workflows/doc-numbers.yml`. `git diff --name-status origin/main...HEAD -- content/skills/tm-adr.md` prints `M` (modified, not renamed). `crates/trusty-mpm/src/assets/framework-manifest.toml` and `content/skills/tm-init.md` are absent from the diff. `python3 scripts/check_content.py tree`, `python3 scripts/check_content.py bump` and `bash scripts/check_changelog_fragment.sh --staged` exit 0. `bash scripts/check_capabilities.sh` exits 0 on the PR branch. `bash scripts/check_test_pointers.sh` exits 0. `grep -axv '.*' content/skills/tm-adr/scripts/* content/skills/tm-adr/references/*` prints nothing (every shipped file is valid UTF-8; a bad byte would fail every `tm install` and session launch).
- **AC2** Generic text, grep-checkable over `content/skills/tm-adr.md` and `content/skills/tm-adr/**`: `grep -rc` prints 0 in every file for each of `DOC-46`, `DOC-38`, `DOC-43`, `0013`, `MSRV`, `this repo`, `check_doc_numbers`, `docs/<crate>/decisions`, `claude-config/skills`, `.claude/skills`, `pm_guard`, `adr-check.conf`, `[adr]`. `.adr-dir` appears only on lines of the D6 marker list in `tm-adr.md` and in the detection code of `adr_init.sh`; `grep -rn '\.adr-dir' content/skills/tm-adr/` shows no write of that file. `grep -c '#8382' content/skills/tm-adr.md` prints 0. `grep -c 'version: "2.1.0"' content/skills/tm-adr.md` prints 1.
- **AC3** `content/skills/tm-adr/references/adr-standard.md` has one `## ` heading for each of: when to write an ADR, metadata fields, status lifecycle, numbering, consistency vetting, governance. `grep -c` on that file prints at least 1 for each of `Proposed`, `Accepted`, `Rejected`, `Superseded by`, `Amended by`, `Consistent`, `Extends`, `Supersedes`, `Conflict`, `claim`. `adr_init.sh` never writes `adr-standard.md` into a project: the self-test's bootstrapped tree has no such file (`test ! -e docs/adr/adr-standard.md`).
- **AC4** Location is fixed: `grep -c 'docs/adr/' content/skills/tm-adr.md` prints at least 1, and no script carries a directory flag or env — `grep -cE -- '--adr-dir|ADR_CHECK_DIR|ADR_DIR=' content/skills/tm-adr/scripts/*.sh` prints 0 for each. `check_adr.sh` contains `git rev-parse --show-toplevel` with a fallback to `pwd`; its self-test `cd`s into each fixture before invoking the script.
- **AC5** Header and self-verification: the first three lines after the shebang of `check_adr.sh` are `# trusty-adr-check`, `# adr-check-version: <script version>`, `# body-sha256: <hex>`; `adr-check.yml` carries the same three lines as YAML comments. `grep -c 'metadata.version' content/skills/tm-adr/scripts/check_adr.sh` prints 0. `bash content/skills/tm-adr/scripts/check_adr.sh --verify-self` exits 0 on the pristine file, 1 after one body byte changes, 0 on a copy with `\r\n` line endings. With `shasum` absent from `PATH` it uses `sha256sum`; with both absent, `openssl dgst -sha256`; with none, exit 3 naming the missing tool. Evidence: the self-test prints one line per case.
- **AC6** Bash guard: `grep -c 'needs bash 4+ (macOS: brew install bash)'` prints 1 in each of the three scripts, and the `BASH_VERSINFO` check precedes every other statement after `set`. Running each script under `/bin/bash` on macOS (3.2) exits 2 with that message. `grep -c 'bash 4' content/skills/tm-adr/references/README.md` prints at least 1.
- **AC7** Flags and precedence: `grep -cE -- '--crate-decisions|--metadata-from ' content/skills/tm-adr/scripts/check_adr.sh` prints 0; `--extra-dirs`, `--require-metadata-from`, `--no-index` and their mirrors `ADR_CHECK_EXTRA_DIRS`, `ADR_CHECK_REQUIRE_METADATA_FROM`, `ADR_CHECK_NO_INDEX` each appear. Self-test cases: fixture `0002` without metadata passes with `--require-metadata-from 0003` and fails without it; `ADR_CHECK_REQUIRE_METADATA_FROM=0003` with `--require-metadata-from 0001` fails `0002` (flag wins); `--no-index` skips the INDEX checks; without `--extra-dirs` the scan summary counts one directory, with `--extra-dirs 'docs/*/decisions'` it counts every match. A missing `docs/adr/` exits 2; an empty one exits 0 with one notice line. With `--extra-dirs`, each matched directory gets only the README-listing check: a fixture directory with an `NNNN-*.md` not named in its `README.md` fails, and the same directory with an otherwise non-conforming ADR body (no metadata) passes.
- **AC8** Self-tests (D13): `bash content/skills/tm-adr/scripts/check_adr.sh --self-test`, `bash content/skills/tm-adr/scripts/adr_init.sh --self-test` and `bash content/skills/tm-adr/scripts/adr_status.sh --self-test` each exit 0 and print one line per case. Fixtures come from heredocs into `mktemp -d`; `grep -c '/Users\|/home' content/skills/tm-adr/scripts/*.sh` prints 0. The init cases are: fresh repo with `.github/workflows/` (the O1 file set; `cmp docs/adr/check_adr.sh <base>/scripts/check_adr.sh` exits 0; `git ls-files -s docs/adr/check_adr.sh` shows `100755`; `adr-check.yml` present); fresh repo without `.github/` (no workflow; run-it note in README); `.log4brains.yml` only (nothing written, output names the marker, `git status --porcelain` empty); existing `docs/adr/` with a differing `README.md` (that file byte-identical after and named in the output; missing files written); the bootstrapped tree passes `bash docs/adr/check_adr.sh` with exit 0. The status cases are the five D8 outcomes: pristine → `up to date`; `adr-check-version` lowered → `update available`; one body byte changed → `locally edited`, nothing written; header removed → `unmanaged`; `adr-check-version` raised → `tm behind`, nothing written. Each self-test is shown able to fail in the PR body: one fixture mutated, the run goes red, the fixture restored. The `adr_status.sh` self-test adds a case for amendment A8: a copy under `content/skills/tm-adr/` is reported as `source`, and no update PR is offered for it.
- **AC9** `content/skills/tm-adr.md` carries each statement below, grep-checkable: (a) init runs only on an explicit user request and is never invoked by `tm-init`; (b) the D6 marker list, each item followed by "stop and name the marker"; (c) the only source path is the base directory injected at skill load; (d) init refuses with a named reason when `<base>/scripts/check_adr.sh` is absent; (e) the five outcome names from D8; (f) header-only comparison, diff only on request; (g) copies are located by `git grep -l '^# trusty-adr-check'` and each is reported on its own line; (h) the `agent_worktree = false` rule with the D7 stop reason verbatim; (i) the CLAUDE.md pointer waits for a second confirmation; (j) `version-control`, not the dispatched agent, pushes and opens the PR; (k) `docs/adr/` as the one location for every step. `grep -c '^## Consequences' content/skills/tm-adr.md` prints 0 (the template body lives only in `references/template.md`). (l) before creating, superseding or amending an ADR the agent reads `docs/adr/README.md` for project conventions (grep-checkable statement).
- **AC10** Seed content: `grep -c 'No prior decisions to vet against.' content/skills/tm-adr/references/0001-record-architecture-decisions.md` prints 1, its status line reads `Accepted`, and `grep -c 'trusty-tools' content/skills/tm-adr/references/*.md` prints 0 for every file. `INDEX.md` has exactly one data row. `adr-check.yml` has `on.pull_request`, `on.push.branches` with one placeholder the init renders to the default branch, a `concurrency` group, `timeout-minutes`, and `grep -c 'paths' content/skills/tm-adr/references/adr-check.yml` prints 0.
- **AC11** `doc-numbers.yml` gains no sibling workflow; it runs the three content `--self-test`s, `--verify-self`, and the D14 drift step (fails when the `check_adr.sh` or `adr-check.yml` body hash differs from the merge base while `adr-check-version` is unchanged). The job fetches `+refs/heads/${BASE_REF}:refs/remotes/origin/${BASE_REF}` before the drift step and compares HEAD against `git merge-base origin/${BASE_REF} HEAD`. On `push`, and when the base lacks the file, the step prints `drift check: skipped (<reason>)` and exits 0. The PR run log shows the base SHA. Any other git error fails the step. The drift step is shown able to fail in the PR body: one body byte changed without a version bump, the step goes red, the byte restored. The Doc numbers run on the PR is green with the new steps visible in the run log (link in the PR body).
- **AC12** Scratch deploy: build the bundle from the PR branch (`bash scripts/package_content.sh --version <bundle version>`), install it to a scratch home from a cwd outside any trusty-tools checkout (`tm content install --from <bundle>` then `tm install`), then `cmp <scratch tier>/tm-adr/scripts/<name>.sh content/skills/tm-adr/scripts/<name>.sh` for each of the three scripts and `cmp <scratch tier>/tm-adr/references/<name> content/skills/tm-adr/references/<name>` for each reference exit 0 for every tier that holds `tm-adr/SKILL.md`. Commands and output in the PR body. A failure is a separate crate issue filed as blocked-by on Phase 2; it is not fixed in this PR.
- **AC13** The PR body carries the output of a local `cargo test -p trusty-mpm --lib core::bundle::tests core::skill_source::tests core::framework_content::tests` run from the PR branch, with its `test result:` line, because CI skips crate asset tests for a content-only diff.
- **AC14** `grep -c 'content/skills/tm-adr/\*\*' .github/workflows/doc-numbers.yml` prints 2 (amendment A1).

#### Non-goals

`tm-init.md`; the roster and `framework-manifest.toml`; a `/adr` skill or a `tm adr` subcommand; this repository's `scripts/check_adr.sh`, `docs/adr/` and DOC-46 (Phase 2); an adopt mode for projects with existing ADRs; a settings file or `[adr]` table; pre-commit/Makefile/justfile/package.json wiring; the exec bit in the bundle; `tm doctor` rows; non-`.md` orphan pruning; bash 3.2 support.

#### Risk

Agents re-type files they Write, so `adr_init.sh` copies the script with `cp` and proves it with `cmp`; that is the only path to a byte-identical copy. CRLF checkouts would break the digest; `.gitattributes` and CR stripping absorb it. The deploy chain may drop the `.sh` extras or the references (AC12); finding that here, before any project depends on them, is the point of the check. The init self-test cannot cover the worktree dispatch rule (D7); the PR body carries that transcript.

### Phase: trusty-tools adopts the shipped check_adr.sh and cuts DOC-46 to a pointer

Gate: Phase 1 merged; for AC8 (`/tm-adr` reports `up to date`) only, Phase 1 in a published `content-v*` release and deployed on the implementing host.

Replaces this repository's `scripts/check_adr.sh` with the content copy,
makes `doc-numbers.yml` prove the two stay byte-identical, and cuts DOC-46 to
the trusty-tools-only rules plus a pointer to the skill standard. trusty-tools
is then the first consumer of what Phase 1 ships, and `/tm-adr` here stops
reporting `unmanaged`. Rung 3: a gate script and a spec change, verified by
the script's self-test and the Doc numbers workflow; no crate touched.
Decisions D5, D9, D10, D14 and D15 are in the tracker.

#### Scope

- `scripts/check_adr.sh`: replaced by `content/skills/tm-adr/scripts/check_adr.sh`, mode 100755.
- `.github/workflows/doc-numbers.yml`: `cmp scripts/check_adr.sh content/skills/tm-adr/scripts/check_adr.sh`, then `bash scripts/check_adr.sh --self-test`, then `bash scripts/check_adr.sh --verify-self`, then the gate `bash scripts/check_adr.sh --require-metadata-from 0014 --extra-dirs 'docs/*/decisions'`, in that order.
- `scripts/check-ci-helpers-selftest.sh` (amendment A3): lines 649-650 assert exactly one `run: bash scripts/check_adr.sh` line in `doc-numbers.yml`; update that assertion to match the new steps (the expected count, or a narrower pattern such as `run: bash scripts/check_adr.sh --require-metadata-from`).
- `docs/specs/DOC-46-adr-standard.md`: cut to trusty-tools rules (§6 CI gate, per-crate decisions dirs, the ADR 0001–0013 metadata exemption) plus a pointer to `content/skills/tm-adr/references/adr-standard.md`; its stale `scripts/check_adr_selftest.sh` reference (line 237 at the time of writing) replaced by `bash scripts/check_adr.sh --self-test`. The cut-down DOC-46 keeps `## 3.` and `## 4.` as numbered headings, each a short pointer to `content/skills/tm-adr/references/adr-standard.md`, because immutable ADRs cite them (amendment A6).
- Every DOC-46 §3 consumer re-pointed at the skill standard (line numbers as of `de7df1084`; verify before editing): `docs/adr/README.md` (lines 7, 20, 22, 81), `docs/adr/template.md` (33), `scripts/check_adr.sh` (2, 191 — resolved by the content copy, which names no DOC), `docs/reference/documentation-layout.md` (53), `docs/specs/README.md` (66).
- `docs/adr/README.md`: records the exact gate invocation with both flags. It also gains the stage-the-stub-to-claim-a-number rule and names `scripts/check_doc_numbers.sh` as the duplicate-number lint (amendment A7).

#### Acceptance criteria

- **AC1** `cmp scripts/check_adr.sh content/skills/tm-adr/scripts/check_adr.sh` exits 0 and `bash scripts/check_adr.sh --verify-self` exits 0; both outputs in the PR body.
- **AC2** `git ls-files -s scripts/check_adr.sh` shows mode `100755`.
- **AC3** `bash scripts/check_adr.sh --require-metadata-from 0014 --extra-dirs 'docs/*/decisions'` exits 0 on the current corpus and its scan summary counts more than one directory; the same command without `--require-metadata-from` exits 1 naming ADR-0001 through ADR-0013; without `--extra-dirs` the summary counts one directory. `--extra-dirs` applies only the README-listing check to each matched directory (every `NNNN-*.md` is named in that directory's `README.md`); the `--extra-dirs 'docs/*/decisions'` run reports six `docs/*/decisions` directories and zero violations in them (amendment A5).
- **AC4** `doc-numbers.yml` runs the `cmp`, `--self-test`, `--verify-self`, then the gate, in that order; `ls .github/workflows/` shows no new file; the Doc numbers run on the PR is green (link in the PR body).
- **AC5** `grep -c check_adr_selftest docs/specs/DOC-46-adr-standard.md` prints 0 and `grep -c 'check_adr.sh --self-test' docs/specs/DOC-46-adr-standard.md` prints at least 1.
- **AC6** `grep -c 'content/skills/tm-adr/references/adr-standard.md' docs/specs/DOC-46-adr-standard.md` prints at least 1. DOC-46 no longer carries the generic template, status lifecycle or vetting protocol as normative text: `grep -c '^## Related Decisions' docs/specs/DOC-46-adr-standard.md` prints 0 and the file is shorter than 442 lines. `bash scripts/check_sld.sh` exits 0. `grep -c '^## 3\.' docs/specs/DOC-46-adr-standard.md` and `grep -c '^## 4\.' docs/specs/DOC-46-adr-standard.md` each print 1 (amendment A6).
- **AC7** `grep -rn 'DOC-46 §3' docs/adr/README.md docs/adr/template.md docs/reference/documentation-layout.md docs/specs/README.md scripts/check_adr.sh` prints nothing; each former §3 consumer names `content/skills/tm-adr/references/adr-standard.md` or the `tm-adr` skill instead. `grep -c -- "--require-metadata-from 0014 --extra-dirs 'docs/\*/decisions'" docs/adr/README.md` prints at least 1. `grep -c 'scripts/check_doc_numbers.sh' docs/adr/README.md` prints at least 1, and `docs/adr/README.md` states the stage-the-stub-to-claim-a-number rule (amendment A7).
- **AC8** `/tm-adr` run in this checkout reports `up to date` for `scripts/check_adr.sh` (transcript line in the PR body). This criterion alone needs the deployed-release half of the gate.
- **AC9** `bash scripts/check-ci-helpers-selftest.sh` exits 0 with the updated `doc-numbers.yml` assertion (amendment A3).

#### Non-goals

Changing `docs/adr/` numbering or any existing ADR body; a `.adr-dir` or `adr-check.yml` in this repository (`doc-numbers.yml` stays the gate); `docs/*/decisions/` content; any crate change; `tm-adr.md` wording (Phase 1).

#### Risk

The ported script may differ in behaviour from the current one on this corpus — AC3's three runs are the check. A deploy-chain defect found by Phase 1 AC12 means the deployed copy is not what the content holds; AC8 refuses to pass until that issue is closed, while AC1–AC7 proceed against the content copy. Cutting DOC-46 can orphan a consumer link; AC7's grep is the check.

#### Gate procedure

The gate line above is checked by these steps, in order.

1. The Phase 1 PR reports `MERGED` (`gh pr view <pr> --json state`). AC1–AC7 may start here.
2. For AC8 only: a content release contains Phase 1 — `git tag -l 'content-v*' --contains <squash sha>` is non-empty.
3. For AC8 only: the skill is deployed on the implementing host. `tm content update` only re-pins the bundle; skill tiers deploy on `tm install` or a session launch. When `tm content status` prints `source: dev`: fast-forward the main checkout to `origin/main`, then `tm install`. When it prints `source: bundle`: `tm content update`, then `tm install`. Then `cmp content/skills/tm-adr/scripts/check_adr.sh ~/.trusty-tools/trusty-mpm/claude-config/skills/tm-adr/scripts/check_adr.sh` exits 0.
4. Phase 1 AC12 was clean, or the crate issue it filed is closed.

### Deferred

<!-- deferred:start -->
| Item | Why deferred | Where it went |
|------|--------------|---------------|
| Adopt mode for projects with existing ADRs | Init stops on any ADR marker (D6); adoption needs a migration design | unscheduled |
| pre-commit / Makefile / justfile / package.json wiring | Untested grammars; GitHub Actions covers the common case | unscheduled |
| Executable bit on deployed skill extras (`scripts/package_content.sh` writes mode 0644) | Crate change, rung 4; `adr_init.sh` sets the mode on the project copy | unscheduled |
| `tm doctor` stale-copy row for managed scripts | Crate change, rung 3 | unscheduled |
| Orphan pruning of non-`.md` skill files | Crate change, rung 3 | unscheduled |
| Settings file for `check_adr.sh` | Flags and `ADR_CHECK_*` env cover every known case (D10) | unscheduled |
| `agent_worktree = false` handling beyond `isolation: "worktree"` or stop-with-reason | Decide after the manual transcript check shows whether the override is reliable (D13) | unscheduled |
| Support for bash < 4 | Bash 4+ is assumed (D11); a port would remove `mapfile` and `declare -A` for no known user | unscheduled |
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
filing session. The 2026-10-15 owner-approved metadata is reused.

### Tracker

- Title at creation: `[EPIC] tm-adr bootstraps and maintains a generic ADR setup in any project`; retitled `[EPIC <N>] …` after read-back.
- Body: the generated body already carries `Supersedes [#9924](https://github.com/bobmatnyc/trusty-tools/issues/9924).` from the summary; confirm it after creation.
- Labels: `epic`, `dx`, `area:agent-prompts`, `ws/tm-trusty-tools-adr-init`.
- Milestone: `trusty-mpm 1.9.0`. Project #45, Status Backlog. Assignee `mac-duetto`.
- Comment in the same dispatch: `no-component-label: no crate owns content/skills/tm-adr/ or .github/workflows/`.
- Close rule: both phases closed; closing comment maps O1–O4 to evidence.

### Phase 1

- Title: `[EPIC_<N> PHASE_1] tm-adr 2.1.0 ships generic content, adr-standard.md, init/status scripts and CI self-tests`.
- Labels: `enhancement`, `dx`, `area:agent-prompts`, `ws/tm-trusty-tools-adr-init`. Milestone: parent's. Project #45, Backlog. Parent `<N>`. Assignee `mac-duetto`.
- Rung 3: ships executable code into this repo's CI and every bootstrapped repo's CI, and edits `doc-numbers.yml`; no crate source changes.
- Close rule: Merged.
- PR title: `feat(tm-adr): [EPIC_<N> PHASE_1] ship generic tm-adr 2.1.0 with adr-standard, init and status scripts`.
- Comment in the same dispatch: `no-component-label: no crate owns content/skills/tm-adr/ or .github/workflows/`.

### Phase 2

- Title: `[EPIC_<N> PHASE_2] trusty-tools adopts the shipped check_adr.sh and cuts DOC-46 to a pointer`.
- Labels: `chore`, `ci`, `dx`, `area:ci`, `ws/tm-trusty-tools-adr-init`. Milestone: parent's. Project #45, Backlog. Parent `<N>`; blocked-by `<P1>`. Assignee `mac-duetto`.
- Rung 3: behaviour change in a gate script and a spec cut, verified by the script's self-test and the Doc numbers workflow; no crate touched.
- Close rule: Merged.
- PR title: `chore(adr): [EPIC_<N> PHASE_2] run the shipped check_adr.sh in doc-numbers.yml and cut DOC-46 to a pointer`.
- Comment in the same dispatch: `no-component-label: no crate owns scripts/, .github/workflows/ or docs/specs/`.

### Sequence

1. Commit this document to `origin/main` through a PR; record the permalink SHA.
2. Create the label `ws/tm-trusty-tools-adr-init` if it does not exist; run `tm issue seed-labels`.
3. `tm issue epic create --from docs/research/adr-bootstrap/plan.md --milestone "trusty-mpm 1.9.0" --component dx --project 45 --session tm-trusty-tools-adr-init --phase-type enhancement`.
4. Post-create edits (the CLI sets neither assignee nor Project Status): assignee `mac-duetto` on all three; Project #45 Status Backlog on all three; Phase 2 type `chore`; `area:*` on all three; `ci` on Phase 2; blocked-by P1 → P2 if not set; confirm the `ws/` label landed.
5. `no-component-label` comment on each issue.
6. `tm issue audit` on all three; `tm issue epic sync <N>`.
