Documentation

- `tm-delegation-patterns.md`'s "Long-Wait Delegation" and "PM Re-Engagement"
  sections now name `tm wait --for run|file|check --timeout <secs>` as the
  sanctioned in-turn wait for an agent's own condition, and as a bounded
  one-shot-plus-rerun alternative for CI when a brief explicitly wants the
  agent to hold its own turn. PM-side re-engagement stays the default CI
  pattern, since a CI settle notification still is not agent-visible
  (refs [#5843](https://github.com/bobmatnyc/trusty-tools/issues/5843),
  closure condition 2).
