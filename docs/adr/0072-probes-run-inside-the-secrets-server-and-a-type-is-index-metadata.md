# 0072. Probes run inside the secrets server; a type is index metadata

- **Status:** Accepted
- **Date:** 2026-10-24
- **Accepted:** 2026-10-24 (Bob, owner ruling D3b, 10:40Z — all nine plan
  recommendations accepted, [#10237](https://github.com/bobmatnyc/trusty-tools/issues/10237))
- **Scope:** crates `trusty-secrets` (server, index, wire), `trusty-mpm`
  (`tm secrets` CLI, MCP tools)
- **Reversibility Cost:** High after trusty-secrets 0.2.2 — the index format
  version 2 and the `secrets.test` wire contract become persisted and shipped
- **Decision Drivers:** one value boundary, no value in argv or env, the
  existing single unlock (DOC-74 G-5), same-uid index edits, wire
  compatibility across one release train
- **Supersedes / Superseded by:** —

## Context

Line references are at `origin/main` `5facb0b2c8`. The spec text is DOC-74
§15.11 (`SPEC-SECRETS-15~draft`).

The owner asked for a secret in `tm secrets` to carry a type, and for
"adapters to do automated testing" of a stored credential: is the database
reachable, does the token authenticate, whose token is it. The Architect's
Python prototypes do this today outside trusty-tools. The postgres prototype
puts the password in a `psql` child's environment.

What the code holds now:

- Per-key metadata lives in a names-only index file per vault. Its row holds
  `length` and `updated_at` only and is `deny_unknown_fields`
  (`crates/trusty-secrets/src/store/index.rs:46-55`). The format version is 1,
  and a reader accepts only an equal version (`index.rs:31`, `:370`).
- `secrets.resolve` is the only method that returns a value, and only to an
  exec-granted process (`crates/trusty-secrets/src/server/exec.rs:190`).
  `tm secrets exec` hands values to a child and scrubs its captured output
  (DOC-74 §10.5).
- The server already holds the backend session, so one unlock serves every
  method (DOC-74 G-5). An agent caller is detected from the socket peer's
  process ancestry (`exec.rs:75-92`).
- The index is editable by any same-uid process; that is why the agents flag
  moved to the backend (DOC-74 §15.8, #9070).

Two choices are hard to reverse: where a probe runs, which decides how many
places the value travels; and where the type is stored, which decides the
on-disk format and the wire.

## Decision

We will run every probe as in-process Rust code inside the trusty-secrets
server, and we will store a key's type as optional metadata on its
names-only index row.

**D1. Probes run in the server (Q1).** A new `secrets.test` method reads the
value from the backend, hands a reference to an adapter, and drops it when
the probe ends. The value never crosses the socket and never enters an argv,
an environment variable, a file or a child process. Adapters sit behind an
`adapters` cargo feature that implies `server` and joins `default`; no other
feature implies it.

**D2. Native clients only (Q2).** Postgres uses `tokio-postgres` with a
rustls connector; the HTTP types use the workspace `reqwest` (rustls). No
`psql` child and no `PG*` environment.

**D3. A verdict, never a value.** `secrets.test` returns a closed
`ProbeResult` whose `facts` come from a per-probe allow-list. Every outgoing
string passes through `scrub_secrets`, and upstream error text maps to a fixed
status. No method, verb or MCP tool added by this design returns a value.

**D4. A type is index metadata.** `IndexRow`, `KeyMeta` and `SetRequest` gain
an optional `secret_type`. `INDEX_VERSION` becomes 2; a 0.2.2 build reads
versions 1 and 2 and writes 2. Backend items are unchanged.

**D5. The type is not trusted.** Because the index is same-uid editable, each
adapter checks the value's shape before any I/O, and the HTTP types reach
fixed hosts only (`api.github.com`, `api.openai.com`). A retyped key fails the
shape check rather than being sent to another vendor. No request argument
names a host.

**D6. Agents are gated by the existing flag (Q3).** An agent caller may probe
only a key flagged "agents may use"; a human caller is not gated. MCP tool
calls always declare `agent_caller: true`, which can only tighten the gate.

**D7. Scope of v1 (Q4–Q9).** Types `postgres_url`, `github_token`,
`openai_api_key`; verb `tm secrets test`; inference proposes and confirms, never
assigns; `query` returns capped rows to agents; trusty-secrets 0.2.2 and
trusty-mpm 2.0.0 ship as one train; the prototypes stay in use until S14 and
S15 ship.

## Considered Options

**Option A — in-process adapters in the server (chosen).** One value
boundary. No new unlock prompt, no grant token, no child. Cost: an HTTP
client, a postgres client and a SQL parser enter the server binary.

**Option B — out-of-process adapter executables fed over stdin.** Keeps the
server small and lets an adapter be written in any language. Rejected: the
value must be handed down to a second process, which needs a grant or a
pipe protocol and a second scrubbing boundary. That is the `exec` design,
built for arbitrary user commands; a probe is not arbitrary.

**Option C — keep the Python prototypes as the adapters.** No new Rust code.
Rejected: the postgres prototype puts the password in a child environment,
the scripts live outside the workspace's test ladder, and an agent would need
a value-returning path to feed them.

**Option D — a `psql` child for postgres.** Matches the prototype. Rejected:
the password enters a child's environment, and `psql` must be on `PATH`.

**Option E — store the type in the backend item.** Keeps type and value
together. Rejected: it changes 1Password and Keeper items, needs a backend
call to list types, and puts non-secret metadata behind an unlock.

**Option F — a separate type file beside the index.** Avoids the index
version bump. Rejected: two names-only files per vault must stay consistent
under the same lock, and the downgrade problem in D4 is the same.

## Consequences

**Positive:**

- A probe adds no place the value can leak beyond the server's own memory.
  The leak tests are channel-by-channel and can be made to fail by removing
  the scrubber.
- The type rides the existing `list` path; `secrets_get_ref` and the console
  can show it without a backend call.
- The adapter interface is not postgres-specific: two bearer-token types ship
  with it.

**Negative:**

- The server binary grows by an HTTP client, a postgres client and a SQL
  parser. An `api`/`store` consumer with `default-features = false` does not.
- An older server refuses a newer client's typed `set` (`SetRequest` is
  closed), and refuses a version-2 index file as `index_corrupt`. Downgrading
  below 0.2.2 after a typed write needs 0.2.2 reinstalled.
- `scrub_secrets` skips values under 8 characters, so a short postgres
  password can survive in a row cell. The S13 security review rules on it.
- A `query` returns real rows to an agent with the flag on. A per-key `query`
  flag waits on the security review.
- On the daemon's loopback MCP path the socket peer has no agent ancestor, so
  the gate there rests on tm sending `agent_caller: true`.
- OpenAI-compatible endpoints other than `api.openai.com` wait for a later
  type.

**Follow-up:** S12–S15 (#10239–#10242); the security review gate on S13
(#7524 scope or a new child).

## Related Decisions

Vetted against `docs/adr/INDEX.md` on 2026-10-24:

- **ADR-0026 (Credential grants stop at delegation):** Consistent. A probe
  issues no grant and delegates nothing; the agent gate judges the caller
  itself.
- **ADR-0032 (Console is the only HTTP surface), amended by 0034, 0035:**
  Consistent. The server stays on its Unix socket and adds no console route;
  probes are outbound connections to the credential's own service.
- **ADR-0064 (Instructional content tracked separately):** Consistent. The
  S15 skill and agent text changes land in `content/` under its own cadence.
- **DOC-74 §15.8 "agents may use" flag:** Extends. The flag now gates probes
  as well as exec grants.
- **DOC-74 §10.2 reference-only MCP rule:** Consistent. The new tools return
  verdicts, never values.

## Amendment 1 — 2026-10-28: the agent gate is a per-key deny (#10386)

**Amended, not superseded.** D1–D5 and D7–D9 are unchanged. D6 and the
Related Decisions verdict on DOC-74 §15.8 were written when keys were closed
to agents unless flagged "agents may use". Owner ruling (Bob D69/Af9, D6a,
2026-10-28): *"Security model is exactly like .env.local. If it's in the
scope (user/owner/project), then it's usable by the agent."*

- **D6 now reads:** an agent caller may probe any key in its scope unless the
  operator denied that key to agents; a human caller is not gated. MCP tool
  calls still declare `agent_caller: true`, which applies the deny.
- The per-key deny is an operator opt-out held by the backend (Keychain item,
  or a 0600 file on the file backend). An agent may write a deny; only a
  person may remove one. 1Password and Keeper cannot hold a deny, so a deny
  there is refused (`unsupported`).
- Existing vaults migrate once; the default (`DEFAULT_AGENT_DENY_MIGRATION`)
  is open-all.
- The `secrets.test` method this ADR specifies was not on `main` when the
  deny shipped; when it lands, it gates on the deny through the same check
  `secrets.grant` uses.

Spec: DOC-74 §15.8 and §15.11.5 as amended 2026-10-28.

## References

- DOC-74 §15.11 (`docs/specs/DOC-74-secrets-integration.md`)
- [#10237](https://github.com/bobmatnyc/trusty-tools/issues/10237) epic;
  [#10238](https://github.com/bobmatnyc/trusty-tools/issues/10238) this spec slice
- [#7517](https://github.com/bobmatnyc/trusty-tools/issues/7517) secrets epic;
  [#7524](https://github.com/bobmatnyc/trusty-tools/issues/7524) security review
