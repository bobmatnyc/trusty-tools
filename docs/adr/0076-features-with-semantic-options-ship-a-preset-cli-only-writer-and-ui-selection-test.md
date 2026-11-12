# 0076. Features with semantic options ship a pre-canned default, the CLI is the only writer, and a selection test decides UI surfaces

- **Status:** Proposed
- **Date:** 2026-11-10
- **Scope:** Workspace-wide
- **Reversibility Cost:** **Low.** The rule governs new features and the config of existing ones as they are touched. No data format changes by itself; each feature adds its own `schema_version`.
- **Decision Drivers:** users cannot learn a config schema from a blank file; a dashboard that writes config duplicates the CLI and drifts from it; defaults shipped as files do not propagate to existing installs; a UI per feature is a permanent cost; epic [#10894](https://github.com/bobmatnyc/trusty-tools/issues/10894) decision D6.
- **Supersedes / Superseded by:** —

---

## Context

Some features take options a user must choose. The Architect watch set and the Architect IL schedule are two. The IL schedule ships a default, but its per-session entries (`sessions: []`) have no default, no example and no CLI verb. Users edit the file by hand.

Epic #10894 decision D6 already states one rule for one file: the CLI is the only writer of `watch.json`, and the dashboard queues a request that a deterministic applier runs through the CLI. No ADR states this rule for other features, or says which features need a dashboard surface.

Proposed by Maciek; evaluated in Architect brief MB1045.

## Decision

**D1. Semantic options.** An option is semantic when the user must choose a value that changes behaviour and the choice persists per project or per host. A toggle is not a semantic option.

**D2. Preset.** A feature with semantic options ships a default preset embedded in the binary. The preset works for the common case and is also the documented example.

**D3. CLI is the only writer.** The feature has a CLI with `show`, edit verbs (`set`, `add`, `remove` as fits) and `reset` to the preset. The config file carries `schema_version`. Nothing else writes the file.

**D4. UI calls the CLI.** A dashboard or other UI calls the CLI, or the same daemon method the CLI calls. It never writes the file. Parity test: the same fixture through the CLI path and the UI path yields identical files.

**D5. Selection test.** A feature needs a preset and the CLI verbs of D3 when all of these hold:

1. It takes semantic options (D1).
2. The options persist per project or per host.
3. Someone other than the author uses it. If only the author uses it, keep a documented file and add no surface.

It also needs a dashboard surface only when one of these holds:

4. The user acts on it while supervising, weekly or more often.
5. Users who do not use the CLI change the options.

Otherwise it is CLI only. Applied: the watch set meets 1–5 (dashboard in Phase 2 of #10894). The IL schedule thresholds are CLI plus preset, no dashboard. Per-session IL enrollment is CLI plus preset; a dashboard is optional.

**D6. Preset updates.** A new preset never overwrites an edited file. The CLI offers `diff` against the current preset and a versioned migration, in the style of [ADR-0067](0067-versioned-palace-format-with-n-minus-1-migration.md). Importing a hand-written config happens once (epic #10894 D9).

**D7. Generated docs.** Docs and examples are generated from the preset. A CI check compares the generated output with the committed copy.

**Out of scope.** Awareness of new features (a whats-new notice) is a separate concern and is tracked separately.

## Consequences

- A new feature with semantic options has a defined minimum: preset, `show`, edit verbs, `reset`, `schema_version`, `diff`.
- Dashboards stay thin. A feature that fails the D5 test gets no UI.
- Docs cannot drift from the shipped default.
- Each affected feature pays for the CLI and the migration step. Existing hand-edited configs need a one-time import.
- The parity test adds one fixture per UI-backed feature.

## Related Decisions

Vetted against prior ADRs on 2026-11-10:

- **ADR-0060 (One MCP config authority in trusty-mcp):** Extends in spirit. It makes one component the authority for one config domain; this ADR applies the single-writer idea to feature config generally. No conflict.
- **ADR-0042 (MCP configuration is static and persistent) and ADR-0058:** Consistent. Config persists once at user scope; this ADR adds no runtime-only config.
- **ADR-0007 (Tool contract versioning and verb model):** Consistent. The monotonic-integer version model is reused for `schema_version`.
- **ADR-0064 (Instructional content tracked separately from code):** Consistent. It governs where preset files live in the source tree.
- **ADR-0067 (Versioned palace format with N-1 migration):** Consistent. It is the precedent for migrating stored user data in D6.
- **ADR-0063 (TUI is the primary interactive surface):** Consistent. It covers `trusty-code` only; D5 decides UI surfaces for other products and does not override it.
- **Epic #10894 decision D6:** Generalised by D3 and D4.
