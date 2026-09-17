# Relevance experiment security review

Reviewed 2026-09-17, after the parent confirmed final source freeze, in `/Users/masa/trusty-search-experiment/worktree`.

## Summary and gate

**PASS — zero new HIGH/CRITICAL findings in the isolated, trusted-fixture scope.** No new security remediation is required before this experiment's evaluation. The two previously reviewed LOW observations remain: unbounded helper-response waiting and direct-only dependency pins without a transitive artifact lock. They are retained limitations, not newly discovered regressions.

Scope: all eleven Python files and requirements in `experiments/trusty-memory-relevance/`, plus its protocol and interface. Focus was checkpoint deserialization, maintenance publication, source fallback, evaluator separation, execution/network paths, and credential patterns. The previous graph-prompt implementation and Rust helper were reviewed in `/tmp/graph-prompt-security.md`; their broad dependency/security review was not repeated. No repository source edits, Git operations, tickets, installations, downloads, benchmark runs, official fixture reads, or official gold reads were performed by this review.

## Findings and controls

- **No unsafe checkpoint deserialization found.** `maintenance.py:44` parses JSON and constructs typed data. No eval, pickle, executable object restoration, or path-controlled module import is used. `maintenance.py:35` requires exact derived content equality with authoritative source data, in addition to revision, policy and digest agreement. A corrupted derived claim survived parsing only as an invalid record in the independent corruption check; it was not eligible for derived retrieval.
- **Fallback preserves eligibility.** `relevance_index.py:39` creates eligible evidence first. `relevance_index.py:66` marks invalid/missing derived sources, and claim documents include only eligible evidence from valid records. `relevance.py:28` maps source fallback hits back to the eligible by-source records. Stale/partial/old-policy derived records do not authorize arbitrary checkpoint text for emission. The source-ranking text can contain ineligible claim text, as explicitly disclosed in the interface; the emitted evidence remains filtered.
- **Maintenance stages before publication.** `maintenance.py:86` copies state, checks conflicting replays and revision/tombstone watermarks, and publishes only after derivation completes at `maintenance.py:141`. Failed derivation tests preserve state. The checkpoint is an in-memory JSON roundtrip, not a crash-durable database transaction or authenticated external interchange format.
- **Gold is evaluator-only.** `contracts.py:10` exposes prompt, scope and two clocks to retrieval. `evaluate.py:80` scores after retrieval and packet construction. `evaluate.py:172` writes selection before parsing heldout gold at line 173. Manifest verification hashes fixture bytes earlier; this is not ranking access to parsed gold. This ordering was source-reviewed; no official evaluation was executed.
- **No encoder construction or remote inference path found.** The import bridge resolves its sibling directory from `__file__`, and imports only prior reusable modules. ONNX/tokenizer packages remain import dependencies, but no embedding adapter is instantiated. Requirements contain exactly the previous six pinned dependencies, with no new pins. Existing helper subprocess execution and exclusive result creation are retained.
- **Final corrections preserve safety boundaries.** `relevance.py:75` includes alias support in unknown-intent groups and rejects absent candidate support. `relation_support.py:66` enforces the visited-node cap before accepting a new seed. `metrics.py:42` counts requested entity bindings, and the evaluator supplies parsed demands. Updated contracts preserve the support entity through consolidation. Final regression tests exercise these paths.
- **Credential pattern scan: no matches.** Scoped code, requirements, protocol and interface produced `SCOPED_FILES 14` and `CREDENTIAL_PATTERN_FILES []`. Patterns covered private-key headers, AWS/GitHub/OpenAI-style tokens and credential assignments. Official fixture data and repository history were intentionally not scanned.

File references above are relative to `experiments/trusty-memory-relevance/` and were reread against the frozen source.

## Verification Results

### What changed

No repository files changed during review. Only `/tmp` review and verification artifacts were created.

### Verification performed

Command: `PYTHONDONTWRITEBYTECODE=1 PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 /Users/masa/trusty-search-experiment/venv/bin/python /tmp/relevance-security-guard.py`.

Observed result: **`EXIT=0`**. Log: `/tmp/relevance-security-guard.log`.

The script runs the entire independent `test_relevance.py` suite with pytest cache disabled. An audit hook rejects official sources/queries/gold file reads and Python socket connections/DNS. Guards reject both `LocalEncoder` construction and ONNX `InferenceSession` construction. The script also fails if any forbidden attempt was caught internally by a test, and independently checks forged-derived-claim invalidation after checkpoint restore. The guarded execution succeeded after final freeze. Parent-owned normal verification is recorded separately as 16 tests passing and mypy clean for ten modules; those are parent observations, not substitute evidence for this guarded run.

### Status: VERIFIED within the stated local experiment scope

## Limits and OWASP coverage

Checkpoints, manifests, module search paths, helper binaries, CLI paths and source records remain trusted local inputs. Checkpoint sources and replay watermarks are not authenticated, JSON resource sizes are not bounded for hostile input, and scope selectors are not authentication. Retain that local boundary; production reuse needs separately specified input limits and authenticated state ownership.

Python guards do not prove that native libraries or the Rust child cannot make syscalls. This was not an OS-level egress audit. No current advisory lookup, transitive license scan, dependency obsolescence check, production library re-audit, or installed-daemon verification occurred. Existing LOW remediation remains to add a helper IPC deadline with child cleanup and a complete hash-locked dependency set if operational reuse requires them.

OWASP topics covered: experiment-level access/scope separation, injection/deserialization, data integrity and unsafe execution. Vulnerable-component coverage is limited to confirming unchanged pins and prior review; no new CVE-clearance claim is made. The experiment exposes no web authentication, session, CSRF, XSS or SSRF endpoint. Arbitrary natural-language prompt injection is not solved by claim provenance; no answering model consumes these packets in this experiment.

## Handoff

Parent continues evaluation. No HIGH/CRITICAL issue blocks the stated gate. Preserve the inherited LOW observations and the native-egress, trusted-checkpoint and dependency-advisory limitations in completion claims.
