# 0040. `trusty-mcp` extracted from `trusty-common`; `trusty-mcp-services` (gworkspace only) depends on it — ADR-0033 was wrong about two of its three subjects

- **Status:** Accepted
- **Date:** 2026-08-10
- **Scope:** Workspace-wide — `trusty-common`'s `mcp` module and its verified
  consumers (`trusty-agents`, `trusty-analyze`, `trusty-channels`,
  `trusty-code`, `trusty-gworkspace`, `trusty-kb`, `trusty-memory`,
  `trusty-mpm`, `trusty-review`, `trusty-search`); `trusty-gworkspace` and
  its call sites (`tc-services`, `trusty-agents`); the installer's
  `stable_set`/picker; `.mcp.json`. **Explicitly out of scope:**
  `trusty-channels` (not MCP at all — see Error, below) and `trusty-kb`'s
  disposition (owner's separate, unmade call).
- **Reversibility Cost:** Two clocks. `trusty-gworkspace` absorption — Low
  today, High after this week's public release (never published a live
  version; `0.1.0` is yanked). Extracting `trusty-mcp` out of `trusty-common`
  — a live clock: `trusty-common` is a real published crate
  (`crates/trusty-common/Cargo.toml:3`: `version = "0.30.0"`;
  `docs/reference/semver-gate.md:163` shows a real prior bump,
  `v0.28.1 -> v0.29.0`). Removing `mcp`'s public API is a breaking change to
  a version already on crates.io — Medium: a real MINOR bump (`0.30.0` ->
  `0.31.0`, this workspace's own 0.x convention) plus import-path migration
  across every verified consumer. Affordable now because the workspace
  still controls every consumer directly, pre-public-release.
- **Decision Drivers:** owner ruling 2026-08-10 (layering: extract
  `trusty-mcp` from `trusty-common`; `trusty-channels` exists *to be* a
  native communication model, not MCP — ADR-0033 was a mistake about it;
  `trusty-kb`'s native-vs-MCP call is separate and unmade); "common is
  already full" (verified: 357 source files, ~134.5k lines, 63 Cargo
  features); a verified 10-crate `trusty_common::mcp` dependency surface
  (not six); epic [#3633](https://github.com/bobmatnyc/trusty-tools/issues/3633);
  [#5331](https://github.com/bobmatnyc/trusty-tools/issues/5331);
  [#4644](https://github.com/bobmatnyc/trusty-tools/issues/4644); the
  gworkspace free-rename window closing at public release
- **Supersedes / Superseded by:** **Supersedes ADR-0033** — a supersession
  that corrects an error, not a passive overtaking-by-circumstances. See
  "ADR-0033's disposition" below for why this status, and not a softer one,
  is the honest one.

## Context

### The dependency claim, verified against the manifests

The owner named six crates depending on the whole of `trusty-common` to
reach MCP. `grep -rl "trusty_common::mcp\b" --include=*.rs crates/` finds
**ten**: `trusty-agents`, `trusty-analyze`, `trusty-channels`, `trusty-code`,
`trusty-gworkspace`, `trusty-kb`, `trusty-memory`, `trusty-mpm`,
`trusty-review`, `trusty-search`. The owner's six are a subset — verified
count is higher, strengthening "common is already full" rather than
weakening it. Two crates — `trusty-gworkspace`
(`crates/trusty-gworkspace/Cargo.toml:18`) and `trusty-kb`
(`crates/trusty-kb/Cargo.toml`) — declare `mcp` as their *only*
`trusty-common` feature: for them, a 357-file, 63-feature crate is pulled in
for JSON-RPC primitives alone. `trusty-common/Cargo.toml:292-295` still
documents `mcp` as "formerly the `trusty-mcp-core` crate" — the absorption
this ADR reverses.

### This reverses `trusty-mcp-core`'s absorption — stated plainly, and why reversing is now correct

Commit `a90ba071` folded standalone `trusty-mcp-core` into `trusty-common`,
trading one fewer crate for one shared home. That trade has come due: at
357 files / 63 features, every MCP-only consumer pays the whole crate's
surface for ~750 lines of primitives — worse now that the consumer set
includes heavy, OAuth-bearing service crates. Extracting `trusty-mcp`
restores what `trusty-mcp-core` was before absorption. Not a change of
heart; the absorption's cost outgrew its benefit.

### The decision rule: who is the consumer — stated once, generally, as the normative criterion

Owner, verbatim: **"MCP is preferable when the consumer is the agent. In the
case of channels, the consumer is the FRAMEWORK, deterministic and
invisible to the agents, but usable by them."**

This is the durable finding in this ADR — a rule that predicts future
cases, not a justification manufactured for the two calls at hand:

- **Consumer is the agent -> MCP.** The model decides *when* to invoke, so
  it needs discoverable tools with descriptions and schemas — MCP's whole
  shape (tool listing, model-initiated calls, serialized request/response)
  exists to serve exactly that. `trusty-gworkspace` is this: an agent
  decides, at its own discretion, to read a calendar.
- **Consumer is the framework -> native.** The framework invokes it
  deterministically, in code, on its own schedule — invisible to agents as
  a tool surface while remaining usable by them through framework
  affordances. Routing deterministic framework code through a
  model-facing protocol buys nothing and costs a process boundary.
  `trusty-channels` is this.
- **Two arguments this ADR does NOT lean on, checked and found unsupported:**
  performance, and a unified event bus. Neither is evidence for anything
  here, and citing them would be citing something that doesn't exist. On the
  bus: five independent, per-harness `tokio::sync::broadcast` instances exist
  today (`trusty-agents/src/events.rs`, `trusty-agents/src/bus/mod.rs`,
  `trusty-agents-common/src/events/bus.rs`, `trusty-code/src/events.rs`,
  `trusty-mpm/src/daemon/state/resources.rs`) — there is no unified
  cross-harness bus to integrate with. **ADR-0019 (Accepted, 2026-07-21)
  already decided one should exist and records it as unimplemented**; the
  epic consolidating that work (#3052) shows no completed sub-issue evidence
  of it landing. ADR-0019 is cited here as the unbuilt prerequisite it is,
  not as support for anything in this ADR. On performance: `trusty-channels`
  has never run in production and carries no instrumentation — there is no
  measurement to cite. The nearest measured analog in this workspace
  (ADR-0034, issue #5028) found zero webhook deliveries in 14 days, which is
  evidence about webhooks, not about channel-message volume. Dropped.
- **A capability can be both** — and the verification below shows this is
  not hypothetical for channels specifically: an inbound/dispatch path and
  an agent-initiated outbound send action are different capabilities that
  can coexist under one domain name without contradiction.

**This should be a normative rule, not only a decision record — see "Where
this rule lives" below**, which reports the `docs/specs/` search this ADR's
Related Decisions depends on, rather than deciding spec placement here.

### Error: ADR-0033 was wrong about `trusty-channels` — and the error was categorical

Owner, verbatim: **"ADR-0033 is a mistake. THE POINT of trusty-channels is a
native communication model."**

This is recorded as an error, not a supersession-by-circumstances, following
the form ADR-0037 §Context already established for this repo. Verification
sharpens *why* it was an error, more specifically than "MCP was the wrong
protocol for this crate": **ADR-0033 reasoned about what a crate was for
without checking what it contained, or what already existed natively
elsewhere.**

**The native, framework-consumed path is not hypothetical — it already
exists, predates `trusty-channels`, and has nothing to do with it.** Inbound
Slack is native today: `crates/trusty-mpm/src/slack/mod.rs` (860 SLOC,
confirmed — Socket Mode WebSocket push, dispatched to `SessionProxy`) and
`crates/trusty-agents/src/slack/mod.rs` (497 SLOC, confirmed). Inbound
Telegram is native via long-polling (`run_telegram_bot`, confirmed three
occurrences in `crates/trusty-agents/src/telegram/mod.rs`); `trusty-mpm`'s
own `crates/trusty-mpm/src/telegram/mod.rs` runs its outbound alert loop as
`run_alert_loop(bot: Bot, chat_id: ChatId, …)` (confirmed) — a
`teloxide::Bot` held and driven directly, no MCP hop. None of this routes
through `trusty-channels` in any way.

**`trusty-channels` itself has zero consumers of any kind.**
`grep -rn "trusty_channels::" crates/` outside its own crate returns nothing
(confirmed). No workspace `Cargo.toml` has a real dependency edge on it —
the three cross-crate hits for the string "trusty-channels" are the root
workspace-member listing and two code comments referencing its test
patterns, not `[dependencies]` entries (confirmed). Its `slack-mcp`/
`telegram-mcp` binaries appear in no `.mcp.json` or config anywhere. What
the crate contains — `src/slack/server.rs` and `src/telegram/server.rs`,
both calling `trusty_common::mcp::run_stdio_loop` directly (confirmed) — is
an agent-facing *outbound* tool surface: `slack_send_message`,
`telegram_send_message`, and similar. Under the consumer criterion above,
that shape is not wrong on its own terms — an agent choosing to compose and
send a message is an agent-consumer action, which the rule puts on the MCP
side. What's wrong is that this tool surface connects to nothing: it shares
no code with, wraps, and replaces none of the native path above, and
nothing in the workspace calls it.

**The owner's ruling and the evidence are consistent only if the ruling
describes intent for the domain, not the crate's present code — so say that
plainly.** "The point of `trusty-channels` is a native communication model"
is true of what channel communication *should be built around going
forward* (the native path that already does the real work), not of what the
`trusty-channels` crate presently contains (a dormant, correctly
agent-shaped, but entirely unwired MCP tool surface). A reader taking the
ruling as a description of current code will be wrong about the code.
ADR-0033's error, restated with this precision: it inferred an
MCP-consolidation target from a crate's name and contents, and checked
neither whether that content had any consumers nor whether the domain's
real implementation already lived elsewhere.

**This does not contradict ADR-0014 — it is largely ADR-0014 restated.**
ADR-0014 (Accepted, 2026-07-14) already split this domain into two halves
without naming the split explicitly. Its Context: "the three-layer
communication model (DOC-22/DOC-36) already routes Slack/Telegram/MCP
channels through the `SessionProxy` natively" — the pre-existing
inbound/dispatch path confirmed above. Its Decision, separately: "Slack/
Telegram channel integrations" built "in-workspace, in Rust, on the shared
`trusty-common` `mcp` feature," grouped with `trusty-gworkspace` as
"MCP-exposed product surfaces" — what became `trusty-channels`' actual
content, the agent-initiated send tools. Read this way, nothing here
contradicts ADR-0014: its Context already had the native half; its Decision
authorized the MCP half, whose stated rationale — install/signing story,
single toolchain, consistency — is exactly what this ADR keeps, and neither
performance nor an event bus, exactly what this ADR drops (above). The
owner's ruling restates ADR-0014's own Context, made explicit because
ADR-0033 missed it — it treated "channels" as one undifferentiated
MCP-consolidation candidate when ADR-0014's own text had already implied
the split this ADR's consumer criterion now states directly.

**This ADR does not design `trusty-channels`' native future**, and does not
resolve what becomes of its dormant, correctly-agent-shaped send-tool
content — revived as a thin MCP layer over the existing native
`SessionProxy`/`teloxide::Bot` plumbing (the "can be both" case), rewritten,
or left dormant is not decided here. `trusty-channels` is removed from this
consolidation's scope, full stop.

### `trusty-kb`: unfounded, not verified, disposition reserved — a different kind of problem than channels

Owner, verbatim: "`trusty-kb` is probably obsolete, that's a feature in
trusty-agents that uses OKG. Not sure if this should be native or MCP,
that's a separate call." Unlike `trusty-channels`, the owner flagged this
as uncertain ("probably") rather than ruling it an error — and it does not
survive verification as stated, which is a different failure mode than
channels': not "wrong about the crate's purpose," but "asserted without
verification, and the assertion doesn't hold."

`trusty-agents`' OKG feature is not a replacement for `trusty-kb` — it is
built **on top of** `trusty-kb`'s library. Verified via a dozen-plus
`trusty_kb::` call sites in `crates/trusty-agents/src`: `KbStore`
(`stores/index_feed.rs:62`, `stores/status.rs:188`), `okg::registry::{
SourceSpec, Locator}` (`index_feed.rs:61`, `tools/okg/gmail.rs:22`),
`okg::policy::DocStorePolicy` (`tools/okg/config.rs:21`),
`okg::ingest::{IngestReport, SourceItem}` (`tools/okg/gmail.rs:20`),
`okg::ledger::Ledger`, `okg::trust::TrustLabel`
(`tools/memory/okg_fence.rs:72`), `okg::index_journal::{IndexJournal,
IndexOp, IndexTask}` (`index_feed.rs:60`), `schema::Profile`,
`entity::slugify`, `roots::Roots`. Real, load-bearing production usage. If
`trusty-kb` were deleted, `trusty-agents`' OKG ingestion/store subsystem
breaks. `trusty-kb` is not superseded by the OKG feature; it *is* the OKG
engine underneath it.

One distinction the investigation did surface, separate from the
(unfounded) obsolescence claim: `trusty-kb` also ships its own standalone
MCP server binary (`src/main.rs`, eight tools per `src/tooldefs.rs`:
`kb_status`, `kb_list_trees`, `kb_put_entity`, `kb_get_entity`, `kb_list`,
`kb_ensure_structure`, `kb_validate`, `kb_convert_tree`). That binary is
registered in neither `.mcp.json` nor `stable_set()` nor `picker.rs` —
`trusty-agents` reaches the same `KbStore` through native `ToolExecutor`
implementations (`src/tools/okg/{gmail,docstore,drive,sources}.rs`) instead.
So: the **library is alive and essential; the standalone MCP-server binary
is a plausibly unused, unwired surface** — "unwired" is not proof of
deliberate abandonment; it may simply share `trusty-gworkspace`'s
never-registered starting condition rather than intentional deprecation.

**This ADR does not decide `trusty-kb`'s native-vs-MCP disposition** — the
owner reserved that explicitly. But the decision rule above converts this
from an open-ended question into an answerable one, and stating the test is
useful without applying it: the OKG capability's disposition turns on
**whether it is invoked by an agent choosing to search** (agent-consumer ->
MCP) **or by the framework, deterministically, as part of an ingest/index
pipeline** (framework-consumer -> native) — quite possibly both at once,
given `trusty-agents`' OKG feature already does deterministic ingest
(`index_feed.rs`'s backlog reconcile) *and* exposes agent-facing
`ToolExecutor`s (`tools/okg/{gmail,docstore,drive,sources}.rs`) over the
same `KbStore`. That dual shape is itself evidence for the "can be both"
case the rule allows for — recorded as an observation for whoever the owner
delegates the actual call to, not as this ADR pre-deciding it. Issue
[#5083](https://github.com/bobmatnyc/trusty-tools/issues/5083)
(crate-packaging resolution for all three services, citing ADR-0033) should
be re-pointed at this ADR's narrower scope, not closed.

### Epic #3633, sequencing, and the extraction boundary

Epic #3633 governs protocol *implementation* (every server uses the shared
MCP layer), unaffected in substance by which crate that layer lives in.
#5331 (its child, gworkspace's OpenRPC-builder dedup — 317 hand-rolled lines
vs. the 445-line shared `OpenRpcBuilder`) lands **before** the crate move;
its document-equivalence proof guards against a silent `rpc.discover`
change during relocation. Unchanged from prior analysis.

Extraction boundary, checked file-by-file:

| `trusty-common/src/mcp/*` | Non-`mcp/` coupling found | Destination |
|---|---|---|
| `mod.rs` (Request/Response/error_codes/initialize_response/run_stdio_loop) | None | `trusty-mcp` |
| `service.rs` (ServiceDescriptor) | None | `trusty-mcp` |
| `openrpc.rs` (OpenRpcBuilder) | Only `crate::mcp::service` (internal to what's moving) | `trusty-mcp` |
| `daemon_bridge.rs` (ensure_daemon_up) | None found | `trusty-mcp` |
| `memory_rpc.rs` | **Real:** `crate::daemon_addr::read_daemon_addr`, `crate::data_dir::{ENV_LOCK, DATA_DIR_OVERRIDE_ENV}` | **Stays in `trusty-common`** |

`daemon_addr`/`data_dir` are general trusty-common daemon-discovery infra
(also used by `search_readiness.rs`, `monitor/search_client.rs`,
`test_harness.rs`) — dragging them into `trusty-mcp` would recreate "common
is already full" one crate over. `memory_rpc.rs` stays, now importing
`trusty_mcp::{Request, Response}` instead of owning them; its consumers
(`trusty-code`, `trusty-mpm`) are unaffected. `daemon_bridge.rs`'s consumers
(`trusty-analyze`, `trusty-memory`, `trusty-mpm`, `trusty-search`) move to
depending on `trusty-mcp`.

## Options Considered (unchanged from prior analysis)

- **Absorb into `trusty-common`** — foreclosed by this ADR's own Decision.
- **Absorb into `trusty-agents`** — rejected: `rpc/mod.rs`'s `tools/call`
  dispatch is still `TODO(#460)`; wrong coupling for a growing service set.
- **Dynamic loading (`libloading`/`abi_stable`)** — rejected outright. No
  such dependency exists anywhere in the workspace; no ABI stability across
  compiler runs; no sandboxing; `trusty-gworkspace` holds live OAuth tokens
  behind `fd-lock`-protected `tokens.json` that a dylib boundary would move
  outside single-MSRV/single-lockfile discipline.
- **WASM** — deferred, not rejected (see below).
- **Compile-time feature flags** — rejected: converts "choose what to
  install" into "choose what to compile," incompatible with
  `stable_set.rs`'s crates.io-install model.

## Decision

**Two crates.** `trusty-mcp` (new) holds the protocol primitives per the
extraction table above. `trusty-mcp-services` (new, depends on
`trusty-mcp`) holds services and their heavy deps; `trusty-gworkspace`
absorbs into it first, after #5331 lands, and retires as a standalone crate
name (abandoned, not stubbed; its yanked `0.1.0` untouched).

**Breaking change, stated plainly:** removing `mcp` from `trusty-common`'s
public API bumps `0.30.0` -> `0.31.0` (0.x MINOR convention,
`docs/reference/semver-gate.md`). Ten verified consumers need import-path
migration (`trusty_common::mcp::…` -> `trusty_mcp::…`, except
`memory_rpc`-only call sites in `trusty-code`/`trusty-mpm`, which stay on
`trusty-common`). Real, unscoped follow-up work — not designed line-by-line
here.

**`trusty-channels`: removed from scope, and the removal is a correction of
an error**, not a phase-out. No target module, no migration plan, no
timeline — the owner names what the crate is for; how the native model
works is not designed here.

**`trusty-kb`: not folded into `trusty-mcp-services`, not declared
deprecated.** The obsolescence claim does not survive verification (its
library is essential to `trusty-agents`' OKG feature). Native-vs-MCP
disposition is the owner's unmade call. The one fact for that future
decision: its standalone MCP binary is unregistered anywhere today.

**Packaging particulars (unchanged):** `trusty-mcp-services` publishes to
crates.io, one binary per service (`trusty-gworkspace-mcp`, unchanged
name), consistent with `tctl install`'s per-binary picker over
`stable_set()`. Dials `trusty-search`/`trusty-memory`/`trusty-mpm` as
existing stdio proxies (#1078); `trusty-review` joins once #5064 lands.
Transport unchanged, per ADR-0032. Pluggability stays at the process
boundary, and additionally at the registry boundary inside
`trusty-mcp-services` (below) — neither introduces dynamic loading.

**Not a self-cancelling reversal:** `trusty-mcp-core` held primitives,
absorbed into `trusty-common`; this ADR extracts primitives back out as
`trusty-mcp`. `trusty-mcp-services` holds *services* — never part of
`trusty-mcp-core`, unaffected by either move's logic.

**Link-time registry (unchanged):** `inventory` for `trusty-mcp-services`'
native-service registry — chosen over `linkme` (narrower platform support)
and over a hand-maintained table (wrong at anticipated "hundreds of
services," third-party-authored, scale). New workspace dependency.
Config-driven enablement is separate from link-time registration.

**Service contract constraints for future WASM (unchanged):** (1)
serialized-values-only at the boundary — true today for
`tools()`/`scopes_for()`, not yet true for execution (`AppState`-based
dispatch takes a host reference; no shared `call`/`execute` method exists
yet — not designed here, only required); (2) capabilities passed explicitly
at construction, not reached for ambiently; (3) no live async state crosses
the boundary — host-initiated, guest-completes, matching MCP's existing
wire shape.

**WASM: deferred, two triggers (unchanged):** a third-party service the
host cannot compile (primary, owner-named); native-service build-health
pressure (secondary).

**Bundled skills (unchanged):** co-located per service, `include_str!`'d
next to the module, not centralized — keeps a service self-contained for
eventual third-party authorship. `trusty-mpm`'s existing bundle mechanism
(`bundle::ALL`, `skill_bundle_stamp()`) cannot carry this as-is — internal
to `trusty-mpm`'s own crate, no cross-crate contribution path; the
integration is real follow-up, not built here. `#4644` (no CI gate for
skill byte-parity) is made more urgent by this design's copy-multiplication
at scale — not resolved, not edited here.

## Where the agent-consumer / framework-consumer rule lives

The owner: "We should include this distinction in the spec." This makes the
rule normative, not only this ADR's reasoning — and this repo mandates one
copy of a normative rule, not a paraphrase that can drift from it (the exact
failure ADR-0037 recorded for ADR-0036/0037 misattribution earlier the same
day).

**Searched `docs/specs/` for the document that should own it; none does.**
Candidates checked and rejected:

- **`SPEC-MCPSVC-01-trusty-mcp-service.md` (DOC-27)** — the closest subject
  match (MCP service architecture), but its own header reads "**SUPERSEDED
  BY ADR-0014**... retained for historical/design context only; do not
  implement against it." Adding new normative content to a document whose
  own banner says not to implement against it would bury the rule where
  readers are told to stop reading.
- **DOC-65 (Universal Framework Agents: Catalog, Boundaries, and the
  Four-Category Model)** — governs the bundled *agent catalog* (which agents
  exist, delegation authority, language scoping), not the
  capability/service architecture question of MCP vs. native. Wrong
  subject.
- **DOC-61 (Canonical Agent Standard)** — governs the agent *source format*
  (Markdown+YAML compose-chain), not service architecture. Wrong subject.

No living spec owns "MCP service architecture" or "the framework/agent
consumption boundary" today. **This ADR does not create one** — the owner
asked to be told rather than have a new spec created unilaterally, and a new
`DOC-N` requires checking `scripts/check_doc_numbers.sh`'s allocation ledger
before claiming a number (the script itself documents four real collisions
this exact gap already caused: DOC-42, DOC-46, ADR-0023 twice, and a stale
"next free" hint in `docs/specs/README.md`). **Reported here as a decision
the coordinator/owner needs to make**, not resolved: either author a new
spec (with a properly-allocated `DOC-N`) whose subject is native-vs-MCP
service architecture, or find/designate an existing living document to
extend. Until that placement is decided, the normative text lives nowhere
official — the rule as stated in this ADR's "decision rule" section above
is the only copy that exists, and it is explicitly ADR-shaped (dated,
reasoned, alternatives-rejected) rather than spec-shaped (operative,
timeless, second-person-applicable). **If SLD (DOC-38) ends up applying**
once the spec home is chosen, code implementing the criterion (e.g. wherever
a future service registration declares itself agent- or framework-consumed)
would need a source<->spec reference per `scripts/check_sld.sh` — recorded
as follow-up, not done here.

## ADR-0033's disposition: superseded, and the supersession corrects an error

The tm-adr standard offers exactly five statuses: Proposed, Accepted,
Rejected, Superseded by NNNN, Amended by NNNN. None literally reads "was
wrong" — so the correction has to live in the *justification*, the same way
ADR-0037 recorded commit `adf4faf7`'s wrong attribution in prose while using
the standard vocabulary for the status field itself. Given that constraint,
**Superseded by 0040** is the right field value — but stated plainly here so
a reader does not mistake it for the softer "circumstances changed" sense
the word can also carry:

- **`trusty-channels` was an error, not an overtaking.** ADR-0033 inferred
  an MCP-consolidation target from a crate's name and contents, without
  checking whether that content had any consumers (it has none, verified)
  or whether the domain's real implementation already existed natively
  elsewhere (it does, predating `trusty-channels` entirely — see Context).
  Largely a restatement of ADR-0014's own already-accepted Context, missed
  by ADR-0033. Wrong when decided, not overtaken by later facts.
- **`trusty-kb`'s inclusion was unfounded, a different defect than
  channels'.** Not shown wrong about the crate's *purpose* the way channels
  was — but included on an obsolescence premise that turns out false on
  verification, and its actual disposition was never the owner's decision
  to fold in the first place.
- **What survives, and only in direction, not in specifics:** the instinct
  that `trusty-gworkspace` should not remain a standalone, unpublished
  crate. Nothing about ADR-0033's specifics survives unmodified — not the
  target crate's meaning (a *services* adapter named `trusty-mcp`, vs. this
  ADR's `trusty-mcp` as *primitives*), not the one-crate structure (now
  two layers), not the primitives-stay-in-`trusty-common` premise its whole
  "boundary, stated precisely" argument depended on (now reversed).

Two of three subjects were wrong or unfounded; the crate-naming and
crate-structure choices for the one remaining subject do not survive either.
**Superseded**, not **Amended** — an amendment implies the original holds
with refinement, and by the count above almost nothing does. The status
field says "Superseded by 0040"; a reader relying on the field alone without
this section would reasonably (and wrongly) assume ADR-0033 was sound and
merely overtaken — this section exists so that reader instead sees the
error named.

## Open Questions

- **Where the agent-consumer/framework-consumer rule lives normatively** —
  no existing spec fits (see dedicated section above); creating one needs a
  `DOC-N` allocation the coordinator/owner should make, not this ADR.
- `trusty-channels`' native `SessionProxy` redesign — not scoped here,
  needs its own decision and issue.
- `trusty-kb`'s native-vs-MCP disposition — owner's unmade call. Verified
  input: library essential, standalone MCP binary unregistered.
- `memory_rpc.rs`'s final module path once it's the sole file remaining
  under what was `trusty-common/src/mcp/` — not decided here.
- The `call`/execute trait method's exact signature — constraints stated,
  not designed. Likely `trusty-agents`' `TODO(#460)`'s resolution.
- The trusty-mpm skill-deployer integration for service-owned skills — not
  built here.
- Why `trusty-gworkspace` `0.1.0` was yanked — no explanation findable
  in-repo.
- The two WASM triggers are qualitative, not numeric thresholds.
- Re-pointing #5083 at this ADR's scope — not this ADR's job to execute.
- **Unrelated, found in passing:** `docs/adr/` has two files both numbered
  `0021` (`0021-cargo-bin-policy.md`, Proposed, absent from `INDEX.md`; and
  `0021-slack-inbound-hybrid-gateway-eventstream.md`, Accepted, the one
  `INDEX.md` lists) — a pre-existing collision, orthogonal to this ADR,
  flagged not fixed.
- **Noted, not waited on:** a separate verification is in progress on
  whether the event-bus/push-semantics/performance case for
  `trusty-channels` going native survives contact with real message rates.
  If it contradicts anything here, this ADR records the owner's ruling as
  the ruling and would need a follow-up amendment, not a silent edit.

## Related Decisions

Vetted against `docs/adr/INDEX.md` on 2026-08-10:

- **ADR-0033: Superseded by this ADR**, correcting an error on
  `trusty-channels` and an unfounded inclusion of `trusty-kb` — see
  disposition section above. Status updated to `Superseded by 0040`.
- **ADR-0014 (Ship full native MCP support, Accepted): Consistent,
  untouched.** This ADR's `trusty-channels` correction realigns with
  ADR-0014's own original "native home via `SessionProxy`" framing rather
  than contradicting it.
- **Epic #3633: Consistent, orthogonal axis** — governs protocol
  implementation, unaffected by which crate hosts it. Not edited here.
- **ADR-0031/ADR-0032: Consistent, orthogonal axis** — no transport
  decision added.
- **DOC-27/SPEC-MCPSVC-01: Consistent, not resurrected.**
- **#4644, #5083 (open issues, not ADRs): Consistent**, made more urgent /
  needing re-pointing respectively — neither edited here.

No other Accepted ADR addresses MCP crate packaging, link-time service
registration, or WASM plugin isolation for this workspace.

## References

- [docs/adr/0033-trusty-mcp-consolidates-native-mcp-services-into-one-crate.md](0033-trusty-mcp-consolidates-native-mcp-services-into-one-crate.md) —
  superseded by this ADR
- [docs/adr/0014-native-mcp-support.md](0014-native-mcp-support.md),
  [docs/adr/0037-...](0037-pm-placement-precedence-main-checkout-by-default.md) —
  the "wrongly attributed" precedent this ADR's Error section follows
- Epic #3633, #5331, #3577, #4644, #5083
- `crates/trusty-common/Cargo.toml:3,292-295`;
  `crates/trusty-common/src/mcp/{mod,service,openrpc,daemon_bridge,memory_rpc}.rs`;
  `src/daemon_addr.rs`, `src/data_dir.rs`
- `crates/trusty-channels/Cargo.toml`, `src/slack/server.rs`,
  `src/telegram/server.rs`
- `crates/trusty-kb/src/tooldefs.rs`, `src/main.rs`;
  `crates/trusty-agents/src/stores/{index_feed,status,binding}.rs`,
  `src/tools/okg/*.rs`, `src/tools/memory/okg_fence.rs`
- `docs/reference/semver-gate.md:163,203-218`
- `crates/trusty-installer/src/commands/stable_set.rs`, `picker.rs`
- `crates/trusty-mpm/src/core/bundle.rs`, `skill_source.rs:69`
- crates.io — `trusty-gworkspace` `0.1.0`, yanked
