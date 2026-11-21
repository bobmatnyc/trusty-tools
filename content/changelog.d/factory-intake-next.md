Changed

- `tm-issues-prune` 1.3.0: a factory session takes its next issue with `tm issue next`, which lists only ready issues with acceptance criteria and no other assignee, ranked by goal, priority and age. Suggest Next ranks from that list. The agent re-reads the one issue it picks (state, assignee, Status) before claiming it.
