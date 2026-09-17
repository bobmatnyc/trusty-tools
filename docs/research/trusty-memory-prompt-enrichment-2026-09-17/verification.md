# Verification and measurement boundaries

The experiment adds a Rust example and isolated Python harness; it changes no production implementation or live store. The previous search and deterministic-memory experiments remain intact.

## Gates

Final focused Python tests: 15 passed. Strict mypy: nine source modules clean. Rust example: two tests passed, none ignored. Example check/build, clippy with warnings denied, formatting, line-cap, and repository test-pointer checks passed. Logs are retained in [evidence](evidence/).

The [initial independent review](evidence/critic-initial.md) found missing-vector fallback, batch failure atomicity, overly trusting packet scoring, and standing-token accounting defects. These were corrected before the first tuning or heldout ranking. Regression tests exercise missing and mixed vectors, retry after second-encoding failure, independently reconstructed actual prompt text, extra assertions, forged rendering metadata, token recounts, and standing-only packets. Standing coverage now uses eligible acceptable standing identities as its denominator; gold labels are unchanged.

## Deviations and interpretation

The frozen protocol is retained rather than rewritten after implementation. The runner measures five heldout repetitions after one warmup, but only one tuning repetition after one warmup. Latency is not part of policy selection. Treatment order is fixed and query order is rotated per treatment; this does not implement the protocol's intended treatment-order rotation, so thermal/load drift may affect between-treatment timings.

Eligibility is computed while constructing finite scope/time/scenario projections, outside query timing. Reported warm packet latency includes retrieval, entity resolution, fusion, formatting, token counting, and helper IPC where used, but excludes projection eligibility/construction, evaluation scoring, startup and corpus encoding. It is not arbitrary-clock production request latency.

The standing-cache control preformats only fixture facts explicitly marked standing, scoped and time-filtered by the adapter. It uses the public formatter but does not reproduce the production cache's global collection of all hot-predicate facts. The `current_lexical_graph` treatment uses the actual newest-200 native page operation followed by a replica of the private lexical selector; it is the protocol's `current_policy_replica` under a different machine label.

The helper is a debug build. Scaling native API times exclude helper IPC; end-to-end scaling times include it. Python bounded-graph times measure the experimental projection, not a new native Rust method. Parent-process peak RSS excludes helper memory and is not a per-treatment footprint.

Source revision updates and vector backfill have bounded batches, atomic publication and no-op tests. Lexical/graph projections are rebuilt outside query timing; this experiment does not implement incremental production graph publication or prove bounded total maintenance CPU. Sidecar provenance is validated but is outside the prompt token budget. No answer generation or downstream task-success evaluation runs.

## Completed run and independent audit

Run 01 completed with all eight heldout treatments and scaling probes. No labels or selection changed after heldout exposure. [Final critic](evidence/critic-final.md): APPROVE. [Security](evidence/security.md): zero HIGH/CRITICAL findings; two LOW hardening observations retained. [Independent results audit](evidence/results-audit.md): PASS. [Artifact audit](evidence/artifact-audit.log) verifies frozen hashes, evaluated source hashes, parsed compressed results and targeted credential scanning. The [result checksum manifest](../../../experiments/trusty-memory-prompt-enrichment/results/run-01/results.sha256) covers every original result file.
