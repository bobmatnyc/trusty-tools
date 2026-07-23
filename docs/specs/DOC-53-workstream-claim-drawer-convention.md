---
spec_refs:
  - id: SPEC-SHAREDWS-01~draft
    path: docs/specs/DOC-52-shared-workstream-definition.md
    anchor: SPEC-SHAREDWS-01~draft
  - id: SPEC-SLD-01~draft
    path: docs/specs/spec-linked-documentation.md
    anchor: SPEC-SLD-01~draft
---

# DOC-53 — Workstream Claim-Drawer Convention: Cross-Workstream Coordination via trusty-memory

**Status:** Draft (Rev 2 — see revision note below)
**Subsystem:** trusty-memory — attribution / drawer conventions; trusty-mpm — PM dispatch protocol
**Owner:** Engineering (trusty-mpm, trusty-memory)
**Last-updated:** 2026-07-23
**Spec ID:** `SPEC-WSCLAIM-01~draft` … `SPEC-WSCLAIM-04~draft` (DOC-53)

**Revision note (Rev 2, code-critic BLOCK round, same day):** Rev 1's §4.3 described identity resolution as happening entirely inside the shared `trusty-memory` daemon's own dispatch handlers (`CreatorInfo::new_self`, reading the daemon PROCESS' own env/cwd). Review correctly identified this as a critical daemon-vs-caller mis-attribution bug: the daemon is ONE process serving every concurrently-attached session, so resolving "this process' identity" there stamps the SAME value onto every caller's writes. Rev 2 corrects §4.3 to the caller-supplied architecture actually implemented: the MCP stdio bridge (a genuinely per-session process) resolves its own identity ONCE at startup and injects it into each forwarded request's tool arguments; the daemon's dispatch handlers (`CreatorInfo::new_for_caller`) trust ONLY that per-request value, never their own environment. Rev 2 also corrects §4.3's characterization of UUID-named worktrees (was: "ephemeral/anonymous scratch worktrees" — wrong for this repo, where UUID-named worktrees are the majority, real, tracked sessions) and adds §4.4 declaring the canonical workstream-identity source explicitly.
**Builds on:**
- [`docs/specs/DOC-52-shared-workstream-definition.md`](./DOC-52-shared-workstream-definition.md) (`SPEC-SHAREDWS-01~draft`) — the 1:1 session↔workstream binding this spec's `ws:<name>` identity is drawn from.
- [`docs/specs/spec-linked-documentation.md`](./spec-linked-documentation.md) (DOC-38) — the numbering/anchor/frontmatter conventions this document follows.
- Issue #3168 (BUS-7) — the event bus, the real-time channel this spec explicitly does **not** duplicate.
- Epic #3524 slices 5+6 — the motivating incident (two sessions independently picked up the same `/health` work; see §1).

---

## 1. Motivation

**Bob's decision (2026-07-23):** enable lightweight cross-workstream awareness through trusty-memory, without building a second locking or messaging system on top of it.

**Incident:** epic #3524 slices 5 and 6 ran in two separate `tm` PM sessions. Both sessions independently picked up `/health` work because neither had any way to see that the other was already there — GitHub state (branch/PR) only becomes visible once a session has pushed, and by then both had already started. The result was duplicated implementation effort that had to be reconciled after the fact.

**Why memory, and why not a lock or a message bus:**
- A **lock** would require a coordinator with exclusivity semantics (who grants it? what happens on a crashed holder? does it survive a `tm restart`?) — heavyweight machinery for a problem that is really "reduce the odds of collision," not "guarantee mutual exclusion." Git/GitHub branch and PR state is *already* the authoritative lock: a pushed branch or an open PR against a file is a fact, checkable at any time, requiring no new subsystem.
- A **message bus** already exists and is being built out (#3168 BUS-7) for exactly the live, ordered, "tell my siblings something right now" case. trusty-memory has no ordering guarantees, no delivery guarantees, and no real-time push — using it as a channel would silently produce a *worse* message bus. Palace messaging (the prior ad-hoc attempt at this) is deprecated for the same reason (see `single-event-bus-messaging-decision` memory record, 2026-07-21).
- **Memory's actual strength** is durable, queryable, low-friction recall that's already wired into every PM session's `UserPromptSubmit` hook and MCP tool surface. It is well-suited to one narrow job: let a PM *check before it acts* — "has anyone told memory they're working here?" — as one more signal alongside GitHub state, cheap enough to check on every multi-agent dispatch.

## 2. The Three-Layer Coordination Model {#SPEC-WSCLAIM-01~draft}

**ID:** SPEC-WSCLAIM-01~draft
**Status:** Draft

| Layer | Question it answers | Authority | Real-time? |
|---|---|---|---|
| **git / GitHub** (branch, PR, issue, label state) | "Is this area *actually* claimed or already landed, right now?" | **Authoritative** — the only layer that can be trusted without cross-checking | No (poll-based) |
| **trusty-memory** (claim drawers, this spec) | "Did a sibling session *say* it was starting work here, as of when it wrote that?" | **Advisory** — a hint, never ground truth | No (point-in-time write, hook-recalled) |
| **event bus** (#3168 BUS-7) | "What is happening across sessions *right now*?" | Authoritative for live events it carries | Yes |

**Normative rules:**

1. A claim drawer (§3) is **never** sufficient reason to skip a git/GitHub check. A reader that finds a claim MUST verify it against live git/GitHub state before treating it as still valid (§3.3 — stale-claim semantics).
2. trusty-memory MUST NOT grow request/response or push-delivery semantics to serve this convention. If a use case needs "tell a specific sibling something now," that need is out of scope for this spec and belongs on the event bus (#3168 BUS-7), not on a new memory messaging primitive.
3. Nothing in this spec introduces a new drawer type, storage layer, or API. Claim drawers are ordinary drawers distinguished purely by tag convention (§3.1) — any existing `memory_note`/`memory_remember`/`kg_assert` MCP tool can write and query them.

## 3. The Claim-Drawer Convention {#SPEC-WSCLAIM-02~draft}

**ID:** SPEC-WSCLAIM-02~draft
**Status:** Draft

### 3.1 Drawer shape

| Field | Value |
|---|---|
| Title (first line of content) | `WS-CLAIM <workstream>: <area>` — e.g. `WS-CLAIM tm-search-eviction-01: trusty-search idle-eviction sweep` |
| Tags | `ws-claim` (fixed marker tag), `ws:<name>` (the claiming workstream — §4.2), one or more `area:<slug>` tags (e.g. `area:health-endpoint`, `area:session_manager-rename`) |
| Body | Scope (what files/subsystem this covers, in plain language a sibling PM would type into a prompt — recall is `q=`-based semantic/BM25 search over content, not a tag filter, so the body's wording matters, not just its tags), branch name, PR/issue refs, expected-land condition (e.g. "supersede on PR #NNNN merge") |
| Room / palace | The project's own palace (cwd-resolved, same as every other write from that session) |
| Importance | Normal — claim drawers are operational hints, not durable facts; they should not out-compete genuinely important memories in recall ranking |

A companion KG triple is asserted via `kg_assert`: `ws:<name> owns area:<slug>`, so the same claim is queryable in triple form (`kg_query`, `search_kg`) independent of drawer recall.

**Dedup with the auto-stamp (normative).** A claim drawer's own `tags` already includes `ws:<name>` by the shape above — the auto-stamp described in §4 (`CreatorInfo::merge_into_deduped`, `crates/trusty-memory/src/tools/helpers.rs::attach_mcp_attribution`) MUST NOT append a second, duplicate copy of a tag the caller already supplied verbatim. A claim-drawer write therefore ends up with exactly one `ws:<name>` tag, not two, regardless of whether the PM hand-wrote it or the daemon derived the identical value from the same request's caller-supplied context.

### 3.2 Lifecycle

1. **Written at dispatch** — before a PM session hands a multi-agent task to an area, it writes the claim drawer (and the `owns` triple).
2. **Superseded at land** — once the branch merges (or the PR closes), the session that wrote the claim writes a new drawer tagged `ws-claim-closed` (or calls `memory_forget` on the original) and MAY retract the `owns` triple. A claim is not required to persist past the work it describes.
3. **Superseded at abandon** — if the session abandons the area (context switch, reassignment, session pause without resume), the same closure step applies. An abandoned claim that is never closed is handled by §3.3, not by a cleanup job — this spec does not add a reaper.

### 3.3 Stale-claim semantics (normative)

Memories are point-in-time. A claim drawer states "as of the write, this workstream intended to work here" — it says nothing about whether that is still true. **A reader MUST treat a claim as void, not binding, once its referenced branch/PR no longer exists** (deleted branch, closed-without-merge PR, or an issue closed as won't-do). Concretely: before a PM changes its dispatch plan because of a claim hit, it checks the claim's stated branch/PR against live `git`/`gh` state (`git ls-remote`, `gh pr view`). A live claim narrows the dispatch (coordinate, or pick a different area); a dead claim is ignored and, ideally, cleared by whichever session notices it (best-effort — no reader is obligated to clean up another session's stale drawer).

This mirrors the general memory posture already established for the codebase: point-in-time records go stale and must be checked against ground truth before being acted on (see the `agent-noop-return-not-dead` and `duplicate-agent-worktree-races-20260719` memory records for the same principle applied to agent liveness and worktree state).

## 4. Workstream-Attributed Memory {#SPEC-WSCLAIM-03~draft}

**ID:** SPEC-WSCLAIM-03~draft
**Status:** Draft

### 4.1 Extends the existing `creator:*` namespace

`crates/trusty-memory/src/attribution.rs` already stamps every drawer write with a reserved `creator:*` tag namespace — `creator:client=`, `creator:version=`, `creator:source=`, `creator:cwd=` (and `creator:session=` when a session UUID is present in caller-supplied tags). This spec adds one more reserved tag to the same namespace:

- `creator:workstream=<name>` — the originating session's workstream name, when resolvable (§4.3).

And one bare, non-namespaced tag for ergonomic filtering (parallels the just-approved GitHub `ws/<name>` label format, using `:` rather than `/` to match trusty-memory's existing bare-tag convention, e.g. `msg:`):

- `ws:<name>` — same name, rendered only alongside `creator:workstream=`, never alone.

Both are appended by `CreatorInfo::into_tags()` in the same stable-order, omit-when-absent style as the four existing tags — **no placeholder value is ever emitted**; when the workstream name cannot be resolved (§4.3), both tags are simply absent from the drawer, exactly as `creator:cwd=` is already omitted when the writing process has no resolvable cwd.

### 4.2 Why a bare `ws:` tag in addition to `creator:workstream=`

The `creator:*` namespace is deliberately hidden from the primary UI tag chips (`is_creator_tag`) so operators aren't shown attribution noise alongside meaningful tags — but that means `creator:workstream=` alone is invisible to `memory_list`'s exact-tag filter unless the caller already knows the reserved-prefix form. `ws:<name>` is a first-class, visible tag specifically so `memory_list(tag: "ws:<name>")` and the claim-drawer convention's own `ws:<name>` tag (§3.1) compose naturally — a claim drawer and an attribution-stamped drawer from the same workstream carry the identical `ws:<name>` tag, so "everything workstream X touched" is one filter regardless of whether the drawer was hand-written (a claim) or auto-stamped (any other write).

### 4.3 Identity resolution: caller-supplied only, never the shared daemon's own identity (normative)

**The hazard this section exists to prevent.** `trusty-memory` runs its MCP/HTTP tool surface inside ONE shared daemon process serving every concurrently-attached session (every `tm`-managed PM session's `serve --stdio` bridge talks to the same daemon over `POST /rpc`). A dispatch handler that resolves "my own" identity via `std::env::current_dir()`/an env var reads the **daemon's** cwd/environment — identical for every caller — which would stamp the SAME `ws:<name>` onto every session's writes regardless of which session actually wrote them. That is a WORSE bug than having no attribution at all: it looks correct (a plausible-looking tag is present) while being silently wrong, and a PM trusting it to avoid a collision (§3.3) could be misled into believing a claim belongs to a different workstream than it does. **The daemon-side dispatch handlers therefore never call `std::env::current_dir()`/read an env var for this purpose — identity is caller-supplied, full stop, or absent.**

**Two-sided architecture:**

1. **The MCP stdio bridge** (`crates/trusty-memory/src/commands/serve_stdio_bridge.rs::run_stdio_bridge`) is, unlike the daemon it proxies to, a genuinely fresh process spawned per `serve --stdio` invocation — it correctly runs inside its caller's own process tree/environment. It resolves its own identity ONCE at startup:
   - `TM_WORKSTREAM_NAME` environment variable, if set — forward-compatible: `tm` does not export this today (the only session-identity env var it currently exports is `TM_MANAGED_SESSION_ID`, a UUID, set via `ManagedTmuxDriver::set_environment` in `crates/trusty-mpm/src/runtime/claude_code.rs`/`session_manager/manager.rs`); the day it starts exporting a human-readable name under this key, the bridge picks it up with zero code change.
   - Else, the bridge's own cwd: when it contains a `.worktrees/<segment>` path component, `<segment>` is a workstream-name candidate.
   - Every candidate — from either source — passes a validation gate: non-empty, at most 64 characters, matching `^[A-Za-z0-9][A-Za-z0-9_.-]*$`, and NOT UUID-shaped (the canonical 8-4-4-4-12 hyphenated form). A candidate that fails validation resolves to nothing — §4.1's omit-cleanly rule, not a sanitized substitute.
   - The resolved `workstream` (and the bridge's own `cwd`) are then injected into every forwarded request's tool `arguments` — mirroring the existing `--palace` default-injection mechanism (`inject_default_palace`) — UNLESS the caller already supplied its own value, which always wins.
2. **The daemon's dispatch handlers** (`tools::helpers::attach_mcp_attribution`, `CreatorInfo::new_for_caller`; the HTTP path via `X-Trusty-Client-Workstream`/`X-Trusty-Client-Cwd` headers, `web::rpc::creator_info_from_http`) read ONLY `args["workstream"]`/`args["cwd"]` (or the HTTP headers) — mirroring the pre-existing `args["cwd"]` precedent in `tools::palace_ops::handle_palace_create`. An explicit, valid `workstream` wins; else it's derived from `cwd` via the same validated `.worktrees/<name>` heuristic; else both tags are simply absent. Neither field EVER falls back to the daemon's own `std::env::current_dir()`/env.

**Known coverage gap (honest, not silently assumed away):** because resolution (on both sides) still depends on the `.worktrees/<name>` cwd heuristic whenever `TM_WORKSTREAM_NAME` is unset (true today, for every session), a session running in a **UUID-named** worktree directory gets NO `creator:workstream=`/`ws:` attribution — not "correctly excluded as anonymous/ephemeral" (Rev 1's framing, which was wrong: UUID-named worktrees are the **majority** shape in this repository today, and are real, tracked sessions, not scratch space). This is a genuine coverage gap, not a feature. It resolves itself automatically once `tm` exports a real session-name identity that the bridge picks up via `TM_WORKSTREAM_NAME` (§4.4) — at that point every session gets correct attribution regardless of its worktree directory's name, because resolution stops depending on directory naming at all. Until then: UUID-worktree sessions' writes carry no `ws:`/`creator:workstream=` tags, and a PM should not assume a claim-drawer omission means "not a real session" — it may just mean the writer hasn't gotten a named-worktree identity yet.

### 4.4 Canonical workstream-identity source (declared, cross-feature alignment)

DOC-53's `ws:<name>` is designed to parallel the just-approved GitHub `ws/<name>` label convention (`feat-ws-labels`, in flight alongside this spec) — both exist so "everything workstream X touched" is one filter, whether that's a GitHub search or a `memory_list(tag: "ws:X")` call. For that parallel to actually hold, `<name>` must be the SAME value in both places.

**Declared canonical source: the `tm`-assigned session name** (the tmux session name, `SessionRecord`'s `tmux_name` field in `crates/trusty-mpm/src/session_manager/`) — the same value `feat-ws-labels` derives its `ws/<name>` GitHub label from. This is the right choice because DOC-52 (`SPEC-SHAREDWS-01~draft`) establishes a 1:1 session↔workstream binding: the session name IS the workstream name, unambiguously, for the lifetime of the binding.

**Current implementation status vs. that declaration:** `tm` does not yet export the session name as an environment variable to a managed session (§4.3) — only the UUID `TM_MANAGED_SESSION_ID`. This spec's `.worktrees/<name>` cwd heuristic is therefore an explicitly-acknowledged **best-effort stand-in**, not the canonical source itself, and the two can diverge in real, enumerable ways: a session renamed after its worktree was created (`tm rename`, live per PR #97c5d6f7-style auto-suffixing), a session adopted into an existing differently-named worktree, or (§4.3's coverage gap) any UUID-named worktree. `feat-ws-labels`' GitHub label — driven directly from the live session name — does not share these failure modes.

**Consequence for callers:** treat `ws:<name>` tags written before `tm` exports the canonical session name as **advisory and possibly stale relative to the session's current name** — exactly the same "verify before trusting" posture §3.3 already requires for claim drawers generally, just for a second, narrower reason (name drift, not branch/PR staleness). The two features are NOT guaranteed to agree today; the follow-up that makes them agree (`tm` exporting its session name via `TM_WORKSTREAM_NAME` or equivalent) is out of scope for this PR and is the natural next increment once filed.

## 5. Non-Goals / Explicit Boundaries {#SPEC-WSCLAIM-04~draft}

**ID:** SPEC-WSCLAIM-04~draft
**Status:** Draft

- **Not a lock.** No claim drawer ever blocks a write, a dispatch, or a merge. Enforcement, if any is ever wanted, belongs on git/GitHub (branch protection, required reviews) — not on trusty-memory.
- **Not a message channel.** No new "notify workstream X" primitive is introduced. Cross-session real-time coordination is #3168 BUS-7's job; palace messaging remains deprecated (see `slack-phase2-hardblock-cancelled` / `single-event-bus-messaging-decision` memory records) and this spec does not resurrect it under a new name.
- **Not a new storage/API surface.** No new drawer type, no new MCP tool, no new HTTP route. The entire convention is: two new tags on the existing `CreatorInfo` (§4), a tagging/body convention documented for humans and PM instructions (§3), and a PM-instruction habit (recall before dispatch, write on dispatch, supersede on land — `crates/trusty-mpm/src/assets/instructions/PM_INSTRUCTIONS.md`).
- **Not authoritative.** Every claim is stale until checked (§3.3). A PM that skips the git/GitHub verification step and dispatches (or refrains from dispatching) purely on a claim-drawer hit is using this convention wrong.
