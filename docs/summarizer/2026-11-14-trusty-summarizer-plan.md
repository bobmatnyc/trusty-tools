# trusty-summarizer: one-page plan (2026-11-14)

Answers the project's kickoff brief (not carried into this repository). Facts below were checked on 2026-11-14; corrections to the brief are at the end. The code now lives in `crates/trusty-summarizer` and `workers/summarizer-convert`.

## 1. Crate shape

A local git repository with a two-member Cargo workspace, edition 2024, toolchain pinned in `rust-toolchain.toml`.

- `trusty-summarizer`: library and CLI (binary `trusty-summarizer`). No dependency on trusty-search, trusty-common or Xberg.
- `trusty-summarizer-convert`: a small worker binary that links Xberg. It exists so that the converter's dependencies and any parser crash stay out of the library.

Pipeline per file, cheapest step first:

1. Convert to text (worker), or read directly for text, Markdown and code.
2. Segment into sections and paragraphs with line ranges. No model.
3. Classify with a local embedding and label prototypes. No LLM tokens. Labels come from a TOML file.
4. Summarize. `extractive` mode ranks sentences by embedding and spends no LLM tokens. `llm` mode makes one call per file on a bounded excerpt (opening text plus top-ranked sections), never the whole document.
5. Cache every result by content hash, so a repeated file costs nothing.

Output is one JSON line per file: path, hash, class and score, summary, outline, and per-stage time, tokens and cost.

## 2. Backend interface

Two traits. Callers name a backend from config; adding one adds an implementation and a config entry and changes no caller.

- `InferenceBackend::complete(request) -> text, tokens in, tokens out, cost, latency`.
- `ClassifierBackend::classify(texts, labels) -> scores`.

First implementations:

| Role | Local | Remote |
|---|---|---|
| Inference | Ollama through its OpenAI-compatible endpoint (`qwen3:8b`, already pulled) | OpenRouter; Atlas coding plan (see Q1) |
| Classifier | fastembed, all-MiniLM-L6-v2, in-process | an embeddings endpoint, or any inference backend used as a labeller |

One OpenAI-compatible client covers Ollama and OpenRouter. Keys are read from an environment variable injected by `tm secrets exec`; no key is written to config or logs.

## 3. Converter choice

Xberg, called in a separate worker process with a size limit and a timeout. This follows the Codex recommendation and keeps a malformed file from stopping a batch.

- Default: `xberg =1.3.10`, `default-features = false`, features `pdf`, `office`, `html`, `email`. It builds on the installed rustc 1.98.1.
- XLSX stays off on 1.3.10. calamine 0.36.1 has an open advisory (RUSTSEC-2026-0436, panic and memory exhaustion on malformed input).
- Xberg 1.4.0 (published 2026-11-13) takes fixed `zip` and `calamine`, but needs rustc 1.99. See Q3.

## 4. Warm process choice

An on-demand server on a unix socket with an idle timeout, the pattern trusty-secrets uses. The first CLI call starts the server; later calls connect to it; it exits after 10 minutes idle. No launchd entry and no install.

Reason: the start-up costs are the embedding model load, the converter worker start and the HTTP connection setup. The server holds all three, plus the cache. `--no-warm` runs one-shot in-process for scripts and tests. A batch in a single call is warm either way.

## 5. Line and paragraph references

The outline lists each topic with a line range and a paragraph range of the converted text. `trusty-summarizer read --lines A-B <file>` returns that range from the cache, so the references also work for PDF and Office files. References are on by default and can be turned off, so both arms can be measured.

## 6. Measurement set and method

Set: about 24 documents, public or written for the purpose, in five size bands (about 1, 5, 20, 80 and 300 pages) across PDF, DOCX, PPTX, Markdown and plain text. Sources: NIST and GovInfo publications (US federal works), the Rust Book, Project Gutenberg, and documents I write. Each file's licence is recorded. No employer, client or personal document is used.

Method:

- Per file and per backend: wall time by stage, tokens in and out, and cost. OpenRouter returns cost in each response; local cost is reported as time.
- Question test: five questions per document with a fixed answer key. One fixed answering model. Three arms:
  - A: the full document in the prompt.
  - B: summary and class only; the model may then ask for the full document.
  - C: summary plus outline with references; the model asks for line ranges.
- Reported per arm and size band: total tokens, answer correctness, and the document size at which C costs fewer tokens than A and than B. The tool's own token cost is counted in B and C.

The report stayed with the original repository.

## 7. Build order

One compiling agent at a time. Steps 1 to 4 need no answer from the owner.

1. Skeleton, segmenter, references, extractive summary, cache.
2. Converter worker.
3. Local backends: fastembed classifier, Ollama inference.
4. Warm server and batch.
5. Remote backends (waits on Q1 and Q2).
6. Measurement harness and report.

## 8. Questions for the owner

**Q1. How is the Atlas coding plan called? (blocks the Atlas backend only)**
The stored credential for the coding plan names no provider, URL or model. No local record names them.
- A (recommended): the owner gives the base URL and model name, and it is OpenAI-compatible. No new client code.
- B: it is Anthropic-compatible. the owner gives the base URL and model; I add a second client.
- C: leave Atlas out of the first build and measure OpenRouter only.

**Q2. How does this checkout get credentials? (blocks all remote backends)**
`tm secrets` finds a credential store from the `origin` remote. This repository has none, so no key resolves here. No OpenRouter key is stored for it.
- A (recommended): set `origin` to the `bobmatnyc/trusty-summarizer` URL in local git config only. No GitHub repository is created and nothing is pushed. Add a new OpenRouter key for this project, with a spend limit, so the measured cost is separate from other projects.
- B: as A, but reuse an existing OpenRouter key through `tm secrets assign`.
- C: create a private GitHub repository now.

**Q3. Toolchain for Xberg 1.4.0 (does not block; default is 1.3.10 without XLSX)**
- A (recommended): install rustc 1.99 for this repository only and use Xberg 1.4.0. XLSX is then supported with the fixed parsers.
- B: stay on 1.98.1 and Xberg 1.3.10. XLSX is not supported until the host toolchain moves.

## 9. Corrections to the brief's findings

- Xberg 1.4.0 and `xberg-cli` 1.4.0 are on crates.io since 2026-11-13. 1.4.0 needs rustc 1.99; 1.3.10 needs 1.92.
- The installed toolchain is rustc 1.98.1.
- The okf worker now vendors calamine with `zip >= 9.0.5`. Its lockfile has zip 9.3.0, which has no open advisory. The calamine advisory was not checked against the vendored copy.
- `trusty-okf` beside this directory is a docs repository, not a crate. The layout template used here is `trusty-tools/workers/okf-extract`.
- Dependabot alerts 416 and 419 could not be read from this session; the advisory ids above come from OSV.
