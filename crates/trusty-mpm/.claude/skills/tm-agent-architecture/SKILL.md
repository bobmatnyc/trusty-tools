---
name: tm-agent-architecture
description: Official vs custom agent workflow — how to safely update trusty-mpm's compose-chain agent catalog
user-invocable: false
version: "1.0.0"
category: pm-workflow
tags: [agents, architecture, compose-chain, build-pipeline]
effort: medium
---

# Agent Architecture: Official vs Custom

## Fundamental Rule

**Never edit a deployed agent file directly.** `~/.claude/agents/*.md` are
build *outputs* of `core/agent_builder.rs::compose_agent` — editing them
edits a generated artifact that the next `tm install` / redeploy silently
overwrites.

### Detection: Is This Agent "Official"?

An agent is **official** (source-controlled, built via the compose chain) if
its name resolves inside `paths.agent_source_dir()` — which prefers the
`agents/agents/` git submodule when present, otherwise the bundled assets
under `crates/trusty-mpm/src/assets/agents/`. Check:

```bash
ls crates/trusty-mpm/src/assets/agents/ | grep <agent-name>
# or, if the submodule is populated:
ls agents/agents/ | grep <agent-name>
```

If found → official; follow the Official Agent Workflow below. If not found
and the file only exists under a project's `.claude/agents/`, it is
**custom** — edit it directly, no rebuild step needed.

## trusty-mpm's Compose Chain (Not claude-mpm's Python Agent Roster)

Unlike claude-mpm's Python agent registry, trusty-mpm agents are plain
Markdown files with YAML frontmatter that declare inheritance via `extends:`:

```yaml
---
name: rust-engineer
extends: base-engineer
---
```

`core/agent_builder.rs::compose_agent`:
1. Resolves `extends: base-engineer` against a case-insensitive map of
   `BASE-*.md` files (base templates use UPPERCASE stems; `extends:` values
   are lowercase — macOS's case-insensitive filesystem hides this mismatch,
   Linux does not, so resolution goes through an explicit lowercase map, not
   a raw path join).
2. Walks the chain base-first (max depth 8, cycle-detected), stripping
   intermediate frontmatter.
3. Returns one self-contained Markdown document with a single merged
   frontmatter block — this is what Claude Code actually reads.

`core/agent_deployer.rs::deploy_agents` / `deploy_agents_filtered` write the
composed output to `~/.claude/agents/<name>.md`, tracked by
`agent_manifest.rs` using the same managed/skipped/unchanged ownership model
`tm-skills` deployment uses (a hand-edited deployed file is detected via
checksum mismatch and left alone, never clobbered).

## Official Agent Update Workflow

### Step 1: Identify the Source

```bash
ls crates/trusty-mpm/src/assets/agents/ | grep <agent-name>
```

### Step 2: Update the Source

Edit `crates/trusty-mpm/src/assets/agents/<agent-name>.md` (or the `BASE-*.md`
template it extends, if the change is base-wide). Follow the project's
Why/What/Test doc-comment convention if you touch the surrounding Rust
wiring, not the Markdown itself.

### Step 3: Rebuild and Redeploy

```bash
cargo build -p trusty-mpm
tm install          # redeploys the composed agents to ~/.claude/agents/
# or, for the constants + ALL table wiring itself:
cargo test -p trusty-mpm --lib agent_builder
cargo test -p trusty-mpm --lib bundle
```

There is no `deepeval test --agent` equivalent in trusty-mpm — validate with
`cargo test -p trusty-mpm agent_builder` (composition correctness) and
`cargo test -p trusty-mpm --lib bundle` (the agent is present in `ALL` with
valid frontmatter and an `extends:` chain that resolves).

## Circuit Breaker

**BLOCK** any attempt to edit `.claude/agents/<official-agent>.md` directly —
it is a build output. Update the source under
`crates/trusty-mpm/src/assets/agents/`, rebuild, and redeploy instead. This
is the CB#1 (Large Implementation) pattern applied to agent files
specifically: delegate the source edit to the appropriate Engineer agent, do
not Edit the deployed copy.

### Wrong

```
Edit: ~/.claude/agents/web-qa.md   # VIOLATION — this is a composed build output
```

### Correct

```
1. Edit: crates/trusty-mpm/src/assets/agents/web-qa.md   # update source
2. Run:  cargo build -p trusty-mpm && tm install          # rebuild + redeploy
3. Test: cargo test -p trusty-mpm --lib bundle            # validate wiring
```

### Correct (Custom Agent)

```
Edit: .claude/agents/my-project-specific-agent.md   # OK — not in agent_source_dir()
```

## Related Skills

- `tm-workflow` — the phase model this agent catalog is delegated into
- `tm-delegation-patterns` — which agent to select for a given task
