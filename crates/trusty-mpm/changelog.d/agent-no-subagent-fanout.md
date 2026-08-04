Added

- `BASE-AGENT.md` (synced to `trusty-code`) now states a "No Subagent
  Fan-Out" rule: a delegated agent does its own work or reports back to its
  dispatcher — it never spawns its own subagents. The Agent/Task tool is
  reserved for the top-level PM/orchestrator. Prompted by a `rust-engineer`
  spawning an untyped child agent that bypassed the roster entirely for
  routine documentation work
- `BASE-AGENT.md` (synced to `trusty-code`) adds "Agent-Authored Prose",
  extending the PM's "Write Plainly" register (`core.md`,
  [#4757](https://github.com/bobmatnyc/trusty-tools/issues/4757)) to the
  agent side: review verdicts, reports back to the PM, ticket/PR body text,
  and generated documentation — lead with the concrete referent, state cause
  then effect, show before-and-after, cut hedges and process narration, end
  options as a bare enumeration. Governs how, never whether: the evidence
  rule is unchanged
- `BASE-AGENT.md` (synced to `trusty-code`) Agent-Authored Prose adds "never
  announce the register you're writing in" — no heading or preamble that
  labels the writing as plain, honest, direct, candid, blunt, or unvarnished
  (e.g. "What remains unknown, stated plainly:"); state the fact in place
  instead. Same family as the banned word "honest" — the label is forbidden,
  not the disclosure
