Fixed
- A reviewer finding with `"title": null` or `"severity": null` no longer
  turns the whole review into UNKNOWN. A null title is derived from the body,
  as a missing one is, and a null severity is derived from effort (#9310).
- When a double-encoded `findings` string does not decode, the fail-safe
  reason names one position, its line and column in the reply. Before, it
  also named a second position, counted inside the decoded string (#9310).
