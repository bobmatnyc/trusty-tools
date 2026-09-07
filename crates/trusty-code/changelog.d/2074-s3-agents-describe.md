Added

- **`agents.describe` reports one agent's effective configuration, source
  provenance, and warnings (#2074).** `agents.list` answers "what could I
  dispatch"; the new method answers the question after it — which file on disk
  this name resolves from, whether Trusty Code wrote that file, when, from which
  compose chain, and whether anyone has edited it since. It resolves through
  `agents::resolve_agent`, the same disk-wins/embedded-fallback/plugin chain
  dispatch uses, so inspection and dispatch cannot disagree about which
  definition wins, and it reads the provenance back out of the deployed-agent
  ledger slice 1 writes. The payload carries the tier (`embedded`, `project`,
  `user`, `plugin`, or `broken`), the source path, model, role, description,
  the `tools.allowed` allowlist, the declared skills, the resolved instruction
  text's length — and the text itself only when `include_instructions: true`,
  because a composed roster prompt runs to tens of kilobytes. Provenance reports
  the manifest state, the recorded origin and whether it is framework-owned, the
  deploy timestamp, the resolved compose chain, and whether the file still
  matches its recorded checksum. Warnings name what an operator would otherwise
  have to diff for: a disk copy that diverges from the bundled roster, a file
  the manifest does not track, a missing ledger in Trusty Code's own agents
  directory, and an unreadable one. An agent whose file exists but fails to
  parse is reported with `tier: "broken"` and the parse error in `warnings`
  rather than raised as an RPC error, so the reason a name fell back to the
  embedded roster is visible; a name that resolves nowhere is a typed
  `not_found`. Capability grants are deliberately absent from the payload:
  Trusty Code has no grant model yet, so the flat allowlist that exists is
  reported and no `grants` key is invented.
- **`agents.list` rows carry `has_warnings` (#2074).** An additive boolean
  computed by the same module `agents.describe` uses, so a client can badge the
  rows worth opening without describing every one. The ledger is read once per
  listing rather than once per row. The four original keys are unchanged, so a
  client written against the previous shape deserializes an entry as before.
