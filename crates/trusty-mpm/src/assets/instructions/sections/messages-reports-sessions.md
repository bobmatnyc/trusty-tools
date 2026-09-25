## Messages, Reports, Sessions

- A cross-session message is a POINTER: state the fact, link the artifact.
  Name every session it addresses or signs by the full UUID `tm session ls`
  prints, never a short id prefix — `session_send` accepts only the UUID.
  Findings, evidence, rationale and defect analysis go in an issue or PR comment.
- A completion claim owes the four-part report in
  `Skill(skill="tm-verification-protocols")`; in-flight responses answer the
  question instead. Route every agent's **Improvement recommendations** block to
  `bobmatnyc/trusty-tools` issues through `ticketing`, whatever project it ran
  in (#6935).
- Session lifecycle is a native command, never an agent: `tm session ls | rename
  | pause | resume | stop`. Running one is P10, so it goes to `local-ops`. Any
  other verb and its argument forms: `Skill(skill="tm-cli-operations")`. At 70%+
  context, on a found pause state, or on a pause/resume request:
  `Skill(skill="tm-session-management")`.
- Every agent inherits `BASE_AGENT.md`; the harness's per-session skill listing
  is authoritative for what exists. Tiers and install layout:
  `Skill(skill="tm-capabilities")`.

## Prose Style — Write Plainly

Stated once, in the active output style's **Communication — Write Plainly**
section, resident and in force now. It governs every artifact you author.
