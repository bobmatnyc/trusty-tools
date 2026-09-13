Fixed

- The managed `<claude_config_dir>/settings.json` is no longer silently coerced
  to `{}` and rewritten when it does not parse as a JSON object. Both writers on
  that path — `ensure_settings_defaults` (the `outputStyle` / `statusLine` /
  `attribution` seed) and the managed hook merge behind `ensure_managed_hooks`
  and `tm install` — now copy the original bytes to
  `settings.json.malformed-<UTC stamp>`, warn naming that copy, and only then
  rewrite. Recovery on this path used to be the 3-deep pruned `.bak` snapshot
  ring alone (#7789).
- The converted hook writer is tier-agnostic, so `tm launch` gains the same
  preserve-then-rewrite for the PROJECT `.claude/settings.json` inside the
  managed clone it writes hooks into (#7789).
- A copy that cannot be written abandons that write and leaves the file exactly
  as it was, so damaged managed settings are never replaced by bytes nothing
  preserved (#7789).
- Both writers share the loader added for the project tier in #7780 rather than
  carrying a second copy of the rule. An empty or whitespace-only file now takes
  no copy on either tier — it holds nothing to preserve, and the managed hook
  writer has always read one as `{}` (#7789).
