Fixed

- Bundled agent assets now reach running agents without a manual `tm install` (closes [#4840](https://github.com/bobmatnyc/trusty-tools/issues/4840))
  - session provisioning re-materializes `~/.trusty-mpm/framework/agents/` from the compiled-in bundle and deploys it, gated on a sha256 stamp so an unchanged bundle costs one file read
  - a bundled-origin file whose checksum drifted is still overwritten (corruption, not user ownership); an untracked-and-differing or user-edited file is still preserved — but is now named in a warning instead of being skipped silently
  - the whole step fails open: a refresh or deploy that cannot run degrades to a warning and never blocks the session
