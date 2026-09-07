## A sidecar, on purpose

trusty-analyze does not index anything. It pulls the chunk corpus trusty-search
has already built, runs static analysis over it, and serves the results on its
own port. One parse of your repository feeds both, and a crash in either does
not take the other down.

That coupling is explicit rather than best-effort: the analyzer health-checks
trusty-search at startup and exits rather than come up half-useful. There is no
offline mode to accidentally end up in.

## What it measures

- **Complexity.** Cyclomatic and cognitive scores per chunk, per file, and
  aggregated per index — the second because branch counting alone rewards code
  that is short and unreadable.
- **Smells.** Named categories — long functions, deep nesting, too many
  parameters — each with a threshold you can move rather than a hard-coded
  opinion.
- **Grades.** An A-to-F letter per file and per index, for the times a number is
  more argument than signal.
- **Age.** A temporal-decay score over git blame, with a half-life of about ten
  weeks. Complex code touched yesterday is being worked on; complex code nobody
  has touched in a year is the one to worry about.
- **Structure.** Concept clusters over the corpus, entity extraction, SCIP
  protobuf ingest for symbol data an LSP already computed, and a facts store of
  subject/predicate/object triples persisted locally.

## Fourteen languages, one shape

Tree-sitter adapters cover Rust, Python, TypeScript, JavaScript, Java, Go, Ruby,
PHP, C, C++, C#, Kotlin, Swift, and Scala. They all produce the same metric
shape, so a polyglot repository gets one comparable report rather than a
per-language dialect of the truth.

## Two ways in

An HTTP API serves complexity hotspots, smells, quality grades, clusters, and
the facts store; an MCP server exposes the same analysis to an agent over stdio
or SSE. A deep-analysis pass will additionally write a prose narrative over an
analyzed index, routed through OpenRouter or AWS Bedrock depending on the model
id you configure — it is opt-in, and nothing else in the crate calls an LLM.

The default build links no ONNX runtime and downloads no model. One install
command works on every supported host.
