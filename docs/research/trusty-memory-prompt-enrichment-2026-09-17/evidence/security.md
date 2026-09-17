# Security review: isolated prompt-enrichment experiment

Reviewed 2026-09-17 in `/Users/masa/trusty-search-experiment/worktree`.

## Summary and gate

**PASS for the requested zero HIGH/CRITICAL gate:** no HIGH or CRITICAL issue identified in the trusted, local, synthetic-fixture experiment. Two LOW hardening observations follow. This is a bounded source and local-runtime review, not a production security certification or a current CVE clearance.

Scope: all ten `experiments/trusty-memory-prompt-enrichment/*.py` files, its `requirements.txt`, `crates/trusty-memory/examples/memory_prompt_probe.rs`, and the frozen protocol. README and installed package metadata supplied supporting context. No source edits, Git mutations, dependency installs, model downloads, live-store access, daemon changes, benchmark execution, or heldout evaluation occurred.

## Findings and remediation

1. **LOW — helper requests can hang indefinitely.** `experiments/trusty-memory-prompt-enrichment/adapters.py:35` blocks on `readline()` without a response deadline. `adapters.py:48` calls that same blocking request before the ten-second process wait, so the close timeout cannot rescue a silent or wedged helper. Impact is an indefinitely hung local experiment; the input stream is not remotely exposed. Add a bounded IPC deadline and terminate/reap the child after expiration. Ensure failed startup handshakes also reap the child. This does not block the isolated experiment's HIGH/CRITICAL gate.

2. **LOW — installation is only partially reproducible.** `experiments/trusty-memory-prompt-enrichment/requirements.txt:1` begins six exact direct dependency pins, with no artifact hashes or transitive lock. Installed metadata confirms runtime transitive requirements including `huggingface-hub`, `requests`, `protobuf`, and `flatbuffers`; these can resolve differently on a fresh installation. Freeze a complete dependency lock with distribution hashes before treating setup as reproducible or reusing this as a shared operational tool. No compromised dependency was established.

## Controls verified

- Execution uses an explicit absolute helper path and an argument list with no shell (`adapters.py:22`). Source strings are JSON data, not commands. No eval, pickle, unsafe YAML, or dynamic library loading appears in the reviewed Python implementation.
- The Rust helper receives JSONL over stdin and returns JSON over stdout. Unknown fields and operations fail schema parsing. It creates a fresh temporary directory under the caller's scratch directory and opens only its own `synthetic.db` (`memory_prompt_probe.rs:103`). Projection names and source identities are not interpolated into filesystem paths.
- The runner supplies a new `TemporaryDirectory`, requires a previously nonexistent output directory, and writes result files using exclusive creation. The CLI helper/model/output paths and manifest are trusted operator inputs; this is not a sandbox for untrusted manifests, binaries, or direct helper clients.
- Model and tokenizer SHA-256 checks precede ONNX/JSON loading; ONNX uses `CPUExecutionProvider`. Prompt tokenization reads local cache bytes and checks their SHA-256 before parsing (`offline_encoding.py:24`). Missing artifacts fail without downloader fallback. No hosted inference or explicit network call appears in the reviewed code.
- Eligibility applies scope, deletion, observation cutoff, expiry, and validity checks (`records.py:233`) before graph projection construction (`projection.py:94`). All projection data is synthetic. These scope selectors are experiment controls, not user authentication or multi-tenant authorization.
- No credential patterns were found in scoped code, requirements, or protocol. Checks covered private-key headers, AWS key IDs, GitHub tokens, long OpenAI-style keys, and secret/password/bearer assignments. This was a targeted pattern review, not repository-history or high-entropy scanning.
- Installed direct versions match all six requirement pins. Metadata identifies MIT licenses for ONNX Runtime, tiktoken, pytest, and mypy; tokenizers declares an Apache license classifier; NumPy lists permissive BSD/MIT/0BSD/Zlib/CC0 expressions. No direct copyleft license indication was found. This is metadata inspection, not legal or transitive-license clearance.

## Verification and limitations

The full scoped Python suite ran with plugin autoload disabled, pytest cache disabled, Python bytecode writes disabled, and Python socket connection methods replaced with a failing guard. It used the real local helper/model and existing synthetic tests; it did not invoke `evaluate.run` or load benchmark gold.

Observed command result: **`EXIT=0`**. Raw execution log: `/tmp/graph-prompt-security-tests.log`. The parent already owns the normal test/mypy gate results. No Rust build or production-suite rerun was needed for this read-only review.

The Python socket guard does not instrument native ONNX or Rust syscalls and is not OS-level proof of zero egress. The helper has no socket/service initialization in the reviewed example. No packet capture or OS sandbox test was performed. SHA checks do not defend against a hostile same-user process replacing local files between verification and loading.

`pip-audit` is absent in the existing environment. No vulnerability database, latest-version comparison, transitive license audit, or Rust dependency audit was run. Consequently dependency advisory status and obsolescence remain unverified. Existing production library implementations called by the helper were not re-audited. Source prompt content is copied as evidence, so production adoption still needs an explicit untrusted-content/prompt-injection policy; no answering agent consumes these packets in this experiment.

OWASP coverage: access-control boundaries and injection/deserialization patterns reviewed within the local experiment; integrity controls and dependency configuration reviewed with limitations above. Web authentication, CSRF, XSS, SSRF endpoints, and web-session controls are not exposed by this experiment. Operational logging, production authorization, deployment hardening, and comprehensive vulnerable-component coverage are outside this gate.

## Handoff

Parent continues the experiment. Review is complete; no HIGH/CRITICAL remediation is required for the stated scope. Parent may address or explicitly retain the two LOW observations and should preserve the dependency-advisory and native-egress limitations in completion claims.
