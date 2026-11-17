Fixed
- An Architect launch, resume or in-place relaunch whose project hook write fails is now refused with an error naming `.claude/settings.json`, instead of starting without the exec-form `tm hook --pm-guard` entry. A PM session keeps the previous best-effort behaviour (#10612).
