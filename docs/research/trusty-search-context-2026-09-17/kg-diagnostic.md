# KG adapter tuning diagnostic

Evidence: `replays/baseline-directed-tuning-v2/directed.json`, compared with `replays/baseline-fullkg-tuning/original.json` and `normalized.json`. Only the eight KG tuning questions and their graph rows were inspected. No held-out questions were read. This report changes no treatment.

## Observed result

Direct calls traversal succeeds on 7/8 questions at top 5 and top 10, compared with 4/8 at top 5 and 6/8 at top 10 for both original and normalized search. MRR@10 rises from 0.4427 to 0.6250. Top-one success remains 3/8. These are tuning results on a small set, not held-out proof.

All three runs use the baseline chunk/context treatment and full KG. The directed replay reports 100,319 nodes, 590,664 edges (131,427 CallsFunction and 459,237 ModuleContains), and zero embedding calls. The earlier 100,000-node cap is not present in this replay's graph.

| Query | Success@10 | RR@10 | Seeds | Results | HTTP calls | Response bytes | Response tokens | Median warm ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| kg-01 | 1 | 0.500 | 1 | 2 | 3 | 35,517 | 9,113 | 836.9 |
| kg-02 | 1 | 1.000 | 1 | 1 | 3 | 22,929 | 6,093 | 804.3 |
| kg-03 | 1 | 1.000 | 1 | 2 | 4 | 48,503 | 12,368 | 793.7 |
| kg-05 | 1 | 0.500 | 1 | 2 | 4 | 66,067 | 18,258 | 761.3 |
| kg-07 | 1 | 0.500 | 1 | 2 | 4 | 55,423 | 15,428 | 1061.4 |
| kg-10 | 0 | 0.000 | 0 | 0 | 1 | 6,954 | 1,783 | 574.6 |
| kg-11 | 1 | 0.500 | 2 | 6 | 7 | 28,241 | 7,454 | 796.8 |
| kg-12 | 1 | 1.000 | 1 | 1 | 3 | 32,323 | 9,092 | 1056.8 |

## Real endpoint positives and failures

- `kg-01`: `plannable_grace` outgoing neighbors contain `termination_grace` at rank 2. Returned source materializes successfully through `/chunks`.
- `kg-02`: `termination_grace` reaches `termination_grace_from` at rank 1.
- `kg-03`: `loopback_client` reaches `loopback_client_builder` at rank 1. It also returns `IdleTracker::timeout` in an unrelated UDS module; the HTTP graph itself returned that edge. This is evidence of an imprecise graph call resolution, not a fabricated adapter result.
- `kg-05`: `to_canvas_markdown` reaches `parse_options` at rank 2 alongside `render_document`.
- `kg-07`: incoming calls to `longest_backtick_run` return `code_span` at rank 2 alongside `render_code_block`.
- `kg-11`: `inbox_root` produces two exact-name seeds in trusty-analyze and trusty-review. Both are traversed; six results include the wanted analyze `run` at rank 2. Query prose specifying analyze does not disambiguate the lexical seed stage.
- `kg-12`: incoming calls to `is_slack_mention` return `render_image` at rank 1.
- `kg-10` fails during seed selection. Lexical `socket_path` returns two unnamed chunks plus `SocketMonitor::socket_path`; none exactly equals `socket_path`. No graph request follows. This is a zero-seed failure, not evidence that the target graph relationship is absent.

No selected traversal exceeds three neighbors per seed; the maximum returned set has six neighbors across two seeds. This tuning set does not exercise the ten-neighbor truncation boundary or demonstrate high-fanout behavior. Most excess intermediate bytes come from enumerating a file's chunks, not graph neighbors.

## Intermediate cost

For one initial execution of each of the eight questions, the trace contains 29 HTTP responses totaling 295,957 normalized JSON bytes and 79,589 cl100k_base tokens: mean 36,994.6 bytes and 9,948.6 tokens per question.

| Response class | Calls | Bytes | Tokens |
|---|---:|---:|---:|
| Lexical seeds | 8 | 77,723 | 20,921 |
| Graph neighbors | 8 | 3,811 | 983 |
| Source chunk pages | 13 | 214,423 | 57,685 |

The final directed result array averages only 582.75 tokens, compared with 3,420.5 for the baseline full result array. Compact final output averages 207.625 versus 1,195.75 tokens. Those final-output savings exclude the intermediate requests above. The adapter returns its trace in the diagnostic envelope; delivering that envelope to an agent would expose the additional trace payload as well. Production integration would need to keep internal diagnostics separate from agent output.

`benchmark.size` reserializes parsed JSON without whitespace and counts cl100k_base tokens. These are normalized response-payload sizes, not captured wire bytes: HTTP headers, compression, request bodies, final adapter envelope overhead, card-rendering calls, and transport framing are excluded. Intermediate totals use the initial response traces, not three warm repetitions. No claim about billed model tokens follows unless those payloads are actually supplied to a model.

## Latency

Warm end-to-end graph query latency, pooled across 24 repeated executions:

| Treatment | p50 ms | p95 ms |
|---|---:|---:|
| Original search | 42.57 | 481.28 |
| Normalized search | 41.82 | 486.62 |
| Explicit directed adapter | 800.11 | 1,097.79 |

The adapter's latency includes lexical seed lookup, neighbors, and source materialization. It excludes the later presentation-card request, as do baseline measurements. Runs are separate replay processes rather than randomized simultaneous A/B timing; cache and host-load effects limit precise attribution. The observed median is about 18.8 times the original baseline. This prototype improves tuning retrieval and final output size while increasing end-to-end latency and intermediate payload costs.

## Interpretation

The direct endpoint validates that useful call relationships exist. The ordinary graph search's discounted-neighbor merge can keep those relationships outside top ten, while the adapter exposes them explicitly. The result supports a bounded graph-query route and compact source retrieval as further experiments; it does not justify making this multi-request diagnostic adapter the default search path. Parent continues with selected-treatment and held-out evaluation.
