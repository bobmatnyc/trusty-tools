# PASS — deterministic memory experiment security review

Scope: local synthetic JSON evaluator and Python runner in /Users/masa/trusty-search-experiment/worktree. No source edits, Git mutations, live databases, model calls, or network requests were performed.

Findings: no unresolved security findings within this experiment boundary. One LOW finding was fixed during review: evaluate.py previously called tiktoken.get_encoding at import and attempted an HTTPS download with a cold cache. The replacement offline_encoding.py:24 reads local bytes, line 25 verifies a pinned SHA256, and line 29 constructs the encoding directly. Missing/corrupt caches fail closed. Setup download is explicit in README.md:8.

Verification:
- Final Rust executable adversarial checks: `security_smoke: 8 passed; 0 failed`. Cases: BM25/KG scope isolation, opaque path identifiers, unknown request fields, atomic validation errors, policy cap, tombstone resurrection, JSONL recovery, invalid imported byte locator.
- Independent Python cold/corrupt/warm-cache probes: `offline_security: 3 passed; 0 failed; network calls=0`.
- `gitleaks dir --no-banner --redact --exit-code 1 crates/trusty-memory/examples/support/memory_deterministic`: `EXIT=0`.
- Initial gitleaks invocation for experiments/trusty-memory-deterministic: `EXIT=0`. Final scan after adding offline_encoding.py: `EXIT=1`, `leaks found: 1`. Verified false positive at offline_encoding.py:10: CACHE_KEY is exactly SHA1 of the fixed public tokenizer URL; it is a cache filename, not an API credential. Evidence: /tmp/memory-security-python-findings.json. No baseline/suppression or source edit was made.
- Tracked .secrets.baseline unchanged. No new dependency versions or full dependency vulnerability audit were part of this bounded review.

Boundary review: Rust consumes stdin and emits JSON; identifiers never become filesystem paths. Python subprocesses use argv lists, fixed engine flags, and a 60-second child timeout. Caller-selected binary/output paths remain operator authority. Query scope is checked before truncation and graph targets are resolved inside the same scope. The entire portable state is returned to the caller that supplied it; this is not an authentication or tenant-serving API.

Resource limits: publication budgets, top_k, policy caps, and KG candidate/edge selection caps are validated. Entire input/state is loaded and indexed, with no total RAM/CPU limit. This is acceptable for frozen local synthetic fixtures and does not establish suitability for an untrusted public service.

OWASP coverage: reviewed access-control-style scope filtering, injection, malformed input/state integrity, secret exposure, external I/O, and resource consumption. Web authentication, CSRF, browser XSS, production logging, and deployed configuration are outside this non-networked CLI scope. No general OWASP compliance certification is claimed.

Final binary SHA256: 32cacc0a86aad18d69b10c21dc66090f803f19592866904093c57566863a7c82

Final source manifest SHA256: 7292c1c53feb257b47185bce7de526af2f29ae0717d80bf4e6464edbcb0cd09e

```text
a84246195cc1b9cff2f814cb39fcbcb3b29a887b7652a1625dd116550bafb683  crates/trusty-memory/examples/memory_deterministic_eval.rs
bd69cf5c75642505d42314c77b92788130a1c8f53af871ba135579cbbdae3e42  crates/trusty-memory/examples/support/memory_deterministic/derive.rs
18c79422276016dcf700dabcbfd4b51377ab24266ba09f00a6fdc48de9ab0329  crates/trusty-memory/examples/support/memory_deterministic/maintain.rs
9202592fe05323f9eb13d54ca830c4aa40f96158e830f6355750fb3d4ab8d557  crates/trusty-memory/examples/support/memory_deterministic/mod.rs
c8315fc96714d9628c6456cf19ec99c69981d87faefd9962903a9cb1642faef2  crates/trusty-memory/examples/support/memory_deterministic/retrieval.rs
7c2087d5aa0c3dc4c1496797fb373e2c302768c9c453b96f1731113c2c7ecf20  crates/trusty-memory/examples/support/memory_deterministic/tests.rs
c4d0940ab1a144bd9c747783627d05578e3d139e86c3ce77065e70084b222df2  crates/trusty-memory/examples/support/memory_deterministic/types.rs
e1314e6c1031d231aba28428df50f401d1c432a6ce9ad1199e37ccc4dd663e51  crates/trusty-memory/examples/support/memory_deterministic/validate.rs
8ca13778ba6eea3bdb395ee404cd694fcf979e2b380c9fabb721fd8aa56e68eb  experiments/trusty-memory-deterministic/evaluate.py
ef7077f458f24cc872ed0853575df0af3445287b3eba093a90994e39b9a4e549  experiments/trusty-memory-deterministic/metrics.py
cfb4ea73e84e51f2719cb01ed879935b6d3fc819919750e9205b76ea1b0e848a  experiments/trusty-memory-deterministic/oracle.py
81f07dfef4413719b60020a2915ed4591e8115a6e94c74ccb542182609230697  experiments/trusty-memory-deterministic/offline_encoding.py
50aeb5366379f4f865bd4ac931ff13ddf5fd25cb9a0e845a4f0dfcd664b14897  experiments/trusty-memory-deterministic/test_offline_encoding.py
583f343d8218273d51c086e37ffec57a66e4cb60d73be3189663ee39858564b5  docs/research/trusty-memory-deterministic-2026-09-17/interface.md
```

Status: VERIFIED WORKING for the bounded security checks above. Parent agent continues the official evaluation and result interpretation.

# PASS — final v2 security delta addendum

This addendum supersedes the v1 binary/source provenance above for the current experiment. Reviewed amendment-01.md and the changed derive.rs, types.rs, tests.rs, and interface.md. All other previously recorded source hashes are unchanged, including the Python offline tokenizer guard, Rust I/O, state validation, maintenance, and retrieval modules.

Findings: no unresolved security findings in this delta. derive.rs:6 defines a 4096-byte source-body cap; derive.rs:10 counts repeated whitespace-delimited lexical runs; derive.rs:94 splits oversized runs only at UTF-8 scalar boundaries. types.rs:4 changes policy identity to v2, and validate.rs:150 rejects imported v1 state. The cap applies to child source-body ranges, not total indexed text after added context, total request size, or process memory. Prior local-fixture resource and authorization limits still apply.

Verification performed against the frozen v2 binary, without rebuilding or editing source:
- `chunk_security: 12 passed; 0 failed (lossless UTF-8 ranges, 4096-byte cap, repeated terms)`: four bodies (1000 repeated words, long Unicode runs, 9000 whitespace bytes, repeated paragraphs) across chunks, temporal, and kg. Every derived range was nonempty, contiguous, valid UTF-8, at most 4096 bytes, and collectively preserved the exact original body. Repeated-word children also stayed within the 32-occurrence test policy.
- `PASS v1 state rejected without replacement state`.
- `PASS split publication respects atomic byte budget`.
- `security_v2_delta: 14 passed; 0 failed`.
- Final Rust support gitleaks scan: `EXIT=0`. Python is unchanged; its previous single cache-key false-positive triage remains valid.

OWASP coverage delta: input/state integrity and resource-use controls reviewed. No new authentication, network, filesystem, subprocess, secret, or browser entry points. This remains a bounded local experiment review, not production compliance certification.

Final v2 binary SHA256: 679b2842c2891afa9707a58fb68f4fd01ec7f9bdf91c267b68d4f42f09a0cd19

Final v2 source manifest SHA256: 29f0e95d876a12d975e11c43968ffc66b352fd33e3c9b62edb41f187771e7bc5

```text
a84246195cc1b9cff2f814cb39fcbcb3b29a887b7652a1625dd116550bafb683  crates/trusty-memory/examples/memory_deterministic_eval.rs
3aa75c159c99a1fdc45e559ff15777867989764b1076338b6237473b3c8111f8  crates/trusty-memory/examples/support/memory_deterministic/derive.rs
18c79422276016dcf700dabcbfd4b51377ab24266ba09f00a6fdc48de9ab0329  crates/trusty-memory/examples/support/memory_deterministic/maintain.rs
9202592fe05323f9eb13d54ca830c4aa40f96158e830f6355750fb3d4ab8d557  crates/trusty-memory/examples/support/memory_deterministic/mod.rs
c8315fc96714d9628c6456cf19ec99c69981d87faefd9962903a9cb1642faef2  crates/trusty-memory/examples/support/memory_deterministic/retrieval.rs
be52bec977a914e693f12db8c8fdb3c24efb619d207b47d516be0f4c019745d7  crates/trusty-memory/examples/support/memory_deterministic/tests.rs
db68198ded4a3b4ebc5163210a1c58230e46f300d71607945c268bd66653f2ee  crates/trusty-memory/examples/support/memory_deterministic/types.rs
e1314e6c1031d231aba28428df50f401d1c432a6ce9ad1199e37ccc4dd663e51  crates/trusty-memory/examples/support/memory_deterministic/validate.rs
8ca13778ba6eea3bdb395ee404cd694fcf979e2b380c9fabb721fd8aa56e68eb  experiments/trusty-memory-deterministic/evaluate.py
ef7077f458f24cc872ed0853575df0af3445287b3eba093a90994e39b9a4e549  experiments/trusty-memory-deterministic/metrics.py
cfb4ea73e84e51f2719cb01ed879935b6d3fc819919750e9205b76ea1b0e848a  experiments/trusty-memory-deterministic/oracle.py
81f07dfef4413719b60020a2915ed4591e8115a6e94c74ccb542182609230697  experiments/trusty-memory-deterministic/offline_encoding.py
50aeb5366379f4f865bd4ac931ff13ddf5fd25cb9a0e845a4f0dfcd664b14897  experiments/trusty-memory-deterministic/test_offline_encoding.py
bbcbc599c5632065f541c0d2d8d3123b1c6ccc00b2f5f02cad36a45042e78429  docs/research/trusty-memory-deterministic-2026-09-17/interface.md
de80685b646e4740ff534f40e1eb78b1f583a7b9c74346260939bb6092b54d5e  docs/research/trusty-memory-deterministic-2026-09-17/amendment-01.md
```

Status: VERIFIED WORKING for the v2 security delta checks. Parent agent continues the serial benchmark and interpretation; no remaining security remediation from this review.
