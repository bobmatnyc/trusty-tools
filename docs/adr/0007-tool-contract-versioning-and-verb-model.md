# 0007. Monotonic-integer `contract_version` + 3-layer extensible verb model

- **Status:** Accepted
- **Date:** 2026-06-08
- **Scope:** Workspace-wide (the `trusty_common` contract module; every stack
  member that `trusty-controller`/`tctl` manages — trusty-search, trusty-memory,
  trusty-analyze, trusty-review, and the external claude-mpm orchestrator;
  consumed by the entire trusty-controller design set under
  `docs/trusty-controller/research/02-design/`, esp. DOC-1 and DOC-6)
- **Supersedes / Superseded by:** —

## Context

`trusty-controller` (`tctl`, ADR-0006) is a thin, verb-agnostic control plane
for the whole claude-mpm stack. It contains **zero tool-specific logic**: every
operation (`doctor`, `health`, `version`, `start`, `stop`, `restart`, `config`)
flows through a single versioned per-tool contract (DOC-1) discovered from a
stack manifest (DOC-2). For that to hold, the controller needs a stable answer
to two orthogonal questions about any stack member:

1. *"Do you speak a protocol shape I understand?"* — a compatibility check.
2. *"Which operations do you support?"* — a capability check.

Today (grounding captured in DOC-1) the surfaces are heterogeneous and
human-oriented: `doctor`/`health` emit coloured text, not JSON; there is no
`version` subcommand on any tool (only clap's `--version` flag); `--json` exists
only on a scattered handful of subcommands (`port`, `status`, `list`,
`monitor`); only daemons expose a JSON `GET /health`, and even those three
health bodies disagree on fields and on the `status` vocabulary. `trusty-review`
implements none of the verbs (only `serve`). The orchestrator is **pluggable**:
claude-mpm (Python, external) today, trusty-mpm (Rust) later (ADR-0006) — so the
compatibility/capability scheme must not bind the controller to any one
implementation or release cadence.

Forces:

- The contract is a single negotiated capability **level**, not an
  independently-versioned package. Semver's major/minor/patch axes carry no
  meaning for a *"do you speak level N?"* check, and bring pre-release /
  build-metadata comparison edge cases.
- Tools already carry their own semver `version` (`CARGO_PKG_VERSION`). Conflating
  "tool version" with "contract level" would force lock-step bumps and obscure
  which axis actually changed.
- The verb set must grow freely (new operations are routine and additive), while
  the wire protocol must stay stable (a controller release should not be required
  to use a tool's new verb, and a contract bump should be rare).
- Whatever is chosen is **costly to reverse**: it is baked into the
  `trusty_common` contract module's serde structs, every retrofitted tool's
  output, the controller's negotiation logic, and the stack manifest — changing
  it after tools ship would be a breaking churn across the whole stack. That
  clears the repo's ADR bar (`docs/adr/README.md`).

## Decision

We will version the tool contract with a **monotonic integer `contract_version`,
starting at `1`**, and structure the verb surface as a **three-layer extensible
model** in which **verb presence is decoupled from `contract_version`**.

1. **`contract_version` is a monotonic integer (baseline `1`).** Negotiation is
   integer comparison only: the controller targets some version N, accepts any
   tool whose `contract_version >= a declared floor`, and for a lower version
   renders only the fields that version guarantees (graceful degrade, never
   hard-fail). Each version is an **additive superset** of the previous one. A
   "what's new per `contract_version`" ledger lives in DOC-1 (`v1` is the
   baseline). It stays orthogonal to each tool's own semver `version`.

2. **Three-layer verb model:**
   - **(a) Uniform response envelope** — every verb returns the same outer JSON
     (`contract_version`, `tool`, `tool_version`, `verb`, `scope`, `status`,
     `data`, `messages`); only `data` varies per verb. The controller parses the
     envelope generically.
   - **(b) Capability advertisement** — `<tool> version --json` lists implemented
     verbs in a `verbs: [...]` array. The controller discovers supported verbs at
     runtime and never hard-codes a per-tool verb set.
   - **(c) Generic passthrough** — `tctl <tool> <verb> [args]` forwards any
     *advertised* verb and renders the returned envelope. First-class commands
     (`tctl doctor`, …) are sugar over this passthrough.

3. **Verb presence is a capability, not a version.** Adding a verb is advertised
   through `verbs[]` and **does not bump `contract_version`**. The integer is
   bumped **only** when the envelope shape or an existing verb's `data` schema
   changes in a way that is not a pure additive superset. Missing verbs →
   graceful degrade; unknown verbs → ignored by older controllers.

   The mandatory-bump rule is **machine-enforced**, not left to reviewer
   vigilance: a **golden-snapshot conformance test in the `trusty_common`
   contract module** serializes a canonical instance of the envelope and every
   per-verb `data` struct and gates CI so any change to a serialized shape fails
   unless the snapshot is regenerated **and** `contract_version` is bumped **and**
   a ledger row is added (the test asserts the three move together). This closes
   the gap where a tool could ship a changed `data` shape under a stale
   `contract_version`; a skewed-version consumer then reliably observes the
   bumped integer rather than silently misdeserializing.

The serde types for the envelope, the per-verb `data` structs, the status enums,
and the trait each Rust tool implements live in a shared `trusty_common`
contract module (ADR-context for DOC-6). The Python claude-mpm orchestrator
satisfies the *same JSON shapes* via an adapter (DOC-6); the contract is a wire
format, not a Rust API, so a non-Rust member can conform.

## Consequences

**Easier / positive:**

- The controller stays **verb-agnostic with zero tool-specific logic**: it
  negotiates one integer, reads `verbs[]`, and renders one envelope shape.
- Tools can **add verbs without a controller release** (passthrough + capability
  advertisement), so the verb set evolves freely.
- The integer stays **slow-moving** — bumped only on genuine wire-incompatible
  changes — which keeps negotiation trivial and the ledger short.
- **Tool version and contract level stay orthogonal**: a routine tool release
  (bugfix, feature) never implies a contract bump, and vice-versa.
- The **orchestrator stays swappable** (claude-mpm → trusty-mpm) because
  conformance is defined by the wire contract + advertised verbs, not by a
  language or a specific binary (honours ADR-0006's forward-compatibility
  requirement).
- Graceful degradation is well-defined: older tool (lower `contract_version`) or
  CLI-only tool (no `start`/`stop`) → render what is guaranteed/advertised, never
  hard-fail.

**Harder / trade-offs:**

- Because verb presence is decoupled from the integer, the controller **must
  always consult `verbs[]`** before invoking a verb; it cannot infer support from
  `contract_version` alone. (This is intentional — it is what makes the verb set
  extensible — but it is one more runtime step than a single version gate.)
- An **additive-only** discipline is required for the integer to mean what it
  says: any non-additive change to the envelope or an existing verb's `data` is a
  mandatory bump. Enforcement is a **golden-snapshot CI test in `trusty_common`**
  (it fails any serialized-shape change not accompanied by a `contract_version`
  bump + ledger row); reviewers are a backstop, not the gate. The real failure
  mode this guards against is **version skew across independently-installed
  crates** — within one workspace build producer and consumer compile against the
  same shared `data` structs and cannot diverge, but `cargo install`-ing tools
  per-crate at different `trusty_common` versions can pair a controller built
  against one shape with a tool that changed the shape without bumping. The
  cross-language coverage splits in two: **Rust tools** are gated here, at the
  shared-types snapshot test; **non-Rust members** (the claude-mpm Python adapter,
  which hand-rolls the JSON and can diverge freely) are gated by a captured
  `--json`-output conformance fixture in DOC-10's harness.
- Every Rust tool must implement the shared trait and wire the subcommands
  (DOC-6 retrofit); `trusty-review` (the current laggard) needs the most work.
- claude-mpm needs a Python adapter to emit the JSON shapes; there is no machine
  contract there today (DOC-6 owns that adapter design).

**Follow-up work:**

- DOC-1 carries the concrete per-verb `data` schemas at `contract_version: 1`,
  the `trusty_common` contract-module API sketch, the `contract_version` ledger,
  and the conformance snapshot (the tool×verb gap matrix DOC-6 will action).
- DOC-3 owns the **behavioural** scope model; DOC-1 owns the `scope` **wire
  format**. The `scope` fields must be reconciled with DOC-3 before this contract
  is marked Accepted.
- Secret redaction (fixed marker `***redacted***`) applies to all verb output and
  is specified in DOC-1; the shared redaction helper is introduced in the
  `trusty_common` contract module (none exists today).
- The golden-snapshot conformance test lands in the `trusty_common` contract
  module (its home, DOC-1 D6) and is wired into CI so that any change to a
  serialized envelope/`data` shape couples to a `contract_version` bump + ledger
  row. DOC-10 extends the same coverage to non-Rust members by validating each
  member's captured `--json` output against the committed golden schema.
