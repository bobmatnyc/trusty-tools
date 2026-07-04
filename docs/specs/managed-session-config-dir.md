# DOC-34 — Managed sessions launch with a tm-owned CLAUDE_CONFIG_DIR under ~/.trusty-tools

**Status:** Draft
**Subsystem:** trusty-mpm — managed-session launch / provisioning
**Owner:** Engineering (trusty-tools)
**Last-updated:** 2026-07-03
**Spec ID:** `SPEC-CFGDIR-01~draft` … `SPEC-CFGDIR-05~draft` (DOC-34)
**Builds on:** the FULL-SEGREGATION design philosophy (Bob, 2026-07-02 — tm-managed
session artifacts must never leak into the user's live checkouts or global `~/.claude`);
DOC-31 SPEC-PROVISION (system vs project agents & skills).
**Cross-ref:** `crates/trusty-mpm/src/core/standalone/global_config.rs`
(`ensure_global_config_dir` — the reference impl), `core/model_inject.rs`
(`SETTING_SOURCES_FLAG`, `build_claude_command`), `core/session_launch/mod.rs`,
`core/paths.rs` (`FrameworkPaths`, currently rooted at `~/.trusty-mpm`),
`core/trusty_tools_config.rs` (`~/.trusty-tools` base).

---

## 1. Problem (regression)

The **original design** launches Claude Code against a **tm-owned config directory**,
fully provisioned by tm (agents, skills, settings, hooks, MCP, scoped `.claude.json`)
— never `~/.claude`, never the project's committed `.claude/`. The **standalone** path
still does this (`ensure_global_config_dir(managed_root, <managed_root>/claude-config)`).

The **managed daemon launch regressed** to `claude --setting-sources project,local`
(`model_inject.rs:70`) with **no `CLAUDE_CONFIG_DIR`**. Its comment claims it reads "the
project (tm-owned workspace `.claude/settings.json`)" — but that is false whenever the
project **commits its own `.claude/`** (e.g. trusty-tools). Consequences:
- Segregation defeated — tm reads the repo's committed config, not a tm-owned one.
- The agent roster is whatever the project committed (trusty-tools: a partial set of 7,
  one malformed) → the PM cannot spawn `rust-engineer`/`research`/`qa`/… → falls back to
  `general-purpose` (issue #1996).
- `~/.claude` is still consulted for the user-settings exclusion instead of being
  replaced outright.

## 2. Decision

- **Managed sessions MUST launch with `CLAUDE_CONFIG_DIR` pointing at a tm-owned config
  directory**, fully provisioned by tm with the complete, valid roster — reusing the
  standalone `ensure_global_config_dir` machinery. `~/.claude` is never used.
- **Config home consolidates under `~/.trusty-tools`** (Bob directive 2026-07-03: all
  crate config under `~/.trusty-tools`). The managed config dir is
  **`~/.trusty-tools/trusty-mpm/claude-config/`** (shared framework roster for managed
  sessions), superseding the `~/.trusty-mpm` `FrameworkPaths` root for this purpose.
- **Provisioning is complete, not filtered** — the full asset roster
  (`src/assets/agents/*` incl. engineer/rust-engineer/research/qa/local-ops/
  version-control/ticketing + specialists; all `tm-*` skills) deploys into the config
  dir. No partial curation.

## 3. Requirements

- **SPEC-CFGDIR-01 (config home):** Managed launch computes
  `~/.trusty-tools/trusty-mpm/claude-config/` (a shared, tm-owned config dir) and sets
  `CLAUDE_CONFIG_DIR=<that dir>` in the launched `claude` process environment (via the
  tmux command env prefix). `.claude.json` trust for the session's workspace is seeded
  INTO this dir (`preseed_managed_trust`), never `~/.claude.json`.
- **SPEC-CFGDIR-02 (full provisioning):** The config dir is provisioned via
  `ensure_global_config_dir` (settings.json + managed hooks + MCP + agents + skills),
  deploying the COMPLETE valid agent roster. The provisioning runs/refreshes at daemon
  start (or first managed spawn) and is idempotent.
- **SPEC-CFGDIR-03 (no project-.claude shadowing):** Because a managed workspace is a
  checkout of the repo, a **committed** `<workspace>/.claude/agents/` would still shadow
  the tm config dir. The launch MUST NOT let a committed project `.claude/` override the
  framework roster — resolve via `--setting-sources` selection and/or by not treating a
  committed project `.claude/agents` as authoritative. (Verify the exact `CLAUDE_CONFIG_DIR`
  + `--setting-sources` interaction empirically.)
- **SPEC-CFGDIR-04 (trusty-tools cleanup):** Remove trusty-tools' committed
  `.claude/agents/` (the partial set with the malformed `rust-engineer.md`) and its
  committed `.claude/settings.json` so the repo stops shadowing the tm config dir.
- **SPEC-CFGDIR-05 (verification):** A managed session spawned after this change MUST
  expose the full specialist roster as spawnable subagents (`rust-engineer`, `research`,
  `qa`, `engineer`, `local-ops`, `version-control`, `ticketing`) and MUST NOT read
  `~/.claude`. A `tm doctor` check surfaces the effective config dir + roster health.

## 4. Non-goals / follow-ups

- The **broader `~/.trusty-mpm` → `~/.trusty-tools` migration** of all `FrameworkPaths`
  and other crates' config homes is a separate epic (this spec only relocates the managed
  claude-config). Track separately.
- Per-session (vs shared) config dirs: shared is chosen for efficiency (identical roster);
  revisit only if per-session agent/skill overrides become a requirement.

## 5. Open questions

1. `CLAUDE_CONFIG_DIR` + `--setting-sources` precise interaction — does the project layer
   still add/override agents? Determine empirically and set `--setting-sources` accordingly
   (likely keep `project,local` for project CLAUDE.md while the framework roster comes from
   `CLAUDE_CONFIG_DIR`).
2. Refresh cadence — provision once at daemon start vs per spawn (idempotent either way).
