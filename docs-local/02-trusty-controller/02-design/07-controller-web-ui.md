# DOC-7 — Controller Web UI (link-out control plane)

**Status:** Draft — stub
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Specify the out-of-the-box UI: list tools + versions, show upgrade indicators and
actions, show health, and run per-tool + stack doctor — while LINKING OUT to each
tool's existing UI rather than reimplementing it.

## Open Questions / Decisions to Resolve

- Enforce the strict link-out boundary: link to search/memory `/ui` rather than
  render our own (the spec: must not reimplement existing tool UIs).
- Pick the tech: embedded Svelte + `ui-dist/` + `build.rs` + `include_dir!` like
  search/memory (recommended). Note the `SKIP_UI_BUILD` flag and the macOS
  codesign/cdhash publish caveats.
- Define UI URL discovery: from the manifest + a `monitor web`-style port discovery.
- Define the auth/loopback posture: daemons are loopback-only with no auth — the
  UI must match.

## Dependencies

### Consumes (inputs)
- DOC-2 (manifest: tool list, versions, UI URLs).
- DOC-4 (rollup verdicts to render).
- DOC-5 (the CLI actions the UI mirrors).
- DOC-1 (contract verbs the UI triggers).

### Produces (consumed by)
- Terminal — nothing depends on the UI.

## Grounding (exists vs. net-new)

- **Exists:** `monitor web` link-out already exists in search & memory; the
  embedded-Svelte + `build.rs` + `include_dir!` pattern is established; UI URL
  discovery can reuse `<tool> port`.
- **Net-new:** mostly assembly — wiring the existing patterns into a new control-plane UI.

## Cross-cutting notes

- **Security:** loopback-only + no-auth parity with the daemons.
- **Contract-versioning behavior:** include an "older-contract behavior"
  subsection — the UI shows a "contract too old" badge for members on an older
  `contract_version`.

## TODO

- [ ] Resolve open questions above
- [ ] Define schemas/contracts
- [ ] Review with team
