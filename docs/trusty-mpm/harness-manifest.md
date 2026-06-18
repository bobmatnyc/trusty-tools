# Harness Manifest (HR-2 / DOC-17)

`trusty-mpm` provisions every harness from a **manifest** — a TOML document that
declares *what* a harness receives: which agents and skills to deploy, which
instruction layers to fold in, the output style, which MCP servers to inject, and
per-tier model overrides. The manifest is **optional**: when none is present the
compiled-in default reproduces the historical provisioning behavior exactly, so
no manifest = no change.

This implements **HR-2** of the harness-runner vision
(`docs/specs/harness-runner-vision.md`, DOC-17).

## Precedence (NORMATIVE)

The effective manifest is resolved layer-by-layer, highest precedence first:

1. **Project override** — `<project>/.trusty-mpm/manifest.toml`
2. **User config** — `~/.trusty-mpm/manifest.toml`
3. **Catalog manifest** — `~/.trusty-mpm/catalog/repo/.claude/manifest.toml`
   (synced by `CatalogSync` from the configurable claude-mpm repo/ref)
4. **Compiled-in default** — the floor that reproduces today's behavior

A higher layer overrides only the **sections it sets**; every other section falls
through to the layer below. A missing or malformed layer is logged and skipped —
a launch is **never** blocked by an unreadable manifest.

**Per-section merge mode.** How a *set* section combines with the layer below
depends on the section:

| Section | Merge mode | Rationale |
|---|---|---|
| `[mcp]`, `[instructions]` | **Field-by-field** | Small structs of independent toggles; a higher layer's `Some` wins *per field*, a `None` field inherits the lower layer. So `[mcp] trusty_search = false` alone leaves `trusty_memory` untouched. |
| `[agents]`, `[skills]` | **Whole-section replacement** | The include/exclude lists are a complete set; a higher layer states the full agent/skill selection rather than field-merging two lists. |
| `[style]`, `[models]` | **Whole-section replacement** | Simple scalar/group sections; the higher layer's section wins outright. |

The catalog source (repo URL, git ref, TTL) is configurable via the
`[manifest]` section of `~/.trusty-mpm/config.toml` (highest), the
`TRUSTY_MPM_CATALOG_REPO` / `_REF` / `_TTL_HOURS` env vars, then the compiled-in
default.

## Output-style precedence

The manifest's `[style] active` slots in **below** the existing HR-4 sources:
`--style` flag > `[style] active` config key > manifest `[style] active` >
professional default.

## Schema

```toml
# Schema version (forward-compatible; HR-3 hashes/compares manifests).
version = 1

[agents]
# Agent names or glob patterns to deploy. Empty/omitted = all available.
include = ["*-engineer", "qa", "research"]
# Names/patterns to drop. Exclude always wins over include.
exclude = ["php-engineer"]
# Where source agent files come from: "bundled" (default) or "catalog".
source = "bundled"

[skills]
include = []            # empty = all bundled/catalog skills
exclude = []
source = "bundled"

[instructions]
# Optional layers. The non-overridable PM floor (BASE_PM) is always applied.
system = true
contextual = true       # the project CLAUDE.md layer
domain = true           # the delegation-authority layer

[style]
# Default output-style id (must match a bundled style).
active = "trusty-mpm"   # or "trusty-mpm-teacher" / "trusty-mpm-research"

[mcp]
# Which MCP servers to inject into the project's .mcp.json. Both default on.
trusty_memory = true
trusty_search = true

[models]
# Per-tier model id/alias overrides (HR-1's built-in mapping applies otherwise).
lightweight = "haiku"
standard = "sonnet"
high = "sonnet"
intensive = "opus"

[manifest]
# (config.toml only) Configure the catalog source for the catalog layer.
# repo = "https://github.com/me/my-fork"
# git_ref = "main"
# ttl_hours = 24
```

Every section is optional. The simplest useful override is a single section, e.g.
a project that only wants the Rust engineer and the research style:

```toml
# <project>/.trusty-mpm/manifest.toml
[agents]
include = ["rust-engineer", "qa", "research"]

[style]
active = "trusty-mpm-research"
```

## How `prepare_session` consumes it

`prepare_session` resolves the manifest, materializes a `HarnessPlan`
(`crate::core::manifest::HarnessPlan`), and drives the existing deploy machinery
with it:

- the agent/skill **source directory** (bundled vs catalog) and an
  **include/exclude selection predicate** restrict which content deploys;
- the `[mcp]` toggles gate the `trusty-memory` / `trusty-search` injections;
- the `[style] active` becomes the lowest-precedence output-style default.

The manifest decides *which* content; the established compose / ownership /
atomic-write machinery still performs the deployment.

> **⚠️ Deselecting does not prune already-deployed files.** Removing an agent or
> skill from a manifest's selection only stops `prepare_session` from *(re)deploying*
> that file on the next launch — it does **not** delete a copy that a previous launch
> already wrote into `~/.claude/agents/` (or the skills dir). So if you launched once
> with `rust-engineer` included and then drop it from `include`, the stale
> `~/.claude/agents/rust-engineer.md` remains on disk and Claude Code will still load
> it. User-visible implication: tightening a selection has no effect until the
> previously-deployed files are removed by hand. Automatic pruning (reconciling the
> deployed set against the resolved manifest) is an **HR-3** concern, not HR-2.

## Scope

HR-2 covers provisioning **from** a manifest. The update-check + rebuild-offer
(comparing catalog content hashes against the deployed manifest) is **HR-3**
(#1408); the schema's `version` field is the forward-compatibility anchor for it.
