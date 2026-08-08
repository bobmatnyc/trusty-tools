Changed

- the composed PM prompt drops 6,611 bytes (56,783 → 50,172, −11.6%) by relocating content the PM cannot act on and deduplicating the voice rules ([#4574](https://github.com/bobmatnyc/trusty-tools/issues/4574))
  - `Skill Deployment` / `Agent Deployment` / `Skills System` collapse to one dispatch-time block; the tier tables were already generated and drift-gated in `tm-capabilities` (`references/framework.md`), and the deployment lifecycle they also carried moves to that skill's hand-authored `references/workflows.md`
  - the `gh label create` / `gh issue create` shell block leaves the prompt — `tm-ticketing` and `tm-pr-workflow` already carry it verbatim, and P6/P7 forbid the PM running it
  - the Fail-Open Check's five review steps move to the `code-review-standards` skill, which `code-critic` already loaded and `code-analyzer` now declares; the BLOCKING rule and its error-arm-test requirement stay in the prompt
  - `Prose Style — Write Plainly` becomes a pointer: the voice rules are stated once, in the output style, which was carrying a live mirror of the same text. Both copies were session-resident, so the project paid for them twice
  - dead rules dropped: `/mpm-configure --preview` and `/mpm-init` name no command that exists
  - `Delegation Mechanics` said bundled agents deploy to `~/.claude/agents/`; they deploy to `$CLAUDE_CONFIG_DIR/agents/`, which tm never writes into the operator's own install
- the PM voice rules gain two entries, in the output styles and in `BASE-AGENT.md`: don't justify the restraint ("I don't know yet" is the whole answer), and no trailing emphatic negation ("— not before" restates the sentence by negating its opposite)
