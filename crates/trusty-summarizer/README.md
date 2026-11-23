# trusty-summarizer

Fast, low-inference summaries, classes and outlines for files, so you can
sort and review many files before spending expensive inference on them.
Plan: `docs/summarizer/2026-11-14-trusty-summarizer-plan.md`.

This build reads plain text, Markdown and source code directly, and converts
PDF, DOCX, PPTX, HTML and EML files through the converter worker (below).
Other formats get an `unsupported_format` error record. XLSX depends on the
build (see "Build"): the Xberg 1.3.10 build here gives
`unsupported_format` (calamine RUSTSEC-2026-0436) and says so.
By default summaries use the `extractive-tf` mode (no model) and the
classifier is `none`; see "Summary modes" and "Classifiers" below.

## Defaults and the recommended setup

Owner ruling Dnb (2026-11-15), carried out in this build:

- Outline: the plain outline (`outline.describe = false`). The described
  outline (`outline.describe = true`: terms per entry and a hard line bound)
  stays available; it was measured and was not better: on the held-out
  questions it added tokens (about 14,200 against 12,700 per question with
  GLM 5.3 Flash) and scored 56 of 63 against 61 of 63 for the plain one.
- `find`: on (`find.enabled = true`).
- Reader (the agent or model that reads this tool's output): GLM 5.3 Flash
  on Atlas Cloud, `zai-org/glm-5.3-flash` with `reasoning_effort = "low"`
  (it refuses to turn reasoning off; leave room for about 60 reasoning
  tokens). The same model is the recommended summary backend and, through
  the LLM labeller, the classifier (ruling Dn0); `config.example.toml` has
  the commented `[backends.atlas]` table. DeepSeek V4.1 Flash
  (`deepseek-ai/deepseek-v4.1-flash`, `thinking = { type = "disabled" }`)
  is the option for documents beyond GLM's context.
- First-call threshold: 15,000 document tokens. Below it, read the document
  directly. From it up, call the tool first and use the outline, `find` and
  `read`. The tool has no setting for this; the reader applies it, from
  the record's `tokens_est`, or from `trusty-summarizer size FILE` before
  any summarize (see "Output fields" for the estimate and its error).

Limit: these defaults rest on one test of 63 held-out questions on 21
documents (the same documents as the earlier set), single runs, one
reviewer. GLM 5.3 Flash with `find` and the plain outline answered 61 of
63, against 61 of 63 for the full document; on non-Markdown documents above
10,000 tokens 20 of 21 against 20 of 21, at about a quarter of the tokens.
With 63 questions a difference of two or three answers is within run-to-run
noise. The measurement report stayed with the original repository.

## Commands

```sh
# One JSON line per file; directories are walked recursively.
trusty-summarizer summarize docs/ README.md

# Print lines 12 to 40 (1-based, inclusive) of a file's text.
trusty-summarizer read --lines 12-40 docs/plans/2026-11-14-trusty-summarizer-plan.md

# Where does a term or phrase occur? One JSON object with line numbers.
trusty-summarizer find --query "Alder annex 17" report.pdf
```

`find` searches the converted text of any supported format (through the
same text cache as `read`). Words are compared lowercased, with thousands
separators removed (`12,345` = `12345`) and a light English suffix strip
(`annexes`, `annex's` and `annex` match; `studies` and `studying` match). A
number matches only the whole number (`17` does not match `170`). Hits are
ranked, best first: the query's words as a phrase (they may run over a line
break); then all of its content words within three lines; then, for a query
of two or more words, lines holding at least half of its content words,
more words first (fewer is no hit, so an absent subject still answers
`"none"`). Within a rank, hits are in line order; a line inside a better hit is
not listed again. `mode` is the best rank found, or `"none"` when nothing
matched. Output: `{"query", "mode": "phrase" | "all_terms" | "some_terms" |
"none", "total", "hits": [{"line", "match", "lines": [a, b], "text"}],
"terms": [{"term", "lines"}]}`; `match` is that hit's rank; `text` holds the hit line and `--context N` lines either side
(default 1), each `"<n>: <text>"` and cut to 160 characters, a cut line
ending in `…` (`--width N` cuts at N instead; `--width 0` prints whole
lines); at most
`--max-hits N` hits (default 20), `total` counts them all; `terms` gives the
number of lines holding each content word, so a no-match says which word is
absent. No model runs.

`summarize` options: `--no-references` (outline without ranges),
`--no-describe` (plain outline, the default: overrides `outline.describe = true`),
`--no-cache`, `--cache-dir DIR`, `--concurrency N`, `--no-warm` (run in
this process; see "Warm process"), `--outline-max N` (print at most N
outline entries; default all). A capped record keeps the first N entries
and adds `"outline_omitted": {"entries": K, "lines": [a, b]}`: the number of
dropped entries and the lines they cover (`lines` absent with
`--no-references`); `read` those lines for the rest. `--stats` adds the
per-run `cache` and `stages` fields; without it (the default) they are left
out, so a fresh and a cached run of the same file print the same bytes.
Global: `--config PATH`.

```sh
# Converted-text size and token estimate, one JSON line per file; no model.
trusty-summarizer size workers/summarizer-convert/tests/fixtures/survey.pdf
# {"bytes":866,"converter":"xberg-1.3.10+w1+t2","format":"pdf","lines":7,"path":"workers/summarizer-convert/tests/fixtures/survey.pdf","text_chars":233,"tokens_est":59}
```

`size` gives the values `summarize` records. Text, Markdown and code are
read directly; a converted format comes from the converted-text cache, or is
converted locally once (seconds, no model) and cached. Exit 2 when a file
failed (`{"path","error"}`).

```sh
trusty-summarizer server status   # JSON; "running": false when none runs
trusty-summarizer server stop     # stop it and wait for it to exit
```

Exit status: 0 when every file succeeded, 2 when the batch finished with at
least one error record, 1 on a fatal error.

## Output fields

| Field | Meaning |
|---|---|
| `status` | `ok` or `error` |
| `path` | The file |
| `hash` | `sha256:<hex>` of the file's bytes |
| `bytes`, `lines` | File size; line count of its text |
| `text_chars` | Characters (Unicode scalar values) of the converted text: for PDF and Office files the text, not the file |
| `tokens_est` | Estimated model tokens of that text: `ceil(text_chars / 4)`. Against one model's own count on the measurement set the median error was 4.6 %, from -20 % to +33 % per document; no tokenizer is linked. The same through the warm server and `--no-warm`, fresh or from cache |
| `format` | `markdown`, `text`, `code`, or a document format such as `pdf` |
| `class` | `{"status":"classified","label":..,"score":..,"runner_up":{"label":..,"score":..}}` or `{"status":"no_classifier_configured"}` |
| `summary` | The summary text |
| `outline` | `[{"title":..,"lines":[a,b],"paragraphs":[p,q],"terms":[..]}]`; line ranges tile the file. `terms` (with `outline.describe = true`; off by default) lists up to 6 names, numbers, dates or rare words that distinguish the range from the rest of the document, at most 80 characters joined; it is absent when empty. With `outline.describe = true` no entry spans more than `outline.max_section_lines` lines. `paragraphs` is absent on a piece cut from inside a paragraph. In a converted document (PDF, PPTX, ...) an entry's title is its first line that is not a footer or boilerplate (a copyright or rights line, a page number, a bare price, number or date), or empty when none is. No ranges with `--no-references` |
| `outline_omitted` | Only when `--outline-max N` dropped entries: `{"entries": K, "lines": [a, b]}` |
| `backends` | `{"converter":..,"summary":..,"classifier":..}` |
| `cache` | With `--stats` only: `hit`, `miss` or `disabled` |
| `stages` | With `--stats` only: `[{"stage":..,"ms":..,"tokens_in":..,"tokens_out":..,"cost_usd":..,"cost_source":..}]` for the stages that ran; `classify` and `summarize` add `chars_in`, the characters of document text sent, and, when the response reports them, `reasoning_tokens` (part of `tokens_out`) and `cached_tokens_in` (part of `tokens_in`). `cost_source` is `none` (no billable model ran; `cost_usd` 0), `provider` (the provider's `usage.cost`), `configured_prices` (tokens times the backend's prices) or `unknown` (`cost_usd` null). An error record lists the stages that ran, `convert` included when conversion failed; a failed model call whose response reported usage (an empty answer whose budget went to reasoning, for one) adds its `classify` or `summarize` stage with those tokens and their cost, since the provider bills them |
| `error` | Error records only: `{"kind":..,"message":..,"format":..}` |

## Trial

For one week, agents on this Mac call the tool first on non-Markdown
documents over 15,000 tokens (`tokens_est`) and report each question it
failed on. Two commands, from any working directory:

```sh
# Report a miss: one JSON line appended to the log.
trusty-summarizer miss report.pdf --question "What is the annex 17 budget?" \
  --category find-empty --note "find gave mode none for 'annex 17'"

# The log's path and entry count; --print prints the entries.
trusty-summarizer misses
# {"count":1,"exists":true,"log":"~/.local/state/trusty-summarizer/misses.jsonl"}
trusty-summarizer misses --print
```

`--category` is one of `wrong-lines` (the outline or `find` pointed at the
wrong lines), `not-in-lines` (the lines read lacked the answer),
`find-empty` (`find` found nothing for a term the document holds),
`conversion-lost` (the converted text lost content), `tool-error`, `other`.
`--note` is free text. Both are optional; `--question` is required.

Log: `~/.local/state/trusty-summarizer/misses.jsonl`, or
`$TRUSTY_SUMMARIZER_MISS_LOG` when set (tests use it). Directories the tool
creates are made 0700; an existing directory keeps its mode and is refused
(exit 1) when group- or world-writable, `/tmp` included. The file is 0600,
set on the open descriptor; a symbolic link at the log path is refused and
its target left alone. Each append holds an exclusive lock and writes its
line in one call, so concurrent sessions never interleave or lose one.

Recorded per line: `ts` (UTC), `path` (absolute), `hash` (sha256 of the
file), `format`, `converter`, `bytes`, `text_chars` and `tokens_est` (null
for a converted format whose text is not in the cache yet), `question`,
`note`, `category`, `doc_error`, `tool_version`, `outline_describe`,
`references`, `cwd`. Not recorded: any document text, the summary or the
outline. `miss` makes no network call, starts no warm server and converts
nothing. A missing or unreadable document is still recorded, with the
reason in `doc_error`, a warning on stderr and exit 0; a log that cannot be
written is exit 1.

### Usage log

Every `size`, `summarize`, `find` and `read` call appends one line per
document to the usage log, the denominator of a miss rate:

```sh
trusty-summarizer usage                    # counts per day, command and cwd
trusty-summarizer usage --since 2026-11-18 # or --since 2026-11-18T09:00:00Z
# {"by_command":{"read":2,"find":1},"by_cwd":{..},"by_day":{"2026-11-18":3},"count":3,"documents":1,"errors":0,"exists":true,"log":"..","since":null,"unreadable_lines":0}
trusty-summarizer misses                   # miss count and usage count together
# {"count":1,"exists":true,"log":"../misses.jsonl","usage_count":3,"usage_log":"../usage.jsonl"}
```

Log: `$TRUSTY_SUMMARIZER_USAGE_LOG`, else `usage.jsonl` in the miss log's
directory. Same directory, file, symlink and lock rules as the miss log.
Recorded per line: `ts` (UTC), `command`, `path` (absolute), `hash`,
`format`, `tokens_est` (null when the text was not obtained),
`lines_served` (`read`), `range` (`read`: the `--lines` value asked for,
`A-B`), `hits` (`find`, before the `--max-hits` cut), `query` (`find`: the
query as given; `range` and `query` are absent on other commands and on
lines written before they existed),
`outcome` (`ok` or the error kind, such as `io`, `invalid_range`,
`unsupported_format`), `tool_version`, `cwd`. Never document text. The line
is written by the calling process, so warm and `--no-warm` calls record the
same. A usage-log write that fails prints
`trusty-summarizer: warning: usage not recorded: ...` on stderr and does not
change the command's output or exit status: a broken log never stops a read.

### Read hook for Claude Code

`hooks/route-documents` (in this crate's directory) is a `PreToolUse` hook for the `Read` tool. It
needs `python3`. It reads the hook input on stdin (`tool_name`,
`tool_input.file_path`, `cwd`) and decides:

| File | Decision |
|---|---|
| `.pdf`, `.docx`, `.pptx`, `.html`, `.htm`, `.eml` | deny: the reason lists the `size`, `summarize`, `find` and `read` commands for that file |
| `.txt`, `.text`, `.md`, `.markdown` at 15,000 tokens or more (characters / 4, rounded up) | deny, same reason |
| `.xlsx`, source code, smaller text, anything else | allow |
| `trusty-summarizer` not on `PATH`, file missing, bad input, any error | allow, with a note on stderr |

Allow is exit 0 with no output, so the session's own permission rules
apply. Deny is exit 0 with `hookSpecificOutput.permissionDecision = "deny"`
and `permissionDecisionReason`, which the agent sees. The hook writes no
usage line; the commands the agent then runs do.

To register it for one project, add to that project's
`.claude/settings.json` (or `~/.claude/settings.json` for every session),
with the absolute path of this checkout:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Read",
        "hooks": [
          {
            "type": "command",
            "command": "/path/to/trusty-tools/crates/trusty-summarizer/hooks/route-documents",
            "timeout": 10
          }
        ]
      }
    ]
  }
}
```

Nothing in this repository installs it.

## Measurement

`measure/` holds the measurement harness and the document set manifest;
see `measure/README.md`.

## Configuration

TOML, found at `--config`, else `$TRUSTY_SUMMARIZER_CONFIG`, else
`~/.config/trusty-summarizer/config.toml` (or under `$XDG_CONFIG_HOME`).
Every key is optional. `config.example.toml` is a commented example with
every backend; the defaults:

```toml
concurrency = 4            # default: CPU count, at most 8
references = true
max_file_bytes = 67108864

[outline]
max_section_lines = 60     # with describe: no entry spans more lines
describe = false           # true: terms per entry and the hard line bound
max_terms = 6
max_term_chars = 80

[find]
enabled = true             # false: `find` exits 1
max_hits = 20
context_lines = 1
max_line_chars = 160

[summary]
backend = "extractive-tf"
max_input_chars = 32000
max_output_tokens = 200

[classifier]
backend = "none"           # labels default to the built-in set

[backends.extractive-tf]
max_sentences = 3

[warm]
enabled = true             # false = always --no-warm
idle_timeout_secs = 600
```

## Warm process

`summarize` runs through a background server that keeps the embedding
model, the converter workers and the HTTP connections of each
configuration loaded between calls. Nothing is installed: the first call
starts `trusty-summarizer server run` on demand, later calls connect to it,
and it exits by itself after `warm.idle_timeout_secs` (default 600) without
a request. A running server keeps the idle time it was started with.
Records are the same as `--no-warm` gives, streamed in input order; only
the stage times differ. Inside one call a batch is warm either way.

Files, in one directory per user: `$TRUSTY_SUMMARIZER_RUNTIME_DIR` if set,
else `$XDG_RUNTIME_DIR/trusty-summarizer`, else
`$TMPDIR/trusty-summarizer-<uid>` (per user on macOS), else
`/tmp/trusty-summarizer-<uid>`:

| File | What |
|---|---|
| `server.sock` | The unix socket, mode 0600 |
| `start.lock` | Held by the one client that starts a server |
| `server.log` | The server's stderr: start, stop, model load times, warnings; cut back when over 1 MiB at a start |

Rules:

- The directory is created mode 0700 and must be owned by you with no group
  or other access; otherwise the call fails and names the directory.
- Calls racing to start a server take the lock; exactly one starts it. A
  socket file left by a killed server is replaced.
- Each call sends its full configuration (paths made absolute) and its
  working directory; the server computes under exactly that configuration
  and keeps up to 4 configurations warm. A server built from another binary
  or started under other values of the environment variables the library
  reads (`HOME`, `PATH`, `XDG_*`, `TRUSTY_SUMMARIZER_CACHE_DIR`,
  `TRUSTY_SUMMARIZER_CONVERT`, `FASTEMBED_CACHE_DIR`, `HF_HOME`,
  `HF_ENDPOINT`, `ORT_DYLIB_PATH`) is stopped and replaced; it finishes
  batches already running first.
- API keys: the call reads the variables its configuration names in
  `api_key_env` from its own environment and sends them over the socket with
  the request. The server uses them for that batch only and drops them (the
  bytes are overwritten) when the batch ends; it never reads keys from its
  own environment, and is started with every `api_key_env` variable and
  every variable whose name contains KEY, TOKEN, SECRET, PASSWORD or
  CREDENTIAL removed. Keys are never written to the log, the cache or disk.
  So `tm secrets exec --env NAME=KEY -- trusty-summarizer summarize ...`
  works through a server started earlier from another shell.
- A server that crashes, is killed or is stopped mid-batch gives the call
  an error (`connection lost after N records`) and exit status 1; the
  records already printed are the ones before the loss.
- A failed model load is remembered for the server's life, as for one
  process; `server stop` clears it.

## Summary modes

`summary.backend` names the mode. Every mode receives the same bounded
excerpt: the whole text when it has at most `summary.max_input_chars`
characters, else the opening (about a third of the bound, cut at a line
end) plus the outline sections that share the most vocabulary with the
rest, in document order, with `[…]` between non-adjacent parts. The
record's `summarize` stage reports `chars_in`.

| Mode | What it does | Cost |
|---|---|---|
| `extractive-tf` (default) | Top sentences by term frequency | No model |
| `extractive` | Top sentences by similarity to the centroid of their embeddings (local model, below) | No LLM tokens; the model loads once per process |
| any `type = "openai"` endpoint, e.g. `llm` | One chat-completions call per file on the excerpt | Tokens in and out per record |

Each mode's results are cached apart: the cache key holds the mode's name
and its options table. A failing mode gives an error record; nothing falls
back to another mode.

### OpenAI-compatible endpoints

One implementation, configured per endpoint:

```toml
[summary]
backend = "llm"

[backends.llm]
type = "openai"
base_url = "http://localhost:11434/v1"   # Ollama
model = "qwen3:8b"
timeout_ms = 120000                      # per request
# api_key_env = "OPENROUTER_API_KEY"     # the variable's NAME, never the key
# temperature = 0.2
# max_output_tokens = 1000               # replaces summary.max_output_tokens
# price_in_per_mtok = 0.26               # USD per million tokens, both or
# price_out_per_mtok = 0.38              # neither; used without usage.cost
[backends.llm.request]                   # extra JSON fields for the body
reasoning_effort = "none"
```

Pointing at OpenRouter changes `base_url`, `model` and `api_key_env` only.
The key is read from the named variable at each call and sent as a bearer
token; it never appears in the config (an `api_key` option is refused), in
error messages, in records or in the cache key. Tokens in and out come from
the response's `usage` block, with `completion_tokens_details.reasoning_tokens`
and `prompt_tokens_details.cached_tokens` reported separately when present.
The cost is `usage.cost` when the provider sends it (OpenRouter), else the
tokens times `price_in_per_mtok` and `price_out_per_mtok`, else unknown
(`cost_usd` null): Ollama and Atlas Cloud send none. `config.example.toml`
has commented tables for Ollama, OpenRouter and Atlas Cloud.

Per-file failures, all `backend_failed` with a message naming the cause:
`cannot reach <url>` (connection refused, DNS), `timed out after N ms`,
`HTTP <status>` with the start of the body, `malformed response` (not
JSON, provider error object, no `choices[0].message`), `empty answer`
(empty, whitespace-only or null content; "output budget spent on
reasoning" when `finish_reason` is `length` and the response shows
reasoning tokens or reasoning text) and `missing API key: environment
variable X is not set`. A `type = "llm"` labeller fails the same way on an
empty answer.

## Classifiers

`classifier.backend` is `none`, `local`, or a `[backends.<name>]` table
with `type = "embedding"` or `type = "llm"`. Labels come from
`[[classifier.labels]]` (`name`, `description`); with none, the built-in
set is used: specification, report, contract or legal, financial,
correspondence, meeting notes, source code, data, presentation, other. The
record gives the top label, its score and the runner-up.

- `local`: fastembed-rs on ONNX Runtime, in-process, no Python, no server,
  no tokens. Model `all-MiniLM-L6-v2` (384 dimensions; `model =
  "bge-small-en-v1.5"` also works). The document is embedded in up to
  `max_chunks` (8) chunks of `chunk_chars` (1000) characters, averaged, and
  compared by cosine with each label's `"<name>: <description>"` embedding.
  Scores are cosines in [-1, 1]. The model loads on first use, once per
  process, shared with the `extractive` mode; stderr shows the load time.
  The first use downloads it (about 87 MB) from Hugging Face
  (`Qdrant/all-MiniLM-L6-v2-onnx`) into `cache_dir`, default
  `$FASTEMBED_CACHE_DIR`, else `~/.cache/fastembed`.
- `type = "llm"`, `inference = "<inference backend>"`: one call per file
  asking the model for a JSON object of label confidences.

```toml
[classifier]
backend = "local"

[[classifier.labels]]
name = "report"
description = "a report of findings, a survey, study or analysis"
```

The cache lives in `cache.dir`, else `$TRUSTY_SUMMARIZER_CACHE_DIR`, else
`$XDG_CACHE_HOME/trusty-summarizer`, else `~/.cache/trusty-summarizer`. Its
key is the content hash plus every setting that changes output.

## Converter worker

Documents are converted by a second binary, `trusty-summarizer-convert`,
which links Xberg 1.3.10 (features `pdf`, `office`, `html`, `email`; no
`excel`). No OCR,
no ML. The library never links Xberg. The worker runs as a
separate process, so a parser crash or hang costs one file. One worker
process serves many files; a batch keeps up to `convert.workers` of them.

The CLI looks for the worker, in order: `convert.worker` in the config,
`$TRUSTY_SUMMARIZER_CONVERT`, the directory holding `trusty-summarizer`
(so two binaries installed side by side, or built into one target
directory, work), then `PATH`. When none is found, each document gets a
`worker_missing` error record and text files still work.

Per-file failures and their record `kind`:

| Kind | Cause |
|---|---|
| `worker_missing` | No worker binary found; the message lists where it looked |
| `worker_spawn_failed` | The binary could not be started |
| `worker_crashed` | The worker exited mid-file; the message has its exit status and stderr tail |
| `worker_timeout` | No answer within `convert.timeout_ms`; the worker is killed |
| `worker_output_too_large` | The answer exceeded `convert.max_output_bytes`; the worker is killed |
| `worker_protocol` | A garbled answer, or a worker of another version or build; the message names both builds |
| `conversion_failed` | The worker reported a parse error; the worker stays in use |
| `no_text_extracted` | The document converted to empty text, as a scanned PDF does |
| `empty_summary` | The summary mode produced an empty summary, as an extractive mode does on text with no prose sentences; nothing is cached |

After a crash, timeout or oversize answer the next file starts a fresh
worker, and the batch goes on.

```toml
[convert]
# worker = "/path/to/trusty-summarizer-convert"
workers = 2                     # worker processes kept, each serves many files
timeout_ms = 60000              # per file
start_timeout_ms = 10000        # worker start-up and hello
max_output_bytes = 67108864     # one file's answer (the JSON line with its text)
```

DOCX paragraphs lose the `(style_name: …)` labels Xberg appends, so
outline titles and summaries do not carry them. Converted text is cached by
content hash and converter name (`xberg-1.3.10+w1+t2`, so another
build's cached text is never served), beside the results. `read --lines A-B report.pdf` then
prints the lines the outline references from that cache, without running
the worker.

## Build

The library and CLI are `crates/trusty-summarizer`, a member of the
trusty-tools workspace (MSRV 1.94). The converter worker is
`workers/summarizer-convert`, its own Cargo workspace with its own
`Cargo.lock` and `rust-toolchain.toml` (rustc 1.98.1), because Xberg
1.3.10 needs Rust 1.98. The root `[workspace] exclude` keeps it out of the
workspace; `scripts/check_worker_dir_excluded.sh` guards that. The worker
depends on the library by path, for the wire format.

| Part | Toolchain | Manifest and lockfile |
|---|---|---|
| Library and CLI | workspace toolchain (MSRV 1.94) | `crates/trusty-summarizer/Cargo.toml`, root `Cargo.lock` |
| Converter worker | rustc 1.98.1 | `workers/summarizer-convert/Cargo.toml`, `workers/summarizer-convert/Cargo.lock` |

Library and CLI, from the repository root:

```sh
cargo build -p trusty-summarizer
cargo clippy -p trusty-summarizer --all-targets -- -D warnings
cargo test -p trusty-summarizer --no-fail-fast
```

Converter worker, from the repository root (binaries in
`workers/summarizer-convert/target/debug`). The size tests run the
`trusty-summarizer` binary from the same directory, so build it there
first:

```sh
cargo build --locked --manifest-path workers/summarizer-convert/Cargo.toml -p trusty-summarizer
cargo build --locked --manifest-path workers/summarizer-convert/Cargo.toml --all-targets
cargo test --locked --manifest-path workers/summarizer-convert/Cargo.toml --no-fail-fast
```

Install both binaries into one directory, so the CLI finds the worker
beside itself:

```sh
cargo install --locked --path crates/trusty-summarizer
cargo install --locked --path workers/summarizer-convert
```

This build links Xberg 1.3.10 with no `excel` feature, so XLSX gets
`unsupported_format`. The `xberg14` feature names a former Xberg 1.4 build
that this repository does not carry; the code it gates stays, and no
manifest here turns it on. The worker's converter name
(`xberg-1.3.10+w1`) names its build, and a host refuses a worker of
another build with a `worker_protocol` record that names both.

The worker tests use the fixtures in
`workers/summarizer-convert/tests/fixtures/` (see its README) and need
no network. `cargo test` needs no network, no Ollama and no model: the HTTP
client is tested against a server inside the test process, and the
classifier and embedding ranking against fixed vectors. The build itself
downloads the ONNX Runtime library once (fastembed's
`ort-download-binaries`), cached under `~/Library/Caches/ort.pyke.io` on
macOS.

### Live checks (not part of `cargo test`)

Classifier, with the real model (downloads it on first run):

```sh
cargo build --release -p trusty-summarizer
printf '[classifier]\nbackend = "local"\n' > /tmp/local.toml
target/release/trusty-summarizer --config /tmp/local.toml summarize --no-cache \
  workers/summarizer-convert/tests/fixtures/ docs/summarizer/
```

Each record's `class` has the label, score and runner-up; the `classify`
stage has the per-file time. The model load time goes to stderr with
`--no-warm`, and to `server.log` in the runtime directory otherwise.

LLM mode against a local Ollama (the model must already be pulled):

```sh
ollama serve &                              # a server you own
curl -s localhost:11434/api/version         # wait until it answers
target/release/trusty-summarizer --config crates/trusty-summarizer/config.example.toml summarize --no-cache \
  workers/summarizer-convert/tests/fixtures/ docs/summarizer/
kill %1                                     # stop the server you started
```

The `summarize` stage carries `tokens_in`, `tokens_out`, `ms` and
`chars_in`.
