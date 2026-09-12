# Agent Context Minimization — Design Research

**Status:** Draft (research, no code changes)
**Owner directive:** 2026-09-12, "minimize token use with no compromises"
**Scope:** how `tm` delivers instructions to the PM and to dispatched agents,
and what changes reduce fixed per-turn context cost without breaking a
legitimate need.

All citations are against `origin/main` at `626c36eaf40cc74642e39ccbdc39566bb999f4c7`
(2026-09-12), read with `git show origin/main:<path>` — the working tree is
~140 commits stale and was not used for any claim below. No source file was
edited to produce this document.

Measured baseline (owner-supplied): a general-purpose Haiku subagent costs
**44.7K tokens per turn with zero tool calls**, carrying the CLAUDE.md
hierarchy, `MEMORY.md`, a ~131-entry skills listing with descriptions, 151
deferred MCP tool names, and the base agent prompt.

---

## A. How `tm` delivers the PM's compiled instructions

**Why:** confirm the actual delivery mechanism before proposing changes to it,
and settle whether the PM double-pays for the project `CLAUDE.md`.

**What (found):**

1. Delivery is `claude --append-system-prompt-file <path>`, not
   `--append-system-prompt` (inline) and not an output-style-only path for the
   compiled instructions. `crates/trusty-mpm/src/core/session_launch/mod.rs:943-944`
   ("`claude --append-system-prompt-file` — including the HR-4 output-style
   injection — so `tm session instructions` shows what was actually used").
   The prompt file itself is built by
   `build_system_prompt_for_with_style_and_native`
   (`crates/trusty-mpm/src/core/session_launch/mod.rs:956`) and stashed to
   `<project_dir>/.trusty-mpm/last-instructions.md`
   (`crates/trusty-mpm/src/core/session_launch/mod.rs:976`, doc comment at
   `mod.rs:396-401`). `crates/trusty-mpm/src/runtime/claude_code.rs:200-254`
   (`env_bin_prefix`) and `:845` (`args.push("--append-system-prompt-file")`)
   are the two points where the flag actually reaches the `claude` argv for a
   managed spawn.
2. The compiled text is assembled **in memory** from compile-time bundled
   section assets (`crates/trusty-mpm/src/core/instruction_pipeline.rs:60-90`,
   the `SECTION_IDENTITY` / `SECTION_CORE` / `WORKFLOW` / `AGENT_DELEGATION` /
   `SECTION_ENFORCEMENT` constants, each an `include_str!` of
   `assets/instructions/sections/*.md`), not read back from an installed
   `INSTRUCTIONS.md` (`instruction_pipeline.rs:15-24`, #4752/#4832 removed that
   round-trip).
3. Project customization of this compiled text is **not** "the whole
   `CLAUDE.md`" — it is a narrow, explicit override grammar read by
   `crates/trusty-mpm/src/core/claude_md_sections.rs`. The module doc states
   the marker grammar (`claude_md_sections.rs:17-24`) and, load-bearing for
   the double-pay question:

   > "Content is what lies strictly between the two marker lines, trimmed.
   > **Text outside markers is not instruction content and is ignored.**"
   > (`claude_md_sections.rs:28-29`)

   The seeded `CLAUDE.md` stub itself documents the same split, addressed to
   the PM (`crates/trusty-mpm/src/core/instruction_pipeline.rs:585-589`):

   > "`CORE` is the one token that is always declined. Prose outside the
   > markers is project context — Claude Code loads it natively, so it is
   > never copied into the composed prompt."

4. `build_instructions` only uses `CLAUDE.md` to make sure the file **exists**
   (seeding a stub if absent); it discards the content:
   `crates/trusty-mpm/src/core/instruction_pipeline.rs:818`
   (`let (_claude_md, claude_md_created) = load_or_create_claude_md(...)`,
   underscore-prefixed — the return value is never used past that line) with
   the doc comment at `instruction_pipeline.rs:813-817` ("ensure the project
   CLAUDE.md exists ... so a fresh ... reads it natively"). `mod.rs:901-903`
   states the same intent at the call site ("this loads or creates the
   project `CLAUDE.md` so Claude Code picks it up automatically").
5. A legacy code path that used to inject a framework-owned delegation block
   directly into the project's `CLAUDE.md` body was deliberately removed
   (issue #2170) precisely because it double-delivered content already carried
   by the output style / compiled prompt:
   `crates/trusty-mpm/src/core/instruction_pipeline.rs:602-624` — "That
   violated the standing owner constraint that trusty-mpm must NEVER modify a
   target project's `CLAUDE.md`. ... making the `CLAUDE.md` copy redundant."

**Verdict — no double-pay for CLAUDE.md prose today.** The PM receives:
   - the framework sections (`IDENTITY`/`CORE`/`MEMORY`/`SEARCH`/`WORKFLOW`/
     `AGENT-DELEGATION`/`ENFORCEMENT`/floor), optionally with named sections
     replaced by a project's `<!-- TRUSTY-MPM: … -->` marker blocks, via
     `--append-system-prompt-file` (tm's channel); and
   - the full project `CLAUDE.md` (including any marker blocks, which remain
     visible in the file) via Claude Code's own native per-session CLAUDE.md
     load (not tm's channel).

   These are disjoint for ordinary prose: what tm delivers is framework text,
   not the project's own words. The one byte-level duplication is narrow and
   deliberate — a project's override-marker **body** appears twice: once
   inline inside `CLAUDE.md` itself (native load) and once substituted into
   the composed prompt in place of the bundled section (tm's channel). That
   duplication is bounded by how much a project chooses to put inside marker
   blocks, and is the intended mechanism (#4183), not an accident.

**Test:** `crates/trusty-mpm/src/core/instruction_pipeline_tests.rs` covers
`pipeline_creates_claude_md`, `pipeline_claude_md_left_byte_identical`; the
override-marker parser is covered by `claude_md_sections_tests.rs`
(referenced at `claude_md_sections.rs:52`). No test currently asserts the
"never copied" claim in (3) against a hostile `CLAUDE.md` — see Slice 1 below.

---

## B. Design for goal (2): agents get no CLAUDE.md, PM keeps it

**Why:** the owner directive is "agents do not use CLAUDE.md; they get context
only from the PM's dispatch brief." `tm` currently sets none of the levers
that could achieve this — confirmed by an empty-result grep:
`git grep -n "claudeMdExcludes\|skillOverrides\|skillListingMaxDescChars" -- crates/trusty-mpm`
returns nothing. This section is a proposal, not a description of existing
code.

**The mechanism and its limit.** `claudeMdExcludes` is real and does what the
owner wants in isolation: "Patterns are matched against absolute file paths
using glob syntax. You can configure `claudeMdExcludes` at any settings
layer: user, project, local, or managed policy. Arrays merge across layers."
(code.claude.com/docs/en/memory#exclude-specific-claude-md-files). But the
per-agent question has a documented negative answer:

> "Per-Agent Control: No per-subagent field to exclude CLAUDE.md."
> (code.claude.com/docs/en/sub-agents, confirmed against
> code.claude.com/docs/en/settings-reference: `claudeMdExcludes` scope is
> "Any file" — i.e. any settings-file layer, never a per-agent-type key.)

`claudeMdExcludes` is read once per settings resolution for the whole running
`claude` process. A dispatched (non-fork) subagent gets its own fresh
context construction at spawn time (which is why it independently re-derives
CLAUDE.md, `MEMORY.md`, and the skills listing — consistent with the 44.7K
Haiku baseline), but it resolves the **same** settings.json the PM's own
process already loaded. There is no settings layer, key, or frontmatter field
that says "exclude for subagents, keep for the top-level session." Writing
`claudeMdExcludes` for the project's own `CLAUDE.md` at the project settings
layer excludes it **from the PM too**.

**Exact glob and key.** `.claude/settings.json` (project, shared/committed
tier — `crates/trusty-mpm/src/core/session_launch/settings.rs:210` is the
existing precedent: `write_output_style` already reads/merges/writes exactly
this file). Key: `claudeMdExcludes`, an array. Since `tm` knows `project_dir`
at every `prepare_session` call, write literal absolute paths rather than a
broad recursive glob (a broad `**/CLAUDE.md` would also suppress on-demand
subdirectory `CLAUDE.md` files Claude reads while browsing a crate, which is
a different and not-obviously-wanted suppression):

```json
{
  "claudeMdExcludes": [
    "<project_dir>/CLAUDE.md",
    "<project_dir>/.claude/CLAUDE.md",
    "<walk ancestors of project_dir up to $HOME, one entry per CLAUDE.md found>",
    "<home>/.claude/CLAUDE.md"
  ]
}
```

**Consequence — this design cannot give subagents zero CLAUDE.md while the PM
keeps native-loaded CLAUDE.md.** It is genuinely one or the other at this
settings layer. The two honest paths:

- **(i) Exclude for everyone, compensate for the PM through tm's own
  channel.** Turn on `claudeMdExcludes` project-wide, and widen the compiled
  `--append-system-prompt-file` prompt to carry what the PM still needs from
  the project's own prose (build commands, the test ladder, SLOC caps,
  changelog-fragment rule — currently *not* copied per finding A(3)). This
  requires revising the exact code comment this design leans on:
  `instruction_pipeline.rs:587-589` ("Prose outside the markers ... Claude
  Code loads it natively") becomes false the moment native loading is turned
  off, and `claude_md_sections.rs:28-29` ("Text outside markers is not
  instruction content and is ignored") — which governs the *reader*, not the
  loader — would need a second reader that pulls whole-file prose into the
  compiled prompt, in the same PR that flips the setting, or the PM silently
  loses everything it isn't told about verbatim.
- **(ii) Do not use `claudeMdExcludes` for the project's own `CLAUDE.md` at
  all; rely on the fact that subagents never receive
  `--append-system-prompt-file`** (that flag is a PM/session-launch construct
  — `runtime/claude_code.rs:845` fires at `claude` process spawn, not at an
  in-process `Task`/`Agent` tool dispatch) **and instead stop putting anything
  subagent-relevant in `CLAUDE.md` prose in the first place**, moving it to
  skills and dispatch briefs (below). Under this path `claudeMdExcludes`
  is reserved for its documented use — trimming irrelevant ancestor/monorepo
  files — not for suppressing the project's own file.

This document does not pick between (i) and (ii); (ii) is lower-risk (no
compensating-channel work, no risk of the PM regressing) and is what Slice 3
below assumes. (i) is the only way to get subagents to *provably* zero
CLAUDE.md bytes if goal (2) is read literally, since without it a subagent
still natively loads the project `CLAUDE.md` in full — the current
39KB `trusty-tools/CLAUDE.md` (`wc -c CLAUDE.md` → 38736) landing in every
dispatched agent's startup context today, roster-wide.

**What breaks, checked one by one:**

- **`<!-- TRUSTY-MPM: … -->` override markers — nothing breaks.** tm's own
  reader (`claude_md_sections.rs`) opens the file directly with
  `std::fs::read_to_string` inside tm's own Rust binary at compile-prompt
  time; it is not Claude Code's context loader and is not gated by
  `claudeMdExcludes`, which only controls what Claude Code itself injects
  into a session's context. Confirmed by design: `claude_md_sections.rs:34-35`
  ("NEVER FAIL CLOSED... an absent or unreadable host ... resolve to 'keep
  the bundled section'") describes a tm-internal fallback ladder with no
  dependency on Claude Code's own loader.
- **`tm session instructions` — nothing breaks.** It prints
  `.trusty-mpm/last-instructions.md`, written by tm itself
  (`mod.rs:976`, `crates/trusty-mpm/src/bin/tm/commands/session/instructions.rs:58`),
  independent of whether Claude Code's native loader ran.
- **Claude Code's own `/init` and `/memory`** — unverified against this
  repo's actual behavior once `claudeMdExcludes` is set; the fetched docs
  describe `/memory` as listing "CLAUDE.md, CLAUDE.local.md, and other memory
  file locations ... including entries for files that don't exist yet" but do
  not state whether an *excluded* file is still listed (and marked excluded)
  or silently omitted, and `/init`'s "if a CLAUDE.md already exists, suggests
  improvements" flow is not documented against exclusion. Flag as open;
  resolve empirically before shipping path (i), not by assertion.
- **An agent that legitimately needs a repo rule** (test ladder, SLOC cap,
  changelog fragment) — under path (ii), it never had it from `CLAUDE.md`
  going forward regardless of `claudeMdExcludes`, because the fix is "stop
  relying on CLAUDE.md for subagents," not the exclude list. Two carriers
  replace it, matching the task's own steer:
  - **The PM's dispatch brief** — `BASE-AGENT.md`'s existing Handoff Protocol
    already models this ("State four things: which agent continues, what was
    accomplished, what remains, and any constraints" —
    `crates/trusty-agents-common/src/assets/agents/BASE-AGENT.md` Handoff
    Protocol section). Extend the same discipline to initial dispatch: the PM,
    which still reads full `CLAUDE.md` natively, copies the one or two rules
    that bind this specific task (e.g. "rung 3 gate: `cargo test -p
    trusty-search --no-fail-fast`") into the `Agent`/`Task` prompt text.
  - **A project-scoped skill an agent family preloads via `skills:`.** Every
    roster agent already declares `skills:` for exactly this kind of durable,
    role-scoped doctrine (`engineer.md` → `skills: [systematic-debugging,
    test-driven-development]`, `version-control.md` → `skills:
    [git-workflow]` — both confirmed by direct read of
    `crates/trusty-agents-common/src/assets/agents/*.md` frontmatter). A new
    project-authored skill (e.g. `trusty-tools-rust-gates`, carrying the test
    ladder table, the SLOC cap table, and the changelog-fragment rule from
    this project's own `CLAUDE.md`) added to the `skills:` list of
    `rust-engineer`, `engineer`, `qa`, `local-ops` gives exactly those agents
    the rule, verbatim, with none of the surrounding 39KB of unrelated
    `CLAUDE.md` prose (website conventions, TCC scope notes, git tag
    convention) that those agent types never needed. This is strictly better
    than status-quo CLAUDE.md inheritance for goal (1) as well: today an
    engineer agent gets the *entire* file, including sections it has no use
    for.

**Test:** none exists yet — this is a proposal. A conformance test would
assert that `.claude/settings.json` written by `prepare_session` contains
`claudeMdExcludes` (path ii: never containing the project's own `CLAUDE.md`
path; path i: containing it, paired with an assertion that the compiled
prompt's byte length grew to compensate). Neither test exists in
`session_launch/tests*.rs` today.

---

## C. Design for goal (1): per-agent `tools:` allowlist and skill scoping

**Why:** every deployed agent ships with the implicit "all tools" default
today — confirmed by an empty grep for a `tools:` frontmatter key across the
entire roster:
`git grep -n "^tools:" -- crates/trusty-agents-common/src/assets/agents/*.md`
returns nothing, and `crates/trusty-agents-common/src/agents/metadata.rs:88-90`
states it in the type's own doc comment: "`None` when the agent (and its
whole `extends` chain) never declares a `tools:` key — **trusty-mpm agents
never set this key**, so they always project to `None`."

**The mechanism already exists and is precedented in this exact codebase.**
`tools:` is full first-class frontmatter, override-merged (not unioned) across
an `extends:` chain (`crates/trusty-agents-common/src/agents/builder.rs:648-653`,
`:699-711`), with `Some(vec![])` a deliberate, distinguishable deny-all
(`builder.rs:299-311`, `metadata.rs:91-95`). `trusty-code` — a sibling
consumer of the *same* shared `agent_assets.rs` roster — already forks four
agents specifically to add a read-only `tools:` restriction:
`crates/trusty-agents-common/src/agent_assets.rs:26-28` ("4 deliberate forks
that add a read-only `tools:` restriction"), visible directly in
`crates/trusty-code/src/assets/agents/{code-analyzer,code-critic,qa,web-qa}.md`,
each declaring `tools: [read_file, grep, glob, list_dir, search_code,
use_skill, finish_task]` — trusty-code's own tool vocabulary, not Claude
Code's. For trusty-mpm's deployed `.claude/agents/*.md` (real Claude Code
subagents), the equivalent uses Claude Code's own tool names and MCP
server-level patterns (`mcp__<server>` / `mcp__<server>__*` — confirmed at
code.claude.com/docs/en/sub-agents).

**Per-agent MCP need**, from this project's own `.mcp.json`
(`trusty-tools/.mcp.json`) plus the deployed skill/instruction text that
actually calls each server (`BASE-AGENT.md:569` cites
`mcp__trusty-mpm__circuit_breaker_status`; no other agent asset names an MCP
tool today):

| Agent (source file) | Needs MCP server(s) | Rationale |
|---|---|---|
| `engineer`, `rust-engineer`, and other language engineers | `trusty-search` | code discovery per this project's CLAUDE.md "Code Search" section; engineers read/write code and need call-chain/semantic search |
| `research`, `code-analyzer` | `trusty-search`, `trusty-memory` | investigation + captured findings |
| `qa`, `code-critic`, `web-qa`, `api-qa` | `trusty-search` (read-only use: locate code under test) | none of `trusty-memory`/`trusty-mpm` — these agents report findings back to the PM, not persist memory themselves |
| `web-qa` only | `claude-in-chrome` | the only agent family that drives a browser |
| `ticketing` | none (gh CLI via Bash, not MCP) — `ticketing.md` is 23.8KB of gh-CLI/MCP-ticketing-tool doctrine but names no `mcp__` server as a tool dependency beyond what Bash already provides | avoid granting `trusty-search`/`trusty-memory` schemas this agent never calls |
| `version-control` | none beyond Bash/git | git operations only |
| `local-ops` | `trusty-mpm` | the only family gated to the daemon/MCP self-management surface (`config_read`/`config_write`/`circuit_breaker_status`/`session_*`) per this project's own abbreviation table and daemon-restart doctrine |
| `documentation` | none (or `trusty-search` read-only, to find existing docs) | pure Markdown authoring |
| `security` | `trusty-search` | vulnerability sweep needs the same code-discovery path as engineers |
| `memory-manager` | `trusty-memory` only | `memory-manager.md:9-11` states it explicitly: "trusty-mpm uses trusty-memory MCP as the sole memory backend" |
| `code-critic`, `code-analyzer` | `trusty-search` (read-only) | adversarial review needs to locate code, not write memory or tickets |

Concretely, per agent this means a `tools:` line naming Claude Code's built-in
tools the role needs (`Read, Grep, Glob, Bash, Edit, Write` for an engineer;
`Read, Grep, Glob, Bash` and no `Edit`/`Write` for `qa`/`code-critic`/
`code-analyzer`, matching trusty-code's own read-only fork precedent) plus
exactly the MCP server-level pattern(s) from the table, e.g.:

```yaml
# engineer.md
tools: Read, Write, Edit, Bash, Grep, Glob, mcp__trusty-search
```

```yaml
# code-critic.md (adversarial, read-only, no code edits)
tools: Read, Grep, Glob, Bash, mcp__trusty-search
```

```yaml
# memory-manager.md
tools: Read, Grep, mcp__trusty-memory
```

**Are `tm-*` skills needed by agents at all?** No — they are PM-only by
construction. Every `tm-*` skill under
`crates/trusty-mpm/src/assets/skills/tm-*.md` documents PM-side orchestration
(delegation matrices, circuit-breaker enforcement, session pause/resume,
ticketing *authority*, workflow *ownership*) — none is referenced by any
agent asset's `skills:` field (`git grep -n "^skills:.*tm-" --
crates/trusty-agents-common/src/assets/agents/*.md` returns nothing). A
dispatched agent has no PM loop to orchestrate and no delegation to route.
The skills an agent family genuinely uses today are already visible in each
file's own frontmatter (`engineer.md` → `systematic-debugging,
test-driven-development`; `version-control.md` → `git-workflow`;
`code-critic.md` → `code-review-standards, contract-driven-testing`;
`documentation.md` → `documentation-style`; `security.md` →
`security-scanning`). None of these needs to change; the gap is the roster's
current lack of a `tools:` field, not its `skills:` field.

**`skillOverrides` — session-wide, not per-agent; use it for the roster, not
per role.** Confirmed against code.claude.com/docs/en/settings-reference
("Scope: Any file" — any settings-file layer, never a per-agent key) and
code.claude.com/docs/en/skills ("skill availability is scoped per-session,
not granularly per-subagent... There is no documented mechanism to restrict
which skills are available to a *specific* subagent" beyond
`disable-model-invocation` in the skill's own frontmatter or a subagent's own
`skills:` preload list). Because it is blunt, `skillOverrides` is the wrong
tool for "give `engineer` fewer skills than `research`" — it is the right
tool for "this project never uses the `aws-agents:*`, `aws-core:*`,
`claude-in-chrome`, `duetto-design-system`, `xlsx`, `breeze-voice`,
`cto-kb-ingest` skill families, so hide them for every agent and the PM
alike":

```json
{
  "skillOverrides": {
    "aws-agents:agents-build": "off",
    "aws-agents:agents-connect": "off",
    "aws-agents:agents-debug": "off",
    "aws-agents:agents-deploy": "off",
    "aws-agents:agents-get-started": "off",
    "aws-agents:agents-harden": "off",
    "aws-agents:agents-optimize": "off",
    "aws-agents:agents-pay": "off",
    "aws-core:*": "off",
    "claude-in-chrome": "off",
    "duetto-design-system": "off",
    "xlsx": "off",
    "breeze-voice": "off",
    "cto-kb-ingest": "off"
  }
}
```

(`"off"` per code.claude.com/docs/en/skills fully removes the entry from the
listing shown to Claude, not just the `/` menu — as of v2.1.199 it is hidden
from "Terminal `/` menu, Command lists advertised to Remote Control clients,
Agent SDK skill discovery.") This is a project-classification decision
(project type → irrelevant skill families), computed once by `tm` at launch
from the project's manifest/stack profile, not a per-agent-type table.

**Does removing the `Skill` tool suppress the listing, or just invocation?**
Not established by the fetched docs (`skillOverrides` and `disallowedTools`
are documented as independent mechanisms; neither page states whether the
listing bytes are still emitted into context for an agent that has no `Skill`
tool at all). This is exactly what probes E below are built to answer
empirically, and the answer determines whether a per-agent `tools:` omission
of `Skill` (for e.g. `version-control`, `ticketing`, `documentation`, which
declare a fixed `skills:` preload and have no evident need to discover more)
is a real token saving or a no-op.

**Test:** none exists — greenfield proposal. A conformance test would extend
`crates/trusty-mpm/src/core/agent_builder.rs`'s re-exported test surface
(`cargo test -p trusty-agents-common agents::builder`) to assert each roster
agent's composed `tools:` is `Some` and non-empty (never silently `None` →
all-tools) once this ships, mirroring the existing "every agent present in
`ALL` with valid frontmatter" test named in `tm-agent-architecture.md`.

---

## D. Design for goal (4): disable Claude Code auto-memory for tm sessions

**Why:** the owner directive is explicit — "Claude Code auto-memory
(`MEMORY.md`) is not used; trusty-memory is the memory." Confirmed as
currently unset: `git grep -n "DISABLE_AUTO_MEMORY\|autoMemoryEnabled\|autoMemoryDirectory" -- crates/trusty-mpm crates/trusty-agents-common`
returns nothing, and `git grep -n "^memory:" -- crates/trusty-agents-common/src/assets/agents/*.md`
returns nothing — no deployed agent sets the subagent `memory:` frontmatter
field either.

**The lever, from docs (code.claude.com/docs/en/memory#auto-memory):**

> "Auto memory is on by default. To toggle it, open `/memory` ... which saves
> `autoMemoryEnabled` to your user settings at `~/.claude/settings.json`. To
> turn it off for a single project, set `autoMemoryEnabled` in that project's
> settings ... To disable auto memory via environment variable, set
> `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`."

Both exist; no CLI flag is documented. Two write sites in `tm` source, either
sufficient alone, both together for defense-in-depth:

1. **Settings key — project tier, alongside `outputStyle`.**
   `write_output_style` (`crates/trusty-mpm/src/core/session_launch/settings.rs:202-236`)
   is the exact precedent: it already reads-merges-writes
   `<project_dir>/.claude/settings.json` and even explains, at
   `settings.rs:229-232`, why writing the **project tier** — not just an env
   var scoped to tm's own managed spawn — matters: "the tm-owned
   `CLAUDE_CONFIG_DIR` copy of this key reaches only the `claude` child tm
   spawns ... a bare `claude` launched in this project reads the project
   tier regardless, so seed it here too." The identical argument applies to
   auto memory: add `settings["autoMemoryEnabled"] = Value::Bool(false);` in
   the same function (or a sibling called from the same `prepare_session`
   step), so a bare `claude` in a tm-managed project is also covered, not
   only a `tm launch`-spawned one.
2. **Env var — managed-spawn belt.** `crates/trusty-mpm/src/runtime/claude_code.rs:200`
   (`env_bin_prefix`) is the single place that assembles the `env NAME=VALUE…`
   prefix for every managed spawn today (`CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN`,
   `CLAUDE_CODE_DISABLE_MOUSE`, `CLAUDE_CONFIG_DIR`, MCP env, the OAuth token —
   `claude_code.rs:210-225`). Add
   `assignments.push_str(" CLAUDE_CODE_DISABLE_AUTO_MEMORY=1");` unconditionally
   in the same block, following the doc comment's own POSIX-ordering rule
   (`-u` scrub flags before any `NAME=VALUE` assignment,
   `claude_code.rs:169-175`).

Per docs, the env var wins regardless of the subagent's own `memory:` field
("If auto memory is disabled (`CLAUDE_CODE_DISABLE_AUTO_MEMORY`), the
subagent's `memory` field has no effect") — so this also forecloses a future
agent asset accidentally opting back in via `memory: project` frontmatter.

**Confirmed: no deployed agent sets `memory:` today** (grep above), so this
is a pure addition, not a conflict to resolve.

**Test:** none exists — greenfield. `crates/trusty-mpm/src/core/session_launch/tests.rs`
already has `write_output_style_sets_active_style`-style precedent
(`settings.rs` doc comment lists the existing test names at
`settings.rs:195-201`); a new `write_output_style_disables_auto_memory` (or a
renamed sibling) and a `spawn_command_disables_auto_memory` mirroring
`spawn_command_sets_claude_config_dir` (`claude_code.rs` test list at
`claude_code.rs:178-190`) are the natural additions.

---

## E. Probe agents

Two files, written for the PM to copy into this project's `.claude/agents/`
and run (not run by this research agent — this task is design-only):

- `probe-minimal.md` — `model: haiku`, `tools: Read, Grep`, no `skills:` field.
- `probe-minimal-noskill.md` — identical, plus `disallowedTools: Skill`.

Both instruct: *"Answer only from your own context: count the tools you
have, say whether a skills listing is present and how many entries, whether
any CLAUDE.md is present, whether MEMORY.md is present, whether MCP tool
names are present. No tool calls."*

Written to:
`/private/tmp/claude-502/-Users-masa-trusty-mpm-projects-bobmatnyc-trusty-tools/69fad0cf-6275-461b-9585-3601d17fc2d2/scratchpad/probe-agents/probe-minimal.md`
and `probe-minimal-noskill.md` in the same directory.

**To run:** copy both into `<project_dir>/.claude/agents/`, then dispatch each
once (e.g. `Agent(subagent_type: "probe-minimal", ...)`) and read the
self-reported counts back — this is the one honest way to settle the two
open questions in this document: (a) whether `disallowedTools: Skill`
actually removes the skills-listing bytes from context or only blocks
invocation (§C), and (b) whether the roster's baseline (CLAUDE.md present,
MEMORY.md present, MCP names present, full skills listing) matches the
44.7K-token measurement's stated breakdown on a Haiku model with a
near-empty `tools:` list, isolating how much of that 44.7K survives a
`tools:` cut alone versus needing `skillOverrides`/`claudeMdExcludes` too.

**To remove after:** delete both files from `<project_dir>/.claude/agents/`
(they are hand-authored custom agents per `tm-agent-architecture.md`'s
official/custom distinction — no rebuild or `tm install` step undoes them;
a plain `rm` is sufficient and correct).

---

## F. PR-sized slices, ordered

Sizing basis: `wc -c` against `origin/main` and this checkout, all read
during this research session.

| # | Slice | What ships | Expected saving per affected turn | Why this size |
|---|---|---|---|---|
| **1** | **Per-agent `tools:` allowlist, roster-wide** (§C) | Add `tools:` to all ~38 non-BASE agent assets under `crates/trusty-agents-common/src/assets/agents/`, per the table in §C; extend `agent_builder`/`bundle` tests to assert every composed agent has a non-empty `Some(tools)` | Removes full tool **and MCP JSON schemas** for every server/tool the role doesn't use — this is the single largest lever in the measured 44.7K baseline (150 MCP tools' schemas dwarf their ~151 *names*, which alone are only a few KB). Precedented, mechanically simple (frontmatter-only, no new settings-file plumbing), reversible per-agent. | **Do this first.** No new tm-source code path — `tools:` parsing, merging, and deployment already exist and are exercised by `trusty-code`'s four forks. Zero risk to the PM (agent-scoped, not session-wide). Immediately measurable with the existing roster via `tm session instructions`-style before/after, no probe agents needed. |
| **2** | **`skillOverrides` for this project's irrelevant skill families** (§C) | `tm` computes an `"off"` map for skill namespaces the project's stack profile rules out (`aws-*`, `claude-in-chrome`, `duetto-design-system`, `xlsx`, `breeze-voice`, `cto-kb-ingest`, …) and writes it to `.claude/settings.json` at `prepare_session` | Shrinks the ~131-entry skills listing (session-wide, so this benefits the PM and every agent in one write) — a project like this one with ~50+ irrelevant AWS/Chrome/Duetto entries in the listing is paying their description bytes on every single turn, PM included, today | Second because it is session-wide and easy to get wrong (an "off" skill a human still wants to `/`-invoke by hand disappears from the menu too, per the doc's "Hidden" column) — needs a short allow/deny review, not a rebuild |
| **3** | **`CLAUDE_CODE_DISABLE_AUTO_MEMORY`** (§D) | `settings.rs` gains `autoMemoryEnabled: false`; `claude_code.rs::env_bin_prefix` gains the env var; two new tests | Removes `MEMORY.md` (measured 5.6KB for this project alone) plus its per-session read/write overhead, for the PM and every subagent | Third: small, isolated, two known write sites, no design ambiguity (§D has no open question, unlike §B) |
| **4** | **Repo-rule migration off `CLAUDE.md` and onto skills/briefs** (§B, path ii) | Author a `trusty-tools-rust-gates`-style skill carrying the test ladder / SLOC cap / changelog-fragment rules; add it to `skills:` for `rust-engineer`, `engineer`, `qa`, `local-ops`; update `BASE-AGENT.md`'s dispatch-brief guidance to name this as the carrier | Makes goal (2) true in substance (agents stop *needing* `CLAUDE.md`) without touching `claudeMdExcludes` at all, so it carries none of §B's PM-regression risk | Fourth: requires deciding exactly which rules move, a judgment call the earlier three slices don't require |
| **5** | **`claudeMdExcludes`, if goal (2) must be literal-zero-bytes** (§B, path i) | Write `claudeMdExcludes` for the project/ancestor/user `CLAUDE.md` paths at project settings tier; widen the compiled prompt to carry whatever prose the PM would otherwise lose; update the `instruction_pipeline.rs:587-589` / `claude_md_sections.rs:28-29` comments so they stay true | Only slice that gets a dispatched agent to **zero** `CLAUDE.md` bytes, provably | Last and optional: highest blast radius (session-wide, touches the PM's own instruction path, has two open sub-questions in §B — `/init`/`/memory` interaction, and the compensating-channel completeness) that slices 1-4 do not have, and it should not ship until slice 4 has already removed most of the reason a subagent would miss `CLAUDE.md` |

**Do first: Slice 1 (per-agent `tools:` allowlist).** It is the only slice
with zero design ambiguity, zero new settings-file plumbing, a working
precedent already in this codebase (`trusty-code`'s four forks), and the
largest single line-item in the measured 44.7K baseline (full tool/MCP
schemas, confirmed as an established fact in the task brief and consistent
with the size gap between ~151 MCP tool *names* and their *schemas*).

---

## Improvement recommendations

- **Symptom:** the owner-directive goals (2) and (4) both name settings keys
  (`claudeMdExcludes`, auto-memory disable) that no current `tm` test or
  source file references at all — this design had to be built from the
  Claude Code docs alone, with no existing tm behavior to verify against or
  regress-test.
  **Cause:** `crates/trusty-mpm/src/core/session_launch/settings.rs` has an
  established, well-tested pattern for writing a new project-settings key
  (`write_output_style`) but no generic "merge one more scalar/array key into
  `.claude/settings.json`" helper — every new key (`autoMemoryEnabled`,
  `claudeMdExcludes`, `skillOverrides`) will otherwise be implemented as a
  fourth near-duplicate read-merge-write block.
  **Change:** extract a small `merge_settings_key(project_dir, key, value)`
  helper from `write_output_style`'s existing read/merge/write body
  (`settings.rs:210-227`) before Slice 2 or 3 lands, so the three new keys in
  this document share one tested code path instead of three.
  **Evidence:** `settings.rs:202-236` is the only extant writer, and its own
  doc comment (`settings.rs:195-201`) already lists six tests for one key —
  a fourth key copy-pasting that pattern would need a fifth near-identical
  test file.
