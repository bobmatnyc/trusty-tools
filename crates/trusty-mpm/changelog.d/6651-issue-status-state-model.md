Added
- `tm issue` state models can now declare a LABEL-LESS state — one whose only
  artifact is the absence of every state label — plus a `gh_state:
  open|closed` per state and a `requires_note: true` per transition. A
  label-less state is resolved from the issue's own open/closed flag, reaching
  a `gh_state: closed` state closes the issue, and an edge marked
  `requires_note` is refused unless `--note` is given. Load-time validation
  rejects two label-less states sharing one `gh_state`, since nothing could
  tell them apart.
- A project-level `issue-state.yaml` at the repo root encoding this
  workspace's own lifecycle — `open → status:in-progress → status:coded →
  status:merged → status:tested → closed`, plus the release-a-claim and reopen
  edges back to `open`. `tm issue transition <n> <state>` run from the repo
  root now performs each advance as ONE `gh issue edit` carrying both
  `--add-label` and `--remove-label`, so an issue can never be observed
  carrying two `status:*` labels; an undeclared edge exits 1 naming the states
  you may move to, and an issue that already carries two is refused with a
  pointer to `tm issue repair <n>`.
