# 0069. trusty-memory 1.x freezes four surfaces: MCP tools, CLI, palace format, engine API

- **Status:** Proposed
- **Date:** 2026-10-06
- **Scope:** crate `trusty-memory` (MCP tools, CLI, `engine` module, daemon
  socket); crate `trusty-common` (engine source until
  [#9271](https://github.com/bobmatnyc/trusty-tools/issues/9271), then the
  engine's private dependency); consumers `trusty-mpm`, `trusty-agents`,
  `trusty-crate-contracts`
- **Reversibility Cost:** High — once 1.0.0 ships, third-party MCP configs,
  scripts and Rust dependents rely on the frozen surfaces, and narrowing the
  contract later is itself a breaking change
- **Decision Drivers:** owner ruling 2026-10-06 (Bob): the 1.0 semver contract
  covers MCP tools and schemas, the CLI, the on-disk palace format and the
  engine's public Rust API, every public error enum is `#[non_exhaustive]`,
  the engine moves into trusty-memory behind a default `server` feature, and
  memory sync lands in 1.x; the workspace versioning policy of 2026-09-26
  (`docs/reference/semver-gate.md`), under which a break never forces a major
- **Supersedes / Superseded by:** — (amends trusty-memory crate decision
  [0001](../trusty-memory/decisions/0001-frontend-core-split.md) on acceptance;
  see Related Decisions)

Issue: [#9268](https://github.com/bobmatnyc/trusty-tools/issues/9268) (E0).
Plan: `architect/local/briefs/2026-10-06-trusty-memory-1.0-plan.md`.

## Context

trusty-memory is at 0.28.x. Its surfaces change without a stated rule:

- **MCP.** About 48 tools are defined under `crates/trusty-memory/src/tools/`.
  No snapshot pins their names, required params or response shapes, and no
  tool carries a deprecation mark.
- **CLI.** About 33 subcommands. Exit codes are ad hoc: `1` from `main` on
  error, `2` from clap and from `commands/note.rs`, `0` from the
  `prompt-context` hook. No table documents them.
- **On-disk format.** Versioned only in parts. ADR-0070 covers it.
- **Engine.** The engine is `trusty_common::memory_core`, behind the
  `memory-core` feature (crate decision 0001). trusty-common is 0.54.x and
  publishes a minor on most releases, so any engine consumer sees breaks at
  trusty-common's cadence.
- **Daemon socket.** One JSON-RPC 2.0 router over about 75 names
  (`crates/trusty-memory/src/transport/rpc.rs`), plus about 20 socket-only
  methods in `FOLDED_METHODS` (`crates/trusty-memory/src/transport/uds.rs:90`).
  It has no protocol version. `trusty-agents` dials it
  (`crates/trusty-agents/src/memory/trusty_client/mod.rs`), and trusty-console
  polls `memory.health`, `memory.status` and `memory.activity`
  (`crates/trusty-console/src/memory_uds/mod.rs`).

The workspace policy of 2026-09-26 says a public-API break never forces a
major version, and the release-time semver gate records breaks without
blocking them. The 2026-10-06 ruling is later and narrower: it gives
trusty-memory a real semver contract from 1.0.0. This ADR states which
surfaces that contract covers, where each one ends, and what enforces it.

## Decision

We will treat the four surfaces below as the trusty-memory 1.x contract. In
1.x they change additively only. Anything not listed is out of contract.

### D1. Frozen surfaces

**1. MCP tools.** Frozen: each tool's name; each required param's name and
JSON type; each response's field names, JSON types and meaning; JSON-RPC
error codes. Allowed in a 1.x minor:

- a new tool;
- a new **optional** param, whose absence gives the prior behaviour;
- a new response field (clients must ignore unknown fields);
- a new value for a string enum in a param (the tool accepts more input).

String enums in responses are open sets: a client must tolerate an unknown
value, and a new value is additive. Forbidden in 1.x: removing or renaming a
tool, param or response field; a new required param; making an optional
param required; changing a type; changing what a field means. Description
text, the order of tools in `tools/list`, and the size caps in
`tools/byte_cap.rs` are out of contract.

**2. CLI.** Frozen: the binary names (`trusty-memory`, and the
already-deprecated `trusty-memory-mcp-bridge` shim, which stays until 2.0);
subcommand paths; flag long names and short aliases; positional arity; flag
value types; the field names and types of `--json` output (additive like MCP
responses); exit codes. Exit codes in 1.x: `0` success, `1` runtime failure,
`2` usage error. A subcommand's documented nonzero code is frozen. A new code
may be added only for a new subcommand or flag, never for an existing
condition. The documented environment variables and the project pin file
(`.trusty-tools/trusty-memory.yaml`, whose `schema_version` follows ADR-0070's
refuse-newer rule) are part of this surface.

**3. On-disk palace format.** Frozen by reference to
[ADR-0070](0070-versioned-palace-format-with-n-minus-1-migration.md): the
palace format version, the refuse-newer and N-1 migration rules, and the
export format (`SHARE_FORMAT_VERSION`). A format change in 1.x is legal only
as an N → N+1 migration under ADR-0070.

**4. Engine Rust API.** Frozen: every item reachable as
`trusty_memory::engine::*` when the crate is built with
`--no-default-features`, unless it is `#[doc(hidden)]`. Also frozen: the
cargo feature names `server` (default), `daemon` (an alias of `server` for all
of 1.x) and `mcp-schema`. Rules:

- Every public error enum is `#[non_exhaustive]` at 1.0.0
  ([#9273](https://github.com/bobmatnyc/trusty-tools/issues/9273), E5).
- Every public struct with public fields that 1.x may extend (options,
  results, records such as `Palace`, `RememberOptions`, `RecallResult`) is
  `#[non_exhaustive]` at 1.0.0 and has a constructor or builder. Adding
  `#[non_exhaustive]` after 1.0.0 is itself a break, so the
  [#9278](https://github.com/bobmatnyc/trusty-tools/issues/9278) (E10) audit
  decides each struct and enum before release.
- A public trait that downstream code implements gains methods only with a
  default body, or it is sealed.
- trusty-common is a private dependency: no trusty-common type appears in an
  `engine` signature unless E10 lists it and pins it (see D5).
- Raising the MSRV is allowed in a 1.x minor and follows ADR-0029. It is never
  done in a patch.

**5. Daemon socket JSON-RPC: out of contract in 1.0.** Two reasons:

- It is an internal transport between first-party binaries (ADR-0032). Its
  consumers ship from this workspace.
- It has no version handshake today. Freezing about 95 method names without
  one gives rigidity, but no client could detect skew.

The MCP payloads it carries through `tools/call` are already frozen by
surface 1, because the socket and stdio paths share one dispatcher. Before
1.0.0, the daemon adds a monotonic integer `protocol_version` to its
`memory.health`/`memory.status` reply (the ADR-0007 model). In-workspace
clients check it and degrade when it is lower than they expect. A later 1.x
minor may bring the socket into contract through an amending ADR.

### D2. Out of contract

These may change in any release:

- log text, levels and targets;
- human CLI output, help text, colours and `doctor` wording;
- MCP tool descriptions, except the deprecation prefix in D3;
- Rust items outside `engine`, including the server types that
  `trusty-agents` and `trusty-crate-contracts` link today;
- `#[doc(hidden)]` items and the `test-support` feature;
- latency, memory use and other performance characteristics;
- recall ranking order beyond the thresholds of the recall eval gate
  ([#9281](https://github.com/bobmatnyc/trusty-tools/issues/9281), E13);
- the daemon socket (D1.5);
- derived on-disk files, which ADR-0070 lets a release rebuild.

### D3. Change rules in 1.x

- **Additive only.** Every change to a frozen surface follows the "allowed"
  lists above.
- **Deprecation lasts at least one minor release,** and the notice ships in
  that release:

  | Surface | Where the notice appears |
  |---|---|
  | MCP | The tool or param description opens with `DEPRECATED since 1.y; removed in 2.0: use <replacement>.` The tool's `_meta` object carries `"deprecated": true` for machine readers. |
  | CLI | One line on stderr per invocation, never stdout (stdout carries MCP framing under `serve --stdio`): `warning: '<item>' is deprecated since 1.y and is removed in 2.0; use '<replacement>'`. Help keeps the item, marked `(deprecated)`. |
  | Engine | rustdoc `#[deprecated(since = "1.y.0", note = "use <replacement>")]`. |
  | Format | Formats are never deprecated in 1.x. ADR-0070 migrates them. |

- **Removal only in 2.0.** A deprecated item keeps working through the last
  1.x release.
- **Fix exception.** A patch may correct behaviour that contradicts the
  documented contract, and a security fix may break a surface when no
  additive fix exists. Either case needs a changelog entry that names the
  break.

### D4. Enforcement

| Surface | Guard | Issue |
|---|---|---|
| MCP tools | Checked-in JSON snapshot of names, required params and types. A rename, removal or new required param fails, and a deprecated tool must survive one minor. | [#9276](https://github.com/bobmatnyc/trusty-tools/issues/9276) (E8) |
| CLI | Snapshot of the clap tree: subcommands, flags, value types, exit codes. | [#9277](https://github.com/bobmatnyc/trusty-tools/issues/9277) (E9) |
| Engine API | `cargo-semver-checks` against the last 1.x release on `--no-default-features`, plus `scripts/check_semver_types.sh` (ADR-0047). | [#9278](https://github.com/bobmatnyc/trusty-tools/issues/9278) (E10) |
| Error enums | A lint fails on a new public enum without `#[non_exhaustive]`. | [#9273](https://github.com/bobmatnyc/trusty-tools/issues/9273) (E5) |
| Palace format | N-1 fixture migration, N+1 refusal and failed-backup tests. | [#9274](https://github.com/bobmatnyc/trusty-tools/issues/9274) (E6) |

Each guard runs on pull requests that touch trusty-memory, not only at
release. A snapshot is regenerated only together with a changelog fragment
that names the additive change.

### D5. Relation to trusty-common's 0.x cadence

After [#9271](https://github.com/bobmatnyc/trusty-tools/issues/9271) (E3),
the engine source lives in trusty-memory. trusty-common stays 0.x on its own
cadence and keeps the 2026-09-26 policy.

- trusty-memory 1.x may move to a new trusty-common minor in a 1.x minor or
  patch, as long as no trusty-common type leaks into a frozen surface.
- Where E10 finds a trusty-common type in an `engine` signature, there are two
  options: wrap it in an engine-owned type, or re-export it with the
  trusty-common minor pinned (`~0.N`). Moving that pin is then a trusty-memory
  semver event, which the E10 guard checks.
- The `memory-core` feature becomes a deprecated no-op for one trusty-common
  minor, then goes away (E4), with a semver accepted-break entry on the
  trusty-common side.
- The trusty-memory re-export `pub use trusty_common::memory_core as engine;`
  ([#9270](https://github.com/bobmatnyc/trusty-tools/issues/9270), E2) exists
  only before 1.0.0. At 1.0.0, `engine` is trusty-memory's own module.

## Consequences

**Easier:**

- An MCP client author, a script author and a Rust dependent each get a
  precise answer to the question "may this change break me in 1.x?"
- Each surface has its own machine guard, so a reviewer is the backstop, not
  the gate.
- trusty-common is free to keep its 0.x cadence. The engine no longer
  inherits its breaks.

**Harder:**

- Every 1.x feature has to fit the additive rules. A wrong tool name or
  response shape at 1.0.0 lives until 2.0, so the E8/E9/E10 audits before
  1.0.0 matter more than any check after it.
- trusty-memory becomes the one crate that does not follow the 2026-09-26
  policy for its frozen surfaces. Release tooling has to know this.
- Bringing structs under `#[non_exhaustive]` makes downstream struct literals
  fail to compile at the 1.0.0 upgrade. That is a one-time cost, paid in the
  0.x → 1.0 move.
- In-workspace consumers of the server types (`trusty-agents`,
  `trusty-crate-contracts`) are not protected by this contract and need a
  tight version requirement on trusty-memory.

**Follow-up:** E1–E5 and E8–E10 implement this ADR; ADR-0070 and E6
implement surface 3; the `protocol_version` field (D1.5) needs an owner
before 1.0.0.

### Open questions for review

1. **Accepted-break declarations.** `scripts/semver-accepted-breaks/` lets a
   PR merge a declared break. Should it be refused for `trusty-memory`
   `engine` items in 1.x? The recommendation is yes.
2. **Socket in or out.** The recommendation is out, with `protocol_version`
   (D1.5). The alternative is to freeze the `FOLDED_METHODS` names now.
3. **`mcp-schema` types.** `trusty-agents` builds against them. Should they
   join surface 4, or stay out with a pinned requirement?
4. **Environment variables and the pin file.** They are placed in surface 2.
   Confirm, or move them out of contract.
5. **`anyhow` in public signatures** (E10). `anyhow::Error` is stable but
   untyped. Should the engine expose it at 1.0.0?

## Alternatives considered

- **Keep the 2026-09-26 policy for trusty-memory too.** Rejected: the
  2026-10-06 ruling sets a semver contract for 1.0, and a policy where a
  break never forces a major cannot give MCP clients that contract.
- **Freeze everything public, including the server types and the socket.**
  Rejected: it freezes about 40 server modules and about 95 socket names that
  only first-party binaries use, and it blocks the E1–E3 refactor that the
  same ruling asks for.
- **Freeze MCP only, as the user-facing surface.** Rejected: the ruling names
  four surfaces, and a script that parses `--json` output, or a palace
  written by 1.0, breaks as hard as an MCP client does.
- **Version the MCP surface with an integer, as ADR-0007 does for tctl.**
  Rejected for MCP: MCP clients do not negotiate a server-specific level, and
  semver on the crate already carries the signal. The integer model is kept
  for the socket (D1.5), where first-party clients can read it.

## Related Decisions

Vetted against `docs/adr/INDEX.md` and prior decisions on 2026-10-06:

- **ADR-0007 (Tool contract versioning):** Consistent. The tctl verb
  envelope keeps its own `contract_version` and is not part of this
  contract. D1.5 reuses its monotonic-integer model for the socket.
- **ADR-0028 (Memory recall tiers):** Consistent. `fact_key` and the Tier C
  slot semantics become frozen behaviour of the MCP write tools and of the
  format (ADR-0070). This ADR changes neither.
- **ADR-0029 (MSRV 1.94 and edition policy):** Consistent. An MSRV raise is
  allowed in a 1.x minor under ADR-0029's process.
- **ADR-0032 (Console is the only HTTP surface):** Consistent. The socket
  stays an internal transport, which is why D1.5 keeps it out of contract.
- **ADR-0047 (Code contracts as a machine-checkable API surface):** Extends.
  The engine guard uses its `check_semver_types.sh` alongside
  `cargo-semver-checks`.
- **ADR-0051 (Palace id stays hyphen-joined):** Consistent. The
  `#[non_exhaustive]` `Palace` record and its optional fields fit the
  struct rule in D1.4.
- **ADR-0066 / 0067 / 0068 (memory sync; Proposed, PR
  [#9176](https://github.com/bobmatnyc/trusty-tools/pull/9176), unmerged):**
  Consistent. Sync adds MCP tools, CLI verbs and a format migration, all
  additive under D3 and ADR-0070. The sync wire crate of ADR-0067 is
  versioned on its own and is not part of this contract.
- **ADR-0070 (Versioned palace format):** Extends. ADR-0070 defines
  surface 3.
- **trusty-memory crate decision 0001 (Frontend/core split):** Amends on
  acceptance. The engine leaves trusty-common's `memory-core` feature and
  moves into trusty-memory behind `server`. The status of crate decision 0001
  should change to `Amended by` this ADR when this ADR is accepted.
