Changed

- bundled output styles now set `keep-coding-instructions: true` and carry the PM prose rules directly
  - the field defaults to `false`, so all three styles were silently stripping Claude Code's built-in scoping, comment, and verification instructions
  - the `Prose Style — Write Plainly` rules are mirrored from `assets/instructions/sections/core.md` into each style, on the same #2647 rationale as the PRIMARY DIRECTIVE — the output style is the only channel that survives a manual `claude` launch with no tm-appended system prompt
  - the older, lighter `## Communication` block is folded into the mirrored rules rather than left beside them
  - `BASE-AGENT.md` gains a graduated-verbosity rule: sparse on a clean pass, detailed on failures — the evidence rule (raw output for failures, flakes, and performance claims) is unchanged
