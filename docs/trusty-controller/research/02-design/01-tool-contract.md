# DOC-1 — The Versioned Tool Contract (FOUNDATIONAL)

**Status:** Draft — stub
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Define the exact versioned interface every stack member implements —
`doctor | health | version --json`, `restart`, `config` — including JSON schemas,
`contract_version`, and negotiation / graceful-degradation rules.

## Open Questions / Decisions to Resolve

- Pin the canonical JSON schema per verb:
  - `doctor` → `{ checks: [{ id, scope, status, remediation }] }`
  - `health` → `running | degraded | down` + version
  - `version` → `{ version, contract_version }`
- Define `contract_version` semantics: integer vs semver; precise meaning of
  "negotiate / degrade gracefully against older-contract tools" (e.g. target
  contract N, accept ≥ N−k).
- Decide CLI-only vs also-HTTP. Daemons already expose `GET /health` as JSON, but
  the CLI `health` is human text today — reconcile the two surfaces.
- Define `restart` / `config` semantics and exit codes.
- Lock the stdout-JSON vs stderr-logs discipline (repo hard rule: never log to
  stdout; MCP framing owns stdout).
- Decide the `--scope` flag and the per-check `scope` field: DOC-1 owns the **wire
  format**, DOC-3 owns the **behavioral model** (bidirectional edge).

## Dependencies

### Consumes (inputs)
- DOC-0 (the chosen `<name>` for `<tool>`-shaped examples).
- DOC-3 (scope semantics) — bidirectional: DOC-3 feeds the `scope` schema fields.

### Produces (consumed by)
- DOC-2, DOC-4, DOC-5, DOC-6, DOC-7, DOC-9.

## Grounding (exists vs. net-new)

- **BIGGEST GAP.** Existing `doctor` / `health` / `status` commands are
  human-text; there is no `--json`, no `version --json`, no `contract_version`.
- **Partial template:** the daemon `GET /health` returns JSON
  (`{ status, version, indexes }`) — a useful starting shape.
- **Worst offender:** `trusty-review` implements none of the verbs (only `serve`).

## Cross-cutting notes

- **Contract-versioning behavior:** this doc owns the canonical degradation rule
  (accept older contracts, render degraded rather than fail) that DOC-4/5/7/9 reference.
- **Security / secrets:** no secrets in any contract output (doctor remediation
  hints, health, version, config dumps must be redacted).

## TODO

- [ ] Resolve open questions above
- [ ] Define schemas/contracts
- [ ] Review with team
