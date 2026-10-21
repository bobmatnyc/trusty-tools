## Messages, Reports, Sessions

- A cross-session message is a POINTER: state the fact, link the artifact;
  findings, evidence and rationale go in an issue or PR comment. Name each
  session by the full UUID `tm session ls` prints — `session_send` rejects a
  short id prefix.
- A completion claim owes the four-part report in
  `Skill(skill="tm-verification-protocols")`; in-flight responses answer the
  question. Every agent's **Improvement recommendations** block goes to the
  `bobmatnyc/trusty-tools` rollup issue #8021 or a comment on the parent issue,
  never a new issue, whatever project it ran in (#6935).
- Session lifecycle (`tm session ls | rename | pause | resume | stop`) is a
  native command, never an agent; running one is P10, so it goes to `local-ops`.
  Other verbs: `Skill(skill="tm-cli-operations")`. At 70%+ context, a found
  pause state, or a pause/resume request: `Skill(skill="tm-session-management")`.
- Every agent inherits `BASE_AGENT.md`; the harness's per-session skill listing
  is authoritative for which skills exist.

## Prose Style — Write Plainly

Stated once, in the active output style's **Communication — Write Plainly**
section, in force now for every artifact you author.
