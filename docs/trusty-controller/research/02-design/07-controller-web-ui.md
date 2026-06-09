# DOC-7 — Controller Web UI (link-out control plane)

**Status:** Accepted (owner-approved)
**Source spec:** ../01-spec/trusty-end-to-end-setup.md
**Consumes:** [DOC-1](./01-tool-contract.md) (Accepted), [DOC-2](./02-stack-manifest-and-versioning.md) (Accepted), [DOC-4](./04-doctor-health-rollup.md) (Accepted), [DOC-5](./05-controller-cli.md) (Accepted)
**Cross-ref:** [DOC-6](./06-contract-conformance-and-mpm-adapter.md) (Accepted), [DOC-8](./08-install-bootstrap.md) (Accepted), [DOC-9](./09-upgrade-flow.md) (Accepted), [DOC-0](./00-naming-and-doc-charter.md)

## Purpose

Specify the out-of-the-box controller UI the spec asks for (§44–§56): once
`trusty-controller` is installed and running, a web UI is available that shows
all installed tools + versions, surfaces upgrade indicators and an upgrade
action, shows the health of every tool, runs per-tool `doctor` + a comprehensive
stack `doctor`, and renders the results — **while strictly LINKING OUT to each
tool's existing rich UI (trusty-search index browser, trusty-memory palace UI)
rather than reimplementing it.** This is the spec's load-bearing hard rule
(§56, verbatim):

> trusty-controller UI **must not reimplement UI functionality present in e.g.
> trusty-memory trusty-search UIs but rather link to these UIs where applicable.**

DOC-7 is therefore deliberately a **thin link-out control plane**: it owns the
*control-plane* surfaces (stack overview, health/doctor rollup, upgrade
indicators, action buttons) and *links out* for the *data-plane* surfaces (each
tool's own deep UI). It renders the **exact same `--json` rollup the CLI
produces** (DOC-4 §8.2) — it never recomputes a verdict, never re-derives a
dependency graph, never re-implements a tool's view. Every action it offers maps
1:1 onto a DOC-5 controller command, dispatched through a controller HTTP API
that wraps the same DOC-5 dispatch engine the CLI uses, so the UI and the CLI
share **one code path and zero tool-specific logic** (spec §83).

---

## DESIGN

### 1. Scope & the link-out boundary (the hard rule)

The single most important design decision in DOC-7 is the boundary between what
the controller UI **renders itself** and what it **links out to**. The rule is:

> **Render the control plane; link out for the data plane.** The controller UI
> renders only artifacts the controller already owns (the manifest registry, the
> DOC-4 rollup, the DOC-9 updates listing, the contract envelopes it already
> collected). For anything that is a *tool's own functionality* — browsing a
> search index, exploring a memory palace, reading a review — it renders a
> **link** to that tool's `/ui`, never a reimplementation.

This keeps the controller a coordinator (DOC-0 / spec §65: "coordinator and never
a direct implementor of tool-specific operations") and means a tool can evolve
its own UI freely without the controller drifting out of sync. The controller UI
holds **zero tool-specific view code**: every panel it renders is driven by the
generic rollup struct (DOC-4) or the manifest (DOC-2); the only per-tool thing it
knows is "here is a URL to that tool's UI," discovered dynamically (§4).

#### 1.1 Render-vs-link table

| Concern | Controller UI does | Why |
|---|---|---|
| Stack overview: members + installed versions + pinned versions + `stack_version` | **RENDER** | From DOC-2 manifest registry + DOC-9 detection (`version --json`). Controller-owned data. |
| Health / doctor rollup matrix (tools × scope, verdicts `ready\|degraded\|pending\|down`) | **RENDER** | The DOC-4 rollup is *the* controller-owned artifact (DOC-4 §8.2 explicitly: "DOC-7's UI renders the same `--json` rollup"). |
| Per-tool doctor drill-down (`checks[]`, `detail`, `remediation`) | **RENDER** | Verbatim re-print of the DOC-1 `doctor.data.checks[]` the controller already collected (DOC-4 §3.2). Not a reimplementation — it is the contract payload. |
| Upgrade indicators + changelog headlines + "you're on `2026.06-1`, `2026.07-1` available" | **RENDER** | DOC-9 `tctl updates --json` + DOC-2 §5 changelog headlines. Controller-owned. |
| Action buttons (upgrade / restart / run doctor / ensure / start / stop) | **RENDER** (button) → **dispatch** to controller API | The actions ARE DOC-5 controller operations (§3). |
| **trusty-search index browser / search results / chunk explorer** | **LINK OUT** to `trusty-search` `/ui` | Tool's own data-plane UI — hard rule §56. |
| **trusty-memory palace UI / memory browser / KG explorer** | **LINK OUT** to `trusty-memory` `/ui` | Tool's own data-plane UI — hard rule §56. |
| **trusty-analyze dashboard** (if it serves one) | **LINK OUT** to `trusty-analyze` `/ui` if `ui.available` | Tool's own data-plane UI. |
| Tool config editing | **NEITHER** — read-only `config` render only | Spec non-goal (§62: "not a tool-internal config editor"); `config` is rendered read-only + redacted (DOC-1 D8 / DOC-3 §7). |
| claude-mpm "open UI" | **DISABLED / absent** | The orchestrator has no UI member (DOC-6); render n/a, no link (§4.3). |

The boundary is mechanical, not per-tool: the controller renders a link out for a
member **iff** its manifest `ui.available = true` (DOC-2 §3); otherwise the
"open UI" affordance is absent. Nothing else about a tool's UI is known to the
controller.

### 2. Views / screens

The UI is a small SPA with four primary screens, each backed by a controller-owned
data source. No screen has its own computation: each is a render of a `--json`
payload the controller already emits for the CLI.

#### 2.1 (a) Stack dashboard — the landing screen

The default view. Renders the **DOC-4 tools × scope matrix** (rows = manifest
members, columns = `system` / `project`, cells = verdict
`ready|degraded|pending|down|n/a`) plus a stack-verdict banner and the active
`stack_version`. Each member row also shows installed version vs pinned version
and an upgrade chip when an update is available.

- **Primary data source:** `tctl stack health --json` (fast liveness sweep,
  DOC-4 §4) polled for the at-a-glance matrix; a manual "Run full doctor" control
  fetches `tctl stack doctor --json` for the deep verdict.
- **Secondary:** `tctl updates --json` (DOC-9 §1.4) for the per-row upgrade chips;
  the DOC-2 manifest for `display_name`, `kind`, and the `ui.available` flag that
  decides whether the row shows an "Open <tool> UI" link.
- **Rendering rule:** the cell glyphs/verdict vocabulary are exactly DOC-4 §3.1's
  (`✓ ready` · `! degraded` · `… pending` · `✗ down` · `— n/a`); the UI maps them
  to colour but never re-derives them. The de-duplicated dependency clusters
  (DOC-4 §5.4 `clusters[]`) drive a "root cause" callout so the UI shows one root
  failure + N annotated dependents, never N+1 scary failures.

#### 2.2 (b) Per-tool detail

Drill-in from a dashboard row. Shows one member's: installed `tool_version`,
`contract_version` (with the "contract too old" badge when applicable, §7),
its `system`/`project` verdicts, the full doctor `checks[]` drill-down
(`id`, `title`, `scope`, `status`, `detail`, `remediation` — verbatim DOC-1
payload, DOC-4 §3.2), a read-only redacted `config` view (DOC-1 D8 / DOC-3 §7),
per-member action buttons (run doctor, restart, upgrade — §3), and — when
`ui.available` — a prominent **"Open <tool> UI"** link (§4) to that tool's own
deep UI.

- **Primary data source:** the per-member slice of `tctl stack doctor --json`
  (`members[].checks[]`, `cells`, `contract_version`), plus
  `tctl <tool> config --json` (passthrough, DOC-5 §1.1) for the read-only config
  panel, fetched on demand.
- **Link-out:** the "Open <tool> UI" button is the single place the controller
  surfaces a tool's own functionality, and it is a *link*, not an embed (§4).

#### 2.3 (c) Updates / upgrade view

Renders the cross-tool updates listing: per-member installed → available diff,
the installed `stack_version` → target `stack_version` header, and the
**changelog headlines** between current and available for each member with an
update (DOC-9 §1.4, §2). Offers an **"Upgrade stack"** action and per-member
"Upgrade" actions (§3).

- **Primary data source:** `tctl updates --json` (DOC-9 §1.4) — which already
  carries the per-member diff and the best-effort-parsed changelog headlines
  (DOC-2 §5). The UI renders the headlines; it never fetches or parses changelogs
  itself (DOC-9 §2.2: "DOC-7 renders, never re-derives").
- **Drift indicator:** if the controller recorded a drifted/partial stack
  (`2026.06-1+latest` / `(partial)`, DOC-9 §4.4), the view shows a "drifted off
  known-good tuple" warning banner sourced from the same JSON.

#### 2.4 (d) Stack doctor view

The comprehensive-doctor screen the spec calls for (§55: "a comprehensive
'doctor' task to determine overall claude-mpm + trusty-tools health"). Renders
the full DOC-4 deep rollup: the matrix, the stack verdict + exit-code-equivalent,
the verbose per-check drill-down for every member, the dependency `clusters[]`
root-cause grouping, and the bubbled-up top remediation (DOC-4 §6). A "Re-run
stack doctor" button re-dispatches.

- **Primary data source:** `tctl stack doctor --json` (DOC-4 §8.2) — the complete
  rollup struct (`verdict`, `exit_code`, `summary`, per-member `cells`/`checks`,
  `clusters[]`, `remediations[]`). The UI is a pure renderer of this struct.

**Data-source summary (each screen → its controller-owned JSON):**

| Screen | Primary `--json` source | Supporting sources |
|---|---|---|
| (a) Stack dashboard | `tctl stack health --json` (DOC-4) | `tctl updates --json` (DOC-9), manifest `ui`/`display_name`/`kind` (DOC-2) |
| (b) Per-tool detail | per-member slice of `tctl stack doctor --json` (DOC-4) | `tctl <tool> config --json` (DOC-5/DOC-1), `ui` hint (DOC-2) |
| (c) Updates/upgrade | `tctl updates --json` (DOC-9) | DOC-2 §5 changelog headlines (already embedded in the updates JSON) |
| (d) Stack doctor | `tctl stack doctor --json` (DOC-4) | — |

### 3. Actions from the UI — UI → controller HTTP API → DOC-5 dispatch

The spec wants the UI to be a **control plane**, not just a dashboard (§48:
"Visualization is important as it provides an overview and also a control
plane"), and to offer "means to upgrade from UI" (§52). Every mutating affordance
in the UI is one of the DOC-5 controller operations. The mechanism that makes
this share-one-code-path with the CLI:

> The controller daemon serves an **HTTP API that is a thin wrapper over the same
> DOC-5 dispatch engine the CLI invokes.** A UI button does not shell out, does
> not talk to tools directly, and contains no tool-specific logic: it POSTs to a
> controller-local API endpoint, which calls the **identical** DOC-5 §2 dispatch
> pipeline (load manifest → select members → negotiate capability → invoke verb
> at scope → collect envelopes → roll up). CLI and UI are two front-ends over one
> engine.

```
Browser (UI button)
   │  POST http://127.0.0.1:<tctl-port>/api/v1/<action>   (loopback only, §6)
   ▼
Controller HTTP API (axum, in the tctl daemon)
   │  validate request → call the SAME DOC-5 §2 dispatch engine
   ▼
DOC-5 dispatch  (load manifest → select → negotiate → invoke verb → collect)
   │  spawn `<binary> <verb> --scope <S> --json`  (DOC-1 D3c passthrough)
   ▼
Tools (contract verbs)  →  envelopes  →  DOC-4 rollup  →  JSON back to the UI
```

The API surface is the HTTP mirror of the DOC-5 command tree (DOC-5 §1.1); the
exact route shapes are implementation-time, but the mapping is fixed:

| UI action | DOC-5 command it maps to | API call (illustrative) | Mutating? |
|---|---|---|---|
| Run stack doctor | `tctl stack doctor` (DOC-4) | `GET /api/v1/stack/doctor?scope=all` | no |
| Run stack health (poll) | `tctl stack health` (DOC-4) | `GET /api/v1/stack/health` | no |
| Run per-tool doctor | `tctl <tool> doctor` (DOC-5 §1.4) | `GET /api/v1/tool/<id>/doctor` | no |
| List updates | `tctl updates` (DOC-9) | `GET /api/v1/updates` | no |
| Upgrade stack | `tctl upgrade` (DOC-9) | `POST /api/v1/upgrade` | **yes (system)** |
| Upgrade one member | `tctl upgrade <member>` (DOC-9) | `POST /api/v1/upgrade/<id>` | **yes (system)** |
| Restart | `tctl restart [members]` (DOC-5 §7) | `POST /api/v1/restart` | **yes (system)** |
| Start / Stop | `tctl start` / `tctl stop` | `POST /api/v1/{start,stop}/<id>` | **yes (system)** |
| Ensure project | `tctl ensure --scope project` (DOC-8) | `POST /api/v1/ensure` | yes (project) |
| Read config | `tctl <tool> config` (read-only) | `GET /api/v1/tool/<id>/config` | no |

#### 3.1 Blast-radius confirmation for system ops

System-mutating actions (upgrade / restart / start / stop) carry the same
blast-radius warning the CLI shows (DOC-3 §5 / DOC-5 §3.3). In the UI this is a
**confirm dialog** before the POST, stating exactly what the CLI prints — e.g.
"This will restart 3 system daemons + the controller UI service; all active
projects and sessions on this machine will be interrupted. Continue?" The
controller computes the radius mechanically from `scope`/`kind` (DOC-5 §3.3); the
UI just renders it. The non-interactive `--yes` bypass has **no UI analogue** —
a human clicking a button is the confirmation; the UI always shows the dialog for
system ops (a deliberate non-bypass, see Open Question 2). The API itself still
requires an explicit "confirmed" flag on mutating POSTs so a stray request cannot
silently trigger a system op.

#### 3.2 Long-running op progress (reuse the SSE pattern)

Upgrade / restart / install are long-running. The UI must show progress, not
freeze. DOC-7 **reuses the existing SSE progress pattern** that trusty-search
already ships for reindex (verified: `POST /indexes/:id/reindex` → `stream_url`;
`GET /indexes/:id/reindex/stream` emits `start`/`progress`/`complete`/`error`
events with a **replay buffer** so late subscribers still see `start`; typed
client `trusty_common::monitor::search_client::ReindexEvent`) and that DOC-8 §3
reuses for install/ensure progress.

- A mutating POST (e.g. `POST /api/v1/upgrade`) returns a `stream_url`; the UI
  subscribes to `GET /api/v1/ops/<id>/stream` (SSE, `text/event-stream`) and
  renders per-member progress (`upgrading trusty-search 0.24.1 → 0.25.0 …`,
  `health gate … ok`, `restarting daemon (draining) …`, DOC-9 §5.3).
- The **replay buffer** convention (trusty-search reindex stream) carries over so
  a browser that opens the stream slightly after the op started still sees the
  earlier events.
- On completion the UI re-fetches `tctl stack doctor --json` and re-renders the
  matrix, closing the loop ("usable now, indexing in progress" → `project:
  pending` is shown as positive-trajectory, not an error — DOC-4 §2.0 / DOC-3 §2).

This is net-new wiring (the controller's own op stream) but **zero new protocol**:
it is the same SSE+replay shape the daemons already implement.

### 4. UI URL discovery & link-out mechanics

The controller never hard-codes a tool's UI URL — daemons auto-bind their port
(`bind_with_auto_port`, verified in `trusty_common`), so the live URL is
discovered at runtime, exactly as DOC-2 §3 prescribes.

#### 4.1 How a member's live UI URL is found

For each member with `ui = { available = true, path = "/ui", port_source =
"port_json" }` (DOC-2 §3):

1. The controller discovers the member's live port by running
   `<binary> port --json` (verified present on trusty-search / trusty-memory),
   the `port_source = "port_json"` mechanism DOC-2 §3 defines. (Equivalent fallbacks
   exist in the tree — the `http_addr` file / `port.lock` — but the contract path
   is `port --json`.)
2. It composes the link as `http://<addr><ui.path>` → e.g.
   `http://127.0.0.1:7879/ui` (search), `http://127.0.0.1:7070/ui` (memory).
3. The dashboard/per-tool screens render this as the **"Open <tool> UI"** link,
   opening in a new tab. This is the precise analogue of the existing `monitor
   web` / `trusty-search dashboard` link-out behaviour (verified
   `crates/trusty-search/src/commands/dashboard.rs`: builds `http://<addr>/ui`,
   opens via `open::that`, **degrades to a printed URL** on failure) — DOC-7's
   in-browser link is the same idea surfaced as an `<a href>`.

The manifest only ever says "this member has a UI and here is *how to find* its
URL" (`port_source`), never a pinned port — keeping link-out dynamic per DOC-2 §3.

#### 4.2 A tool that is down

If a member's `system` verdict is `down`/`unreachable` (DOC-4 §5.1), its `port
--json` probe fails, so there is no live URL. The UI then:

- **Disables** the "Open <tool> UI" link and shows a hint — "start <tool> to view
  its UI" — with the **start** remediation the rollup already carries (DOC-4 §6:
  *down* → `run <binary> start` → the UI offers the Start action, §3).
- Never renders a dead link to a non-listening port.

#### 4.3 A member with no UI (claude-mpm)

A member whose manifest omits the `ui` sub-table or sets `ui.available = false`
(the orchestrator claude-mpm — DOC-6: it is a Python member synthesized through
the shim and has **no controller-reachable UI**) renders **no "Open UI"
affordance at all** — the cell is simply absent, not a broken link. The
controller still renders its health/doctor row (sourced from the DOC-6
orchestrator shim envelopes); it just offers no link-out. This is the same
graceful-degrade rule as the verb-not-advertised case (DOC-4 §5.3): absence is
`n/a`, never an error.

### 5. Tech & serving

**Recommendation: mirror trusty-search / trusty-memory exactly** — embedded
Svelte SPA + committed `ui-dist/` + `build.rs` + `include_dir!`, served by the
controller's own axum server at `GET /ui`. Rationale: consistency with the two
existing UIs (one mental model, shared conventions, and — per the hard rule — the
controller's UI sits *alongside* them, so matching their serving model is the
least-surprise choice), and every primitive is already grounded in the tree.

Verified patterns this reuses verbatim (from `crates/trusty-search/`):

- **Embedding:** `static UI_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/ui-dist");`
  (verified `src/service/ui.rs:39`) — the Svelte build output is baked into the
  `tctl` binary, so a fresh `cargo install trusty-controller` is fully
  self-contained, no separate static-file server. This is the **same pattern
  DOC-2 §2 already cites** for embedding the default manifest into `tctl`.
- **Serving:** two axum handlers — `GET /ui` → `index.html` (with runtime config
  injected) and `GET /ui/*path` → static asset with SPA fallback to `index.html`
  (verified `ui.rs` `ui_index_handler` / `ui_asset_handler`). Runtime config is
  injected into `index.html` as `window.__*` globals before the bundle loads
  (verified `inject_runtime_config`) — DOC-7 injects the controller's own port so
  the SPA reaches `/api/v1/*` at the right host:port.
- **Build:** a `build.rs` that invokes the pnpm/`make release-prep` UI build and
  falls back to a placeholder `index.html` so `include_dir!` always compiles
  (verified `crates/trusty-search/build.rs`).
- **Publish caveats (DOC-0 / root CLAUDE.md), inherited verbatim:**
  - `SKIP_UI_BUILD=1` must prefix `cargo publish` for the controller (it is a
    UI-embedding crate) so `build.rs` does not try to run pnpm inside cargo's
    verification tarball — the committed `ui-dist/` bundle is used as-is. This is
    the same `ui.available`-derived signal DOC-8 §1.3 / DOC-9 §3.3 set for the
    other UI crates.
  - **macOS cdhash/codesign caveat:** install/upgrade of `tctl` go through
    `cargo install … --locked` (atomic rename), **never `cp`** — a `cp` over the
    on-PATH binary leaves a stale cdhash and the next exec is SIGKILL'd
    (root CLAUDE.md; DOC-9 §3.3). DOC-7's UI being embedded in the same binary
    means the controller-UI release rides this exact convention.

**The `tctl ui` command opens it** (DOC-5 §1.1: `tctl ui` prints/opens the
controller web-UI URL, discovered via `tctl port`; `tctl ui --print` just prints
the URL). This is the controller's own analogue of `trusty-search dashboard` —
same `open::that` + degrade-to-printed-URL behaviour.

### 6. Security posture

The controller UI matches the existing daemons' posture exactly — anything else
would widen the attack surface, which the cross-cutting security concern (README;
DOC-2 §8; DOC-1 D8) forbids.

- **Loopback-only bind.** The controller daemon binds `127.0.0.1` only (the
  `bind_with_auto_port` convention every daemon uses — verified `trusty_common`),
  matching the search/memory daemons (verified: trusty-search CLAUDE.md — "daemon
  binds loopback only; do **not** bind it to a non-loopback interface"). The UI
  and its `/api/v1/*` action API are reachable only from the local machine.
- **No auth, by parity.** Like the existing daemons (verified: trusty-search
  "Authentication: none. The daemon is localhost-only and trusts every caller"),
  the controller UI has no auth layer in v1. This is acceptable *only because* of
  the loopback bind; the action API MUST NOT widen this — it binds the same
  loopback interface, never a routable one (see Open Question 3 on whether
  mutating endpoints warrant a CSRF/origin guard even on loopback).
- **Permissive CORS for the local browser** (the existing daemons set permissive
  CORS for browser admin UIs — verified trusty-search CLAUDE.md), scoped to the
  same loopback-only reachability.
- **No secrets rendered.** The read-only `config` panel renders the
  already-redacted contract output (DOC-1 D8 `***redacted***`); the controller
  passes envelopes through verbatim and never re-introduces secrets (DOC-4 §6,
  DOC-5 cross-cutting). The manifest the dashboard renders carries no secrets by
  construction (DOC-2 §8). No screen displays an API key, token, or credential.
- **No code execution from data.** Action POSTs map to the fixed DOC-5 verb set
  dispatched through the contract; the UI never composes a free-form command
  (DOC-2 §8 / DOC-9 security note).
- **Same-origin guard on mutating `/api/v1/*` endpoints (v1 requirement).** The
  controller daemon binds loopback only (no auth, parity with existing daemons),
  but mutating endpoints are stricter: the action API requires both an explicit
  `confirmed: true` flag on the POST body AND an Origin/same-site check that
  rejects cross-origin requests to loopback. This lightweight guard (no auth
  infrastructure, only a request header check) prevents malicious local pages from
  silently triggering system-wide stack mutations (upgrade, restart, ensure) via
  DNS rebinding or stale tabs — a known local-API class of issue. Included in v1,
  not deferred.

### 7. Contract-version degradation in the UI

The UI must reflect the contract negotiation DOC-1 owns and DOC-4/DOC-5 apply —
it renders the rollup's degradation states, it never invents its own.

- **Older-but-≥-floor contract** (`contract_version` in `[F, N)`, DOC-4 §5.2):
  the member's row/detail renders a **"contract too old — upgrade"** badge
  (degraded, amber), with the **upgrade** remediation the rollup already carries
  (DOC-4 §6 → DOC-9). It is never shown as broken; only the fields that level
  guarantees are rendered. The badge's action is the per-member Upgrade button
  (§3) — closing the DOC-9 loop ("out-of-date contract → run upgrade → cleared").
- **Below-floor / contract-incompatible** (`cv < F`, DOC-4 §5.2 / Resolved Q4):
  the row renders **`down`** with a distinct `reason: "contract_incompatible"`
  treatment and the upgrade remediation. The "Open <tool> UI" link still follows
  the §4 rule (shown only if the member is actually serving a UI).
- **Missing / down / unreachable** (DOC-4 §5.1): rendered per the three distinct
  sub-reasons the rollup carries — *missing* → Install action (DOC-8); *down* →
  Start action (DOC-1 lifecycle); *unreachable* → "investigate" hint. The UI maps
  each to the remediation the rollup already supplies.
- **Verb / scope not advertised** (DOC-4 §5.3): a member that does not advertise a
  verb (e.g. claude-mpm has no `config`/lifecycle; a system-only tool's project
  column) renders **`— n/a`**, never an error or a disabled-looking failure. The
  corresponding action button is simply absent for that member.

In all cases the UI is a faithful renderer of the DOC-4 `--json` rollup's
`reason`/`verdict`/`remediation` fields — the degradation *logic* lives in
DOC-1/DOC-4/DOC-5, never in the UI.

### 8. Out-of-the-box availability

The spec requires the UI to be available "once installed and running" (§50). DOC-7
ties UI availability to the controller's own lifecycle:

- **How it starts.** The UI is **embedded in the controller daemon** (the same
  axum server, §5), so it is available whenever the controller daemon is running —
  there is no separate UI process to start (mirroring the existing daemons, where
  "each member's UI is embedded in its own daemon," DOC-5 §7). The controller
  daemon is **supervised by launchd** (macOS) / systemd (Linux) as part of the
  install flow (DOC-8 §1.1 step 4), so the UI is always live and "available once
  installed" — no lazy startup needed. `tctl ui` opens the already-running URL
  in a browser (DOC-5 §1.1, §5 above).
- **How it stays in sync.** The dashboard **polls `tctl stack health --json`** on
  an interval for the at-a-glance matrix (cheap, fast — DOC-4 §4 fast sweep), and
  the long-running-op screens use the **SSE op stream** (§3.2) for live progress.
  After any action completes, the UI re-fetches the relevant `--json` rollup and
  re-renders — so the control plane always reflects live state without the user
  reloading. (Polling cadence is an Open Question, §4.)
- **Restart inclusion.** `tctl restart` bounces the controller's own UI service
  **last** (DOC-5 §7 / Resolved Q4), so a stack restart from the UI does not
  kill the page mid-sweep before other members are done; the UI surfaces a brief
  "controller restarting — reconnecting…" state and the SSE client reconnects
  (the `mcp_bridge`-style exponential-backoff reconnect convention, CLAUDE.md
  #534, applies to the UI's stream client too).

---

## Dependencies

### Consumes (inputs)
- **DOC-1** (Accepted) — the contract envelopes the UI displays: `version --json`
  (installed version + `contract_version`), `doctor.data.checks[]` (drill-down +
  remediation), `health` status, the read-only redacted `config` (D8), and the
  older-contract degrade rule (D2) that drives the "contract too old" badge (§7).
- **DOC-2** (Accepted) — the manifest registry the UI lists (members,
  `display_name`, `kind`, pinned `version`, `stack_version`) and the `ui`
  sub-table (`available` / `path = "/ui"` / `port_source = "port_json"`) that
  drives link-out URL discovery (§4); the embedded-into-`tctl` pattern reused for
  the UI bundle (§5); the changelog-headline format rendered in the updates view.
- **DOC-4** (Accepted) — the rollup the UI renders: the tools × scope matrix, the
  verdict vocabulary `ready|degraded|pending|down` (+ `n/a`), the `--json` rollup
  structure (§8.2) the UI consumes byte-for-byte, the drill-down checks +
  remediation, and the `clusters[]` dependency root-cause grouping (§2, §7).
- **DOC-5** (Accepted) — the UI's actions ARE these controller operations
  (`stack doctor`, `stack health`, `upgrade`, `restart`, `updates`, `ensure`,
  per-tool `doctor`/`config`); the controller HTTP API wraps the same DOC-5 §2
  dispatch engine (§3); the `tctl ui` / `tctl port` commands expose the UI URL;
  the blast-radius confirmation (DOC-5 §3.3) the UI renders as a confirm dialog;
  the `--json` envelope/rollup shapes the UI renders.

### Produces (consumed by)
- **Terminal** — nothing depends on the controller UI (per the README dependency
  graph: DOC-7 is terminal).

> These edges match the README dependency graph (DOC-7 consumes DOC-1 + DOC-2 +
> DOC-4 + DOC-5; terminal — nothing downstream depends on it).

## Grounding (exists vs. net-new)

Source-first audit, 2026-06-08 (trusty-search MCP search + Read against the tree).

| Area | Reality today | DOC-7 implication |
|---|---|---|
| **Embedded Svelte UI** | Verified `crates/trusty-search/src/service/ui.rs:39`: `include_dir!("$CARGO_MANIFEST_DIR/ui-dist")`; handlers `GET /ui` (`ui_index_handler`) + `GET /ui/*path` (`ui_asset_handler`) with SPA fallback; runtime config injected into `index.html` via `window.__DAEMON_PORT__` globals (`inject_runtime_config`). trusty-memory follows the same pattern. | §5: the controller reuses this **verbatim** — `include_dir!` + axum `/ui` + runtime-config injection of the controller's own port. Net-new = only the controller's Svelte sources + `ui-dist/`. |
| **build.rs UI build + SKIP_UI_BUILD** | Verified `crates/trusty-search/build.rs`: invokes `make release-prep`/pnpm, honours `SKIP_UI_BUILD=1`, falls back to a placeholder `index.html` so `include_dir!` always compiles. Root CLAUDE.md: `SKIP_UI_BUILD=1` for publish; cdhash/`cargo install`-not-`cp` caveat. | §5: the controller's `build.rs` mirrors this; the publish-flow `SKIP_UI_BUILD` + cdhash caveats apply unchanged. |
| **`monitor web` / dashboard link-out precedent** | Verified `crates/trusty-search/src/commands/dashboard.rs`: builds `http://<addr>/ui`, opens via `open::that`, **degrades to a printed URL** on browser-open failure. | §4: DOC-7's "Open <tool> UI" link is the in-browser analogue; `tctl ui` is the CLI analogue (DOC-5). |
| **UI URL discovery** | Verified: `<binary> port [--json]`, `port.lock` / `http_addr` file (`crates/trusty-search/src/commands/daemon_utils.rs`), default fallback. DOC-2 §3 `port_source = "port_json"`. | §4: link-out URL = `port --json` → `http://<addr><ui.path>`; never a pinned port. |
| **Loopback-only, no-auth posture** | Verified `trusty_common::bind_with_auto_port` binds `127.0.0.1`; trusty-search CLAUDE.md — "daemon binds loopback only … Authentication: none … trusts every caller"; permissive CORS for browser admin UIs. | §6: the controller UI + action API match exactly (loopback bind, no auth, permissive local CORS). |
| **SSE progress + replay buffer** | Verified: trusty-search `POST /indexes/:id/reindex` → `stream_url`; `GET …/reindex/stream` emits `start`/`progress`/`complete`/`error` with a **500-event replay buffer**; typed client `trusty_common::monitor::search_client::ReindexEvent`. | §3.2: the controller op-progress stream reuses this exact SSE+replay shape — no new protocol. |
| **The controller UI + its action API** | **Net-new.** No controller crate, no `/api/v1/*` action API wrapping DOC-5 dispatch, no controller Svelte UI exists today. | This document — mostly **assembly**: wiring established patterns (embedded Svelte, axum `/ui`, SSE+replay, `port --json` discovery, loopback/no-auth) into a new control-plane UI over the DOC-4 rollup + DOC-5 dispatch. |

**Net-new (the parts DOC-7 actually adds):** (1) the controller's own Svelte SPA
(four control-plane screens) + committed `ui-dist/`; (2) the controller HTTP
`/api/v1/*` action API that wraps the DOC-5 §2 dispatch engine so UI and CLI
share one code path; (3) the controller's own SSE op-progress stream (reusing the
existing replay-buffer shape). Everything else — embedding, axum `/ui` serving,
runtime-config injection, `port --json` discovery, loopback/no-auth posture,
build.rs + `SKIP_UI_BUILD`, the cdhash publish caveat — is **strongly reusable**
and already in the tree.

## Cross-cutting notes

- **Security / secrets:** loopback-only + no-auth parity with the daemons (§6);
  the action API binds the same loopback interface and never widens the surface;
  no secrets rendered (DOC-1 D8 redaction carries through verbatim; manifest
  carries none, DOC-2 §8).
- **Contract-versioning behavior:** the UI renders the DOC-1/DOC-4 degradation
  states (§7) — "contract too old" badge for older-but-≥-floor members, `down` +
  `contract_incompatible` for below-floor, `n/a` for unadvertised verbs/scopes —
  always as a faithful render of the rollup `--json`, never re-derived.
- **Zero tool-specific logic (spec §83):** the UI holds no per-tool view code; it
  renders the generic DOC-4 rollup + DOC-2 manifest and link-out URLs discovered
  via `port --json`. Swapping claude-mpm → trusty-mpm (DOC-2 §7 / DOC-6 §6) needs
  no UI change — the new member simply appears in the matrix (and gains an "Open
  UI" link iff it advertises `ui.available`).
- **The link-out hard rule (spec §56) is the spine of this doc:** render the
  control plane, link out for the data plane (§1). It is what keeps the controller
  a thin coordinator rather than a competing UI.

## Remaining work

- [x] Define the render-vs-link-out boundary + the concrete render-vs-link table (§1)
- [x] Enumerate the four screens + map each to its controller-owned `--json` source (§2)
- [x] Define the action mechanism (UI → controller HTTP API → DOC-5 dispatch),
      the action→command map, blast-radius confirm dialog, and SSE op progress (§3)
- [x] Define UI URL discovery + link-out mechanics (manifest `ui` + `port --json`;
      down-tool disabled link; no-UI member like claude-mpm) (§4)
- [x] Recommend tech & serving (embedded Svelte + `ui-dist/` + `build.rs` +
      `include_dir!`, axum `/ui`, `SKIP_UI_BUILD`, cdhash caveat, `tctl ui`) (§5)
- [x] Define the security posture (loopback-only, no-auth parity, no secrets) (§6)
- [x] Define contract-version degradation rendering ("contract too old" badge,
      below-floor `down`, `n/a` for unadvertised) (§7)
- [x] Define out-of-the-box availability + stay-in-sync (embedded in the daemon;
      poll rollup / SSE; restart-last) (§8)
- [x] **Owner: resolve all 6 open questions** → **Resolved Decisions (all owner-approved)**
- [ ] Team review → Ready
- [ ] *(implementation-time)* build `crates/trusty-controller/src/` UI server
      (axum `/ui` + `/api/v1/*`), the Svelte sources + `ui-dist/`, and the SSE op
      stream, reusing `trusty_common` (`bind_with_auto_port`, the SSE/replay
      client) and the DOC-5 dispatch engine

---

## Resolved Decisions

1. **Out-of-the-box start model — launchd-supervised daemon (§8).** (Owner-approved)
   The controller is **installed and supervised by launchd (macOS) / systemd
   (Linux)** as a system member during `tctl install` (DOC-8 §1.1 step 4), so the
   UI daemon is always resident and available once the system install completes —
   true "available once installed" parity with search/memory daemons. `tctl ui`
   opens the already-live URL. DOC-8 owns the controller's own service-install
   (in addition to installing the other members). Locked for v1.

2. **Confirmation parity for UI system ops — always-confirm (§3.1).** (Owner-approved)
   The UI **always shows the confirm dialog** for system-mutating operations
   (upgrade / restart / start / stop). There is no "don't ask again" toggle in v1.
   The action API requires an explicit `confirmed: true` flag on mutating POSTs so
   a stray request (CSRF, old state, user error) cannot silently trigger a system
   op. Locked for v1.

3. **CSRF / same-origin guard on mutating `/api/v1/*` endpoints — included in v1 (§6).** (Owner-approved)
   The action API **rejects cross-origin POSTs** via an Origin/same-site header
   check on all system-mutating endpoints (upgrade, restart, ensure, start/stop),
   combined with the mandatory `confirmed: true` flag in the request body. This
   lightweight guard (no auth infrastructure, only a request header check) prevents
   malicious local web pages from silently triggering system-wide stack mutations
   via DNS rebinding or stale tabs — a known local-API threat. No-auth parity with
   existing daemons (loopback-only bind), but stricter guards for endpoints that
   *mutate the whole machine*. Included in v1, not deferred. Locked for v1.

4. **Rollup polling cadence + back-off (§8).** (Owner-approved)
   The dashboard polls `tctl stack health --json` (fast sweep, DOC-4 §4) every ~10 s
   while the browser tab is focused, backs off to a longer interval when the tab is
   hidden (`visibilitychange` event), and relies on the SSE op stream (§3.2) for
   live per-member progress during long-running actions (upgrade, restart, ensure).
   No polling is needed during an in-flight operation. A future push channel (SSE
   "stack changed" events) is noted for post-v1. Locked for v1.

5. **Stack-doctor cost — explicit-only (`run full doctor` button).** (Owner-approved)
   The dashboard auto-runs the *fast* `tctl stack health --json` on page load for
   the at-a-glance matrix (sub-second per member). The deep `tctl stack doctor --json`
   (10 s/tool timeouts, DOC-4 §1.3) is triggered explicitly by a "Run full doctor"
   button on the dashboard + the dedicated stack-doctor screen (§2.4), avoiding a
   heavy multi-tool sweep on every page open. Locked for v1.

6. **Controller's own row and self-link suppression (§1.1 & §2.1).** (Owner-approved)
   The controller's member row is rendered in the matrix with real health/version
   data (important for users to see the controller itself is ready). However, the
   **self "Open trusty-controller UI" link is suppressed** since the user is
   already in it — no redundant self-referential link. Locked for v1.
