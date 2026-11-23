Fixed

- The L0 shell guard test no longer has a blind spot for `[skills].allow`
  (closes [#4519](https://github.com/bobmatnyc/trusty-tools/issues/4519)).
  `bundled_assistant_personas_resolve_l0_and_gain_nothing` iterated raw
  `[tools].allow`, which is not the set a persona can reach: the builtin skill
  `orchestration-shell-run` expands to the literal tool `l0_shell_exec`, and
  `effective_tool_patterns` unions skill-expanded names into the allow patterns
  before the scope gate sees them. A persona gaining one `[skills]` line —
  including `izzie`, which ingests untrusted Gmail/Drive/Calendar content —
  would therefore have been handed a real unsandboxed shell with the guard still
  green. The guard now runs over the effective (tools ∪ skills) patterns,
  keeping glob matching so `l0_*` and `*` are caught too. Latent, not live: no
  bundled persona declares a `[skills]` block. Test hardening only — no persona
  TOML, allow-set, or production code changed.
