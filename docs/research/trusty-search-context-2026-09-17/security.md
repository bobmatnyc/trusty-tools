# Security verdict: APPROVE for isolated local measurements

Read-only review of the experiment diff, Rust helpers/tests, HTTP runner, `benchmark.py`, and `run_treatment.py` against the three protocol documents. The HIGH isolation finding below is resolved in the current runner. No unresolved security finding blocks the authorized local measurements. No build, daemon launch, network request, or source edit was performed by this reviewer.

## Resolved finding

**HIGH, resolved — extra creation fields could select files outside the marked corpus.** The former guard forwarded unchecked `include_paths` into the shared handler's path join (`worktree/crates/trusty-search/src/service/server/indexes.rs:581`) and subtree walk (`worktree/crates/trusty-search/src/service/index_admission.rs:26`). An absolute or parent-relative path could cause reads beyond the experiment boundary. The normal Python driver omitted these fields; no escaped run was observed.

The current guard at `worktree/crates/trusty-search/examples/context_experiment_daemon.rs:148` accepts only the four required creation fields and only `force`/`root_path` for reindex. It rejects nonobjects and every additional field before the shared handler. Existing exact-root and zero-vector checks remain. The regression loop at `worktree/crates/trusty-search/examples/support/context_experiment_daemon_tests.rs:95` verifies eight rejection cases: absolute/parent-relative `include_paths`, `follow_links`, and `allow_sensitive_path`, each against creation and reindex. This closes the reported experiment entry point without changing production handlers.

## Reviewed controls and limits

- Startup canonicalizes marked, disjoint roots, rejects corpus symlinks and existing registries, and binds only explicit `127.0.0.1` ports other than 7878/0. Registry and allowlist paths are local to the experiment. Lifecycle, configuration mutation, chat, and arbitrary file-write routes are blocked.
- Python launches and stops only its owned child process using argument arrays. It creates fresh treatment directories, removes archive symlinks, supplies isolated data directories, and checks the responding daemon's data path before index creation. The archive and local filesystem are trusted inputs; this is not a sandbox against a hostile same-user process.
- RejectingEmbedder loads no model and counts both forbidden methods. Corrected guards cover bulk ingestion, query/refinement, warmup, and metadata refresh. The evaluator rejects nonzero counters and incomplete indexes before accepting results. Supplied smoke evidence demonstrates a small complete reindex and lexical/graph requests with zero calls; each full treatment still requires its own evidence.
- Cards serialize supplied source metadata without filesystem access or code execution. Targeted source inspection found no embedded credentials. This was not a dependency/CVE audit or complete secret scan.

## Verification evidence

Observed existing logs, not newly executed tests:

- `results/tests.log`: `test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 2198 filtered out; finished in 0.17s`
- `results/example-tests.log`: `test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- `results/python-tests.log`: `14 passed in 0.09s`
- `results/python-types.log`: `Success: no issues found in 2 source files`

Additional supplied evidence on re-review:

- `results/full-tests.log`: 36 target summaries all report `ok`; parsed aggregate is `2834 passed; 0 failed; 43 ignored; 0 measured; 0 filtered out`. Main library raw result: `test result: ok. 2205 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 61.14s`. Ignored cases include model/hardware/real-daemon benchmarks, installation, migrations, and performance fixtures; those scenarios are not established by this run.
- Fresh `results/example-tests.log`, including the eight new rejection cases: `test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`.
- `smoke/evidence.json`: `embedding_calls: 0`, `vectors_present: 0`, `chunk_count: 3`, lexical and graph stages `ready`, and saved successful lexical/graph responses. This small smoke precedes the guard-only patch and does not establish full-corpus treatment results.

OWASP coverage is limited to access/path boundaries, injection, configuration, and outbound-request review; no broad compliance certification is implied.

Parent continues with full treatment measurements using the patched runner, requiring complete indexing and zero-call evidence for every accepted result. Preserve the local-only scope; no installation, deployment, or production index/allowlist mutation is authorized.
