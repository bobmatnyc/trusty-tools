Added

- **The embedded agent roster is materialized to `<project>/.trusty-code/agents/`
  with a manifest and recorded provenance (#2074).** A run against a project
  whose agents directory resolves to Trusty Code's own root now writes one `.md`
  per shipped role plus a `.trusty-mpm-manifest.json` ledger recording each
  file's resolved inheritance chain, checksum, deploy time, and origin — so an
  operator can read, diff, and edit the prompt an agent actually runs instead of
  inferring it from a compiled-in table. The write goes through
  `trusty_agents_common::agents::deployer::deploy_agents_filtered` unchanged,
  which brings atomic write-temp-then-rename, strict-YAML validation of every
  composed agent, per-agent compose-failure isolation, and a refusal to proceed
  on a corrupt ledger rather than silently resetting it to empty. The five
  `BASE-*` composition templates are staged so `extends:` chains resolve, but are
  never deployed as dispatchable agents. Two policies sit on top of the shared
  deployer: a project whose `.claude/agents/` or `.open-mpm/agents/` currently
  wins discovery is skipped, so materializing never silently demotes an existing
  catalog; and a deployed file the user has hand-edited is deselected before the
  deploy runs, so the edit survives byte-identical while pristine files still
  refresh. A projectless session writes nothing and keeps the in-memory embedded
  roster, and any deploy failure degrades to that same in-memory roster with a
  loud error rather than aborting the run. `tcode paths show` reports the
  ledger's state — absent, present with a tracked-file count, or corrupt with the
  underlying detail — in both its human and `--json` renderings.
