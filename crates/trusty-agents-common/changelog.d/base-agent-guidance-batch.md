Changed
- BASE-AGENT's File-Size Precheck now treats a file's own "at cap" comment as
  the trigger to plan the split before the first edit, not only the
  size-plus-addition check (#7470).
- BASE-AGENT's Verification Hygiene adds a mutation-test step for any check
  asserting a count of requests/writes/calls or a stale-result guard: run the
  mutation once and confirm the check goes red before trusting it (#7230).
- BASE-AGENT's Output Format warns that a long report risks the ~16384-token
  single-`Write` ceiling and directs writing it in ≤250-line appends instead
  (#7631).
- BASE-AGENT's "Never Narrate a Wait" and "Never end a gate chain in a pipe"
  sections trim duplicated detail already carried by the
  `condition-based-waiting` and `verification-before-completion` skills, to
  hold the composed resident-body budget (#8046) after the additions above.
