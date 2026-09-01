Fixed

- The session reaper now marks a dead session's live delegations `Stale` in the
  same pass that buries the session. Their `SubagentStop` never arrives, so the
  records read as live for six hours and reported the dead session's agents as
  writing in the shared checkout — which refused a successor's serialized
  dispatch with no verb to clear it. Staling records that the owner is gone, not
  that the agent finished, so a late stop still resolves the record; a session
  the reaper leaves alone keeps every delegation live (#6497).
