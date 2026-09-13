Changed

- `BASE-AGENT.md` and `rust-engineer.md` carry only what an agent needs on
  every turn — the long-form mechanics for waiting, empty-output/declarative-
  process verification, the self-improvement reporting loop, and the
  version-control-only worktree-removal guard conditions moved to on-demand
  skills (`condition-based-waiting`, `verification-before-completion`, the new
  `self-improvement-loop`) or to `version-control.md`'s own body, behind short
  resident trigger + pointer text. `rust-engineer.md` also drops a dead
  pre-`extends:` trailer that duplicated `BASE-ENGINEER.md` and stated a wrong
  SLOC cap. Composed `rust-engineer` measured 55,872 bytes (19,981 tokens per
  turn, 32% of an engineer subagent's floor) before this change (#7723).
- Fix round: restores the `tm wait` exit-code table (`0`/`75`/`1`/`2`, the
  VERBATIM re-issue rule for `75`), the scratchpad `$$`/glob collision
  warning, and the backgrounded-`echo` gotcha inline in "Never Narrate a
  Wait", and names the "Improvement recommendations"
  (Symptom/Cause/Change/Evidence) and "Prompt feedback" closing-block
  headings plus the `self-improvement-hypothesis` tag inline in
  "Self-Improvement Reporting" — 35 of 39 roster agents carry no `Skill`
  tool (#7683) and could not otherwise reconstruct them (#7723).
