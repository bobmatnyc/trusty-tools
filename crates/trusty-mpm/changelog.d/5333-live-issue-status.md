Added

- Session launch now ensures the `in-progress` and `blocked` issue-lifecycle
  labels exist, alongside the existing `ws/<session>` label, so
  `tm-ticketing`'s Lifecycle rule works on a repo that has never seen them.
  Fire-and-forget and non-fatal, exactly like the `ws/` ensure — a `gh`
  failure on any label never blocks a launch or suppresses the other labels.
  Unlike `ws/`, the two lifecycle labels are created WITHOUT `--force`, since
  a project may already own and have styled them.
- The resident PM `workflow` instruction section now points at that Lifecycle
  section when work is dispatched against an issue; previously the rule was
  only reachable if the PM happened to load the trigger-loaded skill.
