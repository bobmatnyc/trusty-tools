Added

- The composed PM prompt now carries a "Messages Are Pointers" rule in its core
  section: a cross-session message is a pointer, and long-form content —
  findings, evidence, rationale, defect analysis — belongs in an issue or PR
  comment the message links to. A message dies with its session and is never
  indexed or searchable, so long-form content sent that way is stored where no
  third party and no later session can recover it (#5629). PM-prompt goldens
  regenerated.
