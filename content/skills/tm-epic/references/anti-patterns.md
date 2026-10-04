# Anti-patterns

| Anti-pattern | Why it breaks |
|---|---|
| Patching the phases block by hand | Drifts the first time a child closes without a session running; the next regeneration overwrites the patch anyway |
| Restating the tracker's decisions in each phase issue | Two copies diverge; the phase issue points instead |
| Commenting on the tracker per PR | Buries the plan changes that matter; PR events belong on the phase issue |
| Renumbering phases mid-flight | Breaks every existing title and reference; a late phase takes the next free number and the table says where it runs |
| A phase with no independent acceptance | It is not a phase; fuse it with its neighbour |
| Evidence pasted into the tracker body | The body is a plan, not a record; evidence goes in a comment, a phase issue, or a doc |
| A tracker whose Ordering section is empty | The work does not need this pattern |
| A finding filed under `deferred` because it was found late | Deferred is scope that left the plan; a discovery goes in `followups`, or the reader cannot tell whether the plan changed |
| A third standalone follow-up issue from one phase | The budget is two per phase at severity HIGH or above; the rest are rows in the `followups` block or the rollup |
| Filing the tracker and its phases in one batch | The phase title needs the tracker's number, which does not exist until the tracker is filed and read back |
| Linking the plan by a branch path | The path moves and the link goes stale; pin it to the commit SHA that landed the plan on `origin/main` |
| Writing issue numbers back into the plan document | The plan is authored once and committed before the issues exist; the tracker points at the plan, not the reverse |
| A seventh `phase` type label | A phase's type is the kind of work it does, from the same six-value set every issue uses |
