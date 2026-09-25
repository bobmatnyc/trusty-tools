# Instructional-content tracking: current state, design options, recommendation

Epic: [#8378](https://github.com/bobmatnyc/trusty-tools/issues/8378)
("Instructional content" milestone).

Owner rulings this research answers (2026-09-22):

1. Instructional content — agents, skills, PM instruction sections, output
   styles, generated capability catalogs — gets a separate semantic tracking
   system from code: versions in frontmatter, deployment independent of any
   binary release, its own changelog.
2. Baseline rule: an agent or skill must work with ANY version of
   `trusty-mpm`, `trusty-code`, `trusty-agents`, Codex, Claude Code, etc.
3. (Mid-task) A content-only change must be deployable with no SemVer check,
   no crate version bump, no `cargo-semver-checks` run, no `changelog.d`
   fragment, and no `src/**` path trigger.

## Part A — Current state

### A.1 Where the content lives

| Tree | Contents | Consumer |
|---|---|---|
| `crates/trusty-mpm/src/assets/skills/` | 23 top-level skills + `references/*.md`, `sm_instructions/` | `trusty-mpm` binary |
| `crates/trusty-mpm/src/assets/instructions/sections/` | 11 PM instruction sections (`core.md`, `agent-delegation.md`, …) + `README.md` | `trusty-mpm` PM prompt composer |
| `crates/trusty-mpm/src/assets/output-styles/` | `trusty-mpm.md`, `trusty-mpm-research.md`, `trusty-mpm-teacher.md` | `trusty-mpm` output-style deploy |
| `crates/trusty-mpm/src/assets/framework-manifest.toml` | catalog authority: which agent/skill bundles, its deploy category, its markers | `trusty-mpm` manifest parser |
| `crates/trusty-agents-common/src/assets/agents/` | 43 agent `.md` (5 `BASE-*` + 38 concrete), one physical copy | `trusty-mpm` AND `trusty-code` (both `include_str!` this crate) |
| `crates/trusty-code/src/assets/skills/` | 30 of the 43 shared skills, `trusty-code`'s own subset | `trusty-code` |
| `crates/trusty-code/src/assets/skill-refs/` | 4 files, byte copies of 4 `trusty-mpm` skills, pinned by test | `trusty-code` (projectless/embedded compose) |

164 files under `crates/trusty-mpm/src/assets/` alone (`find … | wc -l`).

### A.2 Embedding mechanism

Every asset is a compiled-in `pub const &str` via `include_str!`, never
`build.rs`, never deploy-time generation:

- `crates/trusty-agents-common/src/agent_assets.rs:1-29,32-` — Why comment:
  "the one-copy ruling"; both `trusty-mpm` and `trusty-code` `include_str!`
  this SAME crate so a fix is applied once. `trusty-code` embeds 30 of the 43
  and keeps 8 files of its own (4 read-only-tool forks, 4 with no counterpart).
- `crates/trusty-mpm/src/core/bundle.rs:24-144` — re-exports every
  `trusty_agents_common::agent_assets::*` const plus its own hook/skill
  consts (`OPTIMIZER_TOML`, `OVERSEER_TOML`, …).
- `crates/trusty-code/src/agents/skill_refs.rs:33-53` — `REFERENCED_SKILL_FILES`
  embeds 4 files under `crates/trusty-code/src/assets/skill-refs/*/SKILL.md`,
  explicitly documented as "Byte copies of trusty-mpm's bundled skills."
- `crates/trusty-agents/src/agents/bundled/mod.rs:509,594` — a THIRD,
  independent `ensure_bundled_agents_deployed[_in]` deploy path in the
  `trusty-agents` crate (the agentic-runtime product), separate from
  `trusty-mpm`'s own `deploy_session_skills` / `agent_deployer`.

### A.3 The mirror test — a literal cross-crate disk dependency

`crates/trusty-code/src/agents/skill_refs.rs:194-206`,
`referenced_copies_match_trusty_mpm_skills`:

```rust
let mpm_skills = Path::new(env!("CARGO_MANIFEST_DIR")).join("../trusty-mpm/src/assets/skills");
```

This test only passes when `trusty-code` and `trusty-mpm` are checked out as
siblings in the same workspace — it reads `trusty-mpm`'s asset tree off disk
at test time via a relative path across crate boundaries. A companion test,
`every_roster_pointer_names_an_embedded_file` (same file, :208-224), asserts
every `{{TM_SKILLS}}/<skill>/SKILL.md` pointer a shared agent body names has a
matching entry in `REFERENCED_SKILL_FILES` — i.e. an agent body (in the ONE
shared `trusty-agents-common` tree) can silently point `trusty-code` at a
skill `trusty-code` never embedded, and only this test catches it.

### A.4 The generated capability catalog and its drift gate

`crates/trusty-mpm/src/core/bundle_tm_capabilities.rs:1-30`: five of six
`tm-capabilities` files are produced by `tm generate capabilities` from the
RUNNING BINARY's own in-process data — clap CLI introspection, the MCP tool
catalog, the bundled agent/skill roster — then committed like any other
bundled skill. `scripts/check_capabilities.sh` runs
`tm generate capabilities --check`, which regenerates every derived file in
memory and diffs it against the committed copy; non-zero exit on drift. This
ties the catalog's content to the exact command/tool surface of ONE binary
build — regenerating it is mandatory whenever a CLI command, MCP tool, agent,
or skill is added/removed/renamed. `references/workflows.md` is the one
hand-authored exception (not diffed).

### A.5 The manifest authority and deploy tiers

`crates/trusty-mpm/src/assets/framework-manifest.toml:1-125` is "THE
AUTHORITY" (comment, line 15) for which agents/skills bundle, their deploy
category (`universal` / `language` / `framework` / `platform` / `deprecated`),
and their gating `markers`. Parsed by the same `HarnessManifest::from_toml`
every manifest tier uses (project `.trusty-mpm/manifest.toml`, operator
`~/.trusty-mpm/manifest.toml`, catalog manifest) — framework tier differs only
by being compiled into the binary.

Deploy has three tiers, precedence project > operator-home > bundled:
`crates/trusty-mpm/src/daemon/doctor_skill_drift.rs:13-16` — "the managed
`$CLAUDE_CONFIG_DIR/skills`, the operator's `~/.claude/skills`, and the
project's `.claude/skills`." Deploy runs through
`crates/trusty-mpm/src/core/session_launch/skills.rs:11-20`
(`deploy_session_skills`) for skills and
`crates/trusty-mpm/src/core/agent_deployer.rs:16`
(`pub use trusty_agents_common::agents::deployer::*`) for agents; both stamp
every written file `provenance: framework-owned`
(`crates/trusty-agents-common/src/agents/deployer.rs:56,300`) so staleness
checks can tell a framework deploy from an operator hand-edit.

### A.6 `extends:` composition — single-directory resolution

`crates/trusty-agents-common/src/agents/builder.rs:1-18` — `compose_agent`
walks an agent's `extends:` chain base-first via a `SourceLookup` trait; the
disk implementation (`build_source_map`) scans ONE directory and resolves
case-insensitively (`BASE-QA.md` vs `extends: base-qa`). A second,
disk-free `builder_in_memory` lookup exists specifically for the embedded
(compiled-in) asset table, reusing the same walk/cycle/depth logic. Any
design that splits the roster across locations must either keep a single
resolvable namespace or extend `SourceLookup` to span two.

### A.7 Doctor staleness checks — binary-content, not cache, as ground truth

`crates/trusty-mpm/src/daemon/doctor_skill_drift.rs:1-24`: the check used to
compare the deployed file against `~/.trusty-mpm/framework/skills/` (an
EXTRACTION CACHE) and missed real drift when the cache lagged the binary
(issue #4604). "The reference point is now the binary's own embedded asset."
`doctor_output_style.rs:1-25` (`check_output_style`) resolves Claude Code's
full settings-precedence chain and checks the effective `outputStyle` string
names a file that exists on disk. Both checks are structurally "does disk
match what THIS BINARY embeds" — i.e. staleness is currently defined as
binary-vs-disk drift, not content-version-vs-binary-compatibility.

### A.8 Frontmatter fields in use today

- Skills carry `version` (free-form semver string, e.g.
  `crates/trusty-mpm/src/assets/skills/tm-workflow.md:4` `version: "2.1.0"`),
  `category`, `user-invocable`, `tags`, `effort`, and optionally `spec_refs:`
  (`crates/trusty-mpm/src/assets/skills/documentation-style.md:9`, governed by
  `documentation-style/references/spec.md:39,62`).
- Agents carry `name`, `role`, `extends`, `description`, `model`, `skills`,
  `tools` — **no `version` field exists on any of the 43 agent `.md` files**
  (`grep -c '^version:' crates/trusty-agents-common/src/assets/agents/*.md`
  returns zero matches for every file). A per-asset-semver design for agents
  starts from nothing, not from a drifted convention.

### A.9 Goldens that embed instruction text

`crates/trusty-mpm/src/core/testdata/pm-prompt-{bundled-fallback,claude-md-override,roster-absent}.md`
(480/365/466 lines) embed the FULL composed PM prompt — every instruction
section plus roster-fallback logic — verbatim. An edit to any
`instructions/sections/*.md` file requires regenerating all three in the same
PR or the composer test suite goes red.

### A.10 The context-budget baseline

`scripts/check_context_budget.sh:1-40` sums the byte size of `CLAUDE.md` +
`crates/trusty-mpm/src/assets/instructions/sections/*.md` +
`crates/trusty-mpm/src/assets/output-styles/trusty-mpm.md`, fails when either
one file or the total grows past a committed baseline (default 5%/10%
allowance). The scanned paths are named literally, not derived — relocating
any of them requires editing this script and its `--update`d baseline in the
same PR (#7424).

### A.11 Hard couplings — a skill/agent body assuming ONE harness's surface

`crates/trusty-mpm/src/assets/skills/tm-workflow.md` names concrete `tm`
subcommands inline: `tm hook --pm-guard`, `tm pr merge`, `tm pr open`,
`tm pr queue-check`, `tm session may`, `tm sessions prune-worktrees` — none
guarded by a capability probe; the skill assumes the binary that reads it has
exactly that subcommand surface.
`crates/trusty-agents-common/src/assets/agents/version-control.md:61,311,317`
names `Skill(skill="tm-workflow")`, `Skill(skill="git-workflow")`,
`Skill(skill="cargo-publish")` — a harness-tool-call convention, not probed.
Agent `tools:` frontmatter lists literal MCP tool names
(`mcp__trusty-search__…`, `mcp__trusty-memory__…`) with no fallback when a
harness's MCP catalog lacks one. This is the direct gap against owner ruling
(2): nothing in the current design detects or degrades for a harness whose
`tm`/MCP/hook surface differs from the one the content was authored against.

### A.12 CI path-filter classification of a content-only change today

`scripts/detect-docs-only.sh:1-52` is the "Cargo-inert change-set detector"
(job `changes` in `.github/workflows/ci.yml:206-260`, which exists as a job
rather than a workflow `paths:` filter specifically so a required check never
hangs pending — see the file's own Why comment). Its classification is a
**denylist-of-inert**: `docs/**` and `website/**` are inert at any depth
(lines 108-115); an unrecognised path defaults to NOT inert. Its own comment
(lines 46-49) states explicitly:

> Deliberately NOT inert, though they look like documentation:
> `crates/*/src/**/*.md` — bundled agent/skill/instruction assets that are
> compiled into binaries via `include_dir!`/`include_str!`. Editing one
> changes program output and breaks asset-pin and drift tests.

So **today, editing one skill `.md` pays the full Rust build/clippy/test
suite** — the opposite of "Cargo-inert." Separately,
`scripts/detect-version-bumps.sh:26-46` keys ONLY on a `Cargo.toml`
`[package].version` diff, so a content-only change never trips the SemVer job
today regardless of where the content lives — that gate is already safe.
`scripts/check_changelog_fragment.sh:16-24,135` treats any changed path
matching `crates/<crate>/src/**` as crate source requiring a changelog
fragment (line 135 cites the `pm-prompt-*.md` goldens tripping exactly this
rule); this is the gate a content-only PR pays today that the owner's
mid-task ruling wants removed.

## Part B — Design options

All three satisfy owner ruling (3): content moves to a tree outside every
`crates/*/src/`, so `detect-docs-only.sh`, `detect-version-bumps.sh`, and
`check_changelog_fragment.sh` need no code change to stop firing on a
content-only diff — none of their patterns match a path outside `crates/`
(`check_changelog_fragment.sh` attributes only `crates/<crate>/src/**`; the
other two are equally crate-src-scoped). Each option still needs
`detect-docs-only.sh` extended with an explicit `content/**` (or equivalent)
inert case — moving the path is necessary but not sufficient; the allowlist
is denylist-of-inert, so an unrecognised new root still defaults to "pay the
full build" until named.

### Option 1 — `content/` tree in this workspace, compile-time fallback only

**Version declaration.** Per-asset frontmatter `version` (semver) stays;
add it to agents (currently absent, A.8). A `content/manifest.toml` — same
shape as `framework-manifest.toml` today — additionally carries ONE
content-bundle version, bumped whenever any per-asset version bumps (a
pre-commit/CI check enforces the bundle version is >= max of its members'
versions, mirroring how `Cargo.lock` and workspace members coexist today).

**Where it lives.** `content/{agents,skills,instructions,output-styles}/` at
the workspace root, outside every `crates/*/src/`. Still this repo, still one
PR, one review, one CI run — no cross-repo sync process to build or maintain.

**Distribution.** A GitHub Actions job triggered only on `content/**` changes
(path-filtered — safe here because this is a NEW, non-required workflow, not
one of the required checks `detect-docs-only.sh` protects) packages
`content/` into a tarball and publishes it as a GitHub Release tagged
`content-vX.Y.Z`, independent of any crate tag.

**Fetch and pin.** `tm content update [--content-ref <tag>]` downloads and
verifies the tarball into `~/.trusty-mpm/content-cache/<ref>/`, writes the
resolved ref to `.trusty-mpm/content-lock.toml` (project) or
`~/.trusty-mpm/content-lock.toml` (operator). Runtime deploy prefers the
cache; `include_str!` of `content/**` (a relative path traversal out of
`crates/trusty-mpm/src/`, which Cargo permits — no different mechanically
from `trusty-code` already `include_str!`-ing `trusty-agents-common`'s
`assets/` today) is compiled in ONLY as the offline bootstrap fallback used
when no cache exists and no network is reachable.

**Changelog.** `content/changelog.d/<issue>-<slug>.md` fragments, same
format as crate fragments, rolled into a `content/CONTENT-CHANGELOG.md` by
the same fragment-rollup tooling `local-ops` already runs for crates —
reuse, not a new system.

**Compatibility rule (owner ruling 2).** No version gate anywhere. A skill
body that names a `tm` subcommand states it as a probe-and-degrade
instruction (a documented convention: "run `tm --help`; if the subcommand is
absent, fall back to <X>"), not a hard requirement. A `requires:` frontmatter
block is advisory metadata for `tm doctor` to REPORT, never to block deploy
or load. A compatibility test matrix (new, small) parses and dry-run-deploys
every content asset against the oldest supported binary's manifest schema —
catches a frontmatter shape a old parser cannot read, not a behavioral gap.

**Doctor.** Reports content bundle version (from `content-lock.toml`)
alongside binary version as two independent lines — no comparison, no
mismatch warning unless the compatibility matrix (above) flagged a real parse
failure.

**Non-`tm` harnesses.** `trusty-code` and Codex/Claude-Code-native consumers
fetch the SAME tarball via their own `content update` equivalent, or read the
`content/` tree directly if co-located in this workspace (as `trusty-code`
already does for `trusty-agents-common`'s agents today).

**Trade-offs.** Cheapest migration (one repo, existing PR/review flow); the
compile-time fallback still needs `content/**` re-embedded and the binary
rebuilt to pick up a NEW offline snapshot — i.e. the "offline fallback"
itself is still coupled to a binary release, by design (it is a bootstrap
floor, not the update channel). Risk: without discipline, "content/" drifts
back into being edited only alongside code PRs since it is still one repo.

### Option 2 — separate `trusty-instructional-content` repository

Same version/manifest/changelog shape as Option 1, but the canonical source
is a SEPARATE git repo with its own tags, its own `CONTENT-CHANGELOG.md`,
its own review/CI. `trusty-tools` carries it as a pinned git submodule (or a
vendored snapshot refreshed by a bot PR) purely for the compile-time offline
fallback; `tm content update --content-ref <git-ref>` shallow-clones or
fetches a release tarball from that repo at runtime, independent of any
`trusty-tools` release.

**Trade-offs.** Cleanest separation — a content-only change genuinely cannot
touch `trusty-tools` CI at all, and the "separate tracking system" ruling is
satisfied by repository boundary rather than by policy. Cost: two repos to
keep in sync (submodule pin drift is a known class of bug — this workspace's
own `crate-map.md` already tracks "former-repos" migrations that trace back
to over-splitting); PR review for a content change that also needs a matching
harness behavior change (e.g. a new `Skill(skill=…)` name) now spans two
PRs and two merge orders. Slower to stand up (new repo, new CI, new release
process) for a first three-PR slice.

### Option 3 — OCI/registry-distributed bundle, `content/` tree stays in-repo

Same `content/` tree and manifest as Option 1, but distribution is an OCI
artifact (e.g. `ghcr.io/bobmatnyc/trusty-content:X.Y.Z`) instead of a GitHub
Release tarball, published by the same path-filtered workflow. `tm content
update` pulls via an OCI client instead of an HTTP tarball fetch.

**Trade-offs.** OCI gives content-addressed immutability and registry-native
tooling (already familiar to anyone running the daemon's own container
images, if any exist) but adds a new dependency (an OCI client, a registry
account/token, pull-through caching) for a marginal gain over a GitHub
Release asset, which is already content-addressed by tag+checksum and needs
no new credential. Best fit only if the roadmap already plans to distribute
OTHER artifacts by OCI; otherwise it is Option 1 with extra infrastructure
for the same guarantee.

## Part C — Recommendation

**Option 1** (`content/` tree in this workspace, GitHub-Release-tarball
distribution, compile-time embed as offline fallback only). It satisfies
every owner ruling with the least new infrastructure: one repo, one review
flow, reuses the existing changelog-fragment tooling and the existing
`include_str!` mechanism (already proven to traverse crate boundaries by
`trusty-code`'s embedding of `trusty-agents-common`), and gets the CI-path-
filter win (`detect-docs-only.sh` extended once) without inventing a
submodule-sync or OCI-registry process this workspace does not otherwise
need. Option 2 is the better long-term shape IF instructional content ever
needs to release faster than `trusty-tools` reviews PRs, or ships to
consumers that must never clone this repo — revisit if that need appears.

### First three PR-sized steps

1. **Move, don't redesign.** `git mv crates/trusty-mpm/src/assets/{skills,instructions,output-styles,framework-manifest.toml} content/trusty-mpm/`
   and `git mv crates/trusty-agents-common/src/assets/agents content/agents`
   (keep `crates/trusty-code/src/assets/skill-refs` and its own `skills/`
   subset as-is for this step — a second step folds it in). Update every
   `include_str!` call site to the new relative path; no content edits. Add
   `content/**` to `scripts/detect-docs-only.sh`'s inert cases; update
   `scripts/check_context_budget.sh`'s scanned-path list. Verify
   `check_capabilities.sh`, `check_changelog_fragment.sh` (should now show
   `content/**` as unattributed-to-any-crate and therefore exempt), and the
   three `pm-prompt-*.md` goldens all still pass unchanged.
2. **`content/manifest.toml` + per-asset `version` on agents.** Add the
   content-bundle manifest (version, per-asset version table) and add
   `version:` frontmatter to all 43 agent files (starting value `1.0.0`,
   informational only — no gate reads it yet). Add
   `content/changelog.d/` and wire it into whatever fragment-rollup
   `local-ops` runs for crates today, scoped to `content/`.
3. **`tm content update` + offline-fallback split.** Add the `tm content`
   subcommand family (`update`, `pin`, `status`), the `content-lock.toml`
   format, and split every `include_str!` site into "prefer cache, fall back
   to compiled-in" via a small resolver — no change to WHAT is embedded at
   compile time, only to what is preferred at runtime. `tm doctor` reports
   content bundle version next to binary version.

### ADR text

See `docs/adr/0064-instructional-content-tracked-separately-from-code.md`
(Status: Proposed).
