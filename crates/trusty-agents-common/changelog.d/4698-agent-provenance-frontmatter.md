Added

- Agent frontmatter carries a `provenance:` field naming which of the three
  writers produced the file — `framework-owned`, `tm-agent-manager-built`, or
  `user-authored`. `agents::manifest::Origin::is_framework_owned` answers only
  from the ownership ledger, so a file with no ledger entry — every hand-written
  agent and every stale leftover — read identically to a framework one. The new
  `agents::provenance` module owns the three values, the parse, and the absence
  rule: an absent field resolves to `user-authored`, never to a framework-owned
  default (#4698).
- `agents::builder::compose_agent_with_provenance` composes an agent and stamps
  the field, and the deployer calls it with `framework-owned` for every file it
  writes. Provenance is a property of the write, not of the source, so the writer
  supplies it — which also means a new bundled asset has no per-file declaration
  to forget (#4698).
- `agents::provenance::reconcile_with_ledger` and
  `agents::tier_audit::ownership_of_declared` check a file's declaration against
  its ledger row. The ledger wins in both directions — it is the record the
  deployer wrote under a lock and checksummed, while the frontmatter is the copy
  anyone with an editor can change — and a disagreement is warned with the file
  named. An untracked file is exempt from the check entirely: a declaration
  cannot stand in for a ledger entry, which would manufacture the user-owned
  quarantine exemption out of a field anybody can type (#4698).
- `agents::metadata::AgentMetadata` projects the declared `provenance:` so the
  read-only surfaces that already report a file's `Origin` can report both
  records (#4698).
