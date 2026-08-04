Changed

- Bundled agent assets no longer prescribe a blocking CI wait: `BASE-AGENT.md`, `BASE-ENGINEER.md`, and `local-ops.md` now direct agents to push, take a ONE-SHOT `gh pr checks` / `gh pr view` status read, report, and end the turn ([#4792](https://github.com/bobmatnyc/trusty-tools/issues/4792))
  - `gh pr checks --watch` is forbidden — it streams check output into the agent's context. Own-gate commands (builds, test suites) still block in the foreground
  - Kept byte-identical to the `trusty-mpm` asset originals
