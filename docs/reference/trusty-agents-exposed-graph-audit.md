# The Exposed Knowledge Graph — Audit (#7430)

> Epic #7425 item (f): the knowledge graph the assistant UI and agent tools
> expose is the OKG graph — triples AND definitions from the assistant's `okg/`
> tree — and never memory drawers. The memory knowledge graph (trusty-memory's
> `kg_*` surface) is a separate store and must not leak into it.

Audited 2026-09-11 against `crates/trusty-agents` and
`crates/trusty-agents-common`. Every path by which graph data reaches the UI or
an agent tool, with the store it reads.

## Paths

| # | Path | Entry point | Source before #7430 | Source now |
|---|---|---|---|---|
| 1 | `GET /api/agents/:name/kg/subjects` | `crates/trusty-agents/src/api/server/agent_kg.rs:183` | trusty-memory `memory.kg_subjects_with_counts` — **memory KG** | OKG tree |
| 2 | `GET /api/agents/:name/kg/all` | `crates/trusty-agents/src/api/server/agent_kg.rs:199` | trusty-memory `memory.kg_all` — **memory KG** | OKG tree |
| 3 | `GET /api/agents/:name/kg?subject=` | `crates/trusty-agents/src/api/server/agent_kg.rs:224` | trusty-memory `kg_query` tool — **memory KG** | OKG tree |
| 4 | `GET /api/agents/:name/kg/count` | `crates/trusty-agents/src/api/server/agent_kg.rs:247` | trusty-memory `memory.kg_count` — **memory KG** | OKG tree |
| 5 | Route wiring | `crates/trusty-agents/src/api/server/routes.rs:246` | — | the four routes above, unchanged paths |
| 6 | SPA client | `crates/trusty-agents/ui/src/lib/kg.ts` | the four routes | the four routes |
| 7 | Browser component | `crates/trusty-agents/ui/src/components/KnowledgeGraphBrowser.svelte` | the SPA client | the SPA client |
| 8 | Graph reader | `crates/trusty-agents/src/stores/okg_graph.rs` | did not exist | OKG tree — the single reader |

No Tauri command and no MCP tool serves graph data: `crates/trusty-agents`
exposes the graph over HTTP only, and the desktop shell loads the same SPA.

## Other surfaces that read a memory palace, and why they are not the graph

| Surface | Reads | Verdict |
|---|---|---|
| `crates/trusty-agents/src/api/server/chat_history.rs` | trusty-memory `chat_session_recall` | Chat transcripts, not graph data. Out of scope. |
| `crates/trusty-agents/src/tools/memory/recall.rs` | memory drawers | The agent's recall tool. A drawer is a memory, never presented as a graph node. |
| `crates/trusty-agents/src/tools/memory/vector_search.rs` | the bound search index | Search hits over the OKG corpus; `crates/trusty-agents/src/tools/memory/okg_fence.rs` fences untrusted ones. Not the graph surface. |
| `crates/trusty-agents/src/stores/status.rs` | probes the bound palace | Reports whether a palace is reachable. Status, not content. |

## What the exposed graph carries

Both halves, from `crates/trusty-agents/src/stores/okg_graph.rs`:

- **Triples** — one per `[[wiki-link]]` target held in an entity's relationship
  frontmatter keys. Envelope fields (`type`, `title`, `description`, `tags`, …)
  are excluded, so a link inside prose is not read as an edge.
- **Definitions** — one per entity: its `type`, collection, one-line summary,
  and tree-relative path.

`/kg/count` reports both (`{"active": N, "definition_count": D}`); the three
content routes return triples in `data` and the matching definitions in
`definitions`. Every envelope declares `"source": "okg"`.

`tree` is the binding's own opaque label — `okg://<agent>`, or the home-relative
`<agent>/okg` — never a filesystem path, and neither is any `reason`. The
underlying error text is logged instead. A triple's `provenance` and a
definition's `path` are tree-relative for the same reason.

## The gate

`no_memory_drawer_or_palace_triple_can_reach_the_exposed_graph`
(`crates/trusty-agents/src/api/server/tests/agent_kg.rs`) drives all four reads
against an agent that binds a memory palace AND has an OKG tree, and fails if
the palace id or the word "drawer" appears in any payload, if the envelope stops
declaring `source: "okg"`, or if either half of the graph goes missing.

`no_envelope_discloses_a_filesystem_path`, in the same file, walks every string
in every envelope on every route in three states and fails on any that starts
with `/` or contains the tree's real root.
