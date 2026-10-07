Fixed
- A reviewer finding with `"title": null` or `"severity": null` no longer
  turns the whole review into UNKNOWN. A null title is derived from the body,
  as a missing one is, and a null severity is derived from effort (#9310).
- When a double-encoded `findings` string does not decode, the fail-safe
  reason names the line and column of that string in the reply. Before, it
  named a position counted inside the decoded string (#9310).
- A reviewer severity padded with whitespace now counts at its level: `" high "`
  is high, not low, so its finding carries High effort and can raise the
  verdict as any High finding can (#9310).
