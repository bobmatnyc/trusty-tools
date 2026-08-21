Added

- The sweep writes the run's inference identity — provider plus the three role
  model ids — into each repository's `manifest.toml` as an `[inference]` table,
  beside the ranking it already writes there. `trusty-review` resolves from that
  section ahead of the host's own config, so `trusty-audit render` on a
  recipient's machine reproduces the provider the engagement ran on instead of
  inheriting whatever that machine is pinned to (owner ruling 2026-08-21: "In
  audit mode, trusty review should use the same provider as audit to make it
  portable. From the manifest."). The same values still go to the child as
  `TRUSTY_REVIEW_*` variables; the manifest is authoritative when present. The
  section carries identity and never a credential — the key stays in the
  environment. A manifest that cannot be written is a named gap, not a failed
  repository, exactly as a ranking that cannot be written is.
- `index.md` gains an Inference section stating which provider and models
  produced the reports in that directory, and which layer selected them. A
  re-render reads it from the manifests it renders, since those outrank anything
  the run itself injects.
