Fixed

- Bundled agent assets now reach a **daemon-managed** session without a manual `tm install` (closes [#4840](https://github.com/bobmatnyc/trusty-tools/issues/4840))
  - managed session provisioning re-materializes `~/.trusty-mpm/framework/agents/` from the compiled-in bundle and deploys it, gated on a sha256 stamp so an unchanged bundle costs one file read
  - the standalone path (`tm run` / `tm load` / `tm login`) still routes through `global_config::ensure_global_config_dir`, which does not refresh — tracked as [#4849](https://github.com/bobmatnyc/trusty-tools/issues/4849)
  - a bundled-origin file whose checksum drifted is still overwritten (corruption, not user ownership); an untracked-and-differing or user-edited file is still preserved — but now surfaces in a bounded count-plus-preview warning carrying the `tm install --reset-agents <name>` pointer, instead of being skipped silently
  - agents that fail to compose (bad frontmatter, strict-YAML rejection) are reported in the same summary — those do not land at all, which is worse than stale
  - an uninitialized `agents/agents` submodule directory no longer wins the source resolution and deploys nothing; it falls back to the compiled-in bundle
  - the whole step fails open: a refresh or deploy that cannot run degrades to a warning and never blocks the session
