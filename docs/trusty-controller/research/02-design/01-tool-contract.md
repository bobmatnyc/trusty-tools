# DOC-1 — The Versioned Tool Contract (FOUNDATIONAL)

**Status:** Accepted (owner-approved; schemas + scope coordinated with DOC-3)
**Source spec:** ../01-spec/trusty-end-to-end-setup.md
**ADR:** [0007 — Monotonic-integer `contract_version` + 3-layer extensible verb model](../../../adr/0007-tool-contract-versioning-and-verb-model.md) (Accepted)

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

**Liveness is *answering*, not process existence.** A member is judged live by
whether it **answers** the authoritative CLI/probe within the timeout — not by
whether a process is running. A daemon with a stale PID lockfile or a bound port
that does not answer `health`/`doctor` within the deadline is `down` for verdict
purposes (the controller synthesizes the terminal envelope, stamping a `reason`
discriminator so a wedged-but-running daemon is distinguished from a not-running
one — cross-ref DOC-4 §1.3).

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

This bump rule is **enforced mechanically by a golden-snapshot test in the
`trusty_common` contract module**: any change to the serialized envelope or an
existing verb's `data` shape fails CI unless `contract_version` is bumped and a
ledger row added (see the ledger's "Rule" below). Rust tools are gated here, via
the shared `data` types they compile against; non-Rust members (the claude-mpm
Python adapter) are gated by DOC-10's captured-`--json`-output conformance
fixture. This is what stops a skewed-version consumer — a controller and a tool
`cargo install`ed against different `trusty_common` versions — from
silently misdeserializing a changed shape under a stale integer.

### D4 — Status vocabularies (A4)

- doctor check `status`: `ok | warn | fail | pending | skipped` — where
  `pending` carries the spec's "unindexed = system-ready, project-pending, NOT
  broken" semantic.
- health `status`: `running | degraded | down`.
- The envelope's top-level `status` uses the vocabulary appropriate to the verb.
- **One lattice, two source vocabularies.** These two vocabularies are not two
  independent systems: at the stack level they both map into DOC-4 §2.0's single
  four-value verdict lattice (`ready|degraded|pending|down`), where `health` and
  `doctor` are the fast and deep probes of one total order. A synthesized terminal
  envelope (DOC-4 §1.3) additionally carries a `reason` discriminator
  (`timeout`/`wedged`/`unreachable`/`not_running`) so the same `down` verdict can
  drive different remediation (restart vs start).

### D5 — Exit-code convention (A5)

`0` ok · `1` fail/down · `2` degraded/warn · `3` contract-or-usage error. The
JSON `status` is authoritative; the exit code mirrors it (for scriptable
`stack doctor` in CI).

The **stack** aggregate exit code (across N members in a fan-out stack
doctor / stack health) is the **worst member code** under the precedence
`3 ≻ 1 ≻ 2 ≻ 0` (worst-wins; see DOC-4 §7), where a contract-incompatible member
contributes `3` — distinct from a runtime `down`, which is `1`.

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
edge). The config-provenance axis (*where a config value originated*) is the
**separate `origin` field** on `config.data` `sources[]` (enum
`{env|project|system}`), **not** `scope` — so `scope` is unambiguously the single
D7 wire axis `{project|system|all}` everywhere it appears.

### D8 — Security / secret redaction (A8)

All verb output (`config`, `version`, `doctor` remediation hints, etc.) MUST
redact secrets — API keys, tokens, AWS credentials, connection strings — using
the fixed marker `"***redacted***"`.

Redaction is **layered (defense-in-depth)**, not a single trusted hop. Tools
redact at the source via the shared `redact_value` helper (the canonical,
mandatory path — see D6 / DOC-6 §3); this is the **primary line**. Because a tool
self-reports already-redacted JSON and the controller cannot prove the tool did
so, the controller **additionally** runs a belt-and-suspenders redaction pass
over every envelope string value **before rendering** — on both the CLI output
and the DOC-7 UI — masking high-confidence secret patterns (e.g. `AKIA…`,
`Bearer` / `Authorization` values, `scheme://user:pass@host` credentials, key
prefixes `sk-` / `ghp_` / `xox…`, long high-entropy blobs) with the same
`***redacted***` marker, so a member (or the claude-mpm shim) that forgot to
redact cannot leak a secret into the UI. This controller-side pass is
**heuristic / pattern-based** — it cannot catch every secret shape — so it is
defense-in-depth, **not a guarantee**: tool-side `redact_value` remains the
primary line and a negative CI conformance assertion (DOC-6 §8) is the gate.

## Per-verb `data` schemas (`contract_version: 1`)

Every verb returns the **uniform envelope** from D3; only `data` varies. The
envelope is specified once here, then each verb's `data` schema + one worked
example (full envelope) follows. All examples are at `contract_version: 1` and
respect D8 redaction (`***redacted***`).

> **Notation.** Schemas are precisely-annotated JSON skeletons: `field: type` with
> `?` marking optional fields, `|` for enum unions, and `// …` for notes. Enum
> string sets are listed inline. These map 1:1 onto the serde structs in the
> module API below.

### Envelope (all verbs)

```json
{
  "contract_version": 1,                 // integer; D2. Always present.
  "tool": "trusty-search",               // stable tool id (matches manifest/DOC-2 key)
  "tool_version": "0.12.3",              // tool's own semver (CARGO_PKG_VERSION)
  "verb": "doctor",                      // the verb that produced this envelope
  "scope": "system",                     // "project" | "system" | "all"  (D7 wire format; DOC-3 owns behaviour)
  "status": "ok",                        // verb-appropriate vocabulary (D4); mirrored by exit code (D5)
  "data": {},                            // verb-specific; schemas below
  "messages": [                          // human-readable notes; never carries secrets (D8)
    { "level": "info", "text": "…" }     // level: "info" | "warn" | "error"
  ]
}
```

- `status` vocabulary by verb (D4): introspection `doctor`/`version` and
  lifecycle/`config` use `ok | warn | fail`; `health` uses `running | degraded |
  down`. `messages` is always an array (may be empty).
- Exit codes (D5): `0` ok · `1` fail/down · `2` degraded/warn · `3`
  contract-or-usage error (e.g. unknown/unadvertised verb, malformed args).
  **Code `3` is produced at the dispatcher/controller boundary, *not* by the
  envelope-status→exit-code mapping** — there is no envelope `status` value that
  maps to `3`. A `3` means the request never produced a valid envelope (unknown
  verb, malformed args, contract-incompatible tool below the floor); the
  `EnvelopeStatus::exit_code()` mapping only ever yields `0`/`1`/`2`.

### `doctor.data`

```json
{
  "checks": [
    {
      "id": "daemon_running",            // stable machine id (snake_case), unique within tool
      "title": "Daemon running",         // short human label
      "scope": "system",                 // "project" | "system"  (per-check; D7)
      "status": "ok",                    // "ok" | "warn" | "fail" | "pending" | "skipped"
      "detail": "Daemon running at 127.0.0.1:7879 (v0.12.3)",  // human detail; redacted
      "remediation": "Run `trusty-search start`.",             // optional fix hint; null when none / status ok
      "pending_since": "2026-06-10T14:03:21Z", // ? ISO-8601 (or epoch) when this pending state began; present only when status == "pending"
      "progress_pct": 42                 // ? optional 0–100 advisory progress; display only, never drives a verdict
    }
  ],
  "summary": {                           // aggregate counts over checks[]
    "ok": 5, "warn": 1, "fail": 0, "pending": 1, "skipped": 0,
    "total": 7
  }
}
```

- `pending` carries the spec's *"unindexed = system-ready, project-pending, NOT
  broken"* semantic (D4): a project-scope check that cannot run yet but is not a
  failure. `skipped` = deliberately not run (e.g. platform N/A).
- `pending_since` is an **optional** ISO-8601 (or epoch) timestamp recording when
  the `pending` state began; tools SHOULD emit it whenever `status == "pending"`.
  It is the sole input the controller (DOC-4) uses to **time-escalate a stalled
  `pending` to `degraded`** purely by elapsed time, without ever inspecting the
  index internals (it cannot — freshness stays tool-reported). If a tool omits
  `pending_since`, the controller cannot time-escalate and the check stays
  `pending` (current behavior). See DOC-4 §5.5.
- `progress_pct` is an **optional** advisory `0`–`100` value for display only; it
  never drives a verdict or the time-escalation.
- Envelope `status` rollup: any `fail` → `fail`; else any `warn` → `warn`; else
  `ok`. (`pending`/`skipped` never worsen the rollup.) This rollup rule is the
  per-tool input DOC-4 consumes for the stack-wide rollup.

**Worked example** (full envelope):

```json
{
  "contract_version": 1,
  "tool": "trusty-search",
  "tool_version": "0.12.3",
  "verb": "doctor",
  "scope": "all",
  "status": "warn",
  "data": {
    "checks": [
      { "id": "daemon_running", "title": "Daemon running", "scope": "system",
        "status": "ok", "detail": "Daemon running at 127.0.0.1:7879 (v0.12.3)", "remediation": null },
      { "id": "model_cache", "title": "Embedding model cached", "scope": "system",
        "status": "ok", "detail": "all-MiniLM-L6-v2 present (84 MB)", "remediation": null },
      { "id": "data_dir_writable", "title": "Data directory writable", "scope": "system",
        "status": "ok", "detail": "/Users/me/Library/Application Support/trusty-search (writable)", "remediation": null },
      { "id": "lock_file", "title": "Lock file healthy", "scope": "system",
        "status": "ok", "detail": "PID 4821 is running", "remediation": null },
      { "id": "log_rotation", "title": "Log rotation configured", "scope": "system",
        "status": "warn", "detail": "stderr.log has no rotation policy",
        "remediation": "Run `trusty-search doctor --fix` to install newsyslog rotation." },
      { "id": "project_index", "title": "Project index present", "scope": "project",
        "status": "pending", "detail": "No index registered for this project yet (system is ready).",
        "remediation": "Run `trusty-search index` in the project root.",
        "pending_since": "2026-06-10T14:03:21Z", "progress_pct": 42 }
    ],
    "summary": { "ok": 4, "warn": 1, "fail": 0, "pending": 1, "skipped": 0, "total": 6 }
  },
  "messages": [{ "level": "warn", "text": "1 warning. Re-run with --fix to auto-repair." }]
}
```

Exit code: `2` (warn).

### `health.data`

Minimal by design (D4): the **envelope `status`** carries the
`running | degraded | down` verdict; `data` carries supporting telemetry.

```json
{
  "uptime_secs": 4210,                   // ? omitted when down / not applicable
  "port": 7879,                          // ? bound port; omitted when down
  "addr": "127.0.0.1:7879",             // ? full bound address
  "pid": 4821,                           // ? daemon pid when known
  "deps": [                              // ? dependency reachability (from trusty-review/analyze prior art)
    { "id": "trusty-search", "required": true, "reachable": true }
  ],
  "detail": "store/recall round-trip ok",  // ? short triage phrase; when up-but-not-ready set to "model loading"/"warming"/"restarting"
  "reason": "wedged"                       // ? only on a controller-synthesized down envelope: "timeout"|"wedged"|"unreachable"|"not_running"
}
```

- `down` ⇒ the tool/daemon is not answering within the timeout (DOC-1 D1:
  liveness = answering, not process existence); the controller synthesizes a
  `down` envelope (it still gets `tool`, `verb`, `status:"down"`, empty/partial
  `data`) rather than the tool emitting it. On a synthesized `down` the controller
  stamps a `reason` discriminator — `not_running` (no process → remediation
  **start**) vs `timeout`/`wedged`/`unreachable` (up but not answering →
  remediation **restart**/investigate) — so a wedged daemon is distinguished from
  a stopped one (cross-ref DOC-4 §1.3/§5.1). `degraded` ⇒ up but a required dep is
  unreachable or a self-probe failed (prior art: review `compute_status`, analyze
  `search_reachable`, memory round-trip probe).
- **Up-but-not-ready ⇒ answer promptly, never hang.** A daemon that is up but not
  yet ready — cold start, ONNX/model loading, warming, or mid-graceful-restart —
  MUST answer `health` **promptly** with `status:"degraded"` (or `pending` at the
  doctor layer) plus a `detail` naming the state (`"model loading"`,
  `"restarting"`), rather than hanging until the probe deadline. This lets the
  controller distinguish a healthy-but-warming daemon from a wedged one: warming
  is reported as `degraded`/`pending`, not false-failed into a synthesized `down`
  (DOC-4 §1.3; the restart window maps to `pending`, consistent with C5).

**Worked example** (degraded — required dep down):

```json
{
  "contract_version": 1,
  "tool": "trusty-analyze",
  "tool_version": "0.4.1",
  "verb": "health",
  "scope": "system",
  "status": "degraded",
  "data": {
    "uptime_secs": 120,
    "port": 7879,
    "addr": "127.0.0.1:7879",
    "deps": [ { "id": "trusty-search", "required": true, "reachable": false } ],
    "detail": "trusty-search unreachable at 127.0.0.1:7878"
  },
  "messages": [{ "level": "warn", "text": "Required dependency trusty-search is unreachable." }]
}
```

Exit code: `2` (degraded).

### `version.data`

Carries the capability advertisement (D3b) and version axes (D2). Envelope
`status` is `ok` whenever the tool can answer.

```json
{
  "verbs": ["doctor", "health", "version", "start", "stop", "restart", "config"],
                                         // capability advertisement (D3b); the authoritative supported-verb list
  "tool_version": "0.12.3",             // duplicated from envelope for standalone parsing convenience
  "contract_version": 1,                // duplicated from envelope; the negotiated level this tool speaks
  "min_contract_version": 1,            // ? lowest level this tool can still serve (defaults to contract_version)
  "build": {                            // ? optional, redacted build metadata
    "git_sha": "a1b2c3d",
    "rustc": "1.91.0",
    "target": "aarch64-apple-darwin"
  }
}
```

- `verbs[]` presence is **independent of `contract_version`** (D3, critical
  split): the controller MUST read `verbs[]` to decide what to invoke, never
  infer from the integer.
- **claude-mpm's `verbs[]` advertisement is deferred to DOC-6** (the
  orchestrator-adapter design): the *shape* of `verbs[]` is locked here, but
  exactly which verbs the Python claude-mpm adapter will advertise (and how it
  synthesizes them) is **not** decided in this doc — see DOC-6.

**Worked example:**

```json
{
  "contract_version": 1,
  "tool": "trusty-search",
  "tool_version": "0.12.3",
  "verb": "version",
  "scope": "system",
  "status": "ok",
  "data": {
    "verbs": ["doctor", "health", "version", "start", "stop", "restart", "config"],
    "tool_version": "0.12.3",
    "contract_version": 1,
    "min_contract_version": 1,
    "build": { "git_sha": "a1b2c3d", "rustc": "1.91.0", "target": "aarch64-apple-darwin" }
  },
  "messages": []
}
```

Exit code: `0`.

### `start` / `stop` / `restart`.data (lifecycle)

One shared schema (the three verbs differ only in the `action` value):

```json
{
  "action": "restart",                   // "start" | "stop" | "restart"
  "previous_state": "running",           // ENUM: "running" | "stopped" | "unknown"
  "new_state": "running",                // ENUM: "running" | "stopped" | "unknown"
  "pid": 4821,                           // ? new daemon pid (start/restart, when applicable)
  "port": 7879,                          // ? bound port (start/restart)
  "noop": false                          // true when already in target state (e.g. start an already-running daemon)
}
```

- **`previous_state` / `new_state` are a fixed enum `running | stopped |
  unknown`**, not free strings — for contract stability and consistency with the
  other status enums (D4). `unknown` covers a `previous_state` the tool cannot
  determine (e.g. no PID lockfile before a `start`).
- Idempotency: `start` on a running daemon → `status:"ok"`, `noop:true`,
  `previous_state==new_state=="running"`. `stop` on a stopped daemon likewise.
- CLI-only members that advertise no lifecycle verbs simply omit them from
  `verbs[]`; the controller degrades gracefully (D3).

**Worked example** (restart):

```json
{
  "contract_version": 1,
  "tool": "trusty-memory",
  "tool_version": "0.10.0",
  "verb": "restart",
  "scope": "system",
  "status": "ok",
  "data": {
    "action": "restart",
    "previous_state": "running",
    "new_state": "running",
    "pid": 5099,
    "port": 7070,
    "noop": false
  },
  "messages": [{ "level": "info", "text": "Daemon restarted gracefully (drained in-flight requests)." }]
}
```

Exit code: `0`.

### `config.data`

**Read-only** effective merged config (system + project per D7); editing is an
explicit spec non-goal. Secrets redacted with the fixed marker (D8). The `config`
contract verb accepts only read selectors (`--scope`, optional single-key
projection) and never mutating arguments; tool-native runtime mutation (e.g.
trusty-search memory limits) lives in a separate, non-contract verb (`tune`) that
is NOT advertised in `verbs[]`, so the contract `config` surface is read-only by
construction (cross-ref DOC-6 §2.6, DOC-5 §2/§3).

```json
{
  "effective": {                         // fully-merged view the tool actually runs with
    "memory_limit_mb": 8192,
    "embedder": "stdio",
    "openrouter_api_key": "***redacted***",
    "port": 7879
  },
  "sources": [                           // ? provenance, lowest→highest precedence; values redacted too
    { "origin": "system",  "path": "~/.config/trusty-search/config.yaml", "keys": ["memory_limit_mb", "embedder"] },
    { "origin": "project", "path": "./trusty-search.yaml",               "keys": ["port"] },
    { "origin": "env",     "path": null, "keys": ["openrouter_api_key"] } // origin enum: env | project | system (NOT the D7 scope axis)
  ],
  "redacted_keys": ["openrouter_api_key"] // ? convenience list of which keys were masked
}
```

- The precedence model that produces `effective` is the tool's own; DOC-1 only
  fixes the *shape*.
- **One `scope` axis + a separate typed `origin` (keep these distinct).**
  `scope` = the **D7 wire axis** `{project|system|all}` — *which layer a verb or
  check addresses* — and it is the **only** field named `scope` anywhere in an
  envelope (the `--scope` flag, the envelope `scope`, and every per-check
  `scope`). `origin` = **config provenance** `{env|project|system}` on
  `config.data` `sources[]` — *where a config value originated* (an environment
  variable vs a project config file vs a system default). They are distinct axes
  with distinct value sets: `all` is meaningful for `scope` but never an
  `origin`, and `env` is an `origin` but never a `scope` value. Giving provenance
  its own field name **and its own enum** means authors cannot conflate the two,
  and a typo in `origin` is catchable against its own enum rather than silently
  accepted as a stringly-typed `scope`.

**Worked example:**

```json
{
  "contract_version": 1,
  "tool": "trusty-search",
  "tool_version": "0.12.3",
  "verb": "config",
  "scope": "all",
  "status": "ok",
  "data": {
    "effective": {
      "memory_limit_mb": 8192,
      "embedder": "stdio",
      "openrouter_api_key": "***redacted***",
      "port": 7879
    },
    "sources": [
      { "origin": "system",  "path": "~/.config/trusty-search/config.yaml", "keys": ["memory_limit_mb", "embedder"] },
      { "origin": "project", "path": "./trusty-search.yaml",               "keys": ["port"] },
      { "origin": "env",     "path": null,                                  "keys": ["openrouter_api_key"] }
    ],
    "redacted_keys": ["openrouter_api_key"]
  },
  "messages": []
}
```

Exit code: `0`.

## `trusty_common` contract module — API sketch (DOC-6 build target)

Home: a new `contract` module in `trusty-common` (D6), sibling to the existing
`mcp` / `rpc` / `launchd` modules. The Rust types below are a **design sketch**,
not committed source — DOC-6's retrofits implement against them. Edition-2021
safe (no let-chains) so every member crate can depend on it.

The module also ships a **golden-snapshot conformance test**: it serializes a
canonical instance of `Envelope<T>` and each per-verb `data` struct to JSON and
asserts the result against committed fixtures, so any change to a serialized
shape fails CI unless `CONTRACT_VERSION` is bumped and a ledger row added —
coupling shape changes to the integer (see the ledger "Rule"). This gates the
Rust tools; non-Rust members are held to the same shapes via DOC-10's
captured-`--json` fixture.

```rust
// trusty_common::contract  (new module; D6)

use serde::{Deserialize, Serialize};

/// The single negotiated capability level. Monotonic integer (D2).
pub const CONTRACT_VERSION: u32 = 1;

/// Verb-appropriate top-level status (D4). Serialized lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeStatus {
    Ok, Warn, Fail,            // introspection (doctor/version), config, lifecycle
    Running, Degraded, Down,   // health
}

impl EnvelopeStatus {
    /// Exit code mirror (D5): 0 ok/running · 1 fail/down · 2 warn/degraded.
    /// (Code 3 = contract/usage error is produced by the dispatcher, not here.)
    pub fn exit_code(self) -> i32 {
        match self {
            EnvelopeStatus::Ok | EnvelopeStatus::Running => 0,
            EnvelopeStatus::Fail | EnvelopeStatus::Down => 1,
            EnvelopeStatus::Warn | EnvelopeStatus::Degraded => 2,
        }
    }
}

/// Wire scope (D7). DOC-3 owns the behavioural model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope { Project, System, All }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageLevel { Info, Warn, Error }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message { pub level: MessageLevel, pub text: String }

/// The uniform envelope (D3a), generic over the per-verb `data`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub contract_version: u32,
    pub tool: String,
    pub tool_version: String,
    pub verb: String,
    pub scope: Scope,
    pub status: EnvelopeStatus,
    pub data: T,
    #[serde(default)]
    pub messages: Vec<Message>,
}

// ---- per-verb data structs (one per schema above) ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus { Ok, Warn, Fail, Pending, Skipped }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub id: String,
    pub title: String,
    pub scope: Scope,
    pub status: CheckStatus,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    /// When this `pending` state began (DOC-4 §5.5 time-escalation input);
    /// present only when `status == Pending`. ISO-8601/RFC-3339 string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_since: Option<String>,
    /// Advisory 0–100 progress; display only, never drives a verdict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_pct: Option<u8>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DoctorSummary { pub ok: u32, pub warn: u32, pub fail: u32,
                           pub pending: u32, pub skipped: u32, pub total: u32 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorData { pub checks: Vec<DoctorCheck>, pub summary: DoctorSummary }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepInfo { pub id: String, pub required: bool, pub reachable: bool }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HealthData {
    #[serde(skip_serializing_if = "Option::is_none")] pub uptime_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")] pub addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub deps: Vec<DepInfo>,
    #[serde(skip_serializing_if = "Option::is_none")] pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionData {
    pub verbs: Vec<String>,                 // D3b capability advertisement
    pub tool_version: String,
    pub contract_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")] pub min_contract_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")] pub build: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleAction { Start, Stop, Restart }

/// Fixed lifecycle state enum (not a free string) for contract stability and
/// consistency with the other status enums.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState { Running, Stopped, Unknown }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleData {
    pub action: LifecycleAction,
    pub previous_state: LifecycleState,     // running | stopped | unknown
    pub new_state: LifecycleState,          // running | stopped | unknown
    #[serde(skip_serializing_if = "Option::is_none")] pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")] pub port: Option<u16>,
    #[serde(default)] pub noop: bool,
}

/// Config provenance (NOT the D7 wire `Scope`): a separate, typed axis so a
/// bad provenance value is catchable against its own enum, never a stray string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigOrigin { Env, Project, System }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigSource { pub origin: ConfigOrigin, pub path: Option<String>, pub keys: Vec<String> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigData {
    pub effective: serde_json::Value,       // already-redacted merged view
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub sources: Vec<ConfigSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub redacted_keys: Vec<String>,
}

/// Fixed secret marker (D8). The module also offers a redaction helper so every
/// tool masks identically (none exists in trusty-common today).
pub const REDACTED: &str = "***redacted***";
pub fn redact_value(_key: &str, _val: &str) -> String { REDACTED.to_string() }

/// The trait every Rust tool implements (D6). Each method returns typed data;
/// the tool never builds the envelope by hand.
#[allow(async_fn_in_trait)]
pub trait ContractTool {
    /// Stable tool id (matches the DOC-2 manifest key).
    fn tool_id(&self) -> &str;
    /// The tool's own semver (typically env!("CARGO_PKG_VERSION")).
    fn tool_version(&self) -> &str;
    /// Verbs this tool actually implements — drives D3b advertisement.
    fn supported_verbs(&self) -> Vec<String>;

    async fn doctor(&self, scope: Scope) -> DoctorData;
    async fn health(&self, scope: Scope) -> (EnvelopeStatus, HealthData); // status is running/degraded/down
    fn version(&self, scope: Scope) -> VersionData;                       // pure; never fails
    async fn start(&self, scope: Scope) -> LifecycleData;                 // optional verbs may be unimplemented
    async fn stop(&self, scope: Scope) -> LifecycleData;
    async fn restart(&self, scope: Scope) -> LifecycleData;
    async fn config(&self, scope: Scope) -> ConfigData;                   // values pre-redacted by the tool
}

/// Dispatcher: serializes the envelope and computes the process exit code (D5).
/// Tools wire their `main()` subcommand to call this. It owns:
///  - wrapping typed `data` in `Envelope<T>` with the shared metadata,
///  - rolling doctor checks up to the envelope status,
///  - rejecting unadvertised/unknown verbs with exit code 3,
///  - printing JSON to stdout (logs stay on stderr per repo rule).
pub struct Dispatcher;

impl Dispatcher {
    /// Roll a doctor result up to an envelope status (any fail→Fail, else any
    /// warn→Warn, else Ok; pending/skipped never worsen it).
    pub fn doctor_status(d: &DoctorData) -> EnvelopeStatus { /* … */ EnvelopeStatus::Ok }

    /// Build, print to stdout, and return the exit code for a typed result.
    pub fn emit<T: Serialize>(env: &Envelope<T>) -> i32 { /* serde_json to stdout */ env.status.exit_code() }
}
```

DOC-6 retrofit then reduces to: *implement `ContractTool` + wire each subcommand
to `Dispatcher::emit`*. claude-mpm (Python) emits the same JSON shapes from its
adapter rather than using these Rust types (D6).

## `contract_version` ledger

**Negotiation floor.** The controller declares a **floor** `F` and targets a
level `N` (`N >= F`). It accepts any tool whose advertised `contract_version >=
F`, and for a tool below `N` renders only the fields level `tool_cv` guarantees
(graceful degrade, never hard-fail). A tool below `F` is rejected as
contract-incompatible (exit code `3` at the controller boundary). For the v1
launch, **floor `F = 1`**.

**Rule (D3, restated):** *adding a verb does NOT bump `contract_version`.* Verb
presence is advertised via `verbs[]` (a capability). Bump the integer **only**
when the envelope shape or an existing verb's `data` schema changes in a way that
is **not** a pure additive superset (i.e., would break an older consumer).
Adding an **optional** field is additive and does NOT bump. This rule is
CI-enforced via the `trusty_common` golden-snapshot test — the snapshot, the
integer bump, and the ledger row move together — not by reviewer discipline.

| `contract_version` | What it guarantees / what changed |
|---|---|
| **1** (baseline) | The uniform envelope (`contract_version`, `tool`, `tool_version`, `verb`, `scope`, `status`, `data`, `messages`). Status vocabularies: `ok\|warn\|fail` (doctor/version/config/lifecycle), `running\|degraded\|down` (health). Doctor-check status `ok\|warn\|fail\|pending\|skipped`. Scope wire vocabulary `project\|system\|all`. Exit-code mirror `0/1/2/3`. Secret redaction marker `***redacted***`. Per-verb `data` schemas for `doctor`, `health`, `version` (incl. `verbs[]` advertisement), `start`/`stop`/`restart`, `config` exactly as specified above. Capability advertisement + generic passthrough semantics. |
| 2+ (future, illustrative — not yet defined) | Reserved for genuinely breaking shape changes, e.g. renaming/removing an envelope field, changing a `status` enum's meaning, or making a previously-optional field required. New **verbs** and new **optional** fields land at v1 without a bump. |

## Conformance snapshot (DOC-6 input)

Gap audit as of 2026-06-08 — the matrix DOC-6 will action. Rows = stack members;
columns = the seven verbs + the `version.verbs[]` advertisement + a uniform
`--json`/envelope output + `contract_version`. Cells: `✅` conformant today ·
`⚠️` exists but text-only / different shape / partial · `❌` missing.

> **Reality check:** essentially every introspection/lifecycle verb is `⚠️` or
> `❌` against the contract — the *commands* often exist but emit human text, not
> the envelope. No tool advertises `verbs[]` or emits `contract_version` today.
> So the controller-facing contract is effectively **net-new across the board**;
> the `⚠️` cells just mean "there's existing behaviour to wrap," not "almost
> done."

| Tool | `doctor` | `health` | `version` | `start` | `stop` | `restart` | `config` | `version.verbs[]` | uniform `--json`/envelope | `contract_version` |
|---|---|---|---|---|---|---|---|---|---|---|
| **trusty-search** | ⚠️ text checks | ⚠️ `status`/`GET /health` JSON, wrong shape | ⚠️ `--version` flag only | ⚠️ text | ⚠️ text | ❌ | ⚠️ runtime get/set only | ❌ | ⚠️ scattered (`port`,`status`,`list`) | ❌ |
| **trusty-memory** | ⚠️ text checks (`Pass/Warn/Fail`) | ⚠️ `GET /health` JSON (`ok/degraded`), no CLI verb | ⚠️ `--version` flag only | ⚠️ text | ⚠️ text | ❌ | ❌ | ❌ | ⚠️ scattered (`port`,`monitor`) | ❌ |
| **trusty-analyze** | ⚠️ text checks | ⚠️ `health` CLI + `GET /health` (`ok/degraded`) | ⚠️ `--version` flag only | ⚠️ text | ⚠️ text | ❌ | ❌ | ❌ | ❌ | ❌ |
| **trusty-review** | ❌ | ⚠️ `GET /health` JSON (`ok/degraded` + `deps`), no CLI verb | ❌ (`--version` flag only) | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **claude-mpm** (ext.) | ⚠️ `mpm-doctor` text (assumed) | ⚠️ process liveness only (assumed) | ❌ | ⚠️ depends on run mode | ⚠️ depends on run mode | ❌ | ❌ | ❌ | ❌ | ❌ |

Highest-leverage DOC-6 work, in order: (1) land the `trusty_common::contract`
module + `Dispatcher`; (2) retrofit trusty-search (richest existing surface →
fastest conformance, sets the pattern); (3) memory & analyze (their `GET /health`
already speaks the degraded vocabulary); (4) trusty-review (net-new CLI verbs —
the laggard); (5) the claude-mpm Python adapter (synthesizes everything).

## Dependencies

### Consumes (inputs)
- DOC-0 (the chosen `<name>` for `<tool>`-shaped examples).
- DOC-3 (scope semantics) — bidirectional: DOC-3 feeds the `scope` schema fields.

### Produces (consumed by)
- DOC-2, DOC-4, DOC-5, DOC-6, DOC-7, DOC-9.

## Grounding (exists vs. net-new)

Source-first audit of the **real** current command/HTTP surfaces (clap command
enums + axum handlers), 2026-06-08. The schemas below standardize the **union**
of what already exists rather than inventing a new surface; the daemon `GET
/health` JSON is the template for the envelope's introspection payload. Headline
findings:

- **BIGGEST GAP — no JSON introspection contract.** `doctor` / `health` /
  `status` emit coloured human text. No tool has a `version` subcommand (only
  clap's `--version` flag); none emits a `contract_version`; none advertises a
  `verbs[]` array. `--json` exists only on a scattered handful of subcommands
  (`port`, `status`/`query`, `list`, `monitor`) — never as a uniform
  contract flag on the introspection verbs.
- **Partial template — daemon `GET /health`.** All four daemons return a JSON
  health body, but with **divergent shapes and status vocabularies** (see table).
  trusty-search is the richest; trusty-review and trusty-analyze already use a
  `status: "ok"|"degraded"` vocabulary that maps cleanly onto D4's health enum.
- **No `restart` anywhere.** Every tool has `start`+`stop`, but `restart` is
  net-new for all of them (operators currently use launchd `bootout`/`bootstrap`).
- **`config` is search-only and runtime-only.** Only trusty-search has a `config`
  subcommand, and it is a `get`/`set` for live daemon memory limits — not the
  read-only *effective merged config* this contract defines. memory / analyze /
  review have no `config` verb at all.
- **Worst offender — trusty-review.** Implements **none** of the seven verbs at
  the CLI level (only `Run` / `Compare` / `Serve` / `Profile`). Its only
  contract-relevant surface is the daemon's `GET /health` + `GET /status`, which
  is, ironically, the closest existing match to the target envelope `status`
  semantics. trusty-review is the heaviest DOC-6 retrofit.
- **claude-mpm has no machine contract.** It is Python, external to this repo
  (orchestrator is pluggable per ADR-0006), and exposes only human-oriented
  `mpm-doctor`-style commands today (spec §82). The adapter must **synthesize**
  the entire contract surface; details are DOC-6, summarized below.

### Per-tool command surface

`✅` = exists & contract-shaped · `⚠️` = exists but text-only / different shape /
partial · `❌` = absent. "Source" cites the clap enum / axum handler read.

#### trusty-search (richest) — `crates/trusty-search/src/main.rs`, `commands/`, `service/server/health.rs`

| Verb | Current CLI | JSON? | Notes |
|---|---|---|---|
| `doctor` | `doctor [--fix]` (`commands/doctor.rs`) | ❌ text | 6 checks via `CheckResult::{Ok,Warn,Error}(String)` — message-only, no ids/scope/remediation |
| `health` | `status` (alias `health`) | ⚠️ `status --json` exists | richest daemon body |
| `version` | clap `--version` flag only | ❌ | no `verbs[]` advertisement |
| `start` / `stop` | `start [...]` / `stop` | ❌ | text |
| `restart` | ❌ absent | — | net-new |
| `config` | `config get\|set <key> [val]` | ❌ text | live memory-limit knobs, not effective merged config |
| extras | `serve`, `port [--json]`, `service install\|uninstall\|status\|logs`, `upgrade [--check\|--yes]`, `integrate`, `setup`, `monitor`, `index`, `query [--json]`, `list [--json]`, `reindex`, `cleanup`, `convert`, `migrate*` | mixed | — |

`GET /health` body (`HealthResponse`, the **template**):
`{ status:"ok", version, indexes, uptime_secs, embedder, embedder_error?, rss_mb, rss_limit_mb, disk_bytes, cpu_pct, embedder_info?{dimension,provider,quantized}, embedderd_rss_mb?, background_reindex_queue_depth, update_available? }`.
`status` is the literal `"ok"` (not the running/degraded/down vocabulary).
`POST /upgrade` returns `{ status:"checked"|"installing"|"up_to_date", current, latest, update_available, message }`.

#### trusty-memory — `crates/trusty-memory/src/main.rs`, `commands/doctor.rs`, `web.rs`

| Verb | Current CLI | JSON? | Notes |
|---|---|---|---|
| `doctor` | `doctor` (`commands/doctor.rs`) | ❌ text | `CheckStatus::{Pass,Warn,Fail}` + label + optional detail; richer than search (has a `label`), still no ids/scope/remediation; also a separate palace-audit (`PalaceAuditStatus`) |
| `health` | ❌ no CLI verb (HTTP only) | ❌ CLI | daemon body has `status:"ok"|"degraded"` already |
| `version` | clap `--version` only | ❌ | — |
| `start` / `stop` | `start` / `stop` | ❌ | text |
| `restart` | ❌ | — | net-new |
| `config` | ❌ | — | net-new |
| extras | `serve`, `setup`, `migrate`, `prompt-context`, `service`, `monitor [status\|palaces --json]`, `send-message`, `inbox-check`, `note`, `kg-rebuild`, `link`, `port [--json]`, `upgrade` | mixed | — |

`GET /health` body (`HealthResponse`): `{ status:"ok"|"degraded", detail?, version, rss_mb, disk_bytes, cpu_pct, uptime_secs, addr?, open_fds?, fd_soft_limit?, update_available? }`. **Already carries the degraded vocabulary + `detail` + `uptime` + `addr`** — closest to the target `health.data`/envelope split.

#### trusty-analyze — `crates/trusty-analyze/src/main.rs`, `service/mod.rs`

| Verb | Current CLI | JSON? | Notes |
|---|---|---|---|
| `doctor` | `doctor [--port]` | ❌ text | also an MCP `run_diagnostics` tool |
| `health` | `health` (CLI verb) + `GET /health` | ⚠️ | daemon body uses `status:"ok"|"degraded"` |
| `version` | clap `--version` only | ❌ | — |
| `start` / `stop` | `start [...]` / `stop [...]` | ❌ | text |
| `restart` | ❌ | — | net-new |
| `config` | ❌ | — | net-new |
| extras | `serve`, `status`, `service install\|uninstall\|status\|logs`, `setup`, `facts`, `analyze`, `review`, `deep`, `review-pr`, `mcp`, `dashboard`, `completions` | mixed | hard runtime dep on trusty-search |

`GET /health` body (`HealthResponse`): `{ status:"ok"|"degraded", version, search_reachable }` — minimal; `degraded` ⇔ trusty-search unreachable (200 vs 503). A natural model for the envelope's dependency-aware health `status`.

#### trusty-review (laggard) — `crates/trusty-review/src/main.rs`, `service/handlers.rs`

| Verb | Current CLI | JSON? | Notes |
|---|---|---|---|
| all seven verbs | ❌ **none at CLI** | ❌ | CLI is `Run` / `Compare` / `Serve` / `Profile` only |
| `health` (HTTP) | `GET /health`, `GET /status` (daemon) | ✅ JSON | best existing match to target `status` semantics |

`GET /health` body (`HealthResponse`): `{ status:"ok"|"degraded", version, dry_run, reviewer_model, inference:"ok"|"unreachable"|"auth_error"|"unknown", deps:{ trusty_search:{required,reachable}, trusty_analyze:{required,reachable} } }`, with a `compute_status()` that degrades on bad inference or an unreachable *required* dep. `GET /status` → `{ in_flight, last_error? }`. This dep-graph + `compute_status` rollup is the prototype for the controller-level health rollup (DOC-4) and informs `health.data`'s optional `deps` block.

#### claude-mpm (Python orchestrator — external, pluggable per ADR-0006)

Not in this repo. Known/assumed today: a human-oriented `mpm-doctor`-style
capability exists (spec §82) but there is **no machine-JSON contract** — no
envelope, no `contract_version`, no `verbs[]`, no `--json`. To satisfy this
contract the **adapter must synthesize the entire surface**: wrap whatever
`mpm-doctor`/CLI output exists into `doctor.data.checks[]`, emit a `version`
envelope with a hand-maintained `verbs[]` (likely `doctor`,`health`,`version`;
`start`/`stop`/`restart`/`config` may be `❌`/degraded depending on how
claude-mpm is run), and map process liveness to `health`. Full adapter design is
**DOC-6** — this paragraph is the contract-side note only. Because the
orchestrator is swappable (claude-mpm now → trusty-mpm later), the adapter is the
*only* place orchestrator-specific glue lives; the controller treats it like any
other contract-conformant member.

## Cross-cutting notes

- **Contract-versioning behavior:** this doc owns the canonical degradation rule
  (accept older contracts, render degraded rather than fail) that DOC-4/5/7/9 reference.
- **Security / secrets:** no secrets in any contract output (doctor remediation
  hints, health, version, config dumps must be redacted).

## Remaining work

- [x] Author per-verb `data` JSON schemas at `contract_version: 1` (`doctor`, `health`, `version`, `start`, `stop`, `restart`, `config`) with worked examples
- [x] Grounding: capture actual current CLI/HTTP output of trusty-search / trusty-memory / trusty-analyze / trusty-review + claude-mpm `mpm-doctor`, and standardize the union into the schemas
- [x] Specify the `trusty_common` contract module API (trait + envelope + data types)
- [x] Define the `contract_version` floor + the "what's new per version" ledger (v1 baseline)
- [x] Publish the conformance checklist for DOC-6
- [x] Write the contract-version ADR (charter B2) — [ADR-0007](../../../adr/0007-tool-contract-versioning-and-verb-model.md) (now Accepted)
- [x] Coordinate `scope` fields with DOC-3 (DOC-3 Accepted; D7 wire vocabulary
      `{project|system|all}` documented as distinct from the config-provenance
      `sources[].origin` axis `{env|project|system}`)
- [x] Team review → Accepted (owner-approved)

> **Deferred (not design-time):** claude-mpm's concrete `verbs[]` advertisement
> is deferred to DOC-6 (orchestrator-adapter design). Everything remaining is
> implementation-time work (DOC-6 retrofits against the `trusty_common::contract`
> module), not design.
