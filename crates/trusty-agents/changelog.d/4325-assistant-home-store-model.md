Added

- **`assistants` — per-assistant home directory + OKG store model (issue
  [#4325](https://github.com/bobmatnyc/trusty-tools/issues/4325)).** "Assistant"
  is a TYPE (`[agent] role = "assistant"`); `izzie` and `cto-assistant` are
  INSTANCES of it, not distinct agent types. Until now nothing modelled the
  instance half: a persona lived in the shared agents directory, the OKG tree in
  the flat `<knowledge_dir>/<slug(agent)>` pool addressed by naming convention,
  and `[[stores]]` had no field naming a per-assistant root at all (the
  data-model gap recorded in
  [#3890](https://github.com/bobmatnyc/trusty-tools/issues/3890)). The new
  module supplies the missing container:
  - `AssistantHome` — the app-generated, DOTLESS, human-browsable home at
    `~/trusty-agents/<instance>/` (override with
    `TAGENT_ASSISTANTS_DIR`), holding `instructions.md`, `config.toml`,
    `agents/`, `okg/`, `attachments/` and `stores/`. Both store paths are the
    owner's, verbatim (2026-08-01): `trusty-agents/<agent>/okg/` (indexed by
    trusty-search) and `trusty-agents/<agent>/stores/<store-identifier>/` (one
    subdirectory per remote store). This change creates the `stores/` PARENT
    only — store-identifier derivation, extraction state and extraction logic
    belong to a separate spec. `ensure()` is additive and idempotent
    — it creates what is missing and never overwrites what a user edited,
    because #4325 makes external modification expected rather than an error.
  - `AssistantInstanceId` — a validated instance name. It becomes a directory
    name, so it is checked (no separators, no `.`/`..`, no uppercase) and
    rejected rather than silently slugged.
  - `assistants::inspect()` — the DETECTION half of #4325's resilience
    requirement: structured findings for missing / wrong-kind / unreadable /
    malformed entries, each carrying a remedy, plus `HomeHealth::narration()`
    as the seam the concierge narrates from
    ([#4320](https://github.com/bobmatnyc/trusty-tools/issues/4320)).
- **`[[stores]] root` — the per-assistant OKG tree root.** A new optional,
  relative, traversal-free path on `AgentStoreBinding`, resolved and confined by
  `AssistantHome::store_root()` and defaulting to the home's `okg/` directory.
  `AgentStoreBinding::validate()` reports an absolute or `..`-bearing value as a
  store problem instead of failing the agent's boot, matching the module's
  existing fail-soft posture. Existing bindings that omit `root` parse and
  validate exactly as before.
