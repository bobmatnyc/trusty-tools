---
spec_refs:
  - id: SPEC-TCUI-09~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-09~draft
  - id: SPEC-WS-01~draft
    path: docs/specs/DOC-48-tcode-workstreams.md
    anchor: SPEC-WS-01~draft
  - id: SPEC-TCPLUGIN-01~draft
    path: docs/specs/DOC-51-tcode-plugin-support-phase1.md
    anchor: SPEC-TCPLUGIN-01~draft
  - id: SPEC-AGENTBUS-01~draft
    path: docs/specs/DOC-60-bus-based-agent-messaging.md
    anchor: SPEC-AGENTBUS-01~draft
  - id: SPEC-AGENTSTD-01~draft
    path: docs/specs/DOC-61-canonical-agent-standard.md
    anchor: SPEC-AGENTSTD-01~draft
---

# Trusty Code specification and ADR reconciliation

**Status:** Accepted implementation-status catalog
**Owner:** Trusty Code
**Last updated:** 2026-09-03
**Roadmap gate:** [#5433](https://github.com/bobmatnyc/trusty-tools/issues/5433)

## Authority and lifecycle

This catalog is the maintained implementation-status companion to the Trusty
Code specifications. It does not replace their behavioral detail. When old
prose, source, and roadmap state disagree, apply this precedence:

1. Accepted ADRs define architecture and ownership.
2. Accepted spec sections define behavior.
3. This catalog states whether that behavior is shipped, partial, deferred, or
   superseded, and names the work that closes the gap.
4. Draft specs are design inputs. A draft is not a claim that its complete
   surface ships.
5. Historical research records host behavior observed on its stated date. It is
   not a current Trusty Code product contract.

`docs/trusty-code/vision-and-architecture-spec.md` remains the accepted product
vision. Its dated phase tables are historical planning evidence; this catalog
and milestones R1–R4 are authoritative for current implementation status.

## Normative statement catalog

| Contract | Status | Implementation owner | Code evidence | Verification | Roadmap | Disposition |
|---|---|---|---|---|---|---|
| Code is a daemon-first, independent coding harness | Implemented, hardening | Trusty Code | `src/main.rs`, `src/serve/mod.rs`, `src/jsonrpc/`, `src/session/` | `tests/m1_cutline_e2e.rs`, `tests/connector_e2e.rs` | #2063 / R1 | Retained; ownership fixed by ADR-0058 |
| JSON-RPC stdio is a supported local client surface | Implemented | Trusty Code | `src/serve/mod.rs`, `src/jsonrpc/` | daemon and M1 end-to-end tests | #2063 / R1 | Retained |
| The local client API converges on UDS; the loopback TCP listener is interim | Partially implemented | Trusty Code | `src/serve/http.rs`, `src/serve/transport.rs`, `src/session/connector.rs` | `src/serve/http_auth_tests.rs`, `src/serve/http_origin_guard_tests.rs` | #6637 / R1 | Bearer auth shipped (#6491, 0.5.0). ADR-0058 applies ADR-0032's UDS ruling to Code rather than granting an HTTP exception; the surviving TCP listener is a tracked deviation blocked on the GUI's `fetch`/`EventSource` transport |
| Session create/list/status/send/attach/detach/cancel and transcript APIs are daemon-authoritative | Implemented in memory | Trusty Code | `src/session/protocol.rs`, `src/session/registry.rs` | `src/session/registry_tests.rs`, M1 end-to-end tests | #2063 / R1 | Retained; restart durability is not implied |
| Workstreams are durable named aggregates with activation rules | Implemented core, draft spec remains broader | Trusty Code | `src/workstreams/store.rs`, `src/workstreams/activation.rs`, `src/workstreams/protocol.rs` | workstream module tests | #2063 / R1 | DOC-48 remains Draft; only code-linked clauses ship |
| CLI and TUI are thin daemon clients | Partially implemented | Trusty Code | `src/cli/`, `src/cli_client/` | CLI/TUI unit and daemon-autospawn tests | #2063 / R1 | DOC-50 remains Draft; unbuilt Web/UI clauses are deferred |
| Local Claude-compatible plugins contribute namespaced agents and skills | Implemented compatibility adapter | Trusty Code | `src/plugins/`, `src/agents/`, `src/skills/` | `src/plugins/agents_tests.rs`, `src/plugins/skills_tests.rs` | #5425 / R2 | DOC-51 Phase 1 ships; the native root moves to `.trusty-code/` |
| Project `.trusty-code/` owns declarative config; user `~/.trusty-code/` owns private mutable state | Accepted, not complete | Trusty Code | `src/build_info.rs`, `src/workstreams/path.rs`, `src/agent_loop/telemetry.rs`; other readers still use `.claude/` | path, permission, symlink, and secret-exclusion tests required | #5426 / R2 | ADR-0058 supersedes native `.claude/` ownership. The private root is live; the project root is not |
| Instructions, agents, and skills have one canonical authored source | Accepted, not complete | Trusty Code + Trusty Agents; MPM via handoff | `src/assets/agents/`, `src/assets/skills/` still coexist with host copies | provenance and byte/semantic drift tests required | #5425 / R2 | ADR-0059 supersedes ADR-0015 and narrows DOC-61's per-product-source wording |
| Skills use the Agent Skills `SKILL.md` package as the interoperable baseline | Partially implemented | Trusty Code + Trusty Agents | `src/assets/skills/`, `src/skills/` | asset and skill protocol tests | #5425 / R2 | Retained; adapters may add host metadata without forking behavior |
| MCP declarations are shared by definition while Code owns resolution and process lifecycle | Designed, not wired generically | Trusty Code; shared declaration work via MPM handoff | `src/tools/trusty_search.rs`, `src/session/registry_memory_sink.rs` are specific adapters; generic external invocation is absent | contract, import, and lifecycle tests required | #5428 / R3 | ADR-0058 amends ADR-0042; `.mcp.json`-as-native wording is superseded |
| Code session events retain ordered replay and live streaming | Implemented | Trusty Code | `src/events.rs`, `src/session/registry.rs` | registry replay/eviction tests, connector tests | #2063 / R1 | `SessionEventEnvelope` remains the local session stream |
| Cross-agent and cross-user messaging uses the shared durable channel contract | Designed, not adopted by Code | Trusty Code + Trusty Agents; bus host via MPM handoff | `src/events.rs` provides only the local session bus and the legacy stderr prefix | channel conformance tests required | #5429 / R3 | DOC-60 remains Draft; local session events are not the durable message bus |
| Delegated coding agents are durable daemon-owned tasks | Accepted, not implemented | Trusty Code | `src/session/registry.rs` documents itself as in-memory only | restart, reattach, cancel, and credential-boundary tests required | #5430 / R3 | ADR-0058 distinguishes these from ADR-0024's Agents L1 leaves |
| MPM integrates with Code only through a versioned structured API | Accepted; Code side pending | Trusty Code API; the MPM client is owned outside this crate | `src/jsonrpc/` provides the starting protocol types | compatibility and failure-contract tests required | #5431 / MPM release | No pane scraping and no shared state paths |
| `run-workflow` is a supported workflow engine | Not implemented | None in R1–R4 | `src/main.rs` declares the command and exits as unimplemented | implementation and contract tests required if reactivated | Deferred | Historical phase promise; no clean-release requirement |
| Style modes `hack`, `vibe`, and `engineer` are a public delegation contract | Accepted spec, implementation absent | Trusty Code + Trusty Agents | no style-mode type or dispatch branch exists under `src/` | contract tests required | #4348 / #4349 | DOC-62's status is Accepted, but its `~draft` IDs must be versioned before implementation is claimed |

Paths above are relative to `crates/trusty-code/` unless they begin with
`docs/`.

## Source and deployment boundary

| Artifact | Canonical authored source | Trusty Code native result | Compatibility outputs |
|---|---|---|---|
| Instructions | shared, host-neutral authored sections | `.trusty-code/instructions/` plus provenance | `AGENTS.md`, `CLAUDE.md`, host-specific instruction files |
| Agents | shared canonical agent catalog | `.trusty-code/agents/` resolved catalog | `.codex/agents/`, `.claude/agents/` where supported |
| Skills | shared Agent Skills-compatible packages | `.trusty-code/skills/` resolved catalog | `.codex/skills/`, `.claude/skills/` |
| MCP | shared versioned server definitions | project `.trusty-code/mcp/` references and provenance; private `~/.trusty-code/mcp/` trusted resolution and lifecycle | host config import and export; never executable workspace injection |
| Channels | shared versioned envelope schema | project `.trusty-code/channels/` declarations; private `~/.trusty-code/channels/` cursors and task routing | host transport adapters |
| Runtime | Trusty Code daemon | private `~/.trusty-code/` logs, transcripts, credentials, and task/session state | read-only client views |

Exact filenames and migration behavior remain implementation details of #5425,
#5426, #5428, and #5429. The ownership and the direction of conversion are fixed
by ADR-0058 and ADR-0059.

## Reconciled document dispositions

- **`architecture/harnesses.md`:** Accepted architecture overview. A `.claude/`
  path there states current compatibility behavior, not native ownership.
  ADR-0058 controls the target boundary.
- **Vision and architecture spec:** Accepted product vision. Its implementation
  gap table and phase issue numbers are historical as of 2026-07-06; use this
  catalog for current status.
- **Parity spec:** Accepted only for explicit cross-model parity runs. Its TOML
  agent-source statement is superseded by canonical Markdown/YAML sources and
  generated adapters under ADR-0059.
- **DOC-39, DOC-48, DOC-50:** Draft product and UI specifications with partially
  shipped API, workstream, and TUI slices. An unchecked or unlinked section is
  not a release claim.
- **DOC-51:** Draft specification whose local-directory Phase 1 behavior ships.
  Its `.claude/plugins/` root becomes a compatibility adapter after R2.
- **DOC-60:** Draft shared communication specification. Trusty Code's
  `SessionEventEnvelope` survives as a local replay and live stream; it is not
  the cross-agent durable bus.
- **DOC-61:** Draft source-model specification, amended in direction by
  ADR-0059: product builders stay separate, authored behavior does not.
- **DOC-62:** Accepted behavior decision with stale `~draft` section IDs. Treat
  implementation as absent until the IDs and code linkage are reconciled.
- **DOC-65:** Draft catalog. Its MPM source paths describe current provenance,
  not permanent canonical ownership; MPM implementation is #5431.
- **Research under `docs/trusty-code/research/`:** Historical evidence only,
  governed by the source date in each file.

## Mechanical reconciliation gate

`scripts/check_trusty_code_specs.sh` runs in the `Doc numbers` workflow and
rejects these regressions:

- a claim that `.claude/`, `.codex/`, or `.open-mpm/` is Code's native source,
  or that repository-local `.trusty-code/` owns private mutable runtime state;
- a new Trusty Code event prefix or cross-agent envelope that bypasses the
  shared versioned contract;
- an implementation claim for a Draft or `~draft` requirement with no code and
  test evidence;
- a normative Trusty Code requirement with no owner, verification target, and
  roadmap disposition in this catalog; or
- a generated agent, skill, or instruction artifact with no canonical-source
  provenance.

The repository ADR and document-number checks stay mandatory. R2 and R3 may
begin only while this catalog, ADR-0058, and ADR-0059 are present and those
checks pass.
