## Three lanes, one ranking

A grep knows the token you typed. An embedding knows what you meant. A symbol
graph knows what calls what. trusty-search runs all three over the same corpus
and merges them with Reciprocal Rank Fusion, so a query lands whether you
spelled the identifier right or only described it.

- **Lexical.** A code-aware BM25 that splits `CodeIndexer` into `code` and
  `indexer`, so a half-remembered identifier still matches.
- **Vector.** An HNSW index over usearch, holding 384-dimension embeddings
  produced locally — no text leaves the machine to be indexed.
- **Graph.** A petgraph symbol graph built from tree-sitter parses, walked one or
  two hops to pull in the callers and callees around a hit.

Fusion uses a fixed damping constant of 60 — there is no relevance dial to tune
wrong.

## The query picks the weighting

Asking for a definition and asking a conceptual question want opposite rankings.
A sub-millisecond regex classifier sorts every query into one of five intents
and sets the vector/lexical weights accordingly, before any search runs.

| Intent     | Vector | Lexical | Graph-first |
| ---------- | ------ | ------- | ----------- |
| Definition | 0.3    | 0.7     | —           |
| Usage      | 0.5    | 0.5     | yes         |
| Conceptual | 0.8    | 0.2     | —           |
| Bug / debt | 0.1    | 0.9     | —           |
| Unknown    | 0.6    | 0.4     | —           |

Graph expansion is gated to Usage, where caller and callee chains are what you
actually asked for. Everywhere else it would just add noise.

## One daemon for the whole machine

Install once, run one process, register as many named indexes as you have
projects. Nothing is per-project except the index itself, and re-running an
index is cheap: content fingerprints skip files that have not changed, so only
the diff pays for embedding.

- Working on a branch? Pass it, and chunks from the files it touched get a 1.5×
  score multiplier — every result reports whether it was boosted.
- Don't need semantics? `--lexical-only` skips embedding entirely and leaves you
  a daemonised BM25.
- Don't need call chains? `--no-kg` skips the symbol-graph rebuild on every
  reindex.
- Memory limits — chunk caps, batch sizes, cache sizes — are computed from
  detected system RAM at startup rather than guessed at compile time.

## Nothing is indexed until you say so

A fresh daemon accepts zero indexes. A path has to be added to the allowlist
before it can be registered, whether the request arrives over HTTP, from the
CLI, or from an MCP tool call.

On top of that sits a denylist that the allowlist cannot override: credential
directories such as `.ssh`, `.aws`, `.gnupg` and `.kube`, paths carrying secret
markers, and the top level of your home directory. Those are refused with the
matched pattern named in the error, not silently skipped.

## 21 tools over MCP

The MCP server speaks stdio and HTTP/SSE and exposes each retrieval lane
separately, so an agent can pick the one that fits the question instead of
always paying for the fused search.

```
search · search_lexical · search_semantic · search_kg · search_all ·
search_similar · get_call_chain · grep · typeahead · index_file · remove_file ·
list_indexes · create_index · delete_index · reindex · index_status ·
list_chunks · search_health · chat · console_metrics · upgrade
```
