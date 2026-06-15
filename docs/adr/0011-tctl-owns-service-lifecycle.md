# 0011. `tctl` owns service lifecycle (headless control plane); `trusty-console` owns the single HTTP surface

- **Status:** Proposed
- **Date:** 2026-06-15
- **Scope:** Workspace-wide (trusty-controller boot model; reconciles DOC-7 with
  the #1104 console architecture). Consumed by
  `docs/trusty-controller/research/02-design/DOC-12-bootstrap-orchestration.md`
  and affects DOC-7's status.
- **Supersedes / Superseded by:** Supersedes the DOC-7 "controller-embedded web
  UI" *surface* decision (DOC-7 content relocates to trusty-console; DOC-7 to be
  re-statused Superseded).

## Context

Two owner-directed positions must be reconciled:

1. **DOC-7 (Accepted)** specified that `trusty-controller` ships its **own
   embedded web UI** — a loopback-only Svelte SPA served by the controller
   process — as the "out-of-the-box stack control plane" the spec asks for
   (§44–§56), with an embedded `ui-dist/` bundle and a `SKIP_UI_BUILD=1` publish
   path (DOC-0 A5).

2. **#1104 (CLOSED epic) + #1222 (open)** established a later, owner-directed
   architecture principle: **HTTP is implemented exactly once — in
   `trusty-console`.** Local services stay JSON-RPC-over-stdio (MCP); the console
   is the only HTTP surface and a stdio-MCP *client* that renders each service's
   metrics natively (no per-service HTTP, no iframes, no service-rendered HTML).

These conflict: DOC-7 adds a second HTTP UI inside the controller, which #1104
forbids. The 2026-06-15 owner directive (DOC-12) additionally makes `tctl` the
**first-loaded control plane** that boots the whole stack — sharpening the need
to define what `tctl` *is* (a lifecycle/control plane) versus what the console
*is* (the HTTP/UI front door).

The decision is architecturally significant and costly to reverse (it determines
whether the controller crate ships a web bundle, how the stack dashboard is
served, and what `tctl ui`/`tctl port` mean), so it is recorded as an ADR per the
repo's ADR bar.

## Decision

We will make **`trusty-controller` (`tctl`) a headless control plane** and
**`trusty-console` the single HTTP/UI surface** for the stack:

1. **`tctl` owns service lifecycle, headlessly.** Boot orchestration (`tctl up`,
   DOC-12), install/upgrade/restart, the doctor/health rollup, ensure-project,
   and the manifest/contract dispatch engine are CLI + a supervised control
   process. `tctl` serves **no HTTP** and ships **no web UI bundle**.

2. **`tctl` exposes its stack rollup as data, not a page.** The DOC-4 rollup is
   emitted as `--json` and via a stdio-MCP `console_metrics`-style tool (the
   #1104 shared "console metrics" contract). The controller is a metrics
   *producer*; it does not render HTML.

3. **`trusty-console` renders the controller/stack view.** The control-plane UI
   DOC-7 described (installed tools + versions, upgrade indicators, health,
   doctor results, link-out to each tool's own view) **relocates into a
   trusty-console tab**, fed by the controller's `--json`/MCP rollup — consistent
   with #1104 (console = only HTTP, native render, stdio-MCP client).

4. **DOC-7 is superseded in surface.** The "controller-embedded web UI" surface
   decision in DOC-7 is withdrawn; DOC-7's *content/semantics* (link-out control
   plane, upgrade indicators, doctor rollup) survive inside the console. The
   controller crate drops its Svelte/`ui-dist/`/`SKIP_UI_BUILD` surface and the
   loopback-auth/DNS-rebind concerns that came with serving its own HTTP.

5. **Boot relationship.** `tctl up` **starts** trusty-console as the
   `boot_stage = "console"` always-on member (DOC-12 §3 STAGE 3); the console
   depends on the controller having ensured the core, not vice versa. `tctl ui`
   prints/opens **the trusty-console URL** (discovered via the console's
   `port --json`), not a controller-served page.

6. **The `kind = "controller"` process stays, HTTP-less.** `tctl` still needs a
   small supervised process (launchd/systemd) to trigger boot and hold the
   system advisory lock; under this ADR that process exposes its state via
   stdio-MCP to the console, never via its own HTTP port.

## Consequences

**Easier / positive:**
- One HTTP surface for the whole stack (the #1104 invariant holds with no
  exception for the controller); one place for auth/DNS-rebind/CORS concerns.
- The controller crate's publish simplifies — no embedded UI build, no
  `SKIP_UI_BUILD` dance, no committed `ui-dist/` (DOC-0 A5 simplifies).
- `tctl` stays a clean Unix-philosophy CLI (stdout=data/JSON), which composes
  with the console and with scripts/CI identically.
- The stack dashboard becomes one more console tab fed by a uniform metrics
  contract, so new control-plane views are console UI work, not controller HTTP.

**Harder / negative / trade-offs:**
- The stack dashboard now **requires trusty-console running** to be visible in a
  browser; there is no controller-only fallback page. Mitigation: `tctl stack
  doctor` / `tctl status` give the same information on the CLI headlessly, and
  DOC-12 §5 starts the console even on partial-core for observability.
- DOC-7 must be re-statused and partially rewritten (content → console). This is
  documentation follow-up, not code already shipped (the controller crate does
  not yet exist — DOC-0).
- A hard dependency edge is added: the controller's stack view is only as
  available as the console; a console outage hides the GUI dashboard (but not the
  CLI control plane).

**Follow-up work:**
- Re-status DOC-7 → **Superseded** (by this ADR / #1104) and move its link-out /
  upgrade-indicator / doctor-rollup content into a trusty-console controller tab.
- Define the controller's `console_metrics`/stack-rollup stdio-MCP tool in the
  shared #1104 contract (`trusty-common`).
- Update DOC-5 `tctl ui`/`tctl port` semantics (redirect to the console URL).
- Confirm DOC-12 Open Questions Q4/Q5 with the owner.
