Fixed

- The PM prompt's Memory Protocol section no longer claims a `UserPromptSubmit`
  hook injects a palace-context block into every prompt
  (refs [#7835](https://github.com/bobmatnyc/trusty-tools/issues/7835))
  - no such hook was registered in any settings tier, so the PM skipped
    `memory_recall` waiting on a block that never arrived
  - the section now describes the real mechanism — palace context arrives once,
    as the launch-time catch-up seed (`core::session_launch`, `catchup_context`),
    re-readable on demand through the `session_context_catchup` MCP tool — and
    instructs the PM to call `memory_recall` for targeted recall before any
    research or delegation
  - the three committed PM-prompt goldens are regenerated, and
    `the_memory_section_claims_no_per_prompt_context_hook` pins the claim across
    both composers
