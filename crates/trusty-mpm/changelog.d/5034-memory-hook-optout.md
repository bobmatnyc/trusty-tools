Added

- `[hooks] prompt_context = false` in `~/.trusty-mpm/config.toml` turns off the per-prompt `trusty-memory prompt-context` injection (closes [#5034](https://github.com/bobmatnyc/trusty-tools/issues/5034))
  - the hook costs a measured ~1,211 tokens on every prompt (median 1,252, range 693–1,438 across 1,114 firings over five days) — roughly 24,000 tokens across a 20-turn session, recurring — and [#4904](https://github.com/bobmatnyc/trusty-tools/issues/4904) measured 0 clean matches out of 17 curated facts on that same corpus. There was no way to stop paying it: the hook block is a hardcoded const written unconditionally at every session launch, so a hand edit to `.claude/settings.json` was overwritten on the next launch
  - default is `true`. An absent `[hooks]` section, and a present-but-empty one, both leave the write byte-identical to before
  - only the `UserPromptSubmit` entry is suppressed. `SessionStart` → `trusty-memory inbox-check`, the `PreToolUse` PM guard, and the six-event lifecycle triad are written either way
  - the strip that removes trusty-mpm's own prior entries now covers every event trusty-mpm owns rather than only the ones the current config writes. Without that, the key would have done nothing on any project already launched once — the stale `UserPromptSubmit` entry would have stayed in `.claude/settings.json` and kept firing
  - config file only, by design: no CLI flag and no environment variable
  - `config.rs`'s inline test module moved to a sibling `config_tests.rs` (no test changed) — the new section pushed the production file to 533 SLOC, over the 500 cap
