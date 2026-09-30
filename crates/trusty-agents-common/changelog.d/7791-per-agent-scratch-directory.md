Changed
- `BASE-AGENT` rule: an agent writes scratch files only in the scratch directory its brief names (`<scratchpad>/<issue>-<round>/` or `<scratchpad>/<agent-id>/`), never at the scratchpad root or in another agent's directory (Refs #7791).
