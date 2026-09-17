# Verification evidence

Recorded 2026-09-17 for the isolated deterministic memory experiment and
`memory-deterministic-v2` policy. The final Rust review is **APPROVE**, the Python
review is **APPROVE**, and the final bounded security review is **PASS**.
This record covers implementation gates; benchmark results belong in the separate
experiment report. No production deployment or live daemon behavior is established.

## Gate results and timing

The full crate test run and all-target Clippy completed **before** the final fixes
confined to the example. The final v2 example was then checked, tested, linted,
built, and format-checked again. No production/core module changed in those fixes.
Do not describe the earlier full crate run as a rerun against the final example.

Commands below run from the repository root unless a different directory is named.
Original logs retain host paths and warnings. Relative links below are portable.
Successful exit statuses for copied Cargo logs are recorded in the
[implementation handoff](evidence/memory-rust-implementation.md); the fresh Python
and SLD logs contain their exit statuses directly.

| Gate | Command | Observed evidence |
|---|---|---|
| Full crate, before final example fixes | `cargo test -p trusty-memory --no-fail-fast --offline --locked -j6` | `EXIT=0`; 35 raw target summaries total **1071 passed, 0 failed, 16 ignored**. [Full log](evidence/memory-crate-tests.log) |
| Full crate Clippy, same earlier stage | `cargo clippy -p trusty-memory --all-targets --offline --locked -j6 -- -D warnings` | `EXIT=0`. [Log](evidence/memory-crate-clippy.log) |
| Final v2 example check | `cargo check -p trusty-memory --example memory_deterministic_eval --offline --locked -j6` | `v2-example-check EXIT=0`. [Log](evidence/memory-v2-example-check.log) |
| Final v2 example tests | `cargo test -p trusty-memory --example memory_deterministic_eval --no-fail-fast --offline --locked -j6` | `test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.45s`. [Log](evidence/memory-v2-example-tests.log) |
| Final v2 example Clippy | `cargo clippy -p trusty-memory --example memory_deterministic_eval --offline --locked -j6 -- -D warnings` | `v2-example-clippy EXIT=0`. [Log](evidence/memory-v2-example-clippy.log) |
| Final v2 example build | `cargo build -p trusty-memory --example memory_deterministic_eval --offline --locked -j6` | `v2-example-build EXIT=0`. [Log](evidence/memory-v2-example-build.log) |
| Final format check | `cargo fmt --all --check` | `v2-fmt EXIT=0`. Existing warnings concern nightly-only rustfmt settings. [Log](evidence/memory-v2-fmt.log) |
| Final line-cap subset | Project line-cap gate on the three amended Rust paths | `line-cap: measured 3 .rs and 0 .swift path(s) given (subset scan; CI runs the whole tree); 0 allowlisted, 0 violations — OK.` [Log](evidence/memory-v2-line-cap.log) |
| Changelog fragment | Project fragment validator | `OK   crates/trusty-memory/changelog.d/memory-deterministic-experiment.md: valid changelog fragment`. [Log](evidence/memory-changelog.log) |

The 16 existing ignored cases consist of four ONNX health tests, four load tests,
six performance-budget tests, and two child-process re-entry helpers. The full log
retains every name and reason. No test was newly ignored. Those cases were not
separately run with `--include-ignored`; ordinary-suite success does not establish
their standalone results. Two zero-test targets are visible in the full log and
are not counted as test coverage.

Fresh Python verification ran from `experiments/trusty-memory-deterministic` with
the existing experiment virtual environment, `PYTHONDONTWRITEBYTECODE=1`, and no
pytest cache. No dependencies were installed and no source files were edited.

```text
python -m pytest -q -p no:cacheprovider test_evaluate.py test_metrics.py test_oracle.py test_offline_encoding.py
14 passed in 0.18s
EXIT=0

python -m mypy --strict --no-incremental --cache-dir=/dev/null evaluate.py metrics.py oracle.py offline_encoding.py
Success: no issues found in 4 source files
EXIT=0
```

See [Python tests](evidence/python-tests.log), [mypy](evidence/python-mypy.log),
and [exact fresh invocations](evidence/gate-invocations.json). The existing SLD
binary was run directly against this worktree, without building another binary:

```text
sld-lint: scanned 68 spec doc(s) + 4168 code file(s); resolved 48 frontmatter + 79 inline reference(s), rejected 0 + 0 on path shape; 0 error(s), 0 warning(s)
EXIT=0
```

The [SLD log](evidence/sld.log) records that pass. The binary path and worktree root
are retained in the exact-invocations artifact for provenance.

## Review disposition

- [Rust implementation and gate handoff](evidence/memory-rust-implementation.md)
  records the original implementation and the final v2 chunk-bound amendment.
- [Rust review](evidence/memory-rust-review.md) retains its initial WARN and three
  findings, their APPROVE recheck, and the later **Policy v2 amendment recheck:
  APPROVE**. The last section is the disposition for the final policy.
- [Python evaluator review](evidence/memory-eval-review.md) retains its initial
  WARN and four findings, followed by **Final disposition after corrections:
  APPROVE**. It covers independent source replay, locator/revision checks, duplicate
  handling, scope-error counting, and full returned-excerpt measurement.
- [Security review](evidence/memory-security.md) includes the offline-tokenizer fix,
  adversarial checks, original provenance, and a **PASS — final v2 security delta
  addendum**. Its final source/binary hashes supersede the earlier v1 hashes.

The security addendum records `security_v2_delta: 14 passed; 0 failed`, including
rejection of v1 state and atomic publication-budget checks. Its final binary SHA256
is `679b2842c2891afa9707a58fb68f4fd01ec7f9bdf91c267b68d4f42f09a0cd19`.

## Secret-scan triage and security limits

The final Rust support scan returned `EXIT=0`. The Python scan returned `EXIT=1`
with one finding: `offline_encoding.py:10`, rule `generic-api-key`, on `CACHE_KEY`.
The value is the SHA-1-derived cache filename for the fixed public tokenizer URL,
not a credential. The reviewer verified that relationship and recorded a false
positive. [Redacted scanner finding](evidence/memory-security-python-findings.json)
preserves the location and rule. No scanner suppression or baseline edit was used.
The final security **PASS** includes this explicit triage; it does not mean the
raw Python gitleaks invocation returned zero findings.

The tokenizer reads pinned local bytes and checks their SHA-256. Missing or corrupt
cache files fail closed. Independent probes recorded
`offline_security: 3 passed; 0 failed; network calls=0`. Setup may explicitly fetch
the public table; evaluator execution has no download fallback.

The review covers a local synthetic CLI, with scope checks and input validation.
The caller supplies and receives the whole portable state. This is not a tenant
authentication boundary or an untrusted public service. Publication budgets do not
bound total request parsing, indexing, CPU, or RAM. The review does not constitute
a full dependency audit, deployment review, or general security certification.

## Evidence packaging

The [SHA-256 manifest](evidence/manifest.sha256) covers every packaged evidence file
except itself. Copies preserve original bytes, including earlier review findings
and host-specific paths. The summary provides relative navigation rather than
rewriting historical evidence. Each artifact is smaller than 1 MB.

After run-02 completed with exit 0, the [first run log](evidence/memory-evaluation-01.log)
and [final run log](evidence/memory-evaluation-02.log) were added. The final
[artifact security scan](evidence/memory-artifact-security.md) covers all 26
decompressed result payloads and reports no unresolved credential findings after
the documented cache-key false-positive triage. See the [experiment report](report.md)
for interpretation, remaining gaps, and production completion order.
The independent [report audit](evidence/memory-report-review.md) is **APPROVE**:
429 response records, 1,999 hits, all 13 implementation/interface hashes, and
all three frozen-input hashes were checked against the reported claims.
