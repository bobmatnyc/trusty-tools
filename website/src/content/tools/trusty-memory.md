## A place to put what was learned

An assistant that forgets everything at the end of a session relearns your
codebase every morning. trusty-memory is the store that stops that: an MCP
server over a local vector index and a key-value store, where an agent writes
what it worked out and recalls it by meaning later.

Memories are organised into named _palaces_ — one per project — with rooms and
wings inside them. The naming is deliberate: a palace is anchored to a real
project directory, so the memories for one repository can never quietly bleed
into another's recall. Work outside any project and there is a single `personal`
palace for the notes that belong to you rather than to a codebase.

## Recall that does not need the words

Recall is hybrid — lexical BM25 alongside vector similarity — so a query finds a
memory that said the same thing differently. Embeddings are computed on the
machine, and both the vector index and the metadata store are ordinary local
files. Nothing is shipped to a hosted memory service.

- `memory_remember` and `memory_recall` are the whole day-to-day surface;
  `memory_recall_deep` trades latency for reach when the fast lane comes up
  short.
- A knowledge-graph layer stores subject/predicate/object triples next to the
  prose, so structured facts can be asserted and queried directly rather than
  fished back out of a paragraph.
- A chat-session store keeps conversation turns verbatim, bypassing the signal
  filters that apply to ordinary memories — a transcript is not a fact and
  should not be deduplicated like one.

## The dream cycle

A memory store that only ever accumulates degrades into a landfill.
trusty-memory runs a consolidation pass — the dream cycle — that merges
near-duplicates, prunes what has gone stale, and, when an inference backend is
configured, summarises a room's older facts into canonical entries and links the
originals to their replacement so the lineage stays traceable.

Task drawers — goals, milestones, checkpoints an application must re-derive
across sessions — are exempt. They are never evicted and never consolidated
away, however old they get, until something deletes them explicitly.

## Wiring it up

`trusty-memory setup` installs the background service, warms the embedding model
cache, and patches the Claude settings files it finds with the right server
entry. From then on the daemon is just there.

For a manual configuration, the canonical entry runs
`trusty-memory serve --stdio`, which forwards every request to the running HTTP
daemon and returns its answers verbatim. The stdio process never opens the
database itself, so it coexists safely with the daemon and with other clients.
The same daemon serves a REST API and an embedded browser dashboard on the port
it bound.
