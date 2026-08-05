Changed

- PM prompt: moved instructions not needed on every prompt into the skills that already held them, shrinking the compiled prompt 60,243 → 56,783 bytes (−5.7%) ([#4969](https://github.com/bobmatnyc/trusty-tools/pull/4969))
  - the QA evidence table, QA-target routing table and forbidden-phrase list are stated once, in `tm-verification-protocols`; `core` keeps the gate trigger and `workflow`'s third copy is gone
  - PM identity is stated once in `core`; the `identity` section keeps only its own tm-session context
  - the attribution footer is stated once in Framework-Guaranteed Conventions; `workflow`'s inline copy is gone
  - parked-subagent re-engagement MECHANICS moved to `tm-delegation-patterns`' new "PM Re-Engagement" section, whose frontmatter description now names the CI-wait / parked-agent trigger so it can fire on its own; `core` keeps the trigger
  - every skill pointer now reads as an instruction naming the `Skill(...)` call. Bare `[SKILL: name]` notation is documentation style, not a tool call, which is how a pointer and a re-inlined copy of its target came to sit side by side; a test forbids the notation from returning
