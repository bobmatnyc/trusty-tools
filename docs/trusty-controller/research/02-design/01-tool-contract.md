# DOC-1 — The Versioned Tool Contract (FOUNDATIONAL)

**Status:** Draft (decisions recorded; per-verb schemas, grounding, conformance checklist & ADR pending)
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Define the exact versioned interface every stack member implements —
`doctor | health | version --json`, `restart`, `config` — including JSON schemas,
`contract_version`, and negotiation / graceful-degradation rules.

## Decisions

The owner has resolved the major design decisions for DOC-1. They are recorded
below as D1–D8.

### D1 — Canonical contract surface (A1)

CLI subcommands emitting JSON to **stdout** are authoritative. Logs go to
**stderr** (repo hard rule; MCP framing owns stdout). Daemons MAY additionally
expose the same JSON over `GET /health` as an optional fast-path the controller
can opt into, but the CLI is the authority — it works when a daemon is down, for
CLI-only tools, and for the Python claude-mpm orchestrator. The controller
obtains the binary to invoke from the stack manifest (DOC-2), never by
hard-coding.

### D2 — `contract_version` semantics (A2)

A **monotonic integer**, starting at `1`. Rationale (recorded verbatim):

- The contract is a single negotiated capability **level**, not an
  independently-versioned package — semver's major/minor/patch axes carry no
  meaning for a "do you speak level N?" check.
- Integer comparison (`tool_cv >= floor`) is trivial with no
  pre-release/build-metadata edge cases.
- Tools already carry their own semver `version`, so a distinct integer keeps
  **tool version** and **contract level** orthogonal.
- The additive-superset rule means there is only one axis of growth → one
  integer.

Negotiation: the controller targets version N, accepts any tool whose
`contract_version >= a declared floor`, and for a lower version renders only the
fields that version guarantees (graceful degrade, never hard-fail). Each version
is an additive superset of the previous. The doc will carry a "what's new per
contract_version" ledger; `contract_version: 1` is the initial baseline.

### D3 — Verb set & extensibility (A3) — the 3-layer model

Verb categories:

- **lifecycle** — `start`, `stop`, `restart`
- **introspection** — `doctor`, `health`, `version`
- **config** — `config`

**(a) Uniform response envelope** — every verb returns the same outer JSON; only
`data` varies:

```json
{
  "contract_version": 1,
  "tool": "trusty-search",
  "tool_version": "0.12.3",
  "verb": "doctor",
  "scope": "system",
  "status": "ok",
  "data": {},
  "messages": []
}
```

The controller parses the envelope generically; a new verb = a new `data` shape
with **zero protocol change**.

**(b) Capability advertisement** — `<tool> version --json` lists implemented
verbs:

```json
{ "tool": "trusty-search", "version": "0.12.3", "contract_version": 1,
  "verbs": ["doctor","health","version","start","stop","restart","config"] }
```

The controller discovers supported verbs at runtime (never hard-codes per tool).
Missing verbs → graceful degrade (e.g. a CLI-only tool may implement no
`start`/`stop`); unknown verbs → ignored by older controllers.

**(c) Generic passthrough** — `tctl <tool> <verb> [args]` forwards any
**advertised** verb and renders the envelope, so a brand-new verb is usable from
the controller **without a controller release**. First-class commands
(`tctl doctor`, etc.) are sugar over this passthrough.

**Versioning split (critical):** verb **presence** is advertised via `verbs[]`
(a capability) and is **independent of `contract_version`**. Bump
`contract_version` ONLY when the envelope or an existing verb's `data` schema
changes incompatibly — NOT when a verb is merely added. This keeps the integer
slow-moving while the verb set stays freely extensible.

### D4 — Status vocabularies (A4)

- doctor check `status`: `ok | warn | fail | pending | skipped` — where
  `pending` carries the spec's "unindexed = system-ready, project-pending, NOT
  broken" semantic.
- health `status`: `running | degraded | down`.
- The envelope's top-level `status` uses the vocabulary appropriate to the verb.

### D5 — Exit-code convention (A5)

`0` ok · `1` fail/down · `2` degraded/warn · `3` contract-or-usage error. The
JSON `status` is authoritative; the exit code mirrors it (for scriptable
`stack doctor` in CI).

### D6 — Contract types home (A6)

A shared **`trusty_common` contract module**: serde structs for the envelope +
per-verb `data` types + a trait each tool implements, so all Rust tools
serialize identically. DOC-6 retrofits then reduce to "implement the trait +
wire the subcommand." (claude-mpm, being Python, implements the same JSON shapes
via its own adapter — see DOC-6.)

### D7 — Scope wire format (A7)

`--scope project|system|all` on verbs; default `all` in a project directory,
else `system`. Each `doctor`/`health` check carries its own `scope` field. DOC-1
owns the **wire format**; DOC-3 owns the **behavioral model** (bidirectional
edge).

### D8 — Security / secret redaction (A8)

All verb output (`config`, `version`, `doctor` remediation hints, etc.) MUST
redact secrets — API keys, tokens, AWS credentials, connection strings — using
the fixed marker `"***redacted***"`.

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

The decisions above standardize the **union** of these existing surfaces rather
than inventing a new one — the daemon `GET /health` JSON is the template for the
envelope's introspection payload.

## Cross-cutting notes

- **Contract-versioning behavior:** this doc owns the canonical degradation rule
  (accept older contracts, render degraded rather than fail) that DOC-4/5/7/9 reference.
- **Security / secrets:** no secrets in any contract output (doctor remediation
  hints, health, version, config dumps must be redacted).

## Remaining work

- [ ] Author per-verb `data` JSON schemas at `contract_version: 1` (`doctor`, `health`, `version`, `start`, `stop`, `restart`, `config`) with worked examples
- [ ] Grounding: capture actual current CLI/HTTP output of trusty-search / trusty-memory / trusty-analyze / trusty-review + claude-mpm `mpm-doctor`, and standardize the union into the schemas
- [ ] Specify the `trusty_common` contract module API (trait + envelope + data types)
- [ ] Define the `contract_version` floor + the "what's new per version" ledger (v1 baseline)
- [ ] Publish the conformance checklist for DOC-6
- [ ] Write the contract-version ADR (charter B2)
- [ ] Coordinate `scope` fields with DOC-3
- [ ] Team review → Accepted
