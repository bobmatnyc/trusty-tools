---
spec_refs: []
---

# DOC-61 — Canonical Agent Standard: A Shared Source Model for trusty-mpm, trusty-code, and trusty-agents

**Status:** DRAFT
**Spec ID:** `SPEC-AGENTSTD-01~draft`
**Subsystem:** cross-crate — trusty-mpm (source model owner today), trusty-code (per-product builder, prospective), trusty-agents (assistant/sub-agent split, `agents::config`)
**Owner:** Architecture / Technical Leadership
**DOC-N claim:** `DOC-61`, scan-before-claim per DOC-38 §4.1. `DOC-60` is left open — a bus-messaging spec is being authored concurrently by another agent in this same review round and is expected to claim it; this document deliberately claims one number past it to avoid a collision. `DOC-42` is explicitly NOT reused: it is currently claimed by `docs/specs/agent-bundled-skills.md` (retiring) and is reserved for ADR-0016's Engineering Lead / Virtual Twin architecture (PR #3006) per owner instruction. See docs/specs/README.md's "next free DOC-N" note for the full collision ledger (DOC-42, DOC-46 both currently double-booked, neither touched by this document).

## 1. Executive Summary {#SPEC-AGENTSTD-01~draft}

This spec proposes a **canonical sub-agent standard** shared by trusty-mpm,
trusty-code, and trusty-agents, built by reusing — not reinventing — the
compose-chain model trusty-mpm already ships: Markdown-with-YAML-frontmatter
source files, `extends:`-chain inheritance flattened at build time, bundled
skill references carried in frontmatter, and a checksum-tracked deploy
manifest that never clobbers a hand-edited output file. This is an explicit
owner directive ("It's like to re-use the model tm defines... We define core
agents by type, they inherit bundled skills and properties in the tree"),
verified in this document against the actual Rust implementation
(`crates/trusty-agents-common/src/agents/{builder,deployer,manifest}.rs`,
moved there from `trusty-mpm::core::agent_*` under issue #2892 specifically
so a second harness could reuse it), not reconstructed from prose or the
`tm-agent-architecture` skill summary alone.

The standard splits cleanly into two halves that must not be conflated:

1. **A shared source model and compose semantics** — the `extends:` chain,
   the frontmatter merge rules, the bundled-skills union — is authoring-time
   and product-agnostic. This is the part this document standardizes.
2. **A per-product builder and per-runtime build artifact** is downstream and
   product-specific. trusty-mpm's builder emits Claude Code subagent files
   because Claude Code is trusty-mpm's runtime; trusty-code and trusty-agents
   have their own runtimes and therefore get their own builders and their own
   artifact shapes. Nothing in this document requires trusty-code or
   trusty-agents to emit a Claude Code subagent file.

Scope is **sub-agents only** (tier L1, in-process leaves, drawn from a typed
catalog). Assistants (tier L0 — `pm`, `izzie`, `cto-assistant`,
`personal-assistant`, `ctrl`) are a **distinct object kind** with their own
five-section schema (personality / knowledge / skills / listeners /
permissions, DOC-57) and are explicitly out of scope here, per the owner's
own framing and the freshly-drafted ADR-0024
(`docs/adr/0024-subagents-in-process-only-assistants-communicate-not-delegate.md`,
Proposed, not edited by this document).

Two findings surfaced during verification are significant enough to flag up
front, both expanded in §7:

- The claim "sub-agents are never hand-edited, never hand-installed" is true
  today only in the sense that no hand-install *entry point* exists. The
  **mechanism** that would need to detect and preserve a hand-edited deployed
  agent file already exists and is already active: `deploy_agents_filtered`
  classifies any deployed `.md` whose checksum no longer matches its manifest
  entry as user-modified and silently skips re-deploying it
  (`crates/trusty-agents-common/src/agents/deployer.rs:276-288`). This is a
  latent hazard, not a hypothetical one.
- Neither of the two formats actually in use today is literally "YAML" in the
  sense the owner's declarative-only decision states it. Sub-agents are
  Markdown-with-YAML-*frontmatter* (a hand-rolled line parser, not a full YAML
  library — see §6). Assistants are TOML (`agent.toml` + `persona.md`
  packages), not YAML, at every layer checked in this repository. §9
  addresses this honestly rather than silently rounding either format up to
  "YAML."

Status: **DRAFT**. This document does not commit any code and does not open
a PR.

## 2. Scope and Non-Goals {#SPEC-AGENTSTD-02~draft}

**In scope:**

- The source authoring model for **sub-agents** (tier L1 per ADR-0024): the
  `extends:` inheritance chain, frontmatter merge semantics, bundled-skill
  references, and the compose→deploy pipeline that turns sources into a
  per-product build artifact.
- The build pipeline shape: one shared composer, N per-product builders, N
  per-runtime artifacts — and the versioning/determinism/partial-failure
  questions that shape raises.
- The migration path from today's per-product ad hoc agent definitions to
  this standard.

**Explicitly out of scope (per owner decision 3):**

- Assistants/PM. Each assistant is a unique, user-instantiated entity, not a
  member of a typed catalog with an inheritance tree. Its configuration
  schema (five sections + a sub-agents whitelist) is governed by DOC-57 and
  ADR-0024, not by this document. Where this document must reference the
  assistant side (to justify treating the two as distinct schemas rather than
  one schema with a flag, §5), it defers to those documents rather than
  restating them.
- The specific mechanics ADR-0024 leaves to the owner (the reachable
  sub-agent whitelist's write-time floor, the 3-tool-call routing threshold,
  the lateral-assistant delegation gate). This document does not re-litigate
  those; it assumes ADR-0024's Decision 4 (an editable whitelist *over* the
  sub-agent catalog this document standardizes) as a downstream consumer of
  the catalog, not as something this document itself designs.
- Runtime tool-calling, permission scopes, and dispatch — those are
  per-runtime concerns downstream of the build artifact, not part of the
  source model.

## 3. tm's Actual Compose-Chain Model (Verified Against Code) {#SPEC-AGENTSTD-03~draft}

Verified directly against `crates/trusty-agents-common/src/agents/{builder,deployer,manifest}.rs`
(the implementation `trusty-mpm::core::agent_{builder,deployer}` now re-exports
verbatim, moved there under issue #2892 "so a second harness can reuse the
same composer instead of forking it" — `agent_builder.rs:1-16` is a pure
`pub use` shim). This is the actual, current mechanism, not the
`tm-agent-architecture` skill's summary of it — the skill is directionally
correct but this section is the primary source.

### 3.1 On-disk source format

A source agent is one Markdown file: an optional `---`-delimited frontmatter
block of `key: value` lines, followed by free-form instruction prose as the
body. Base templates live alongside concrete agents in the same source
directory, named with UPPERCASE stems (`BASE-AGENT.md`, `BASE-ENGINEER.md`,
`BASE-QA.md`, ...); concrete agents declare `extends: base-engineer` in
**lowercase**. Because macOS's default filesystem is case-insensitive and
Linux's is not, resolution never does a raw path join — `build_source_map`
(`builder.rs:161-177`) scans the directory once into a
`HashMap<lowercased_stem, PathBuf>`, and every lookup goes through that map.

### 3.2 Inheritance resolution

`resolve` (`builder.rs:839-866`) walks `extends:` recursively, base-first,
with two guards: a visited-path cycle check and a hard `MAX_DEPTH = 8`
(`builder.rs:42`). It returns two parallel base-first vectors — frontmatter
structs and body strings — which `render_composed` (`builder.rs:880-891`)
folds into one document: a single merged frontmatter block, then the bodies
concatenated with a blank line between each. `compose_agent`
(`builder.rs:906-911`) is the public entry point most callers use; a
disk-free variant, `compose_agent_in_memory` (issue #2958 Slice E1), shares
the identical walk/cycle/depth logic through the `SourceLookup` trait
(`builder.rs:130-150`) so an embedded-asset backend never forks the algorithm
— a concrete precedent for how a second product could plug in its own source
location without forking the composer itself.

### 3.3 Frontmatter merge semantics (this is the part a "canonical standard" must standardize)

`merge_frontmatter` (`builder.rs:708-823`) folds the chain base-first with
**per-field merge policy**, not a single blanket rule:

| Field | Merge policy | Rationale (from code comments) |
|---|---|---|
| `name`, `role`, `description`, `model`, `initialPrompt`, `resource_tier` | scalar, **child wins** | a concrete agent's own value must survive over its base's |
| `max_tokens` | scalar, **child wins** | same as `model` |
| `skills:` | **union**, de-duplicated, first-seen (base-first) order | "a list of dependencies naturally accumulates through inheritance the way body text concatenates" (`builder.rs:690-693`) — DOC-42 |
| `tools:` | **override**, keyed on `Option` presence not emptiness | a restrictive leaf must be able to narrow a permissive base's tool set to zero (`tools: []` is `Some(vec![])`, deliberately distinct from an omitted key, which is `None` and inherits) — issue #2897, code-critic finding on PR #2952 |

Two deploy-time enrichments are applied only after the chain merge, never
per-file: `model` is derived from `resource_tier` when no explicit `model`
survived (`tier_to_model`, `builder.rs:254-261`: `intensive→opus`,
`lightweight→haiku`, everything else including unrecognized values
`→sonnet`), and `initialPrompt` is derived from `role` when the source set
none (`default_initial_prompt`, `builder.rs:273-298`, a fixed per-role table;
interactive/special-purpose roles get no injected prompt).

The parser is deliberately **not a general YAML parser**. It handles the
subset trusty-mpm's own frontmatter needs: scalar `key: value` lines, an
inline flow-array grammar shared by `skills:`/`tools:` (`parse_list_value`),
and — since issue #2906's review — the YAML **block-sequence** form for
`skills:` specifically (`- item` continuation lines under a bare `skills:`
key, `builder.rs:334-405`, added because "every realistic upstream
`claude-mpm-agents` asset" uses block style, not inline flow style). A
malformed value warns and degrades to empty rather than hard-failing the
whole file (`builder.rs:419-436`), except an unterminated frontmatter block,
which is a hard `FrontmatterParse` error (`builder.rs:407-411`).

Every freshly composed file is quoted-on-emit where a plain YAML scalar would
be ambiguous or unsafe (`needs_quoting`/`render_scalar`, `builder.rs:596-676`
— empty values, YAML indicator characters, embedded `": "`/trailing `:`/
mid-string `" #"` comment starts, the YAML null tokens) and then **strictly
re-validated** against `serde_yaml` before it is ever written
(`crate::agents::frontmatter::validate_frontmatter`, invoked at
`deployer.rs:224`) — because trusty-mpm's own lenient reader accepting a
composition is not proof a strict downstream consumer (trusty-agents'
`serde_yaml`-based `.md` loader) will. This two-tier validation (lenient
compose, strict pre-write gate) exists precisely because issue #3556 found a
composition that round-tripped through the lenient parser but was invalid
YAML — recomposing alone could never have fixed it, because the bug was in
what was *emitted*, not what was read.

### 3.4 Deployment and the manifest

`deploy_agents_filtered` (`deployer.rs:150-340`) is the write path. For every
source agent whose stem the `select` predicate accepts, it composes, strict-
validates, and classifies the corresponding target file into exactly one of:

- **not present** → write it, record a fresh `ManifestEntry` (`source_chain`,
  `checksum`, `deployed_at`, `origin: Bundled`).
- **present, not manifest-tracked** → compare its checksum to the fresh
  composition; if byte-identical, **silently adopt** it into the manifest
  (issue #2504) with no rewrite; if it differs, **skip conservatively** and
  flag it as `untracked_modified` (surfaced once, aggregated, pointing at
  `tm install --reset-agents`).
- **present, manifest-tracked, checksum matches manifest** → safe to
  refresh; overwrite if the fresh composition differs, otherwise leave
  untouched (`unchanged`).
- **present, manifest-tracked, checksum does NOT match manifest** → the user
  hand-edited it → **skip**, preserving their edit (`deployer.rs:284-288`).

A single malformed source agent (unterminated frontmatter, a strict-YAML
failure) is isolated — logged, recorded in `DeployResult::failed`, and the
rest of the roster still deploys (`deployer.rs:202-236`, `failed`, `#2906`
review CRITICAL finding: "a single malformed agent asset... must never abort
the ENTIRE roster deploy"). §4.4 generalizes this per-agent isolation
guarantee to the cross-product build question.

Deployment priority — project `.claude/agents/` over user `~/.trusty-mpm/agents/`
over cached remote — is a **selection-of-source-directory** concern upstream
of `deploy_agents_filtered` (which agents/*sources* are visible to compose in
the first place), not part of the merge/write logic itself; this document
does not re-derive that precedence chain, which is `tm-agent-architecture`'s
domain and was not found to differ from the skill's description in the code
paths checked here.

### 3.5 Where documented behavior and actual behavior diverge

- The `tm-agent-architecture` skill's phrase "a hand-edited deployed file is
  detected via checksum mismatch and left alone, never clobbered" is
  accurate for the *documented* update workflow (edit source, rebuild,
  redeploy) but understates a consequence: that same detection also silently
  preserves a hand-edit made *without* going through the documented workflow
  at all — there is no distinction in the code between "an operator
  deliberately bypassed the official-agent rule" and "a future hand-install
  feature wrote here." See §7.
- No source or composed frontmatter field carries a **schema version**
  today. The only version present anywhere in this pipeline is the
  manifest's own top-level `"version": 1` (verified: this project's own
  `.claude/agents/.trusty-mpm-manifest.json`), which versions the *manifest
  file format*, not the *agent source schema*. §4.2 treats this as an open
  gap this standard should close, not a solved problem to restate.

## 4. Source Model vs. Build Artifact: One Model, Per-Product Builders {#SPEC-AGENTSTD-04~draft}

Owner directive, verbatim: *"we BUILD tm agents to the claude code standard,
we can have a custom builder if necessary."* The pipeline this standard
specifies is:

```
one shared source model + compose semantics (§3)
        │
        ├─▶ trusty-mpm builder ──▶ Claude Code subagent file (~/.claude/agents/*.md)
        ├─▶ trusty-code builder ─▶ trusty-code's own AgentConfig / runtime shape
        └─▶ trusty-agents builder ▶ trusty-agents' own AgentConfig / runtime shape
```

**This is not purely aspirational** — trusty-code already has a real,
narrower precedent for exactly this shape. `crates/trusty-code/src/plugins/agents.rs`
(`discover_plugin_agents`, Phase-1 plugin agent ingestion, issue #3539)
explicitly reuses trusty-mpm's own frontmatter/body parser
(`trusty_agents_common::agents::metadata::agent_metadata_from_str`,
`agents::md_loader::extract_body`) rather than forking it, and projects the
result into trusty-code's *own* `AgentConfig` type
(`agents::md_loader::project_to_agent_config`) — a different artifact shape
than a Claude Code subagent file. Its own module doc states the point
directly: *"a plugin's `agents/*.md` files are the exact same
Markdown+frontmatter format `agents::md_loader` already parses... the only
new work is namespacing and two Phase-1 leaf-only guarantees."* Two of those
guarantees are directly relevant to this standard: unsupported trusty-mpm
frontmatter fields (`effort`, `maxTurns`, `memory`, `isolation`,
`disallowedTools` — `agents.rs:38-44`) are dropped with one aggregated
warning rather than failing the load, and an `extends:` chain is **not**
composed for a plugin agent — Phase 1 locks every plugin agent as a leaf.
This is real, working evidence that a second product's builder can consume
the same source format without adopting the first product's full compose
semantics wholesale — exactly the "custom builder if necessary" the owner
described — but it also shows the compose-chain (§3.2) is not yet
universally shared: today, only trusty-mpm's own builder actually resolves
`extends:`.

### 4.1 What triggers a rebuild

`tm install` (and, per-session, the HR-2 selective deploy path via
`deploy_agents_filtered`'s `select` predicate) is the only trigger found in
this codebase. There is no file-watcher or on-save auto-rebuild; a source
edit is inert until the next explicit install/deploy call. This standard
does not propose changing that trigger model — it is out of scope for a
source-model spec — but flags it as a fact any per-product builder inherits:
whichever event triggers *that* product's builder (its own install command,
a CI step, a first-launch check) is that product's choice to make, not
something this shared layer prescribes.

### 4.2 Is the source model versioned?

**No, not today**, per §3.5. This is a gap, not a design decision — nothing
in `Frontmatter` (`builder.rs:189-239`) or the manifest schema
(`manifest.rs`, top-level `"version": 1` only) carries a per-agent-source
schema version. Recommendation: a canonical standard shared across three
products should add an explicit, optional `schema_version:` (or similarly
named) frontmatter key, absent-means-`1` for backward compatibility with
every source file that exists today, so a future builder can refuse
(loudly, not silently) to compose a source file declaring a schema version
newer than that builder understands, rather than either crashing on an
unrecognized field or silently misinterpreting it. This is new work, not
already built — flagged as an open question in §10 for the owner to confirm
priority against the migration converter (§8), which would be the natural
place to stamp the version on every converted file.

### 4.3 Is the build deterministic?

**Yes, verified two ways.** First, `compose_agent` is a pure function of its
inputs (source directory contents) with no non-determinism in the merge
logic itself — the dedicated test `fs_and_in_memory_compose_are_byte_equivalent`
(`builder_in_memory.rs`, cited from `render_composed`'s own doc comment,
`builder.rs:868-879`) exists specifically to assert the fs-backed and
in-memory-backed composers produce byte-identical output for identical
input. Second, and more concretely: **this very repository's own deployed
build artifacts are gitignored**, not checked in — `.claude/agents/` is
re-included early in `.gitignore` (line 27-28) for a now-superseded reason,
then unconditionally re-ignored by a later "auto-managed by tm" block
(`.gitignore:106-108`, `git check-ignore -v .claude/agents/BASE-AGENT.md` →
`.gitignore:106`, confirmed directly). A build artifact that is safe to
regenerate on every machine without ever diffing against a checked-in copy
is, by construction, being treated as deterministic-and-disposable in
practice, not merely in theory. **Recommendation: per-product build
artifacts should be gitignored**, matching this precedent, with the source
directory (this standard's actual subject) as the only checked-in tree.

### 4.4 Partial build failure: skip the product, or fail the whole build?

`deploy_agents_filtered` already answers this question for the *existing*
single-product (trusty-mpm) case: a per-agent compose or strict-validation
failure is isolated — logged, recorded, skipped — and the rest of the roster
still deploys (`deployer.rs:202-236`, §3.4). **Recommendation: generalize the
same isolation policy one level up, per (agent × product) rather than only
per agent.** If a source agent's frontmatter converts cleanly (composes,
strict-YAML-validates) but a specific product's builder rejects it — e.g. a
field that product's `AgentConfig` cannot represent, mirroring trusty-code's
own `UNSUPPORTED_AGENT_FIELDS` drop-with-warning precedent (§4 above) at a
stricter fail level — that product's build for that one agent should be
skipped with a loud, aggregated warning, not abort that product's entire
roster, and definitely not abort a *different* product's build. The existing
`DeployResult::failed` shape (`"<name>: <error>"` strings, `deployer.rs:93`)
is a reasonable model to extend with a product dimension rather than
redesigning from scratch.

## 5. Two Distinct Object Kinds: Assistant (L0) vs. Sub-Agent (L1) {#SPEC-AGENTSTD-05~draft}

Owner directive, verbatim: *"For subagents only, the PM/Assistant is
unique."* This is not a new position invented for this document — it is
ADR-0024's own ratified model (Proposed, `docs/adr/0024-...md`, not edited
here), restated in this spec's terms because a canonical agent standard has
to say explicitly which of the two kinds it governs.

| | **Assistant** (tier L0) | **Sub-agent** (tier L1) |
|---|---|---|
| Population | Unique, user-instantiated (`pm`, `izzie`, `cto-assistant`, `personal-assistant`, `ctrl`) | Drawn from a typed catalog; many instances of the same type |
| Delegation | Delegates DOWN to sub-agents; communicates LATERALLY with other assistants (never delegates to them, ADR-0024 decision 2) | Never delegates in any direction — a leaf with exactly one edge (responds only to its invoking assistant), **already structurally enforced today**: `DelegateToAgentTool` is registered only inside the `role == ASSISTANT_TIER_ROLE` branch of `build_registry_for_agent`; every other role branch omits it entirely (ADR-0024 §"Is 'sub-agents never delegate' enforced, or merely conventional?") |
| Configuration schema | Five sections — personality / knowledge / skills / listeners / permissions (DOC-57) — plus a sub-agents whitelist section (ADR-0024 decision 4) | This standard's compose-chain frontmatter + body (§3) |
| Authored by | GUI-driven config writes (e.g. `PATCH /api/agents/:name`) and hand-edited prose (`persona.md`) | Source `.md` files under version control; never GUI-written, never (today) hand-installed |
| Governed by | DOC-57 + ADR-0024 | This document |

**Recommendation: two distinct object kinds with two distinct schemas, not
one schema with a `kind`/`tier` flag.** The two rows above do not differ by
one toggle — they differ in population shape (unique vs. catalog-typed),
delegation direction (down-and-lateral vs. none), authoring surface (GUI +
prose vs. version-controlled source), and consequently in every downstream
question this document raises (versioning, determinism, round-trip
preservation, §6-§7). Collapsing them into one schema with a flag would force
the sub-agent format to carry accommodations (round-trip-safe editing,
GUI-writable fields, a whitelist section) it structurally does not need, and
would force the assistant format to pretend it participates in an
inheritance tree it does not — ADR-0024 is explicit that assistants
"communicate," not "extend," each other. This document's compose-chain
standard applies to the sub-agent schema only; it takes the assistant
schema's existence and shape as given from DOC-57/ADR-0024 and does not
restate or modify it.

## 6. On-Disk Format {#SPEC-AGENTSTD-06~draft}

The owner declined to choose a format and said "whatever the tm standard
is." Verified per §3.1: **Markdown with a `---`-delimited YAML-flavored
frontmatter block, body as free-form instruction prose.** This document
adopts that format as the canonical sub-agent source format, unmodified in
shape, for all three products' sub-agent catalogs.

**Flagged consequences of adopting this format as-is, honestly, rather than
silently deviating:**

- **The frontmatter parser is not a general YAML parser.** It is a hand-
  rolled `key: value` line reader (`parse_kv_line`) plus a purpose-built
  block-sequence reader added specifically for `skills:` (§3.3). A second
  product's builder that wants to add a new structured (non-scalar,
  non-flat-list) frontmatter field will either need the same kind of
  bespoke block-reading code added to this shared parser, or will need to
  restrict itself to scalars and flat lists. This is a real constraint on
  the format's extensibility, not a hypothetical one — it already required
  one purpose-built carve-out (the `skills:` block form) to match real
  upstream agent assets.
- **Quoting correctness was a real, shipped bug** (issue #3556): a
  composition could pass trusty-mpm's own lenient reader while being invalid
  strict YAML, and recomposing alone could not fix it because the bug was in
  what the composer *emitted*. The fix (`needs_quoting`/`render_scalar`,
  §3.3) is now in place, but any new per-product builder that re-serializes
  frontmatter (rather than only reading it) inherits the same class of risk
  and should reuse `render_scalar`/`escape_yaml_double_quoted` rather than
  re-deriving quoting rules.
- **No schema version field** — restated from §4.2, because it is as much an
  on-disk-format gap as a build-pipeline gap: a canonical format shared by
  three products needs a way for a reader to know which version of the
  schema a given file was authored against.
- **The format has no place for the assistant-side five sections.** Per §5,
  this is by design, not a gap — the sub-agent format is not asked to
  represent personality/knowledge/listeners/permissions, because sub-agents
  do not have them. Flagged here only so a future reader does not mistake
  the omission for an oversight.

No alternative format was found to solve these problems better without
abandoning the "reuse what tm already ships" directive, so this document
recommends adopting the format as-is with the version-field addition from
§4.2, rather than deviating.

## 7. Editability and the Round-Trip Boundary {#SPEC-AGENTSTD-07~draft}

Owner position: sub-agents are built artifacts — never hand-edited, never
hand-installed (no entry point exists), never machine-written back — so
there is no round-trip-preservation requirement for them, and YAML
comment/ordering destruction is a non-issue. Assistants have editable
instruction bodies and GUI-written config; the round-trip concern applies
there, and only there.

**This document agrees with the principle** (conflating the two would
over-constrain the sub-agent format for accommodations it does not need,
§5) **and recommends one format for sub-agents (no round-trip requirement,
free to regenerate byte-for-byte) and a format for assistants that
independently satisfies DOC-57/ADR-0024's round-trip needs** — not
necessarily the same format, and this document does not prescribe the
assistant format (out of scope, §2).

### 7.1 Verifying the "never hand-installed" claim against the deployer

**The claim is true about entry points and false about mechanism.** No code
path in this repository lets an operator hand-install a sub-agent file into
a deploy target the way, e.g., a skill can be manually dropped into a skills
directory. But `deploy_agents_filtered`'s classification logic (§3.4,
`deployer.rs:247-289`) is **identical in shape** to the skill deployer's
checksum-based skip the task description asked this be verified against —
and it is not merely analogous, it is the same manifest-ownership pattern
applied to agents specifically:

- A deployed agent file whose checksum no longer matches its manifest entry
  is classified as user-modified and **left alone, never overwritten**
  (`deployer.rs:276-288`).
- An untracked deployed file that happens to differ from the fresh
  composition is **also skipped**, conservatively, on the theory it might be
  user-owned (`deployer.rs:256-273`).

**This is a real, already-shipped latent hazard, not a hypothetical one
introduced by a future feature.** Nothing distinguishes, in this code, "an
operator manually edited a file in `~/.claude/agents/` by hand, bypassing
the documented source-edit-then-rebuild workflow" from "a future
hand-install feature legitimately wrote here." Today this matters only in a
narrow, low-consequence way — an operator who edits a deployed file directly
(against the documented workflow, but not prevented by any tooling) finds
their edit silently preserved across the next `tm install`, which looks like
correct, protective behavior and mostly is. **The hazard "becomes real" the
moment any hand-installation entry point ships**, exactly as this task
description anticipated: at that point, the SAME skip logic that today only
protects an edge-case manual edit will be the mechanism a legitimate,
sanctioned hand-install feature also passes through, and the standard will
need to decide — explicitly, not by default — whether a hand-installed
sub-agent is meant to survive future rebuilds (in which case this is the
correct, intended behavior and should be documented as such) or whether
hand-installed agents should be a visibly distinct, separately-tracked
category (e.g. a manifest `origin` value distinct from `Bundled`, which the
manifest schema already supports as an enum — `Origin::Bundled` is one
variant of a type built to have more than one).

**Recommendation:** this standard should not build new machinery to close
this gap now (no hand-install feature exists yet to close it for), but
should record the finding so the eventual hand-install design treats it as a
known, load-bearing decision rather than rediscovering it under time
pressure. Flagged for §10.

## 8. Migration: Hard Cut + Converter {#SPEC-AGENTSTD-08~draft}

Owner decision: one release converts every agent definition; old-format
support is removed. No dual-read compatibility layer.

### 8.1 Converter design

- **Reads:** every sub-agent source recognized by any of the three products'
  current ad hoc formats — trusty-mpm's existing Markdown+frontmatter
  sources (already this standard's target format, so this is close to a
  no-op pass for that side), and trusty-code's/trusty-agents' own current
  sub-agent-shaped definitions (whatever pre-standard format each has today,
  outside this document's own verified scope — the converter's per-product
  read adapters are per-product work this standard hands off, not something
  this document itself has fully audited across all three codebases).
- **Writes:** this standard's canonical Markdown+YAML-frontmatter format
  (§6), stamped with the `schema_version:` field this document recommends
  adding (§4.2), so every converted file is unambiguously versioned from the
  moment the converter first runs — closing the "no schema version today"
  gap and the migration in the same change, rather than two.
- **`extends:` chain handling:** carried over unchanged in *shape* — a
  converted child still declares `extends: <parent-stem>` — but every stem
  it references must resolve inside the converter's *combined* output set
  (a source that `extends:` a base template the converter did not also
  convert is a converter error for that agent, not a silent dangling
  reference). The converter should run the existing `resolve`/cycle/depth
  logic (§3.2) against its own converted output as a self-check before
  declaring success, since that logic already exists and already does
  exactly this validation.
- **Package-vs-flat-file duality:** today this duality exists on the
  *assistant* side only (`agents/<name>/agent.toml` directory package wins
  over a flat `<name>.toml`, per DOC-57 §"P-1" and `loader.rs:163`) — it is
  out of this document's scope by §5/§2, and the converter does not need to
  resolve it for sub-agents, which have no packaged form today. If a future
  per-product sub-agent source ever grows a packaged form, the converter
  should apply the same win-precedence rule assistants already use, for
  consistency, rather than inventing a second precedence rule.
- **An agent the converter cannot convert:** flagged and skipped, not a
  fatal converter run — mirroring the per-agent isolation principle §3.4/§4.4
  already establish for compose and per-product build failures respectively.
  The converter should report every skipped agent by name and reason in one
  aggregated summary (matching the existing `DeployResult::failed` /
  `untracked_modified` aggregated-warning pattern, §3.4) rather than either
  aborting the whole run or failing silently.
- **When it runs:** **recommend an explicit CLI command** (e.g. `tm agents
  convert` or product-equivalent), run once per install as part of the
  release that ships the hard cut, with a `--dry-run` mode that reports what
  would convert/skip without writing anything. Recommend against install-time
  automatic conversion with no operator visibility (a hard cut with no
  fallback is exactly the case where an operator should see the before/after
  diff, not have it happen invisibly during an unrelated `tm install`), and
  against first-load lazy auto-convert (it would leave the on-disk state
  ambiguous about whether "not yet converted" and "converted, using the old
  format because conversion produced this" are distinguishable — an explicit,
  logged, one-time command is not).
- **Reversibility:** **not reversible** as a hard cut — old-format support is
  removed in the same release, so there is no dual-format fallback to revert
  to. The mitigating fact the task description names is real and load-
  bearing: because deployed sub-agent artifacts are **derived** (§4.3), the
  actual blast radius of an imperfect conversion is the *source* definitions
  only — a corrupted or lossy conversion of one source file does not corrupt
  any already-deployed artifact for any other agent, and re-running the
  builder against a corrected source regenerates the artifact cleanly. This
  is a genuinely different risk profile than migrating, say, a database
  schema: the "data" here is mostly re-derivable, not append-only state.

### 8.2 Breakage cost — live agents affected

Per the task's own enumeration, live agent definitions on disk today include
`assistant`, `cto-assistant`, `izzie`, `researcher`, `ticketing-agent`,
`local-ops-agent`, plus every in-repo bundled definition under
`crates/trusty-mpm/src/assets/agents/` (42 composed files in this
repository's own `.claude/agents/` at the time of writing — §4.3). Per §5,
`assistant`/`cto-assistant`/`izzie` are **assistants (L0)**, out of this
converter's scope entirely — their config format (TOML `agent.toml` +
`persona.md`) is untouched by a sub-agent-format hard cut. `researcher`,
`ticketing-agent`, `local-ops-agent`, and the bundled trusty-mpm roster are
**sub-agents (L1)** and are exactly this converter's subject.

### 8.3 Per-field carry-over table

| Field | Carries over unchanged | Changes shape | Dies |
|---|---|---|---|
| `extends` chain | Yes — same grammar, same base-first walk (§8.1) | | |
| package-vs-flat-file duality | | N/A to sub-agents today (§8.1); assistants keep their own existing duality, untouched by this document | |
| `hidden` | | | **Dies** — not present in trusty-mpm's current `Frontmatter` struct (`builder.rs:189-239` has no such field); found only in trusty-code's plugin-agent `UNSUPPORTED_AGENT_FIELDS` drop list (`agents.rs:38-44`) as a field trusty-mpm's own schema has no slot for today. Not carried forward unless the owner explicitly reintroduces it. |
| `display_name` | | | **Dies** — same evidence as `hidden`; not present in the verified `Frontmatter` struct. |
| `role` | Yes — scalar, child-wins, drives `initialPrompt` derivation (§3.3) | | |
| `tier` | | **Changes shape** — `tier` as ADR-0024/`AgentTier` defines it (`config.rs:680-713`, `l0`/`l1`) is an **assistant-side** concept describing delegation authority, not a sub-agent frontmatter field. A sub-agent is unconditionally L1 by construction (no field needed, §5) — carrying a `tier:` key into the sub-agent schema would be a category error under ADR-0024's own model. | |
| `[skills].allow` | | **Changes shape** — becomes the `skills:` frontmatter list (§3.3, union-across-chain), which is a *declaration of dependency*, not an *allow-list gate*; DOC-42's own model treats it as co-deployment input, not a permission surface. | |
| `[subagents].allowed` | | **Changes shape, and lives on the assistant side, not here** — this is ADR-0024 decision 4's editable whitelist *over* the sub-agent catalog this document produces; it is consumed by, not part of, the sub-agent source format. Out of scope by §2/§5. | |

Recommendation: the converter's own summary output should restate this table
per-agent (which fields it read, which it dropped, which it reshaped) rather
than only reporting success/skip, since `hidden`/`display_name` dying
silently is exactly the kind of loss an operator should see named, not
infer.

## 9. Relationship to the Declarative-Only Decision {#SPEC-AGENTSTD-09~draft}

Standing owner decision: agents are declarative-only — no coded agents,
defined entirely as instructions with YAML primitive bindings. The question
this section must answer honestly: is adopting the compose-chain format
(§6) that decision *finally landing*, or a *new* decision?

**Mostly the former, with one honest correction.** The "no coded agents,
instructions + declarative bindings" half of the standing decision is
already exactly what §3 verified: a sub-agent source is Markdown prose (the
instructions) plus a frontmatter block of scalar/list bindings (`role`,
`model`, `skills:`, `tools:`) — there is no code, no conditional logic, no
programmatic agent definition anywhere in the pipeline checked in this
document. Adopting this format as the *canonical, shared* standard is best
read as **the declarative-only decision landing across all three products**,
not a new decision requiring separate ratification — it generalizes an
already-shipped trusty-mpm reality rather than introducing a new constraint.

**The correction:** the standing decision's own wording — "YAML primitive
bindings" — does not literally match either format verified in this
codebase. Sub-agents are Markdown with YAML-*flavored* frontmatter (a
hand-rolled subset parser, §6, not a full YAML library, and strict-YAML-
validated only at the final pre-write gate). Assistants are **TOML**
(`agent.toml` + `persona.md` packages, verified via DOC-57's own citations —
`agent.toml`'s `[[stores]]`, `[tools].allow`, `[tools].scopes` tables,
`persona.rs`, `loader.rs:163`), not YAML, at every layer this document
checked. Neither side of the product today is literally "YAML" end to end.

This document recommends **not** forcing a literal-YAML rewrite of either
format to make the standing decision's wording true by letter rather than by
spirit — §6 already found real, working reasons to keep the sub-agent format
as-is, and the assistant side's TOML choice is DOC-57/ADR-0024's decision to
revisit, not this document's. Recommendation: treat "YAML primitive
bindings" as shorthand for "declarative, structured, non-programmatic key-
value + list bindings" (which both formats genuinely are), and either amend
the standing decision's wording to say so explicitly, or accept the
imprecision as understood-but-not-literal. This is flagged for the owner in
§10 rather than resolved unilaterally here.

## 10. Open Questions for the Owner {#SPEC-AGENTSTD-10~draft}

These cannot be resolved from code alone; each names the section that raised
it.

1. **Schema versioning (§4.2, §8.1):** approve adding a `schema_version:`
   frontmatter key (absent = `1`), stamped by the migration converter, so a
   future builder can refuse a source it does not understand rather than
   silently misreading it?
2. **Deterministic-artifact gitignore precedent (§4.3):** confirm that
   per-product deployed sub-agent artifacts (trusty-code's, trusty-agents'
   own build outputs, not only trusty-mpm's) should be gitignored by default,
   matching this repository's own existing `.claude/agents/` precedent,
   rather than checked in per product?
3. **Partial-product build failure (§4.4):** confirm the recommended policy
   — isolate per (agent × product), skip that one agent for that one
   product with a loud warning, never abort a whole product's roster or a
   different product's build — as the intended behavior, and confirm whether
   the existing `DeployResult::failed` shape should be extended in place or
   a new cross-product result type introduced.
4. **The hand-install / checksum-skip hazard (§7.1):** now that it is
   verified as an already-shipped mechanism (not a hypothetical), decide
   explicitly — when a hand-install entry point eventually ships, should a
   hand-installed sub-agent be (a) silently preserved across rebuilds
   exactly like an accidental hand-edit is today, or (b) tracked under a
   visibly distinct manifest `origin` so it is never confused with a bundled
   artifact? This document takes no position; it only surfaces that the
   mechanism enabling (a) already exists whether or not (b) is chosen.
5. **Converter timing and command surface (§8.1):** confirm `tm agents
   convert` (or a product-appropriate equivalent name) with a `--dry-run`
   mode, run explicitly once per product as part of the hard-cut release, is
   the intended migration UX — versus, e.g., folding it silently into the
   next `tm install` with no separate command.
6. **`hidden`/`display_name` (§8.3):** confirm these standing claude-mpm-era
   fields are intentionally dropped (not reintroduced) under the canonical
   standard, since no current trusty-mpm `Frontmatter` field represents
   them and no owner decision reviewed here calls for reviving them.
7. **"YAML primitive bindings" wording (§9):** amend the standing
   declarative-only decision's wording to acknowledge neither verified
   format (sub-agent YAML-frontmatter, assistant TOML) is literally YAML end
   to end, or treat the imprecision as already understood and not requiring
   a wording change?
8. **trusty-code's and trusty-agents' current sub-agent formats (§8.1):**
   this document verified trusty-mpm's compose-chain implementation in
   detail and trusty-code's Phase-1 plugin-agent *reader* (which already
   reuses the same frontmatter parser, §4), but did not fully audit every
   pre-standard sub-agent definition format trusty-code and trusty-agents
   use today outside that one plugin path. The converter's per-product read
   adapters (§8.1) are real, non-trivial per-product work this document
   flags but does not scope in detail — worth a follow-up research pass
   per product before implementation begins.

## 11. References {#SPEC-AGENTSTD-11~draft}

**Code (verified directly in this repository, this worktree):**

- `crates/trusty-agents-common/src/agents/builder.rs` — compose-chain
  implementation: `Frontmatter`, `split_frontmatter`, `merge_frontmatter`,
  `resolve`, `render_composed`, `compose_agent`, `source_chain`,
  `needs_quoting`/`render_scalar` quoting-on-emit (issue #3556).
- `crates/trusty-agents-common/src/agents/deployer.rs` — `deploy_agents`,
  `deploy_agents_filtered`, `DeployResult`, checksum-based manifest
  classification, per-agent failure isolation (#2906).
- `crates/trusty-agents-common/src/agents/manifest.rs` — `AgentManifest`,
  `ManifestEntry`, `Origin`, `checksum`, `atomic_write`.
- `crates/trusty-mpm/src/core/{agent_builder,agent_deployer}.rs` —
  source-compatibility re-export shims (#2892).
- `crates/trusty-code/src/plugins/agents.rs` — Phase-1 plugin agent
  ingestion, the concrete precedent for a second product's builder reusing
  the shared frontmatter parser (#3539).
- `crates/trusty-agents/src/agents/config.rs` — `AgentTier`,
  `SubagentsConfig`, the assistant-side TOML config model (`config.rs:240,
  680-790`).
- `.gitignore` (this repository) — the deployed-artifact gitignore
  precedent (§4.3), `git check-ignore -v .claude/agents/BASE-AGENT.md` →
  `.gitignore:106`.
- `.claude/agents/.trusty-mpm-manifest.json` (this repository) — a live
  manifest instance, cited for its `"version": 1` top-level field and
  per-entry `source_chain`/`checksum`/`origin` shape.

**Specs and ADRs:**

- `docs/adr/0024-subagents-in-process-only-assistants-communicate-not-delegate.md`
  — the L0/assistant vs. L1/sub-agent tier model this document takes as
  given (§5). Proposed, not edited by this document.
- `docs/adr/0016-orchestration-hierarchy-lead-pm-assistant.md` — the
  singleton-"ASSISTANT" hierarchy role ADR-0024 §5 distinguishes from
  trusty-agents' plural assistant population; also the reservation target
  for DOC-42 (not claimed by this document).
- `docs/specs/agent-config-five-sections.md` (DOC-57) — the assistant-side
  five-section schema this document defers to rather than restates (§2, §5).
- `docs/specs/agent-bundled-skills.md` (DOC-42) — the `skills:`
  co-deployment model §3.3's `skills:` union merge implements.
- `docs/specs/README.md` — the DOC-N catalog and scan-before-claim
  collision ledger (DOC-42, DOC-46 both currently double-booked; DOC-58/
  DOC-59 the most recently claimed numbers at the time this document claimed
  DOC-61).
- `.claude/skills/tm-agent-architecture/SKILL.md` (skill content) — the
  official-vs-custom-agent workflow summary this document verifies against
  and, in §3.5, notes one place it understates.

**Related open work referenced but not resolved here:** ADR-0024's own open
questions (the reachable-sub-agent-whitelist write-time floor, the
3-tool-call routing threshold, the lateral-delegation gate fix, issue
#4201's persona.rs gap) — all downstream consumers of, not part of, the
sub-agent catalog this document standardizes.

## 12. Change Log {#SPEC-AGENTSTD-12~draft}

- 2026-07-28 — Initial DRAFT. Claims `DOC-61` (scan-before-claim, DOC-38
  §4.1; `DOC-60` deliberately left for a concurrently-authored bus-messaging
  spec; `DOC-42` explicitly not reused). Sub-agent scope only, per ADR-0024
  and owner decision 3. Compose-chain model (§3) verified directly against
  `crates/trusty-agents-common/src/agents/{builder,deployer,manifest}.rs`
  and this repository's own live `.claude/agents/` deployment.
