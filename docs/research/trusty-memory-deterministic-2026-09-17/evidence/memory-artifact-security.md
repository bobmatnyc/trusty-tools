# PASS with triage — experiment artifact secret scan

Scope: current files beneath `experiments/trusty-memory-deterministic` and `docs/research/trusty-memory-deterministic-2026-09-17`, including copied review evidence. No live memory, databases, builds, Git mutations, or source edits were used.

Scanner: gitleaks 8.30.1. Command per snapshot batch (at most five files):
`gitleaks dir --no-banner --redact --exit-code 1 --report-format json --report-path <batch-findings.json> <batch-directory>`

Artifacts were copied into a private temporary directory. Every available `.json.gz` was decompressed, validated as JSON, and pretty-printed before scanning so secrets inside compressed payloads were inspected. All compressed originals and actual scan inputs have SHA256 hashes in the machine-readable manifest. Generated Python bytecode/test caches, `.venv`, and scratch directories were excluded.

Results: 61 source/document/artifact files covered, including 18 expanded gzip payloads; no decompression/JSON failures. Run-01 coverage is all 16 files (13 gzip payloads). Run-02 coverage is protocol.json and tuning-0 through tuning-4.json.gz (5 gzip payloads); later outputs are not included in this snapshot.

Exact scanner outcomes:

```text
batch-000: EXIT=0; findings=0
batch-001: EXIT=0; findings=0
batch-002: EXIT=0; findings=0
batch-003: EXIT=0; findings=0
batch-004: EXIT=0; findings=0
batch-005: EXIT=1; findings=1
batch-006: EXIT=0; findings=0
batch-007: EXIT=0; findings=0
batch-008: EXIT=0; findings=0
batch-009: EXIT=0; findings=0
batch-010: EXIT=0; findings=0
batch-011: EXIT=0; findings=0
batch-012: EXIT=0; findings=0
```

One finding: `generic-api-key`, `experiments/trusty-memory-deterministic/offline_encoding.py:10`. Verified false positive: CACHE_KEY is exactly SHA1 of `https://openaipublic.blob.core.windows.net/encodings/cl100k_base.tiktoken`, used as a public tokenizer cache filename. It is not a credential. No suppressions, baseline edits, or blanket ignores were applied. No unresolved secret findings in covered files; regex scanning cannot prove absence of all private data.

Two late documentation files (evidence/manifest.sha256 and verification.md) were added to batch-012 and scanned successfully. Original snapshot files were unchanged on recheck. These lists and the manifest define coverage; concurrent future output is not implicitly approved.

OWASP coverage: sensitive-data/secret exposure in packaged artifacts only. Other security checks are recorded in memory-security.md. Remediation: none for the approved false positive. Parent should scan subsequent run-02 outputs before treating the complete package as covered.

Snapshot directory: `/var/folders/7s/g9twvy8j0wl58ffgzsmrccv40000gp/T/memory-artifact-security-coytbbhw`.
Manifest: `/var/folders/7s/g9twvy8j0wl58ffgzsmrccv40000gp/T/memory-artifact-security-coytbbhw/manifest.json`.
Raw logs and redacted findings: one `.log` and `-findings.json` per batch in that directory.

Covered original paths and SHA256:

```text
36b9bc91968cb68f035c2d192e47cd6a5c298a4bfc4cb88a5ae43905aac15d7c  docs/research/trusty-memory-deterministic-2026-09-17/README.md
de80685b646e4740ff534f40e1eb78b1f583a7b9c74346260939bb6092b54d5e  docs/research/trusty-memory-deterministic-2026-09-17/amendment-01.md
8090e0ac2dc61c9cef5812424b79051e60ab55617f1b9abb043750ab500e84bd  docs/research/trusty-memory-deterministic-2026-09-17/evaluation.md
0bc4e4d9ce79dc0f1db67f60db9c8092eb37dc4cac168cc4f3f40aa15b751092  docs/research/trusty-memory-deterministic-2026-09-17/evidence/gate-invocations.json
9f113848ec8314ae90a62d09bf282670c9d3fc1768f2d222f88a4525f4fda66e  docs/research/trusty-memory-deterministic-2026-09-17/evidence/memory-changelog.log
8a876282bc1e7db4311f5c72b84c605f60a6e21a1b74eb300aa44f410bb533bf  docs/research/trusty-memory-deterministic-2026-09-17/evidence/memory-crate-clippy.log
ae4531f72ad2802692c9836ea36ecc11cbd9872ac8c0b2a5230a4deb7e56c5b2  docs/research/trusty-memory-deterministic-2026-09-17/evidence/memory-crate-tests.log
8a1ae1302012df17a282b3e869d05f3ffb133ecbcbc5f2f36369a1043a088b0a  docs/research/trusty-memory-deterministic-2026-09-17/evidence/memory-eval-review.md
c6512fdd833f841d1c129290e54f1f414318301cf65aacb2dabbed381c08fd4d  docs/research/trusty-memory-deterministic-2026-09-17/evidence/memory-rust-implementation.md
1212c7f66f8638021a920e6205927b6e1194ef969767598a94e2f48c938790a4  docs/research/trusty-memory-deterministic-2026-09-17/evidence/memory-rust-review.md
901654621955da64e7956a33fcd3770fd37677a75cee3650ca270b527261e9dc  docs/research/trusty-memory-deterministic-2026-09-17/evidence/memory-security-python-findings.json
c99ad562d0ca5408f9446f5e79e174639dd99b2aaabe5ab0a5bd27fae9e9df57  docs/research/trusty-memory-deterministic-2026-09-17/evidence/memory-security.md
52e35e6707ac74db66f96fe5c9e2a707c0153355eaae42c526be612a78ff959c  docs/research/trusty-memory-deterministic-2026-09-17/evidence/memory-v2-example-build.log
58ee3257d8ffadf98e65a1963b0c9d620549b75098ca64ee738b6e4c6d835928  docs/research/trusty-memory-deterministic-2026-09-17/evidence/memory-v2-example-check.log
3c25abe673d90e83449a0539332d009e55f834fad0067e3f59832db2377746a9  docs/research/trusty-memory-deterministic-2026-09-17/evidence/memory-v2-example-clippy.log
7d56648f7d314e3e5b6c079d5772e04c88dbc51b91dcc8c7c5b1b2a725b747f4  docs/research/trusty-memory-deterministic-2026-09-17/evidence/memory-v2-example-tests.log
dc2223de4873b785f9686f9f7c0c6c9c3ea2305734d6fc639a0e55f6f474eab9  docs/research/trusty-memory-deterministic-2026-09-17/evidence/memory-v2-fmt.log
c78d91696a6cf379dabde3617dd3579db52d5d1a72c6c4d95a893cc1414f50ae  docs/research/trusty-memory-deterministic-2026-09-17/evidence/memory-v2-line-cap.log
0dfaaad894ca6b383c0453195523b245c9df36b5e371e0d29e3e3ac19171f822  docs/research/trusty-memory-deterministic-2026-09-17/evidence/python-mypy.log
93d77b8b1797047e415579d368229c74a19b41d4a1564688b03b0e84be9a77d7  docs/research/trusty-memory-deterministic-2026-09-17/evidence/python-tests.log
581a8451378dcee514d37b95cc0abe3af38cef913f00d0ffb59cddfebac21b3f  docs/research/trusty-memory-deterministic-2026-09-17/evidence/sld.log
bbcbc599c5632065f541c0d2d8d3123b1c6ccc00b2f5f02cad36a45042e78429  docs/research/trusty-memory-deterministic-2026-09-17/interface.md
b5f23ea80320de8f035b8f25e6e2ef48144c3d65295aedc6b592fcc7082be01f  docs/research/trusty-memory-deterministic-2026-09-17/report.md
e2cec40eadf1f23d79814b60517dce900b7966ab6520d6fd5127321414a6c8bb  experiments/trusty-memory-deterministic/.gitignore
b0db646053d86581fcb155ca4c9e70f522dff86d7bd1fac6cd7df0305eb36d92  experiments/trusty-memory-deterministic/README.md
8ca13778ba6eea3bdb395ee404cd694fcf979e2b380c9fabb721fd8aa56e68eb  experiments/trusty-memory-deterministic/evaluate.py
a222777264db7b9c02407195c10a08a503c401221614d70068a1245ae3e196be  experiments/trusty-memory-deterministic/fixture.json
6c6912b36ad3109187791a63620a268e967f571a57b60a875e007581c412f59d  experiments/trusty-memory-deterministic/manifest.sha256
ef7077f458f24cc872ed0853575df0af3445287b3eba093a90994e39b9a4e549  experiments/trusty-memory-deterministic/metrics.py
81f07dfef4413719b60020a2915ed4591e8115a6e94c74ccb542182609230697  experiments/trusty-memory-deterministic/offline_encoding.py
cfb4ea73e84e51f2719cb01ed879935b6d3fc819919750e9205b76ea1b0e848a  experiments/trusty-memory-deterministic/oracle.py
3514829cfbe8e81c0ee2b76a1f238dffc13ee501ee876eaf4ce8b894c51fbccd  experiments/trusty-memory-deterministic/queries.json
4669d2decdfe6f7c7acaff0dbff74abf0b5963e0051dee8bb26c3f921d921907  experiments/trusty-memory-deterministic/requirements.txt
30dae546eb51f3aa17608abd394535652fd7e22865dd6775e8fc97391527202d  experiments/trusty-memory-deterministic/results/run-01/heldout-chunks.json.gz
3538f73803f1b1534f29877313fe8331cebc5565c189b82c65434a409fbf8039  experiments/trusty-memory-deterministic/results/run-01/heldout-context.json.gz
51e6b362bd305a2a94f5484611eb4fac36d1fe1ba0f7442db5eebd7e34e71a69  experiments/trusty-memory-deterministic/results/run-01/heldout-kg.json.gz
dba31f9691b610285006c128e10f686256f72f82a7e5dbb07b9c8242ad1d405a  experiments/trusty-memory-deterministic/results/run-01/heldout-raw.json.gz
cb7ba30f11dd89a3bcaa1146d07ad97c1c275384d1d508e82c9f33197b2c859c  experiments/trusty-memory-deterministic/results/run-01/heldout-repaired_raw.json.gz
8af7889df455bc678cf6ddbf9a578829e10cd4519e0eb4c72b62fb6c11611d0f  experiments/trusty-memory-deterministic/results/run-01/heldout-temporal.json.gz
8e9720f47e49156efa9cdbab364ab345c2188a80066d3923937538c77a177f13  experiments/trusty-memory-deterministic/results/run-01/protocol.json
2817a0f34fd955e01cca67231393ce87123ce779a38f4021d903aac0ee1a545e  experiments/trusty-memory-deterministic/results/run-01/selection.json
c2d39c146cf2272c1ad6f304d0d70b22134f82a4d433defd19f033f8ad5c9678  experiments/trusty-memory-deterministic/results/run-01/summary.json
45ef8065f14cf4da5a2169bab1397df6be07cde6c24396f558ecd8229be978db  experiments/trusty-memory-deterministic/results/run-01/tuning-0.json.gz
9cd35eb8a2fdf691d497f80e2cdbcc201d974a1423cecd202ef91e242b927948  experiments/trusty-memory-deterministic/results/run-01/tuning-1.json.gz
7c83cbbe8f6c3ba2660b565846b98afc7d69a02ecc7382f464c81b5de006e068  experiments/trusty-memory-deterministic/results/run-01/tuning-2.json.gz
1a2b74ac3755ff3cc036a3f40a6840dfaf376203df09e36030f761f32ee7d971  experiments/trusty-memory-deterministic/results/run-01/tuning-3.json.gz
d5796ad9f39014e6a89a733a8916fbf5f877e70ac2d6603a4b7177e53c813bf3  experiments/trusty-memory-deterministic/results/run-01/tuning-4.json.gz
25e7d26a6c561f9d19ea5836a238f486b32109cd513803ce96f296e9a0e966de  experiments/trusty-memory-deterministic/results/run-01/tuning-5.json.gz
f016b1895e3ff1f02871b2be3f007025632d33397d65f170be3bfef142225631  experiments/trusty-memory-deterministic/results/run-01/tuning-6.json.gz
8ffa587152199e0de520cbef3f75f58f75991ab1f2bbc6a22f2f462e3e94a213  experiments/trusty-memory-deterministic/results/run-02/protocol.json
f80d0888dfa80998c6f50960aa865bf4c0c99abaa05338fdf2004d9d64bce5fa  experiments/trusty-memory-deterministic/results/run-02/tuning-0.json.gz
d4515d53e6419cb531c2a8a894a97c05e454e0b7d95fd819f034f4c5c541fd07  experiments/trusty-memory-deterministic/results/run-02/tuning-1.json.gz
46a2fba1ae48adcd34c7f830e390ee8349fdd7d1b5dbcd81d6893c4c752b4304  experiments/trusty-memory-deterministic/results/run-02/tuning-2.json.gz
3684a7fb4c7b077bc09fb19e1ad2771a0a676970fd896018e07a4cd4a4b7c0e5  experiments/trusty-memory-deterministic/results/run-02/tuning-3.json.gz
0018fcd5dce9f8b877f0e552e22720a6afaf6ca5dca47d17f3616098b235198e  experiments/trusty-memory-deterministic/results/run-02/tuning-4.json.gz
dbac83a7a020815227d6784aaea4152248d72bea13da8a3ece67bd20193dd058  experiments/trusty-memory-deterministic/test_evaluate.py
0c0cdf61bafcd61da7bb865395083a56837802fd9ed672db3b3e27d50f474406  experiments/trusty-memory-deterministic/test_metrics.py
50aeb5366379f4f865bd4ac931ff13ddf5fd25cb9a0e845a4f0dfcd664b14897  experiments/trusty-memory-deterministic/test_offline_encoding.py
5468fa747cc84e4aae9a65a4c9c4b42584776445a5d325d7f0a761f6d1c0e9c3  experiments/trusty-memory-deterministic/test_oracle.py
```

Status: VERIFIED for the listed snapshot. Parent continues packaging and adds this report after the scan.

Late files included in batch-012:

```text
7dbb1940ead71783a6dbf95a528607714737599328ab30ca9a1afe1fefc912c1  docs/research/trusty-memory-deterministic-2026-09-17/evidence/manifest.sha256
84842752d8b6c4296cc03cbad20326c215b6f0daf6d0cdd23cba005ba77c4fbb  docs/research/trusty-memory-deterministic-2026-09-17/verification.md
```

# Final packaging addendum — PASS with documented false positive

Run-02 completed. This addendum supersedes the earlier incomplete-run coverage note. All 16 run-02 files are now covered, including all 13 decompressed JSON gzip payloads. Combined run-01/run-02 coverage is 32 result files, including 26 expanded gzip payloads.

The final delta scan covered 10 new run-02 files and the updated research report.md. All other previously scanned files were unchanged when the delta snapshot was collected. Total files covered at that snapshot: 71. No gzip or JSON parsing failures occurred. Scanner and invocation match the original report; no suppression or ignore was added.

Raw final delta outcomes:

```text
batch-000: EXIT=0; findings=0
batch-001: EXIT=0; findings=0
batch-002: EXIT=0; findings=0
```

Final credential verdict: PASS for the recorded package snapshots. No unresolved credential findings. The earlier `generic-api-key` finding in offline_encoding.py:10 remains the single known false positive: CACHE_KEY is SHA1 of the fixed public tokenizer URL, not a credential. The original raw scan remains accurately recorded as EXIT=1; the final delta batches all returned EXIT=0.

Final delta snapshot directory: `/var/folders/7s/g9twvy8j0wl58ffgzsmrccv40000gp/T/memory-artifact-security-final-zlpqu7hm`. Its scan-summary.json contains input hashes, expanded payload hashes, and batch outcomes. Files changed between this final snapshot and report generation: []. Later edits to README/report documentation are not implicitly scanned; their reviewed hashes below and in the original manifest identify exact coverage. No source/build/Git/ticket mutations were performed. Only this report and its evidence manifest entry are written for the handoff.

Final delta paths and SHA256:

```text
a98bd28901927437059fb72c02e506ca84489404212076d5162b6f518f9794de  docs/research/trusty-memory-deterministic-2026-09-17/report.md
6748a992c90f4a9ef1a4abdaeaf0cf988bb60d3de6ecda2523b7bec0cafb545f  experiments/trusty-memory-deterministic/results/run-02/heldout-chunks.json.gz
bbbb5f90d5049494c42d2706ea14a3453dcdc2472736eb8a9f48759a64aa1563  experiments/trusty-memory-deterministic/results/run-02/heldout-context.json.gz
375ff67d95f4e126f39e28850c3958066ca11ae3f426d471538deaccf5092c59  experiments/trusty-memory-deterministic/results/run-02/heldout-kg.json.gz
041404ad5cc92061db768d0e3aa0667c8ef679b140f0d17649673e2a6beddeda  experiments/trusty-memory-deterministic/results/run-02/heldout-raw.json.gz
d99b5c7073324cfc5e6891a0cc71bb210b3469bbba6f967b46cb47609d6ed127  experiments/trusty-memory-deterministic/results/run-02/heldout-repaired_raw.json.gz
e2522ed16be8578f6c690b1090732849bded614ed895e7232001e8038024dc9a  experiments/trusty-memory-deterministic/results/run-02/heldout-temporal.json.gz
44022585e92fbd50d220929b7d165d0a46e32d1735e3f16f84cb8e88b8f5dee4  experiments/trusty-memory-deterministic/results/run-02/selection.json
8ea2bb3861df1008ec6e7c5359311aee53642130ccd51377c63ec4acd86109c8  experiments/trusty-memory-deterministic/results/run-02/summary.json
16aae722c42b6ac2e9571a99e95773e2f79a2e41c6252d29201856c17d1fa802  experiments/trusty-memory-deterministic/results/run-02/tuning-5.json.gz
810e7024e6241735ae26a5ff1498d0fc011a6635bede0b0203ef6fca068092be  experiments/trusty-memory-deterministic/results/run-02/tuning-6.json.gz
```

Complete run-02 paths and SHA256 (including earlier unchanged snapshot coverage):

```text
6748a992c90f4a9ef1a4abdaeaf0cf988bb60d3de6ecda2523b7bec0cafb545f  experiments/trusty-memory-deterministic/results/run-02/heldout-chunks.json.gz
bbbb5f90d5049494c42d2706ea14a3453dcdc2472736eb8a9f48759a64aa1563  experiments/trusty-memory-deterministic/results/run-02/heldout-context.json.gz
375ff67d95f4e126f39e28850c3958066ca11ae3f426d471538deaccf5092c59  experiments/trusty-memory-deterministic/results/run-02/heldout-kg.json.gz
041404ad5cc92061db768d0e3aa0667c8ef679b140f0d17649673e2a6beddeda  experiments/trusty-memory-deterministic/results/run-02/heldout-raw.json.gz
d99b5c7073324cfc5e6891a0cc71bb210b3469bbba6f967b46cb47609d6ed127  experiments/trusty-memory-deterministic/results/run-02/heldout-repaired_raw.json.gz
e2522ed16be8578f6c690b1090732849bded614ed895e7232001e8038024dc9a  experiments/trusty-memory-deterministic/results/run-02/heldout-temporal.json.gz
8ffa587152199e0de520cbef3f75f58f75991ab1f2bbc6a22f2f462e3e94a213  experiments/trusty-memory-deterministic/results/run-02/protocol.json
44022585e92fbd50d220929b7d165d0a46e32d1735e3f16f84cb8e88b8f5dee4  experiments/trusty-memory-deterministic/results/run-02/selection.json
8ea2bb3861df1008ec6e7c5359311aee53642130ccd51377c63ec4acd86109c8  experiments/trusty-memory-deterministic/results/run-02/summary.json
f80d0888dfa80998c6f50960aa865bf4c0c99abaa05338fdf2004d9d64bce5fa  experiments/trusty-memory-deterministic/results/run-02/tuning-0.json.gz
d4515d53e6419cb531c2a8a894a97c05e454e0b7d95fd819f034f4c5c541fd07  experiments/trusty-memory-deterministic/results/run-02/tuning-1.json.gz
46a2fba1ae48adcd34c7f830e390ee8349fdd7d1b5dbcd81d6893c4c752b4304  experiments/trusty-memory-deterministic/results/run-02/tuning-2.json.gz
3684a7fb4c7b077bc09fb19e1ad2771a0a676970fd896018e07a4cd4a4b7c0e5  experiments/trusty-memory-deterministic/results/run-02/tuning-3.json.gz
0018fcd5dce9f8b877f0e552e22720a6afaf6ca5dca47d17f3616098b235198e  experiments/trusty-memory-deterministic/results/run-02/tuning-4.json.gz
16aae722c42b6ac2e9571a99e95773e2f79a2e41c6252d29201856c17d1fa802  experiments/trusty-memory-deterministic/results/run-02/tuning-5.json.gz
810e7024e6241735ae26a5ff1498d0fc011a6635bede0b0203ef6fca068092be  experiments/trusty-memory-deterministic/results/run-02/tuning-6.json.gz
```

Status: VERIFIED for all listed experiment artifacts. Parent continues the Git handoff. This credential scan is scoped to artifact secret detection, not an assertion of full OWASP compliance.
